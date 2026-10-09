//! Pointer-level egui regressions. These exercise the real header, gallery, and
//! continuation menu without creating a native window or accessing user files.
use super::*;
use local_image_core::{ProcessReport, import::SourceRecord};
use std::time::Duration;

struct Harness {
    app: ImageToolsApp,
    ctx: egui::Context,
    // Intercept worker replies so pending/completed/cancelled order is entirely
    // deterministic. Only deliver() makes a reply visible to the real poll().
    worker_events: Receiver<Event>,
    deliver: Sender<Event>,
    clock: f64,
    _directory: tempfile::TempDir,
}

#[derive(Debug, PartialEq, Eq)]
struct SessionSnapshot {
    rows: Vec<String>,
    focus: Option<u64>,
    done: usize,
    total: usize,
    started: Option<Instant>,
    preview_original: bool,
}

fn row_signatures(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            format!(
                "{:?}|{:?}|{:?}|{}|{:?}|{}|{:?}|{:?}",
                row.item,
                row.info,
                row.original_info,
                row.selected,
                row.state,
                row.detail,
                row.result,
                row.step_settings,
            )
        })
        .collect()
}

fn snapshot(app: &ImageToolsApp, tool: ToolKind) -> SessionSnapshot {
    if app.current_tool == tool {
        SessionSnapshot {
            rows: row_signatures(&app.rows),
            focus: app.focus,
            done: app.done,
            total: app.total,
            started: app.started,
            preview_original: app.preview_original,
        }
    } else {
        let session = &app.sessions[tool.index()];
        SessionSnapshot {
            rows: row_signatures(&session.rows),
            focus: session.focus,
            done: session.done,
            total: session.total,
            started: session.started,
            preview_original: session.preview_original,
        }
    }
}

fn successful_row(directory: &Path, name: &str, selected: bool) -> Row {
    let path = directory.join(format!("{name}.png"));
    let output_path = directory.join(format!("{name}-output.png"));
    image::RgbaImage::from_pixel(48, 32, image::Rgba([21, 67, 109, 255]))
        .save(&path)
        .unwrap();
    image::RgbaImage::from_pixel(24, 16, image::Rgba([21, 67, 109, 255]))
        .save(&output_path)
        .unwrap();
    let item = ImportedImage::from_file(&path, PathBuf::from(format!("{name}.png"))).unwrap();
    let info = local_image_core::probe(&path).unwrap();
    let output = SourceRecord::capture(&output_path).unwrap();
    let result = TaskResult {
        item: item.clone(),
        elapsed_ms: 7,
        status: TaskStatus::Success {
            report: ProcessReport {
                format: Format::Png,
                width: 24,
                height: 16,
                input_bytes: item.input.fingerprint.len,
                output_bytes: output.fingerprint.len,
                elapsed_ms: 7,
                warnings: vec!["Retained result".into()],
                unchanged: false,
            },
            output,
            change: SizeChange::Smaller,
        },
    };
    Row {
        item,
        info: Some(info.clone()),
        original_info: Some(info),
        selected,
        state: State::Success,
        detail: "Retained result".into(),
        result: Some(result),
        step_settings: None,
    }
}

