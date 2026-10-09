use local_image_core::batch::{
    BatchConfig, BatchEvent, BatchProcessor, SizeChange, TaskStatus, start_batch,
    start_batch_with_processor,
};
use local_image_core::import::{
    ImportOptions, ImportSkipReason, ImportedImage, InputSource, SourceFingerprint, import_paths,
};
use local_image_core::{
    CoreError, Format, ImageInfo, MIB, ProcessOptions, ProcessReport, ResizeMode, ResizeSpec,
    Result,
};
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

fn png(path: &Path, width: u32, height: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 93, 255])
    })
    .save_with_format(path, image::ImageFormat::Png)
    .unwrap();
}

fn item(path: &Path) -> ImportedImage {
    ImportedImage::from_file(path, PathBuf::from(path.file_name().unwrap())).unwrap()
}

fn config(output: &Path, workers: usize) -> BatchConfig {
    BatchConfig {
        output_directory: output.to_owned(),
        workers,
        memory_budget_bytes: 256 * MIB,
    }
}

fn names(directory: &Path) -> Vec<String> {
    let mut paths: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    paths.sort();
    paths
}

#[test]
fn recursive_import_deduplicates_overlapping_selection_and_excludes_output() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("inputs");
    let output = root.join("processed");
    png(&root.join("one.png"), 8, 8);
    png(&root.join("nested/two.png"), 8, 8);
    png(&output.join("old.png"), 8, 8);
    let selections = vec![
        root.clone(),
        root.join("one.png"),
        root.join("nested"),
        output.clone(),
    ];
    let report = import_paths(
        &selections,
        &ImportOptions {
            recursive: true,
            output_directory: output.clone(),
        },
    );
    assert_eq!(report.items.len(), 2);
    assert!(
        report
            .items
            .iter()
            .any(|i| i.relative_path == Path::new("nested/two.png"))
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.reason == ImportSkipReason::Duplicate)
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.reason == ImportSkipReason::OutputDirectory)
    );
    let shallow = import_paths(
        &[root],
        &ImportOptions {
            recursive: false,
            output_directory: output,
        },
    );
    assert_eq!(shallow.items.len(), 1);
}

#[cfg(unix)]
#[test]
fn directory_symlink_back_to_root_does_not_recurse() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("inputs");
    png(&root.join("one.png"), 8, 8);
    std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
    let report = import_paths(
        &[root],
        &ImportOptions {
            recursive: true,
            output_directory: temp.path().join("output"),
        },
    );
    assert_eq!(report.items.len(), 1);
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.reason == ImportSkipReason::DirectoryLink)
    );
}

#[cfg(windows)]
#[test]
fn directory_junction_back_to_root_does_not_recurse() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("inputs");
    png(&root.join("one.png"), 8, 8);
    let created = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.join("loop"))
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let report = import_paths(
        &[root],
        &ImportOptions {
            recursive: true,
            output_directory: temp.path().join("output"),
        },
    );
    assert_eq!(report.items.len(), 1);
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.reason == ImportSkipReason::DirectoryLink)
    );
}

#[test]
fn fingerprint_detects_same_size_and_restored_timestamp_changes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source.png");
    fs::write(&path, b"original").unwrap();
    let queued = item(&path);
    let before = queued.input.fingerprint.clone();
    fs::write(&path, b"modified").unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(before.modified.unwrap()))
        .unwrap();
    let after = SourceFingerprint::capture(&path).unwrap();
    assert_eq!(before.len, after.len);
    assert_eq!(before.modified, after.modified);
    assert_ne!(before.sha256, after.sha256);
    assert!(queued.input.verify().is_err());
    assert!(queued.refreshed_for_retry().unwrap().input.verify().is_ok());
}

#[test]
fn real_processing_never_overwrites_original_or_existing_output() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("image.png");
    png(&source, 20, 12);
    let original = fs::read(&source).unwrap();
    let prior_output = temp.path().join("image (1).png");
    fs::write(&prior_output, b"old result that must survive").unwrap();
    let a = item(&source);
    let b = item(&source);
    let handle = start_batch(
        vec![a, b],
        ProcessOptions::default(),
        config(temp.path(), 2),
    )
    .unwrap();
    let report = handle.join().unwrap();
    assert!(
        report
            .results
            .iter()
            .all(|r| matches!(r.status, TaskStatus::Success { .. }))
    );
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(
        fs::read(&prior_output).unwrap(),
        b"old result that must survive"
    );
    assert_eq!(
        names(temp.path()),
        [
            "image (1).png",
            "image (2).png",
            "image (3).png",
            "image.png"
        ]
    );
    let outputs: Vec<_> = report
        .results
        .iter()
        .map(|r| r.output().unwrap().path.clone())
        .collect();
    assert_ne!(outputs[0], outputs[1]);
    for output in outputs {
        assert_eq!(image::image_dimensions(output).unwrap(), (20, 12));
    }
}

