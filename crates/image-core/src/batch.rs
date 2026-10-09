//! Batch scheduling, resource reservations and no-clobber output commits.

use crate::import::{
    ImportedImage, SourceFingerprint, SourceRecord, path_is_within, validate_relative_path,
};
use crate::{CoreError, ImageInfo, MIB, ProcessOptions, ProcessReport, Result};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::TempPath;

#[derive(Clone, Debug)]
pub struct BatchConfig {
    pub output_directory: PathBuf,
    pub workers: usize,
    /// Shared reservation limit for all simultaneously active processing jobs.
    pub memory_budget_bytes: u64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            output_directory: PathBuf::from("output"),
            workers: thread::available_parallelism().map_or(1, |n| n.get().min(4)),
            memory_budget_bytes: 1536 * MIB,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeChange {
    Smaller,
    Unchanged,
    Larger,
}

#[derive(Clone, Debug)]
pub enum TaskStatus {
    Success {
        output: SourceRecord,
        report: ProcessReport,
        change: SizeChange,
    },
    Skipped {
        reason: String,
    },
    Failed {
        reason: String,
    },
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct TaskResult {
    pub item: ImportedImage,
    pub status: TaskStatus,
    pub elapsed_ms: u64,
}

impl TaskResult {
    pub fn output(&self) -> Option<&SourceRecord> {
        match &self.status {
            TaskStatus::Success { output, .. } => Some(output),
            _ => None,
        }
    }

    /// Failed, skipped and cancelled tasks cannot accidentally pick up an old file.
    pub fn continue_adjusting(&self) -> Result<ImportedImage> {
        let output = self.output().ok_or_else(|| {
            CoreError::Invalid("only a successful output can be continued".into())
        })?;
        self.item.continue_from(output).map_err(CoreError::Io)
    }