impl Harness {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let (sender, worker_events) = mpsc::channel();
        let (deliver, events) = mpsc::channel();
        let source = successful_row(directory.path(), "source", true);
        let focus = Some(source.item.id);
        let destination = vec![
            successful_row(directory.path(), "resize-a", true),
            successful_row(directory.path(), "resize-b", false),
        ];
        let destination_focus = Some(destination[1].item.id);
        let mut sessions = std::array::from_fn(|_| ToolSession::default());
        sessions[ToolKind::Resize.index()] = ToolSession {
            rows: destination,
            focus: destination_focus,
            done: 2,
            total: 2,
            started: Some(Instant::now()),
            preview_original: true,
            ..Default::default()
        };
        let convert = successful_row(directory.path(), "convert", true);
        sessions[ToolKind::Convert.index()] = ToolSession {
            focus: Some(convert.item.id),
            rows: vec![convert],
            done: 1,
            total: 1,
            ..Default::default()
        };
        let app = ImageToolsApp {
            settings: Settings {
                language: Language::English,
                output_directory: directory.path().join("exports").display().to_string(),
                ..Default::default()
            },
            // No discover/load/save may touch a real user profile in these tests.
            settings_store: Err("Test has no persistent settings store".into()),
            settings_load_error: None,
            close_save_error: None,
            close_approved: false,
            page: Page::Tool(ToolKind::Compress),
            current_tool: ToolKind::Compress,
            sessions,
            tool_search: String::new(),
            pending_paths: Vec::new(),
            dialog_open: false,
            next_transfer_id: 1 << 63,
            next_preparation_id: 1,
            settings_notice: None,
            rows: vec![source],
            focus,
            sender,
            events,
            cache: PreviewCache::new(ctx.clone()),
            importing: false,
            preparation: None,
            running: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            cancel_accepted: false,
            closing: false,
            done: 1,
            total: 1,
            started: Some(Instant::now()),
            message: "Source result".into(),
            issues: Vec::new(),
            show_issues: false,
            show_preview: false,
            preview_original: true,
            initial_paths: None,
            diagnostics: None,
        };
        Self {
            app,
            ctx,
            worker_events,
            deliver,
            clock: 0.0,
            _directory: directory,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        self.clock += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1220.0, 820.0),
            )),
            time: Some(self.clock),
            events,
            focused: true,
            ..Default::default()
        };
        let app = &mut self.app;
        self.ctx.run(input, |ctx| {
            app.header(ctx);
            match app.page {
                Page::Home => {
                    if let Some(tool) = tool_gallery::home(ctx, app.lang(), &mut app.tool_search, 0)
                    {
                        app.switch_tool(tool, ctx);
                    }
                }
                Page::Tool(tool) if app.rows.is_empty() => {
                    let _ = tool_gallery::landing(
                        ctx,
                        app.lang(),
                        tool,
                        app.busy(),
                        &mut app.settings.recursive,
                    );
                }
                Page::Tool(_) => {
                    app.footer(ctx);
                    app.parameters(ctx);
                    app.workspace(ctx);
                }
            }
        })
    }

    fn click(&mut self, label: &str, in_header: bool) {
        // The two settling frames let popup sizing and hit-test layers settle.
        // Targets come from rendered text geometry, never guessed coordinates.
        self.frame(Vec::new());
        let output = self.frame(Vec::new());
        let mut matches = Vec::new();
        for shape in output.shapes {
            text_positions(&shape.shape, label, shape.clip_rect, &mut matches);
        }
        matches.retain(|position| (position.y < 76.0) == in_header);
        assert_eq!(
            matches.len(),
            1,
            "Expected one visible target {label:?}; got {matches:?}"
        );
        let position = matches[0];
        self.frame(vec![egui::Event::PointerMoved(position)]);
        self.frame(vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        self.frame(vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        self.frame(Vec::new());
    }

    fn continue_to_resize(&mut self) {
        self.click("Continue with…", false);
        self.click("Resize images", false);
        assert_eq!(self.app.page, Page::Tool(ToolKind::Resize));
        assert!(self.app.preparing());
        assert!(!self.app.running);
    }

    fn deliver(&mut self, event: Event) {
        self.deliver.send(event).unwrap();
        self.app.poll(&self.ctx);
    }

    fn finish_request(&mut self, id: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "Preparation worker did not respond");
            let event = self
                .worker_events
                .recv_timeout(remaining)
                .expect("Preparation worker reply");
            if matches!(&event, Event::Prepared { request_id, .. } if *request_id == id) {
                self.deliver(event);
                break;
            }
        }
    }
}

fn text_positions(
    shape: &egui::Shape,
    label: &str,
    clip: egui::Rect,
    result: &mut Vec<egui::Pos2>,
) {
    match shape {
        egui::Shape::Text(text) if text.galley.job.text == label => {
            let center = text.visual_bounding_rect().center();
            if clip.contains(center) {
                result.push(center);
            }
        }
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                text_positions(shape, label, clip, result);
            }
        }
        _ => {}
    }
}