#[test]
fn continuation_reads_previous_output_and_does_not_require_original() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.png");
    png(&source, 20, 12);
    let original = item(&source);
    let half = ProcessOptions {
        resize: ResizeSpec {
            mode: ResizeMode::Percent(50.0),
            allow_upscale: false,
        },
        ..Default::default()
    };
    let first = start_batch(
        vec![original.clone()],
        half.clone(),
        config(&temp.path().join("out"), 1),
    )
    .unwrap()
    .join()
    .unwrap();
    assert_eq!(
        image::image_dimensions(&first.results[0].output().unwrap().path).unwrap(),
        (10, 6)
    );
    let next = first.results[0].continue_adjusting().unwrap();
    assert_eq!(next.input_source, InputSource::PreviousOutput);
    assert_eq!(next.original.path, original.original.path);
    assert_eq!(next.readjust().unwrap().input.path, original.original.path);
    fs::remove_file(&source).unwrap();
    assert!(next.readjust().is_err());
    let continued = first.results[0].continue_adjusting().unwrap();
    let second = start_batch(vec![continued], half, config(&temp.path().join("out"), 1))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(
        image::image_dimensions(&second.results[0].output().unwrap().path).unwrap(),
        (5, 3)
    );
    fs::remove_file(&first.results[0].output().unwrap().path).unwrap();
    assert!(first.results[0].continue_adjusting().is_err());
}

/// Exercises the scheduler at its codec boundary. This adapter copies real PNG
/// inputs and injects faults after opening the temporary output.
#[derive(Default)]
struct ControlledProcessor {
    entered: Option<mpsc::Sender<()>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    completed: AtomicUsize,
}

impl BatchProcessor for ControlledProcessor {
    fn probe(&self, path: &Path) -> Result<ImageInfo> {
        local_image_core::probe(path)
    }
    fn estimate(&self, info: &ImageInfo, _: &ProcessOptions) -> Result<u64> {
        Ok(if info.width == 999 {
            64 * MIB
        } else {
            16 * MIB
        })
    }
    fn process(
        &self,
        input: &Path,
        output: &Path,
        _: &ProcessOptions,
        cancel: &AtomicBool,
    ) -> Result<ProcessReport> {
        fs::copy(input, output)?;
        let name = input.file_stem().unwrap().to_string_lossy();
        match name.as_ref() {
            "panic" => panic!("deliberate codec panic"),
            "fail" => {
                return Err(CoreError::Io(std::io::Error::other(
                    "deliberate encoder error",
                )));
            }
            "change" => {
                let mut bytes = fs::read(input)?;
                *bytes.last_mut().unwrap() ^= 1;
                fs::write(input, bytes)?;
            }
            "block" => {
                self.entered.as_ref().unwrap().send(()).unwrap();
                while !cancel.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                // Return normally to check the scheduler's final cancellation gate.
            }
            "larger" => {
                use std::io::Write;
                fs::OpenOptions::new()
                    .append(true)
                    .open(output)?
                    .write_all(b"larger output")?;
            }
            _ => {}
        }
        let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(current, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(15));
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.completed.fetch_add(1, Ordering::SeqCst);
        let info = local_image_core::probe(input)?;
        Ok(ProcessReport {
            format: Format::Png,
            width: info.width,
            height: info.height,
            input_bytes: fs::metadata(input)?.len(),
            output_bytes: fs::metadata(output)?.len(),
            elapsed_ms: 0,
            warnings: Vec::new(),
            unchanged: name != "larger",
        })
    }
}

#[test]
fn cancel_preserves_completed_output_removes_temporary_and_stops_queue() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out");
    let inputs: Vec<_> = ["first", "block", "queued"]
        .iter()
        .map(|name| {
            let path = temp.path().join(format!("{name}.png"));
            png(&path, 8, 8);
            item(&path)
        })
        .collect();
    let (entered, observed) = mpsc::channel();
    let processor = Arc::new(ControlledProcessor {
        entered: Some(entered),
        ..Default::default()
    });
    let handle = start_batch_with_processor(
        inputs,
        ProcessOptions::default(),
        config(&output, 1),
        processor.clone(),
    )
    .unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(output.join("first.png").exists());
    handle.cancel();
    let report = handle.join().unwrap();
    assert!(report.cancelled);
    assert!(matches!(
        report.results[0].status,
        TaskStatus::Success { .. }
    ));
    assert!(matches!(report.results[1].status, TaskStatus::Cancelled));
    assert!(matches!(report.results[2].status, TaskStatus::Cancelled));
    assert!(report.results[1].continue_adjusting().is_err());
    assert_eq!(names(&output), ["first.png"]);
    assert_eq!(processor.completed.load(Ordering::SeqCst), 2);
}