    pub fn retry(&self) -> Result<ImportedImage> {
        if !matches!(self.status, TaskStatus::Failed { .. }) {
            return Err(CoreError::Invalid(
                "only failed items are selected for failure retry".into(),
            ));
        }
        self.item.refreshed_for_retry().map_err(CoreError::Io)
    }
}

#[derive(Clone, Debug)]
pub struct BatchReport {
    pub options: ProcessOptions,
    pub config: BatchConfig,
    /// Ordered like the submitted inputs, independent of completion order.
    pub results: Vec<TaskResult>,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

#[derive(Clone, Debug)]
pub enum BatchEvent {
    Started { total: usize },
    ItemStarted { id: u64 },
    ItemFinished(Box<TaskResult>),
    Finished(Box<BatchReport>),
}

#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
    commit_gate: Arc<Mutex<()>>,
}

impl CancellationToken {
    /// Once this returns, no new output can be committed. An already committed
    /// output remains a success even if cancellation arrives immediately after it.
    pub fn cancel(&self) {
        let _guard = lock(&self.commit_gate);
        self.flag.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
    pub fn atomic(&self) -> &AtomicBool {
        &self.flag
    }
}

pub struct BatchHandle {
    pub events: mpsc::Receiver<BatchEvent>,
    cancellation: CancellationToken,
    join: JoinHandle<BatchReport>,
}

impl BatchHandle {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }
    pub fn join(self) -> thread::Result<BatchReport> {
        self.join.join()
    }
}

/// The scheduling boundary allows deterministic fault/cancellation tests and
/// alternative native codecs without duplicating the output safety rules.
pub trait BatchProcessor: Send + Sync + 'static {
    fn probe(&self, path: &Path) -> Result<ImageInfo>;
    fn estimate(&self, info: &ImageInfo, options: &ProcessOptions) -> Result<u64>;
    fn process(
        &self,
        input: &Path,
        output: &Path,
        options: &ProcessOptions,
        cancel: &AtomicBool,
    ) -> Result<ProcessReport>;
}

struct CoreProcessor;
impl BatchProcessor for CoreProcessor {
    fn probe(&self, path: &Path) -> Result<ImageInfo> {
        crate::probe(path)
    }
    fn estimate(&self, info: &ImageInfo, options: &ProcessOptions) -> Result<u64> {
        crate::estimate_memory(info, options)
    }
    fn process(
        &self,
        input: &Path,
        output: &Path,
        options: &ProcessOptions,
        cancel: &AtomicBool,
    ) -> Result<ProcessReport> {
        crate::process_to_path(input, output, options, cancel)
    }
}

pub fn start_batch(
    items: Vec<ImportedImage>,
    options: ProcessOptions,
    config: BatchConfig,
) -> Result<BatchHandle> {
    start_batch_with_processor(items, options, config, Arc::new(CoreProcessor))
}

pub fn start_batch_with_processor(
    items: Vec<ImportedImage>,
    options: ProcessOptions,
    mut config: BatchConfig,
    processor: Arc<dyn BatchProcessor>,
) -> Result<BatchHandle> {
    options.validate()?;
    if config.workers == 0 || config.workers > 64 {
        return Err(CoreError::Invalid(
            "worker count must be between 1 and 64".into(),
        ));
    }
    if config.memory_budget_bytes < 16 * MIB {
        return Err(CoreError::Invalid(
            "batch memory budget must be at least 16 MiB".into(),
        ));
    }
    fs::create_dir_all(&config.output_directory)?;
    config.output_directory = fs::canonicalize(&config.output_directory)?;
    let (sender, events) = mpsc::channel();
    let cancellation = CancellationToken::default();
    let thread_cancel = cancellation.clone();
    let join = thread::Builder::new()
        .name("image-batch".into())
        .spawn(move || run_batch(items, options, config, processor, thread_cancel, sender))?;
    Ok(BatchHandle {
        events,
        cancellation,
        join,
    })
}

fn run_batch(
    items: Vec<ImportedImage>,
    options: ProcessOptions,
    config: BatchConfig,
    processor: Arc<dyn BatchProcessor>,
    cancel: CancellationToken,
    events: mpsc::Sender<BatchEvent>,
) -> BatchReport {
    let began = Instant::now();
    let total = items.len();
    let worker_count = config.workers.min(total);
    let pending = Mutex::new(items.into_iter().enumerate().collect::<VecDeque<_>>());
    let finished = Mutex::new(vec![None; total]);
    let budget = Budget {
        used: Mutex::new(0),
        available: Condvar::new(),
        limit: config.memory_budget_bytes,
    };
    let _ = events.send(BatchEvent::Started { total });
    thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    let Some((index, item)) = lock(&pending).pop_front() else {
                        break;
                    };
                    let started = Instant::now();
                    let status = if cancel.is_cancelled() {
                        TaskStatus::Cancelled
                    } else {
                        let _ = events.send(BatchEvent::ItemStarted { id: item.id });
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            process_item(
                                &item,
                                &options,
                                &config,
                                processor.as_ref(),
                                &budget,
                                &cancel,
                            )
                        })) {
                            Ok(Ok(status)) => status,
                            Ok(Err(error)) => classify_error(error),
                            Err(payload) => {
                                let detail = payload
                                    .downcast_ref::<&str>()
                                    .copied()
                                    .or_else(|| {
                                        payload.downcast_ref::<String>().map(String::as_str)
                                    })
                                    .unwrap_or("unknown worker panic");
                                TaskStatus::Failed {
                                    reason: format!("image worker panicked: {detail}"),
                                }
                            }
                        }
                    };
                    let result = TaskResult {
                        item,
                        status,
                        elapsed_ms: millis(started.elapsed()),
                    };
                    lock(&finished)[index] = Some(result.clone());
                    let _ = events.send(BatchEvent::ItemFinished(Box::new(result)));
                }
            });
        }
    });
    let results = lock(&finished)
        .iter_mut()
        .map(|result| {
            result
                .take()
                .expect("every dequeued item produces a result")
        })
        .collect();
    let report = BatchReport {
        options,
        config,
        results,
        elapsed_ms: millis(began.elapsed()),
        cancelled: cancel.is_cancelled(),
    };
    let _ = events.send(BatchEvent::Finished(Box::new(report.clone())));
    report
}