#[test]
fn pointer_continue_then_all_tools_cancels_pending_transfer_and_preserves_sessions() {
    let mut h = Harness::new();
    let source = snapshot(&h.app, ToolKind::Compress);
    let destination = snapshot(&h.app, ToolKind::Resize);
    let convert = snapshot(&h.app, ToolKind::Convert);
    let settings = h.app.settings.clone();
    h.continue_to_resize();
    let pending = h.app.preparation.as_ref().unwrap().request.clone();
    assert_eq!(h.app.rows.len(), 3);
    assert!(h.app.rows[2].selected);
    assert!(!h.app.rows[0].selected && !h.app.rows[1].selected);

    h.click("← All tools", true);
    assert_eq!(h.app.page, Page::Home);
    assert!(!h.app.preparing());
    assert!(pending.cancelled.load(Ordering::Acquire));
    assert_eq!(snapshot(&h.app, ToolKind::Resize), destination);
    h.click("Convert images", false);
    assert_eq!(h.app.page, Page::Tool(ToolKind::Convert));
    assert_eq!(snapshot(&h.app, ToolKind::Convert), convert);
    h.click("← All tools", true);
    h.click("Compress images", false);
    assert_eq!(snapshot(&h.app, ToolKind::Compress), source);
    h.click("Compress images", true);
    h.click("Resize images", false);
    assert_eq!(h.app.page, Page::Tool(ToolKind::Resize));
    assert_eq!(snapshot(&h.app, ToolKind::Resize), destination);
    assert_eq!(h.app.settings, settings);
}

#[test]
fn pointer_navigation_after_preparation_keeps_ready_transfer_and_prior_results() {
    let mut h = Harness::new();
    let source = snapshot(&h.app, ToolKind::Compress);
    let settings = h.app.settings.clone();
    h.continue_to_resize();
    let request = h.app.preparation.as_ref().unwrap().request.clone();
    let prior_rows = row_signatures(&h.app.rows[..2]);
    h.finish_request(request.id);
    assert!(!h.app.preparing() && !h.app.running);
    assert!(!request.cancelled.load(Ordering::Acquire));
    assert_eq!(row_signatures(&h.app.rows[..2]), prior_rows);
    let incoming = &h.app.rows[2];
    assert_eq!(incoming.state, State::Ready);
    assert!(incoming.selected && incoming.result.is_none());
    assert_eq!(incoming.item.input_source, InputSource::PreviousOutput);
    assert_eq!(
        incoming.info.as_ref().map(|info| (info.width, info.height)),
        Some((24, 16))
    );
    assert_eq!(
        incoming
            .original_info
            .as_ref()
            .map(|info| (info.width, info.height)),
        Some((48, 32))
    );
    let ready = snapshot(&h.app, ToolKind::Resize);
    h.click("Resize images", true);
    h.click("Convert images", false);
    assert_eq!(h.app.page, Page::Tool(ToolKind::Convert));
    h.click("← All tools", true);
    h.click("Resize images", false);
    assert_eq!(h.app.page, Page::Tool(ToolKind::Resize));
    assert_eq!(snapshot(&h.app, ToolKind::Resize), ready);
    assert_eq!(snapshot(&h.app, ToolKind::Compress), source);
    assert_eq!(h.app.settings, settings);
}

#[test]
fn pointer_footer_cancel_restores_existing_destination_without_leaving_tool() {
    let mut h = Harness::new();
    let source = snapshot(&h.app, ToolKind::Compress);
    let destination = snapshot(&h.app, ToolKind::Resize);
    h.continue_to_resize();
    let pending = h.app.preparation.as_ref().unwrap();
    let request = pending.request.clone();
    let staged_id = pending.targets[0].id;
    h.click("Cancel checking", false);
    assert_eq!(h.app.page, Page::Tool(ToolKind::Resize));
    assert!(!h.app.preparing() && !h.app.running);
    assert!(request.cancelled.load(Ordering::Acquire));
    assert_eq!(snapshot(&h.app, ToolKind::Resize), destination);
    assert_eq!(snapshot(&h.app, ToolKind::Compress), source);
    h.deliver(Event::Prepared {
        request_id: request.id,
        tool: request.tool,
        items: vec![(staged_id, Err("Late reply after cancel".into()))],
        retry: true,
    });
    assert_eq!(snapshot(&h.app, ToolKind::Resize), destination);
    h.click("Resize images", true);
    h.click("Convert images", false);
    assert_eq!(h.app.page, Page::Tool(ToolKind::Convert));
}