#[test]
fn panic_error_and_source_change_are_isolated_and_never_commit_partial_files() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("fail.png"), b"old batch result").unwrap();
    let inputs: Vec<_> = ["panic", "fail", "change", "success"]
        .iter()
        .map(|name| {
            let path = temp.path().join(format!("{name}.png"));
            png(&path, 8, 8);
            item(&path)
        })
        .collect();
    let mut batch_config = config(&output, 2);
    batch_config.memory_budget_bytes = 16 * MIB;
    let handle = start_batch_with_processor(
        inputs,
        ProcessOptions::default(),
        batch_config,
        Arc::new(ControlledProcessor::default()),
    )
    .unwrap();
    let report = handle.join().unwrap();
    assert!(
        report.results[..3]
            .iter()
            .all(|r| matches!(r.status, TaskStatus::Failed { .. }))
    );
    assert!(matches!(
        report.results[3].status,
        TaskStatus::Success { .. }
    ));
    assert_eq!(names(&output), ["fail.png", "success.png"]);
    assert_eq!(
        fs::read(output.join("fail.png")).unwrap(),
        b"old batch result"
    );
    assert!(report.results[1].continue_adjusting().is_err());
    assert!(report.results[0].retry().is_ok());
    assert!(report.results[3].retry().is_err());
}

#[test]
fn memory_reservations_limit_concurrency_and_oversized_item_does_not_block_batch() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out");
    let mut inputs = Vec::new();
    for index in 0..8 {
        let path = temp.path().join(format!("item-{index}.png"));
        png(&path, if index == 2 { 999 } else { 8 }, 8);
        inputs.push(item(&path));
    }
    let processor = Arc::new(ControlledProcessor::default());
    let mut batch_config = config(&output, 4);
    batch_config.memory_budget_bytes = 32 * MIB;
    let report = start_batch_with_processor(
        inputs,
        ProcessOptions::default(),
        batch_config,
        processor.clone(),
    )
    .unwrap()
    .join()
    .unwrap();
    assert!(matches!(
        report.results[2].status,
        TaskStatus::Skipped { .. }
    ));
    assert_eq!(
        report
            .results
            .iter()
            .filter(|r| r.output().is_some())
            .count(),
        7
    );
    assert!(processor.peak.load(Ordering::SeqCst) <= 2);
    assert_eq!(processor.completed.load(Ordering::SeqCst), 7);
}

#[test]
fn size_unchanged_and_larger_are_successful_valid_continuation_sources() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out");
    let inputs: Vec<_> = ["same", "larger"]
        .iter()
        .map(|name| {
            let path = temp.path().join(format!("{name}.png"));
            png(&path, 8, 8);
            item(&path)
        })
        .collect();
    let report = start_batch_with_processor(
        inputs,
        ProcessOptions::default(),
        config(&output, 2),
        Arc::new(ControlledProcessor::default()),
    )
    .unwrap()
    .join()
    .unwrap();
    assert!(matches!(
        report.results[0].status,
        TaskStatus::Success {
            change: SizeChange::Unchanged,
            ..
        }
    ));
    assert!(matches!(
        report.results[1].status,
        TaskStatus::Success {
            change: SizeChange::Larger,
            ..
        }
    ));
    assert!(
        report
            .results
            .iter()
            .all(|r| r.continue_adjusting().is_ok())
    );
}

#[test]
fn changed_input_reports_failure_and_explicit_retry_uses_new_fingerprint() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.png");
    let output = temp.path().join("out");
    png(&source, 8, 8);
    let imported = item(&source);
    png(&source, 12, 9);
    let failed = start_batch(
        vec![imported],
        ProcessOptions::default(),
        config(&output, 1),
    )
    .unwrap()
    .join()
    .unwrap();
    assert!(matches!(
        failed.results[0].status,
        TaskStatus::Failed { .. }
    ));
    assert!(names(&output).is_empty());
    let retried = failed.results[0].retry().unwrap();
    let success = start_batch(vec![retried], ProcessOptions::default(), config(&output, 1))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(
        image::image_dimensions(&success.results[0].output().unwrap().path).unwrap(),
        (12, 9)
    );
}

#[test]
fn corrupt_input_and_unwritable_subdirectory_leave_other_items_running_and_send_terminal_event() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("blocked"), b"a file, not a directory").unwrap();
    let bad = temp.path().join("corrupt.png");
    fs::write(&bad, b"not a valid image").unwrap();
    let good = temp.path().join("good.png");
    png(&good, 8, 8);
    let blocked = ImportedImage::from_file(&good, PathBuf::from("blocked/image.png")).unwrap();
    let handle = start_batch(
        vec![item(&bad), blocked, item(&good)],
        ProcessOptions::default(),
        config(&output, 2),
    )
    .unwrap();
    let mut terminal_items = 0;
    loop {
        match handle.events.recv_timeout(Duration::from_secs(10)).unwrap() {
            BatchEvent::ItemFinished(_) => terminal_items += 1,
            BatchEvent::Finished(_) => break,
            _ => {}
        }
    }
    assert_eq!(terminal_items, 3);
    let report = handle.join().unwrap();
    assert!(matches!(
        report.results[0].status,
        TaskStatus::Failed { .. } | TaskStatus::Skipped { .. }
    ));
    assert!(matches!(
        report.results[1].status,
        TaskStatus::Failed { .. }
    ));
    assert!(report.results[2].output().is_some());
    assert_eq!(
        fs::read(output.join("blocked")).unwrap(),
        b"a file, not a directory"
    );
    assert_eq!(names(&output), ["blocked", "good.png"]);
}
