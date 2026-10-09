use crate::tool::ToolKind;
use eframe::egui;
use local_image_core::{
    ImageInfo, ProcessOptions,
    batch::{self, BatchConfig, BatchEvent},
    import::{self, ImportOptions, ImportedImage, SourceRecord},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

pub type Probed = (ImportedImage, Result<ImageInfo, String>);
pub enum Event {
    Paths(Vec<PathBuf>),
    OutputDirectory(Option<PathBuf>),
    Imported {
        items: Vec<Probed>,
        issues: Vec<String>,
    },
    Prepared {
        request_id: u64,
        tool: ToolKind,
        items: Vec<(u64, Result<Probed, String>)>,
        retry: bool,
    },
    Batch(BatchEvent),
    BatchClosed(Result<(), String>),
    CancelAccepted,
    Notice(String),
}

fn post(sender: &Sender<Event>, ctx: &egui::Context, event: Event) {
    let _ = sender.send(event);
    ctx.request_repaint();
}

pub fn pick_files(sender: Sender<Event>, ctx: egui::Context, folders: bool) {
    thread::spawn(move || {
        let dialog = rfd::FileDialog::new();
        let paths = if folders {
            dialog.pick_folders()
        } else {
            dialog
                .add_filter("Images", &["jpg", "jpeg", "png", "webp", "gif"])
                .add_filter("All files", &["*"])
                .pick_files()
        };
        post(&sender, &ctx, Event::Paths(paths.unwrap_or_default()));
    });
}

pub fn pick_output(sender: Sender<Event>, ctx: egui::Context, current: String) {
    thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new();
        if !current.is_empty() {
            dialog = dialog.set_directory(current);
        }
        post(&sender, &ctx, Event::OutputDirectory(dialog.pick_folder()));
    });
}

pub fn open_output(path: String, sender: Sender<Event>, ctx: egui::Context) {
    thread::spawn(move || {
        if let Err(error) = open::that(&path) {
            post(
                &sender,
                &ctx,
                Event::Notice(format!("Cannot open output folder: {error}")),
            );
        }
    });
}

pub fn import(
    paths: Vec<PathBuf>,
    options: ImportOptions,
    sender: Sender<Event>,
    ctx: egui::Context,
) {
    thread::Builder::new()
        .name("image-import".into())
        .spawn(move || {
            let report = import::import_paths(&paths, &options);
            let items = report
                .items
                .into_iter()
                .map(|item| {
                    let info = local_image_core::probe(&item.input.path).map_err(|e| e.to_string());
                    (item, info)
                })
                .collect();
            let issues = report
                .skipped
                .into_iter()
                .map(|issue| format!("{}: {:?}", issue.path.display(), issue.reason))
                .collect();
            post(&sender, &ctx, Event::Imported { items, issues });
        })
        .expect("import worker");
}

pub enum Prepare {
    Original,
    Continue(SourceRecord),
    Retry,
}

#[derive(Clone)]
pub struct PreparationRequest {
    pub id: u64,
    pub tool: ToolKind,
    pub cancelled: Arc<AtomicBool>,
}

type PreparedItem = (u64, Result<Probed, String>);

fn validate_preparation(item: ImportedImage, action: Prepare) -> Result<ImportedImage, String> {
    match action {
        Prepare::Original => item.readjust(),
        Prepare::Continue(output) => item.continue_from(&output),
        Prepare::Retry => item.refreshed_for_retry(),
    }
    .map_err(|error| error.to_string())
}

/// Keep the two file-reading stages separate so cancellation can stop before
/// probing a validated file and before opening the next file. Existing core
/// validation still hashes in bounded chunks; an in-flight read is not aborted.
fn collect_prepared(
    items: Vec<(ImportedImage, Prepare)>,
    cancelled: &AtomicBool,
    mut validate: impl FnMut(ImportedImage, Prepare) -> Result<ImportedImage, String>,
    mut probe: impl FnMut(&Path) -> Result<ImageInfo, String>,
) -> Option<Vec<PreparedItem>> {
    let mut prepared = Vec::with_capacity(items.len());
    for (item, action) in items {
        if cancelled.load(Ordering::Acquire) {
            return None;
        }
        let id = item.id;
        let next = validate(item, action);
        if cancelled.load(Ordering::Acquire) {
            return None;
        }
        let next = next.map(|item| {
            let info = probe(&item.input.path);
            (item, info)
        });
        prepared.push((id, next));
    }
    Some(prepared)
}