#[test]
fn cancelled_readjust_and_stale_retry_cannot_change_new_preparation() {
    let mut h = Harness::new();
    let output = h.app.rows[0]
        .result
        .as_ref()
        .unwrap()
        .output()
        .unwrap()
        .clone();
    h.app.rows[0].item.input = output;
    h.app.rows[0].item.input_source = InputSource::PreviousOutput;
    h.app.rows[0].info = Some(local_image_core::probe(&h.app.rows[0].item.input.path).unwrap());
    let before = snapshot(&h.app, ToolKind::Compress);
    h.app.prepare(&h.ctx, 0);
    let cancelled = h.app.preparation.as_ref().unwrap().request.clone();
    assert_eq!(
        row_signatures(&h.app.rows),
        before.rows,
        "Readjust changed input before validation committed"
    );
    h.click("← All tools", true);
    assert_eq!(snapshot(&h.app, ToolKind::Compress), before);
    assert!(cancelled.cancelled.load(Ordering::Acquire));
    h.click("Compress images", false);
    h.app.prepare(&h.ctx, 0);
    let pending = h.app.preparation.as_ref().unwrap();
    let current = pending.request.clone();
    let target = pending.targets[0].clone();
    assert_ne!(current.id, cancelled.id);
    let info = local_image_core::probe(&target.input.path).unwrap();
    let during = snapshot(&h.app, ToolKind::Compress);
    let message = h.app.message.clone();
    for (id, tool) in [
        (cancelled.id, ToolKind::Compress),
        (current.id, ToolKind::Convert),
    ] {
        h.deliver(Event::Prepared {
            request_id: id,
            tool,
            items: vec![(target.id, Ok((target.clone(), Ok(info.clone()))))],
            retry: true,
        });
        assert_eq!(h.app.preparation.as_ref().unwrap().request.id, current.id);
        assert_eq!(snapshot(&h.app, ToolKind::Compress), during);
        assert_eq!(h.app.message, message, "Stale retry attempted processing");
        assert!(!h.app.running);
    }
    h.finish_request(current.id);
    assert!(!h.app.preparing());
    assert_eq!(h.app.rows[0].item.input_source, InputSource::Original);
    assert_eq!(h.app.rows[0].item.input, h.app.rows[0].item.original);
    assert_eq!(h.app.rows[0].state, State::Ready);
    assert!(h.app.rows[0].result.is_none());
    h.click("← All tools", true);
    let home = snapshot(&h.app, ToolKind::Compress);
    let message = h.app.message.clone();
    h.deliver(Event::Prepared {
        request_id: current.id,
        tool: ToolKind::Compress,
        items: vec![(target.id, Ok((target, Ok(info))))],
        retry: true,
    });
    assert_eq!(h.app.page, Page::Home);
    assert_eq!(snapshot(&h.app, ToolKind::Compress), home);
    assert_eq!(h.app.message, message);
    assert!(!h.app.preparing() && !h.app.running);
}

#[test]
fn pointer_header_still_blocks_navigation_during_processing_import_or_dialog() {
    let mut h = Harness::new();
    let before = snapshot(&h.app, ToolKind::Compress);
    for reason in 0..3 {
        h.app.running = reason == 0;
        h.app.importing = reason == 1;
        h.app.dialog_open = reason == 2;
        h.click("← All tools", true);
        assert_eq!(h.app.page, Page::Tool(ToolKind::Compress));
        assert_eq!(snapshot(&h.app, ToolKind::Compress), before);
        h.click("Compress images", true);
        assert_eq!(h.app.page, Page::Tool(ToolKind::Compress));
        assert_eq!(snapshot(&h.app, ToolKind::Compress), before);
    }
}

#[test]
fn cancelling_readjust_restores_previous_preferences_without_undoing_new_edits() {
    for edited_while_checking in [false, true] {
        let mut h = Harness::new();
        h.app.settings.compress.jpeg_quality = 85;
        h.app.rows[0].step_settings = Some(h.app.settings.snapshot(ToolKind::Compress));
        h.app.settings.compress.jpeg_quality = 40;
        let rows = row_signatures(&h.app.rows);
        let resize = h.app.settings.snapshot(ToolKind::Resize);
        h.app.prepare(&h.ctx, 0);
        assert_eq!(h.app.settings.compress.jpeg_quality, 85);
        if edited_while_checking {
            h.app.settings.compress.jpeg_quality = 72;
        }
        h.click("Cancel checking", false);
        assert!(!h.app.preparing());
        assert_eq!(
            h.app.settings.compress.jpeg_quality,
            if edited_while_checking { 72 } else { 40 }
        );
        assert_eq!(row_signatures(&h.app.rows), rows);
        assert_eq!(h.app.settings.snapshot(ToolKind::Resize), resize);
    }
}