fn process_item(
    item: &ImportedImage,
    options: &ProcessOptions,
    config: &BatchConfig,
    processor: &dyn BatchProcessor,
    budget: &Budget,
    cancel: &CancellationToken,
) -> Result<TaskStatus> {
    check_cancel(cancel)?;
    validate_relative_path(&item.relative_path)?;
    item.input
        .verify()
        .map_err(|_| CoreError::SourceChanged(item.input.path.clone()))?;
    let info = {
        // Container inspection is lightweight but may retain up to 16 MiB of
        // metadata. It shares the same pool so high worker counts cannot bypass
        // the budget while other jobs are decoding or encoding.
        let _inspection = budget.acquire(16 * MIB, cancel)?;
        processor.probe(&item.input.path)?
    };
    check_cancel(cancel)?;
    let estimate = processor.estimate(&info, options)?;
    let individual_limit = options.memory_limit_bytes.min(config.memory_budget_bytes);
    if estimate > individual_limit {
        return Err(CoreError::MemoryBudget {
            estimated: estimate,
            limit: individual_limit,
        });
    }
    let _permit = budget.acquire(estimate, cancel)?;
    // A queued item might have changed while waiting for another task's reservation.
    item.input
        .verify()
        .map_err(|_| CoreError::SourceChanged(item.input.path.clone()))?;
    check_cancel(cancel)?;
    let parent = item.relative_path.parent().unwrap_or_else(|| Path::new(""));
    let directory = create_output_subdirectory(&config.output_directory, parent)?;
    let temporary = tempfile::Builder::new()
        .prefix(".local-image-")
        .suffix(".tmp")
        .tempfile_in(&directory)?;
    let mut report =
        processor.process(&item.input.path, temporary.path(), options, cancel.atomic())?;
    check_cancel(cancel)?;
    item.input
        .verify()
        .map_err(|_| CoreError::SourceChanged(item.input.path.clone()))?;
    temporary.as_file().sync_all()?;
    // Close every writer before capturing the timestamp: Windows may defer the
    // final last-write time until the final writable handle has been closed.
    let temporary = temporary.into_temp_path();
    let fingerprint = SourceFingerprint::capture(&temporary)?;
    if fingerprint.len == 0 {
        return Err(CoreError::Image("encoder produced an empty output".into()));
    }
    let expected_format = options.output_format(&info);
    if report.format != expected_format {
        return Err(CoreError::Image(
            "encoder returned an unexpected output format".into(),
        ));
    }
    report.input_bytes = item.input.fingerprint.len;
    report.output_bytes = fingerprint.len;
    let change = match fingerprint.len.cmp(&item.input.fingerprint.len) {
        std::cmp::Ordering::Less => SizeChange::Smaller,
        std::cmp::Ordering::Equal => SizeChange::Unchanged,
        std::cmp::Ordering::Greater => SizeChange::Larger,
    };
    let stem = item
        .relative_path
        .file_stem()
        .ok_or_else(|| CoreError::Invalid("input has no file stem".into()))?;
    let path = commit_no_clobber(
        temporary,
        &directory,
        stem,
        report.format.extension(),
        cancel,
    )?;
    Ok(TaskStatus::Success {
        output: SourceRecord { path, fingerprint },
        report,
        change,
    })
}

fn commit_no_clobber(
    mut temporary: TempPath,
    directory: &Path,
    stem: &std::ffi::OsStr,
    extension: &str,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let mut number = 0_u64;
    loop {
        let mut name = stem.to_os_string();
        if number != 0 {
            name.push(format!(" ({number})"));
        }
        name.push(".");
        name.push(extension);
        let destination = directory.join(name);
        // This gate covers only the atomic filesystem commit, never encoding or hashing.
        let guard = lock(&cancel.commit_gate);
        check_cancel(cancel)?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => return Ok(destination),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                temporary = error.path;
                drop(guard);
                number = number
                    .checked_add(1)
                    .ok_or_else(|| CoreError::Invalid("too many output name conflicts".into()))?;
            }
            Err(error) => return Err(CoreError::Io(error.error)),
        }
    }
}

fn create_output_subdirectory(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut directory = root.to_owned();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(CoreError::Invalid(
                "output directory escapes its root".into(),
            ));
        };
        directory.push(name);
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let actual = fs::canonicalize(&directory)?;
        if !path_is_within(&actual, root) || !actual.is_dir() {
            return Err(CoreError::Invalid(
                "output subdirectory is not a directory inside the output root".into(),
            ));
        }
        directory = actual;
    }
    Ok(directory)
}

fn classify_error(error: CoreError) -> TaskStatus {
    match error {
        CoreError::Cancelled => TaskStatus::Cancelled,
        CoreError::Unsupported(_) | CoreError::MemoryBudget { .. } => TaskStatus::Skipped {
            reason: error.to_string(),
        },
        _ => TaskStatus::Failed {
            reason: error.to_string(),
        },
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(CoreError::Cancelled)
    } else {
        Ok(())
    }
}

struct Budget {
    used: Mutex<u64>,
    available: Condvar,
    limit: u64,
}
impl Budget {
    fn acquire(&self, amount: u64, cancel: &CancellationToken) -> Result<Permit<'_>> {
        if amount > self.limit {
            return Err(CoreError::MemoryBudget {
                estimated: amount,
                limit: self.limit,
            });
        }
        let mut used = lock(&self.used);
        loop {
            check_cancel(cancel)?;
            if *used <= self.limit - amount {
                *used += amount;
                return Ok(Permit {
                    budget: self,
                    amount,
                });
            }
            let (next, _) = self
                .available
                .wait_timeout(used, Duration::from_millis(25))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            used = next;
        }
    }
}

struct Permit<'a> {
    budget: &'a Budget,
    amount: u64,
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *lock(&self.budget.used) -= self.amount;
        self.budget.available.notify_all();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