fn post_prepared(
    items: Option<Vec<PreparedItem>>,
    retry: bool,
    request: &PreparationRequest,
    sender: &Sender<Event>,
    ctx: &egui::Context,
) {
    let Some(items) = items else {
        return;
    };
    if request.cancelled.load(Ordering::Acquire) {
        return;
    }
    // The UI must also compare request_id/tool: cancellation can race with
    // sending, or an earlier result may already be waiting in the channel.
    post(
        sender,
        ctx,
        Event::Prepared {
            request_id: request.id,
            tool: request.tool,
            items,
            retry,
        },
    );
}

pub fn prepare(
    items: Vec<(ImportedImage, Prepare)>,
    retry: bool,
    request: PreparationRequest,
    sender: Sender<Event>,
    ctx: egui::Context,
) {
    thread::Builder::new()
        .name("image-next-step".into())
        .spawn(move || {
            let items = collect_prepared(items, &request.cancelled, validate_preparation, |path| {
                local_image_core::probe(path).map_err(|error| error.to_string())
            });
            post_prepared(items, retry, &request, &sender, &ctx);
        })
        .expect("next-step worker");
}

pub fn process(
    items: Vec<ImportedImage>,
    options: ProcessOptions,
    config: BatchConfig,
    preview_gate: Arc<Mutex<()>>,
    cancelled: Arc<AtomicBool>,
    sender: Sender<Event>,
    ctx: egui::Context,
) {
    thread::Builder::new()
        .name("batch-events".into())
        .spawn(move || {
            // Wait off the UI thread for an active preview to release its pixel buffer.
            // Holding this gate keeps full-image preview decoding outside batch peaks.
            let _preview_guard = preview_gate.lock().unwrap_or_else(|p| p.into_inner());
            let handle = match batch::start_batch(items, options, config) {
                Ok(handle) => handle,
                Err(error) => {
                    post(&sender, &ctx, Event::BatchClosed(Err(error.to_string())));
                    return;
                }
            };
            let mut cancel_sent = false;
            loop {
                if cancelled.load(Ordering::Acquire) && !cancel_sent {
                    handle.cancel();
                    cancel_sent = true;
                    post(&sender, &ctx, Event::CancelAccepted);
                }
                match handle.events.recv_timeout(Duration::from_millis(30)) {
                    Ok(event) => {
                        let finished = matches!(event, BatchEvent::Finished(_));
                        post(&sender, &ctx, Event::Batch(event));
                        if finished {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if handle.is_finished() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            let result = handle
                .join()
                .map(|_| ())
                .map_err(|_| "The batch worker stopped unexpectedly".into());
            post(&sender, &ctx, Event::BatchClosed(result));
        })
        .expect("batch worker");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, fs};

    fn fixtures() -> (tempfile::TempDir, Vec<(ImportedImage, Prepare)>) {
        let directory = tempfile::tempdir().unwrap();
        let items = (0..2)
            .map(|index| {
                let name = format!("image-{index}.png");
                let path = directory.path().join(&name);
                image::RgbImage::new(2, 3).save(&path).unwrap();
                let item = ImportedImage::from_file(&path, name.into()).unwrap();
                (item, Prepare::Original)
            })
            .collect();
        (directory, items)
    }

    fn request() -> PreparationRequest {
        PreparationRequest {
            id: 41,
            tool: ToolKind::Resize,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn cancelled_before_collection_does_not_read_or_post() {
        let (_directory, items) = fixtures();
        let request = request();
        request.cancelled.store(true, Ordering::Release);
        let result = collect_prepared(
            items,
            &request.cancelled,
            |_, _| panic!("cancelled preparation opened a file for validation"),
            |_| panic!("cancelled preparation opened a file for probing"),
        );
        assert!(result.is_none());
        let (sender, events) = mpsc::channel();
        post_prepared(result, true, &request, &sender, &egui::Context::default());
        assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn cancellation_after_validation_skips_probe_and_later_files() {
        let (_directory, items) = fixtures();
        let request = request();
        let first_id = items[0].0.id;
        let reads = RefCell::new(Vec::new());
        let result = collect_prepared(
            items,
            &request.cancelled,
            |item, action| {
                reads.borrow_mut().push(item.id);
                let result = validate_preparation(item, action);
                assert!(result.is_ok());
                request.cancelled.store(true, Ordering::Release);
                result
            },
            |_| panic!("probe ran after cancellation during identity validation"),
        );
        assert_eq!(*reads.borrow(), vec![first_id]);
        assert!(result.is_none());
        let (sender, events) = mpsc::channel();
        post_prepared(result, false, &request, &sender, &egui::Context::default());
        assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn cancellation_after_probe_discards_partial_results_without_later_reads() {
        let (_directory, items) = fixtures();
        let request = request();
        let first_path = items[0].0.input.path.clone();
        let reads = RefCell::new(Vec::new());
        let result = collect_prepared(
            items,
            &request.cancelled,
            |item, action| {
                reads
                    .borrow_mut()
                    .push(("validate", item.input.path.clone()));
                validate_preparation(item, action)
            },
            |path| {
                reads.borrow_mut().push(("probe", path.to_owned()));
                let result = local_image_core::probe(path).map_err(|error| error.to_string());
                assert!(result.is_ok());
                request.cancelled.store(true, Ordering::Release);
                result
            },
        );
        assert_eq!(
            *reads.borrow(),
            vec![("validate", first_path.clone()), ("probe", first_path)]
        );
        assert!(result.is_none());
        let (sender, events) = mpsc::channel();
        post_prepared(result, true, &request, &sender, &egui::Context::default());
        assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn cancellation_after_collection_suppresses_completed_event() {
        let (_directory, items) = fixtures();
        let request = request();
        let result = collect_prepared(items, &request.cancelled, validate_preparation, |path| {
            local_image_core::probe(path).map_err(|error| error.to_string())
        });
        assert_eq!(result.as_ref().unwrap().len(), 2);
        request.cancelled.store(true, Ordering::Release);
        let (sender, events) = mpsc::channel();
        post_prepared(result, true, &request, &sender, &egui::Context::default());
        assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn completed_event_keeps_request_identity_and_per_file_validation_errors() {
        let (_directory, items) = fixtures();
        let request = request();
        let first_id = items[0].0.id;
        let second_id = items[1].0.id;
        // A changed original must fail identity validation, while its valid
        // neighbour is still probed and returned in the same result order.
        fs::write(&items[0].0.input.path, b"changed after import").unwrap();
        let probes = RefCell::new(Vec::new());
        let result = collect_prepared(items, &request.cancelled, validate_preparation, |path| {
            probes.borrow_mut().push(path.to_owned());
            local_image_core::probe(path).map_err(|error| error.to_string())
        });
        assert_eq!(probes.borrow().len(), 1);
        let (sender, events) = mpsc::channel();
        post_prepared(result, true, &request, &sender, &egui::Context::default());
        let Event::Prepared {
            request_id,
            tool,
            items,
            retry,
        } = events.try_recv().unwrap()
        else {
            panic!("expected preparation result");
        };
        assert_eq!(request_id, 41);
        assert_eq!(tool, ToolKind::Resize);
        assert!(retry);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, first_id);
        assert!(items[0].1.is_err());
        assert_eq!(items[1].0, second_id);
        let (item, info) = items[1].1.as_ref().unwrap();
        assert_eq!(item.id, second_id);
        assert_eq!(info.as_ref().unwrap().display_dimensions(), (2, 3));
        assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
}
