use crate::{
    controls::{percent_slider, quality_slider, thin_progress},
    diagnostics::Diagnostics,
    preview::PreviewCache,
    settings::{Language, Settings, SettingsStore},
    theme::{self, ACCENT, MUTED, PALE},
    tool::{ToolKind, ToolPreferences},
    tool_gallery::{self, LandingAction},
    worker::{self, Event, PreparationRequest, Prepare},
};
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Vec2};
use local_image_core::{
    Format, ImageInfo, ProcessOptions,
    batch::{BatchConfig, BatchEvent, SizeChange, TaskResult, TaskStatus},
    import::{ImportOptions, ImportedImage, InputSource},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::Instant,
};

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ready,
    Queued,
    Running,
    Success,
    Skipped,
    Failed,
    Cancelled,
}

struct Row {
    item: ImportedImage,
    info: Option<ImageInfo>,
    original_info: Option<ImageInfo>,
    selected: bool,
    state: State,
    detail: String,
    result: Option<TaskResult>,
    step_settings: Option<ToolPreferences>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Home,
    Tool(ToolKind),
}

struct ToolSession {
    rows: Vec<Row>,
    focus: Option<u64>,
    done: usize,
    total: usize,
    started: Option<Instant>,
    message: String,
    issues: Vec<String>,
    show_preview: bool,
    preview_original: bool,
}

struct PreparationState {
    focus: Option<u64>,
    done: usize,
    total: usize,
    started: Option<Instant>,
    preview_original: bool,
    preferences: ToolPreferences,
}

struct TransferState {
    row_count: usize,
    selection: Vec<bool>,
}

struct PendingPreparation {
    request: PreparationRequest,
    targets: Vec<ImportedImage>,
    previous: PreparationState,
    transfer: Option<TransferState>,
    restored_preferences: Option<ToolPreferences>,
}

impl Default for ToolSession {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            focus: None,
            done: 0,
            total: 0,
            started: None,
            message: String::new(),
            issues: Vec::new(),
            show_preview: true,
            preview_original: false,
        }
    }
}

pub struct ImageToolsApp {
    settings: Settings,
    settings_store: Result<SettingsStore, String>,
    settings_load_error: Option<String>,
    close_save_error: Option<String>,
    close_approved: bool,
    page: Page,
    current_tool: ToolKind,
    sessions: [ToolSession; 3],
    tool_search: String,
    pending_paths: Vec<PathBuf>,
    dialog_open: bool,
    next_transfer_id: u64,
    next_preparation_id: u64,
    settings_notice: Option<String>,
    rows: Vec<Row>,
    focus: Option<u64>,
    sender: Sender<Event>,
    events: Receiver<Event>,
    cache: PreviewCache,
    importing: bool,
    preparation: Option<PendingPreparation>,
    running: bool,
    cancelled: Arc<AtomicBool>,
    cancel_accepted: bool,
    closing: bool,
    done: usize,
    total: usize,
    started: Option<Instant>,
    message: String,
    issues: Vec<String>,
    show_issues: bool,
    show_preview: bool,
    preview_original: bool,
    initial_paths: Option<Vec<PathBuf>>,
    diagnostics: Option<Diagnostics>,
}

impl ImageToolsApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        theme::install(&cc.egui_ctx);
        let (sender, events) = mpsc::channel();
        let diagnostics = Diagnostics::from_environment();
        if let Some(diagnostics) = &diagnostics {
            crate::diagnostics::record_renderer(cc, diagnostics);
        }
        let settings_store = SettingsStore::discover();
        let mut settings_warning = None;
        let mut settings_load_error = None;
        let settings = if let Some(diagnostics) = &diagnostics
            && !diagnostics.config.use_saved_settings
        {
            diagnostics.settings()
        } else {
            match settings_store.as_ref() {
                Ok(store) => match store.load() {
                    Ok(loaded) => {
                        settings_warning = loaded.warning;
                        loaded.settings
                    }
                    Err(error) => {
                        settings_load_error = Some(error.clone());
                        settings_warning = Some(error);
                        store.defaults()
                    }
                },
                Err(error) => {
                    settings_load_error = Some(error.clone());
                    settings_warning = Some(error.clone());
                    Settings::default()
                }
            }
        };
        let settings_warning = settings_warning.map(|warning| {
            format!(
                "{} {warning}",
                settings.language.text("设置提示：", "Settings notice:")
            )
        });
        if let Some(diagnostics) = &diagnostics {
            diagnostics.log(serde_json::json!({
                "event":"settings-loaded",
                "portable":settings_store.as_ref().is_ok_and(SettingsStore::is_portable),
                "settings_path":settings_store.as_ref().ok().map(SettingsStore::path),
                "settings":settings, "warning":settings_warning
            }));
        }
        let paths = diagnostics
            .as_ref()
            .map(|d| d.config.input_paths.clone())
            .unwrap_or(paths);
        let initial_tool = diagnostics.as_ref().and_then(|d| d.config.tool);
        Self {
            settings,
            settings_store,
            settings_load_error,
            close_save_error: None,
            close_approved: false,
            page: initial_tool.map(Page::Tool).unwrap_or(Page::Home),
            current_tool: initial_tool.unwrap_or(ToolKind::Compress),
            sessions: std::array::from_fn(|_| ToolSession::default()),
            tool_search: String::new(),
            pending_paths: Vec::new(),
            dialog_open: false,
            next_transfer_id: 1 << 63,
            next_preparation_id: 1,
            settings_notice: settings_warning.clone(),
            rows: Vec::new(),
            focus: None,
            sender,
            events,
            cache: PreviewCache::new(cc.egui_ctx.clone()),
            importing: false,
            preparation: None,
            running: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            cancel_accepted: false,
            closing: false,
            done: 0,
            total: 0,
            started: None,
            message: settings_warning.clone().unwrap_or_default(),
            issues: settings_warning.into_iter().collect(),
            show_issues: false,
            show_preview: true,
            preview_original: false,
            initial_paths: Some(paths),
            diagnostics,
        }
    }

    fn busy(&self) -> bool {
        self.preparing() || self.navigation_busy()
    }

    fn preparing(&self) -> bool {
        self.preparation.is_some()
    }

    fn navigation_busy(&self) -> bool {
        self.running
            || self.importing
            || self.dialog_open
            || self.closing
            || self.close_approved
            || self.close_save_error.is_some()
    }

    fn preparation_state(&self) -> PreparationState {
        PreparationState {
            focus: self.focus,
            done: self.done,
            total: self.total,
            started: self.started,
            preview_original: self.preview_original,
            preferences: self.settings.snapshot(self.current_tool),
        }
    }

    fn cancel_preparation(&mut self) {
        let Some(pending) = self.preparation.take() else {
            return;
        };
        pending.request.cancelled.store(true, Ordering::Release);
        if let Some(restored) = &pending.restored_preferences
            && self.settings.snapshot(pending.request.tool) == *restored
        {
            // Undo Readjust's automatic preference restore, but keep any
            // explicit edits the user made while checking was in progress.
            self.settings.restore_tool(&pending.previous.preferences);
        }
        if let Some(transfer) = pending.transfer {
            // Import and row removal are disabled during preparation, so the
            // appended rows are the only additions to this destination session.
            self.rows.truncate(transfer.row_count);
            for (row, selected) in self.rows.iter_mut().zip(transfer.selection) {
                row.selected = selected;
            }
        }
        self.focus = pending.previous.focus;
        self.done = pending.previous.done;
        self.total = pending.previous.total;
        self.started = pending.previous.started;
        self.preview_original = pending.previous.preview_original;
        self.cache.clear();
        self.message = self
            .lang()
            .text(
                "已取消检查，先前的图片和结果已保留。",
                "Checking cancelled. Your previous images and results are kept.",
            )
            .into();
    }

    fn begin_preparation(
        &mut self,
        ctx: &egui::Context,
        items: Vec<(ImportedImage, Prepare)>,
        retry: bool,
        previous: PreparationState,
        transfer: Option<TransferState>,
    ) {
        let request = PreparationRequest {
            id: self.next_preparation_id,
            tool: self.current_tool,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        self.next_preparation_id += 1;
        let targets = items
            .iter()
            .map(|(item, action)| {
                let mut item = item.clone();
                match action {
                    Prepare::Original => {
                        item.input = item.original.clone();
                        item.input_source = InputSource::Original;
                    }
                    Prepare::Continue(output) => {
                        item.input = output.clone();
                        item.input_source = InputSource::PreviousOutput;
                    }
                    Prepare::Retry => {}
                }
                item
            })
            .collect();
        let current_preferences = self.settings.snapshot(self.current_tool);
        let restored_preferences =
            (previous.preferences != current_preferences).then_some(current_preferences);
        self.preparation = Some(PendingPreparation {
            request: request.clone(),
            targets,
            previous,
            transfer,
            restored_preferences,
        });
        worker::prepare(items, retry, request, self.sender.clone(), ctx.clone());
    }
    fn lang(&self) -> Language {
        self.settings.language
    }

    fn active_tool(&self) -> Option<ToolKind> {
        match self.page {
            Page::Home => None,
            Page::Tool(tool) => Some(tool),
        }
    }

    fn choose_files(&mut self, ctx: &egui::Context, folders: bool) {
        if self.busy() || self.active_tool().is_none() {
            return;
        }
        self.dialog_open = true;
        worker::pick_files(self.sender.clone(), ctx.clone(), folders);
    }

    fn choose_output(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let current = self
            .output_directory()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.dialog_open = true;
        worker::pick_output(self.sender.clone(), ctx.clone(), current);
    }

    fn go_home(&mut self) {
        if self.navigation_busy() {
            return;
        }
        self.cancel_preparation();
        self.page = Page::Home;
        self.show_issues = false;
        self.cache.clear();
    }

    fn switch_tool(&mut self, tool: ToolKind, ctx: &egui::Context) {
        if self.navigation_busy() {
            return;
        }
        if self.page == Page::Tool(tool) {
            return;
        }
        self.cancel_preparation();
        if tool != self.current_tool {
            let previous = ToolSession {
                rows: std::mem::take(&mut self.rows),
                focus: self.focus.take(),
                done: self.done,
                total: self.total,
                started: self.started.take(),
                message: std::mem::take(&mut self.message),
                issues: std::mem::take(&mut self.issues),
                show_preview: self.show_preview,
                preview_original: self.preview_original,
            };
            self.sessions[self.current_tool.index()] = previous;
            let next = std::mem::take(&mut self.sessions[tool.index()]);
            self.rows = next.rows;
            self.focus = next.focus;
            self.done = next.done;
            self.total = next.total;
            self.started = next.started;
            self.message = next.message;
            self.issues = next.issues;
            self.show_preview = next.show_preview;
            self.preview_original = next.preview_original;
            self.current_tool = tool;
        }
        self.page = Page::Tool(tool);
        self.show_issues = false;
        self.cache.clear();
        if !self.pending_paths.is_empty() {
            let paths = std::mem::take(&mut self.pending_paths);
            self.add_paths(paths, ctx);
        }
    }

    fn continue_with(&mut self, ctx: &egui::Context, tool: ToolKind) {
        if self.busy() || self.active_tool().is_none() {
            return;
        }
        let incoming: Vec<_> = self
            .rows
            .iter()
            .filter(|row| row.selected)
            .filter_map(|row| {
                Some((
                    row.item.clone(),
                    row.original_info.clone(),
                    row.result.as_ref()?.output()?.clone(),
                ))
            })
            .collect();
        if incoming.is_empty() {
            return;
        }
        self.switch_tool(tool, ctx);
        if self.busy() {
            return;
        }
        let previous = self.preparation_state();
        let transfer = TransferState {
            row_count: self.rows.len(),
            selection: self.rows.iter().map(|row| row.selected).collect(),
        };
        for row in &mut self.rows {
            row.selected = false;
        }
        let mut prepared = Vec::with_capacity(incoming.len());
        for (mut item, original_info, output) in incoming {
            item.id = self.next_transfer_id;
            self.next_transfer_id += 1;
            item.input = output.clone();
            item.input_source = InputSource::PreviousOutput;
            if prepared.is_empty() {
                self.focus = Some(item.id);
            }
            prepared.push((item.clone(), Prepare::Continue(output)));
            self.rows.push(Row {
                item,
                original_info,
                info: None,
                selected: true,
                state: State::Ready,
                detail: String::new(),
                result: None,
                step_settings: None,
            });
        }
        self.cache.clear();
        self.preview_original = false;
        self.done = 0;
        self.total = 0;
        self.started = None;
        self.message = self
            .lang()
            .text(
                "正在检查所选结果；就绪后可设置下一步参数。",
                "Checking the selected outputs; choose settings for the next tool when ready.",
            )
            .into();
        self.begin_preparation(ctx, prepared, false, previous, Some(transfer));
    }

    fn continuation_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, enabled: bool) {
        let language = self.lang();
        let mut next = None;
        ui.add_enabled_ui(enabled, |ui| {
            ui.menu_button(language.text("继续调整…", "Continue with…"), |ui| {
                ui.label(
                    RichText::new(language.text(
                        "以上次成功输出为输入",
                        "Use the selected successful outputs",
                    ))
                    .small()
                    .color(MUTED),
                );
                for tool in ToolKind::ALL {
                    if ui.button(tool_gallery::title(tool, language)).clicked() {
                        next = Some(tool);
                        ui.close();
                    }
                }
            });
        });
        if let Some(tool) = next {
            self.continue_with(ctx, tool);
        }
    }

    fn portable(&self) -> bool {
        self.settings_store
            .as_ref()
            .is_ok_and(SettingsStore::is_portable)
    }

    fn output_directory(&self) -> Result<PathBuf, String> {
        let value = self.settings.output_directory.trim();
        if value.is_empty() {
            return Err(self
                .lang()
                .text("请选择输出目录。", "Choose an output folder.")
                .into());
        }
        match &self.settings_store {
            Ok(store) => store.resolve_output(value),
            Err(error) => Err(error.clone()),
        }
    }

    fn save_settings(&mut self) -> Result<(), String> {
        if self.settings_store.is_err() {
            match SettingsStore::discover() {
                Ok(store) if store.is_portable() => self.settings_store = Ok(store),
                // A repaired portable location can be retried, but removing an
                // invalid marker must never redirect this session to AppData.
                Ok(_) => {}
                Err(error) => self.settings_store = Err(error),
            }
        }
        let result = self
            .settings_store
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|store| store.save(&self.settings));
        if result.is_ok() {
            self.settings_load_error = None;
        }
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.log(serde_json::json!({
                "event":"settings-saved", "ok":result.is_ok(),
                "error":result.as_ref().err(),
                "settings_path":self.settings_store.as_ref().ok().map(SettingsStore::path),
                "portable":self.portable()
            }));
        }
        result
    }

    fn load_settings(&mut self) {
        match self
            .settings_store
            .as_ref()
            .map_err(Clone::clone)
            .and_then(SettingsStore::load)
        {
            Ok(loaded) => {
                let preferences = loaded.settings.snapshot(self.current_tool);
                self.settings.restore_tool(&preferences);
                self.settings_load_error = None;
                self.message = loaded.warning.unwrap_or_else(|| {
                    self.lang()
                        .text(
                            "已载入当前工具的常用设置。",
                            "Preferred settings loaded for this tool.",
                        )
                        .into()
                });
            }
            Err(error) => {
                self.settings_load_error = Some(error.clone());
                self.message = error;
            }
        }
    }

    fn request_close(&mut self, ctx: &egui::Context) {
        self.cancel_preparation();
        if self.running {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
            self.cancelled.store(true, Ordering::Release);
            return;
        }
        let should_save = self
            .diagnostics
            .as_ref()
            .is_none_or(|diagnostics| diagnostics.config.save_settings_on_exit);
        if !self.close_approved && should_save {
            // Do not overwrite an unreadable configuration merely because the
            // app was opened and closed. The dialog offers an explicit save.
            let result = if let Some(error) = &self.settings_load_error {
                Err(error.clone())
            } else {
                self.save_settings()
            };
            if let Err(error) = result {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if let Some(diagnostics) = &self.diagnostics {
                    diagnostics.log(
                        serde_json::json!({"event":"settings-close-blocked","error":error,
                        "load_failed":self.settings_load_error.is_some()}),
                    );
                }
                self.close_save_error = Some(error);
                self.closing = false;
                ctx.request_repaint();
                return;
            }
        }
        self.close_approved = true;
        self.close_save_error = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn settings_error_dialog(&mut self, ctx: &egui::Context) {
        let Some(error) = self.close_save_error.clone() else {
            return;
        };
        let language = self.lang();
        let load_failed = self.settings_load_error.is_some();
        let mut action = 0;
        egui::Modal::new(egui::Id::new("settings-save-error")).show(ctx, |ui| {
            ui.set_max_width(540.0);
            ui.heading(if load_failed {
                language.text("原有设置未能读取", "Could not read saved settings")
            } else {
                language.text("设置未能保存", "Could not save settings")
            });
            ui.label(if load_failed {
                language.text(
                    "原设置文件尚未修改。可以明确保存当前参数替换原设置，或不保存退出。图片处理结果不受影响。",
                    "The existing settings file has not been changed. Save the current settings to replace it, or exit without saving. Processed images are kept.",
                )
            } else {
                language.text(
                    "图片处理结果已保留。请检查保存位置是否可写，然后重试；也可以不保存本次设置退出。",
                    "Processed images are kept. Check that the settings location is writable, then retry, or exit without saving these settings.",
                )
            });
            if let Ok(store) = &self.settings_store {
                ui.label(store.path().display().to_string());
            }
            ui.label(RichText::new(error).small().color(Color32::from_rgb(166, 59, 45)));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(if load_failed { language.text("保存当前设置并退出", "Save current settings and exit") }
                    else { language.text("重试保存并退出", "Save and exit") }).clicked() { action = 1; }
                if ui.button(language.text("不保存退出", "Exit without saving")).clicked() { action = 2; }
                if ui.button(language.text("返回", "Go back")).clicked() { action = 3; }
            });
        });
        match action {
            1 => match self.save_settings() {
                Ok(()) => {
                    self.close_approved = true;
                    self.close_save_error = None;
                    self.request_close(ctx);
                }
                Err(error) => self.close_save_error = Some(error),
            },
            2 => {
                self.close_approved = true;
                self.close_save_error = None;
                self.request_close(ctx);
            }
            3 => self.close_save_error = None,
            _ => {}
        }
    }

    fn options(&self) -> ProcessOptions {
        self.settings.options(self.current_tool)
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        if paths.is_empty() {
            return;
        }
        if self.busy() {
            self.message = self
                .lang()
                .text(
                    "请等待当前操作结束后再添加文件。",
                    "Wait for the current operation before adding files.",
                )
                .into();
            return;
        }
        if self.active_tool().is_none() {
            self.pending_paths.extend(paths);
            return;
        }
        if self.settings.output_directory.is_empty() {
            if self.portable() {
                self.settings.output_directory = "Output".into();
            } else {
                // Input arguments retain the caller's CWD semantics; do not resolve
                // an input-derived default against the portable executable folder.
                let first = match std::path::absolute(&paths[0]) {
                    Ok(path) => path,
                    Err(error) => {
                        self.message = error.to_string();
                        return;
                    }
                };
                let parent = if first.is_dir() {
                    first.as_path()
                } else {
                    first.parent().unwrap_or(Path::new("."))
                };
                self.settings.output_directory = parent
                    .join("LocalImageTools-output")
                    .to_string_lossy()
                    .into_owned();
            }
        }
        let output_directory = match self.output_directory() {
            Ok(path) => path,
            Err(error) => {
                self.message = error;
                return;
            }
        };
        self.importing = true;
        self.message = self
            .lang()
            .text("正在读取文件信息…", "Reading file information…")
            .into();
        worker::import(
            paths,
            ImportOptions {
                recursive: self.settings.recursive,
                output_directory,
            },
            self.sender.clone(),
            ctx.clone(),
        );
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Paths(paths) => {
                    self.dialog_open = false;
                    self.add_paths(paths, ctx);
                }
                Event::OutputDirectory(path) => {
                    self.dialog_open = false;
                    if let Some(path) = path {
                        self.settings.output_directory = path.to_string_lossy().into_owned();
                    }
                }
                Event::Imported { items, issues } => {
                    self.importing = false;
                    let mut known: HashSet<_> = self
                        .rows
                        .iter()
                        .map(|r| path_key(&r.item.original.path))
                        .collect();
                    let mut added = 0;
                    let mut duplicate = 0;
                    for (item, info) in items {
                        if !known.insert(path_key(&item.original.path)) {
                            duplicate += 1;
                            continue;
                        }
                        let detail = info.as_ref().err().cloned().unwrap_or_default();
                        let info = info.ok();
                        self.rows.push(Row {
                            item,
                            original_info: info.clone(),
                            info,
                            selected: true,
                            state: State::Ready,
                            detail,
                            result: None,
                            step_settings: None,
                        });
                        added += 1;
                    }
                    if self.focus.is_none() {
                        self.focus = self.rows.first().map(|r| r.item.id);
                    }
                    self.message = if self.lang() == Language::Chinese {
                        format!(
                            "已添加 {added} 个文件 · 跳过 {} 个重复或不可导入项",
                            issues.len() + duplicate
                        )
                    } else {
                        format!(
                            "Added {added} files · {} duplicate or unavailable items skipped",
                            issues.len() + duplicate
                        )
                    };
                    self.issues.extend(issues);
                    if let Some(diagnostics) = &mut self.diagnostics {
                        diagnostics.deadline = Instant::now()
                            + std::time::Duration::from_millis(diagnostics.config.idle_ms);
                    }
                }
                Event::Prepared {
                    request_id,
                    tool,
                    items,
                    retry,
                } => {
                    // Cancellation can race with an already queued result. Check
                    // both identities before touching any session or busy state.
                    if self.preparation.as_ref().is_none_or(|pending| {
                        pending.request.id != request_id
                            || pending.request.tool != tool
                            || self.active_tool() != Some(tool)
                    }) {
                        continue;
                    }
                    let pending = self.preparation.take().expect("matching preparation");
                    self.cache.clear();
                    let mut success_ids = Vec::new();
                    let mut failed = 0;
                    for (id, result) in items {
                        if let Some(row) = self.rows.iter_mut().find(|r| r.item.id == id) {
                            if let Some(target) =
                                pending.targets.iter().find(|target| target.id == id)
                            {
                                row.item = target.clone();
                                row.info = if target.input_source == InputSource::Original {
                                    row.original_info.clone()
                                } else {
                                    None
                                };
                            }
                            match result {
                                Ok((item, Ok(info))) => {
                                    row.item = item;
                                    row.info = Some(info);
                                    if row.item.input_source == InputSource::Original {
                                        row.original_info = row.info.clone();
                                    }
                                    row.state = State::Ready;
                                    row.result = None;
                                    row.detail.clear();
                                    success_ids.push(id);
                                }
                                Ok((_, Err(error))) | Err(error) => {
                                    row.state = State::Failed;
                                    row.detail = error;
                                    row.result = None;
                                    row.selected = false;
                                    failed += 1;
                                }
                            }
                        }
                    }
                    self.message = if self.lang() == Language::Chinese {
                        format!(
                            "{} 个文件已就绪 · {failed} 个无法读取，可开始处理或切换工具。",
                            success_ids.len()
                        )
                    } else {
                        format!(
                            "{} ready · {failed} unavailable. Start processing or choose another tool.",
                            success_ids.len()
                        )
                    };
                    if retry && !success_ids.is_empty() {
                        self.start(ctx, Some(&success_ids));
                    }
                }
                Event::Batch(event) => match event {
                    BatchEvent::Started { total } => self.total = total,
                    BatchEvent::ItemStarted { id } => {
                        if let Some(row) = self.rows.iter_mut().find(|r| r.item.id == id) {
                            row.state = State::Running;
                        }
                    }
                    BatchEvent::ItemFinished(result) => {
                        self.done += 1;
                        if let Some(row) =
                            self.rows.iter_mut().find(|r| r.item.id == result.item.id)
                        {
                            row.state = match &result.status {
                                TaskStatus::Success { report, .. } => {
                                    row.detail = report.warnings.join("\n");
                                    State::Success
                                }
                                TaskStatus::Skipped { reason } => {
                                    row.detail = reason.clone();
                                    State::Skipped
                                }
                                TaskStatus::Failed { reason } => {
                                    row.detail = reason.clone();
                                    State::Failed
                                }
                                TaskStatus::Cancelled => State::Cancelled,
                            };
                            row.result = Some(*result);
                        }
                    }
                    BatchEvent::Finished(report) => {
                        let successful = report
                            .results
                            .iter()
                            .filter(|r| r.output().is_some())
                            .count();
                        if let Some(diagnostics) = &self.diagnostics {
                            diagnostics.log(serde_json::json!({
                                "event":"batch-finished", "step":diagnostics.index,
                                "elapsed_ms":report.elapsed_ms, "total":report.results.len(),
                                "success":successful, "cancelled":report.cancelled
                            }));
                        }
                        self.message = if self.lang() == Language::Chinese {
                            format!(
                                "{} · {successful}/{} 个有效输出 · {:.1} 秒",
                                if report.cancelled {
                                    "已取消"
                                } else {
                                    "处理完成"
                                },
                                report.results.len(),
                                report.elapsed_ms as f64 / 1000.0
                            )
                        } else {
                            format!(
                                "{} · {successful}/{} valid outputs · {:.1} s",
                                if report.cancelled {
                                    "Cancelled"
                                } else {
                                    "Complete"
                                },
                                report.results.len(),
                                report.elapsed_ms as f64 / 1000.0
                            )
                        };
                    }
                },
                Event::BatchClosed(result) => {
                    if let Some(diagnostics) = &mut self.diagnostics {
                        diagnostics.cancel_closed_ms = diagnostics
                            .cancel_requested
                            .map(|time| time.elapsed().as_secs_f64() * 1000.0);
                        diagnostics.log(serde_json::json!({
                            "event":"batch-closed", "step":diagnostics.index,
                            "ok":result.is_ok(), "error":result.as_ref().err(),
                            "latency_ms":diagnostics.cancel_closed_ms
                        }));
                    }
                    self.running = false;
                    self.cache.processing(false);
                    if let Err(error) = result {
                        self.message = error.clone();
                        for row in &mut self.rows {
                            if matches!(row.state, State::Running | State::Queued) {
                                row.state = State::Failed;
                                row.detail = error.clone();
                            }
                        }
                    }
                    if self.closing {
                        self.request_close(ctx);
                    }
                }
                Event::CancelAccepted => {
                    self.cancel_accepted = true;
                    if let Some(diagnostics) = &mut self.diagnostics {
                        diagnostics.cancel_accepted_ms = diagnostics
                            .cancel_requested
                            .map(|time| time.elapsed().as_secs_f64() * 1000.0);
                        diagnostics.log(serde_json::json!({"event":"cancel-accepted","latency_ms":diagnostics.cancel_accepted_ms}));
                    }
                }
                Event::Notice(message) => {
                    self.message = message;
                }
            }
        }
        self.cache.poll(ctx);
    }

    fn start(&mut self, ctx: &egui::Context, ids: Option<&[u64]>) {
        if self.busy() || self.active_tool().is_none() {
            return;
        }
        if self.settings.output_directory.trim().is_empty() {
            self.message = self
                .lang()
                .text("请选择输出目录。", "Choose an output folder.")
                .into();
            return;
        }
        let options = self.options();
        if let Err(error) = options.validate() {
            self.message = error.to_string();
            return;
        }
        let output_directory = match self.output_directory() {
            Ok(path) => path,
            Err(error) => {
                self.message = error;
                return;
            }
        };
        let step_settings = self.settings.snapshot(self.current_tool);
        let items: Vec<_> = self
            .rows
            .iter_mut()
            .filter(|r| ids.map_or(r.selected, |ids| ids.contains(&r.item.id)))
            .map(|r| {
                r.state = State::Queued;
                r.result = None;
                r.detail.clear();
                r.step_settings = Some(step_settings.clone());
                r.item.clone()
            })
            .collect();
        if items.is_empty() {
            return;
        }
        self.cache.stop_animation();
        self.cache.processing(true);
        self.running = true;
        self.cancelled = Arc::new(AtomicBool::new(false));
        self.cancel_accepted = false;
        self.done = 0;
        self.total = items.len();
        self.started = Some(Instant::now());
        self.message = self
            .lang()
            .text(
                "本次参数已固定，正在处理所选文件。",
                "This batch uses a fixed snapshot of your settings.",
            )
            .into();
        worker::process(
            items,
            options,
            BatchConfig {
                output_directory,
                ..Default::default()
            },
            self.cache.decode_gate(),
            self.cancelled.clone(),
            self.sender.clone(),
            ctx.clone(),
        );
    }

    fn prepare(&mut self, ctx: &egui::Context, mode: u8) {
        if self.busy() || self.active_tool().is_none() {
            return;
        }
        if mode == 1 {
            self.continue_with(ctx, self.current_tool);
            return;
        }
        let items: Vec<_> = self
            .rows
            .iter()
            .filter(|r| r.selected)
            .filter_map(|r| {
                let action = match mode {
                    0 => Prepare::Original,
                    _ if r.state == State::Failed => Prepare::Retry,
                    _ => return None,
                };
                Some((r.item.clone(), action))
            })
            .collect();
        if items.is_empty() {
            return;
        }
        // Keep the current session intact until validation commits. Cancelling
        // or navigating away must not replace a completed row's input/result.
        let previous = self.preparation_state();
        self.cache.stop_animation();
        if mode == 0
            && let Some(settings) = self
                .rows
                .iter()
                .find(|r| r.selected && Some(r.item.id) == self.focus)
                .or_else(|| self.rows.iter().find(|r| r.selected))
                .and_then(|r| r.step_settings.clone())
            && settings.tool() == self.current_tool
        {
            self.settings.restore_tool(&settings);
        }
        self.preview_original = false;
        self.message = self
            .lang()
            .text(
                "正在验证本次输入文件…",
                "Verifying the input files for this step…",
            )
            .into();
        self.begin_preparation(ctx, items, mode == 2, previous, None);
    }

    fn header(&mut self, ctx: &egui::Context) {
        let language = self.lang();
        let busy = self.navigation_busy();
        let mut home = false;
        let mut next = None;
        egui::TopBottomPanel::top("header")
            .exact_height(68.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(24, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
                    ui.painter().rect_filled(rect, 9, ACCENT);
                    let inside = rect.shrink(8.0);
                    ui.painter().rect_stroke(
                        inside,
                        3,
                        Stroke::new(1.6_f32, Color32::WHITE),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().circle_filled(
                        inside.left_top() + egui::vec2(6.0, 6.0),
                        2.2,
                        Color32::WHITE,
                    );
                    ui.painter().line_segment(
                        [
                            inside.left_bottom() + egui::vec2(3.0, -4.0),
                            inside.center(),
                        ],
                        Stroke::new(1.6_f32, Color32::WHITE),
                    );
                    ui.painter().line_segment(
                        [
                            inside.center(),
                            inside.right_bottom() + egui::vec2(-3.0, -4.0),
                        ],
                        Stroke::new(1.6_f32, Color32::WHITE),
                    );
                    if response
                        .on_hover_text(language.text("工具首页", "Tool home"))
                        .clicked()
                        && !busy
                    {
                        home = true;
                    }
                    ui.add_space(3.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if ui
                            .add(
                                egui::Label::new(
                                    RichText::new(
                                        language.text("本地图像工具", "Local Image Tools"),
                                    )
                                    .size(20.0)
                                    .strong(),
                                )
                                .sense(Sense::click()),
                            )
                            .clicked()
                            && !busy
                        {
                            home = true;
                        }
                        ui.label(
                            RichText::new(
                                language.text(
                                    "图片留在本机，处理简单一点",
                                    "Your images. Your device.",
                                ),
                            )
                            .size(12.0)
                            .color(MUTED),
                        );
                    });
                    ui.add_space(16.0);
                    ui.add_enabled_ui(!busy, |ui| {
                        if self.active_tool().is_some() {
                            home |= ui
                                .button(language.text("← 所有工具", "← All tools"))
                                .on_hover_text(language.text(
                                    "返回工具首页，保留各工具的图片和设置。",
                                    "Return home and keep each tool's images and settings.",
                                ))
                                .clicked();
                        }
                        egui::ComboBox::from_id_salt("tool-navigation")
                            .width(146.0)
                            .selected_text(
                                self.active_tool()
                                    .map(|tool| tool_gallery::title(tool, language))
                                    .unwrap_or(language.text("所有工具", "All tools")),
                            )
                            .show_ui(ui, |ui| {
                                if ui.button(language.text("工具首页", "Tool home")).clicked() {
                                    home = true;
                                    ui.close();
                                }
                                ui.separator();
                                for tool in ToolKind::ALL {
                                    if ui
                                        .selectable_label(
                                            self.active_tool() == Some(tool),
                                            tool_gallery::title(tool, language),
                                        )
                                        .clicked()
                                    {
                                        next = Some(tool);
                                        ui.close();
                                    }
                                }
                            });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("language")
                            .width(95.0)
                            .selected_text(if language == Language::Chinese {
                                "简体中文"
                            } else {
                                "English"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.settings.language,
                                    Language::Chinese,
                                    "简体中文",
                                );
                                ui.selectable_value(
                                    &mut self.settings.language,
                                    Language::English,
                                    "English",
                                );
                            });
                        ui.label(
                            RichText::new(if self.portable() {
                                language.text("便携版 · 本地处理", "Portable · Local")
                            } else {
                                language.text("完全本地处理", "Entirely local")
                            })
                            .small()
                            .color(MUTED),
                        )
                        .on_hover_text(
                            self.settings_store
                                .as_ref()
                                .ok()
                                .map(|store| store.path().display().to_string())
                                .unwrap_or_default(),
                        );
                    });
                });
            });
        if home {
            self.go_home();
        }
        if let Some(tool) = next {
            self.switch_tool(tool, ctx);
        }
        if let Some(notice) = self.settings_notice.clone() {
            let mut dismiss = false;
            egui::TopBottomPanel::top("settings-notice")
                .frame(
                    egui::Frame::new()
                        .fill(Color32::from_rgb(255, 247, 226))
                        .inner_margin(egui::Margin::symmetric(24, 8)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(&notice).small()).truncate())
                            .on_hover_text(&notice);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            dismiss = ui
                                .small_button(language.text("关闭提示", "Dismiss"))
                                .clicked();
                        });
                    });
                });
            if dismiss {
                self.settings_notice = None;
            }
        }
    }

    fn parameters(&mut self, ctx: &egui::Context) {
        let language = self.lang();
        let tool = self.current_tool;
        egui::SidePanel::right(egui::Id::new(("parameters", tool.id()))).exact_width(310.0).resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgb(250, 251, 253)).inner_margin(egui::Margin::symmetric(20, 18)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(match tool {
                        ToolKind::Compress => language.text("压缩选项", "Compression options"),
                        ToolKind::Resize => language.text("尺寸选项", "Resize options"),
                        ToolKind::Convert => language.text("转换选项", "Conversion options"),
                    }).strong().size(18.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.menu_button("···", |ui| {
                            if ui.button(language.text("恢复此工具默认参数", "Reset this tool")).clicked() {
                                self.settings.reset_tool(tool); ui.close();
                            }
                            if ui.button(language.text("载入此工具常用设置", "Load this tool's preferences")).clicked() {
                                self.load_settings(); ui.close();
                            }
                        });
                    });
                });
                if self.running {
                    ui.label(RichText::new(language.text("修改将用于下一批次", "Changes apply to the next batch")).small().color(ACCENT));
                }
                ui.add_space(6.0);
                egui::ScrollArea::vertical().id_salt(("settings-scroll", tool.id())).show(ui, |ui| {
                    match tool {
                        ToolKind::Compress => {
                            let settings = &mut self.settings.compress;
                            theme::label(ui, language.text("压缩方式", "Compression mode"), language.text("保持原格式与像素尺寸", "Keep original formats and dimensions"));
                            ui.columns(2, |columns| {
                                for (index, column) in columns.iter_mut().enumerate() {
                                    let lossless = index == 1;
                                    if column.add_sized([column.available_width(), 34.0], egui::Button::selectable(
                                        settings.lossless == lossless,
                                        if lossless { language.text("无损优化", "Lossless") } else { language.text("有损压缩", "Lossy") },
                                    )).clicked() {
                                        settings.lossless = lossless;
                                    }
                                }
                            });
                            if !settings.lossless {
                                quality_slider(ui, language.text("JPEG 质量", "JPEG quality"), &mut settings.jpeg_quality);
                                quality_slider(ui, language.text("WebP 质量", "WebP quality"), &mut settings.webp_quality);
                            }
                            ui.label(RichText::new(language.text(
                                "PNG / GIF 使用无损优化。动画 GIF 保留全部帧。",
                                "PNG / GIF use lossless optimization. Animated GIFs keep all frames.",
                            )).small().color(MUTED));
                            ui.add_space(5.0);
                            ui.checkbox(&mut settings.strip_metadata, language.text("删除非显示必需元数据", "Remove nonessential metadata"));
                            ui.label(RichText::new(language.text(
                                "已充分压缩的图片可能不会变小，结果会如实显示。",
                                "Already optimized images may not become smaller. Results show the actual file size.",
                            )).small().color(MUTED));
                        }
                        ToolKind::Resize => {
                            let settings = &mut self.settings.resize;
                            theme::label(ui, language.text("调整像素尺寸", "Pixel dimensions"), language.text("保持宽高比与原格式", "Keep aspect ratio and original format"));
                            egui::ComboBox::from_id_salt("resize-mode").width(ui.available_width())
                                .selected_text(resize_label(language, settings.mode)).show_ui(ui, |ui| {
                                    for mode in 1..=3 { ui.selectable_value(&mut settings.mode, mode, resize_label(language, mode)); }
                                });
                            match settings.mode {
                                1 => {
                                    ui.horizontal(|ui| {
                                        ui.label(language.text("宽", "W"));
                                        ui.add(egui::DragValue::new(&mut settings.width).range(1..=100000).suffix(" px").speed(10));
                                        ui.label("×");
                                        ui.add(egui::DragValue::new(&mut settings.height).range(1..=100000).suffix(" px").speed(10));
                                    });
                                    ui.label(RichText::new(language.text("图片等比缩放至此范围以内。", "Fit each image inside these dimensions.")).small().color(MUTED));
                                }
                                3 => { percent_slider(ui, language.text("缩放比例", "Scale"), &mut settings.percent); }
                                _ => { ui.add(egui::DragValue::new(&mut settings.longest_edge).range(1..=100000).suffix(" px").speed(10)); }
                            }
                            ui.checkbox(&mut settings.allow_upscale, language.text("允许放大像素尺寸", "Allow upscaling"));
                            ui.add_space(8.0);
                            egui::CollapsingHeader::new(language.text("高级输出选项", "Advanced output options"))
                                .id_salt("resize-advanced").show(ui, |ui| {
                                    ui.label(RichText::new(language.text("缩放后重新编码图片时使用。", "Used when encoding the resized image.")).small().color(MUTED));
                                    quality_slider(ui, language.text("JPEG 质量", "JPEG quality"), &mut settings.jpeg_quality);
                                    quality_slider(ui, language.text("WebP 质量", "WebP quality"), &mut settings.webp_quality);
                                    ui.checkbox(&mut settings.strip_metadata, language.text("删除非显示必需元数据", "Remove nonessential metadata"));
                                    matte_picker(ui, language, &mut settings.background);
                                });
                        }
                        ToolKind::Convert => {
                            let has_animation = self.rows.iter().any(|row| row.info.as_ref().is_some_and(|info| info.animated));
                            let settings = &mut self.settings.convert;
                            theme::label(ui, language.text("转换为", "Convert to"), language.text("保持原有像素尺寸", "Keep original dimensions"));
                            egui::ComboBox::from_id_salt("convert-format").width(ui.available_width())
                                .selected_text(format_label(settings.format)).show_ui(ui, |ui| {
                                    for format in [Format::Jpeg, Format::Png, Format::WebP, Format::Gif] {
                                        ui.selectable_value(&mut settings.format, format, format_label(format));
                                    }
                                });
                            match settings.format {
                                Format::Jpeg => { quality_slider(ui, language.text("JPEG 质量", "JPEG quality"), &mut settings.jpeg_quality); }
                                Format::WebP => {
                                    ui.checkbox(&mut settings.webp_lossless, language.text("无损 WebP", "Lossless WebP"));
                                    if !settings.webp_lossless { quality_slider(ui, language.text("WebP 质量", "WebP quality"), &mut settings.webp_quality); }
                                }
                                Format::Gif => { ui.label(RichText::new(language.text("GIF 最多支持 256 色。", "GIF supports up to 256 colors.")).small().color(MUTED)); }
                                Format::Png => {}
                            }
                            if matches!(settings.format, Format::Jpeg | Format::Gif) {
                                matte_picker(ui, language, &mut settings.background);
                            }
                            if settings.format != Format::Gif && has_animation {
                                ui.add_space(6.0);
                                ui.checkbox(&mut settings.gif_first_frame, language.text("动画 GIF 仅导出首帧", "Export the first GIF frame only"));
                                ui.label(RichText::new(language.text(
                                    "未勾选时跳过动画 GIF，避免丢失动画。WebP 仅支持静态图片。",
                                    "Animated GIFs are skipped unless selected. WebP output is static only.",
                                )).small().color(MUTED));
                            }
                            ui.add_space(5.0);
                            ui.checkbox(&mut settings.strip_metadata, language.text("删除非显示必需元数据", "Remove nonessential metadata"));
                        }
                    }
                    ui.add_space(12.0); ui.separator(); ui.add_space(8.0);
                    theme::label(ui, language.text("输出目录", "Output folder"), "");
                    ui.horizontal(|ui| {
                        ui.add_sized([ui.available_width() - 48.0, 31.0], egui::TextEdit::singleline(&mut self.settings.output_directory)
                            .hint_text(language.text("选择保存位置", "Choose where to save")));
                        if ui.add_enabled(!self.busy(), egui::Button::new("…")).clicked() { self.choose_output(ctx); }
                    });
                    if self.portable() && let Ok(path) = self.output_directory() {
                        let path = path.display().to_string();
                        ui.add(egui::Label::new(RichText::new(&path).small().color(MUTED)).truncate()).on_hover_text(&path);
                    }
                    ui.label(RichText::new(language.text(
                        "原图始终保留，同名文件自动编号。",
                        "Originals are preserved. Existing files are never overwritten.",
                    )).small().color(MUTED));
                    ui.add_space(7.0);
                    if ui.add_sized([ui.available_width(), 32.0], egui::Button::new(language.text("保存常用设置", "Save preferences"))).clicked() {
                        self.message = self.save_settings().map(|_| if self.portable() {
                            language.text("设置已保存，可随整个程序文件夹一起携带。", "Settings saved; take the whole application folder with you.")
                        } else { language.text("设置已保存在本机。", "Settings saved on this device.") }.to_owned()).unwrap_or_else(|error| error);
                    }
                    ui.add_space(10.0);
                });
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        let language = self.lang();
        let selected = self.rows.iter().filter(|r| r.selected).count();
        egui::TopBottomPanel::bottom("footer")
            .exact_height(76.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(24, 14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_max_width((ui.available_width() - 380.0).max(200.0));
                        ui.spacing_mut().item_spacing.y = 7.0;
                        if self.running {
                            let title = if self.cancel_accepted {
                                language
                                    .text(
                                        "取消已受理，正在安全结束…",
                                        "Cancellation accepted; finishing safely…",
                                    )
                                    .to_owned()
                            } else if self.cancelled.load(Ordering::Relaxed) {
                                language
                                    .text("正在请求取消…", "Requesting cancellation…")
                                    .to_owned()
                            } else {
                                format!(
                                    "{}  {} / {}",
                                    language.text("正在处理", "Processing"),
                                    self.done,
                                    self.total
                                )
                            };
                            ui.label(RichText::new(title).strong());
                            thin_progress(
                                ui,
                                self.done as f32 / self.total.max(1) as f32,
                                330.0,
                                ACCENT,
                            );
                        } else if self.preparing() {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.label(
                                    RichText::new(
                                        language
                                            .text("正在检查输入图片…", "Checking input images…"),
                                    )
                                    .strong(),
                                );
                            });
                            ui.label(
                                RichText::new(language.text(
                                    "可取消检查，或直接返回、切换工具。",
                                    "Cancel checking, return home, or switch tools at any time.",
                                ))
                                .small()
                                .color(MUTED),
                            );
                        } else {
                            let saved = self
                                .rows
                                .iter()
                                .filter_map(|r| match &r.result.as_ref()?.status {
                                    TaskStatus::Success { report, .. } => Some(
                                        i128::from(report.input_bytes)
                                            - i128::from(report.output_bytes),
                                    ),
                                    _ => None,
                                })
                                .sum::<i128>();
                            let summary = if language == Language::Chinese {
                                format!("{} 个文件 · 已选 {selected} 个", self.rows.len())
                            } else {
                                format!("{} files · {selected} selected", self.rows.len())
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(summary).strong());
                                if saved != 0 {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} {}",
                                            language.text(
                                                if saved > 0 { "共节省" } else { "共增加" },
                                                if saved > 0 { "Saved" } else { "Increased" }
                                            ),
                                            theme::bytes(saved.unsigned_abs() as u64)
                                        ))
                                        .color(if saved > 0 { ACCENT } else { MUTED }),
                                    );
                                }
                            });
                            let status = if self.message.is_empty() {
                                language.text(
                                    "添加图片，选择设置，然后开始。",
                                    "Add images, choose your settings, and start.",
                                )
                            } else {
                                &self.message
                            };
                            ui.add(
                                egui::Label::new(RichText::new(status).small().color(MUTED))
                                    .truncate(),
                            )
                            .on_hover_text(status);
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.running {
                            if ui
                                .add_enabled(
                                    !self.cancelled.load(Ordering::Relaxed),
                                    egui::Button::new(language.text("取消处理", "Cancel batch"))
                                        .min_size(egui::vec2(152.0, 42.0)),
                                )
                                .clicked()
                            {
                                self.cancelled.store(true, Ordering::Release);
                            }
                        } else if self.preparing() {
                            if ui
                                .add(
                                    egui::Button::new(language.text("取消检查", "Cancel checking"))
                                        .min_size(egui::vec2(180.0, 44.0)),
                                )
                                .clicked()
                            {
                                self.cancel_preparation();
                            }
                        } else if theme::primary(
                            ui,
                            RichText::new(format!(
                                "{}  {selected}",
                                tool_gallery::action_label(self.current_tool, language)
                            ))
                            .color(Color32::WHITE),
                            selected > 0 && !self.busy(),
                            egui::vec2(180.0, 44.0),
                        )
                        .clicked()
                        {
                            self.start(ctx, None);
                        }
                        if ui
                            .add_enabled(
                                !self.settings.output_directory.is_empty(),
                                egui::Button::new(
                                    language.text("打开输出目录", "Open output folder"),
                                )
                                .frame(false)
                                .min_size(egui::vec2(132.0, 36.0)),
                            )
                            .clicked()
                        {
                            match self.output_directory() {
                                Ok(path) => worker::open_output(
                                    path.to_string_lossy().into_owned(),
                                    self.sender.clone(),
                                    ctx.clone(),
                                ),
                                Err(error) => self.message = error,
                            }
                        }
                    });
                });
            });
    }

    fn workspace(&mut self, ctx: &egui::Context) {
        let language = self.lang();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(22, 18)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(tool_gallery::title(self.current_tool, language))
                            .size(18.0)
                            .strong(),
                    );
                    if self.importing || self.preparing() {
                        ui.spinner();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.busy(),
                                egui::Button::new(language.text("+ 文件夹", "+ Folder")),
                            )
                            .clicked()
                        {
                            self.choose_files(ctx, true);
                        }
                        if ui
                            .add_enabled(
                                !self.busy(),
                                egui::Button::new(language.text("+ 添加图片", "+ Add images")),
                            )
                            .clicked()
                        {
                            self.choose_files(ctx, false);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.checkbox(
                        &mut self.settings.recursive,
                        language.text("包括子目录", "Include subfolders"),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !self.issues.is_empty()
                            && ui
                                .link(format!(
                                    "{} ({})",
                                    language.text("导入提示", "Import notes"),
                                    self.issues.len()
                                ))
                                .clicked()
                        {
                            self.show_issues = true;
                        }
                        ui.label(
                            RichText::new("JPEG · PNG · WebP · GIF")
                                .small()
                                .color(MUTED),
                        );
                    });
                });
                ui.add_space(5.0);
                ui.separator();
                if self.rows.is_empty() {
                    return;
                }
                let busy = self.busy();
                ui.horizontal(|ui| {
                    let mut all = self.rows.iter().all(|r| r.selected);
                    if ui
                        .add_enabled(
                            !self.preparing(),
                            egui::Checkbox::new(&mut all, language.text("全选", "All")),
                        )
                        .changed()
                    {
                        for row in &mut self.rows {
                            row.selected = all;
                        }
                    }
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(language.text("移除所选", "Remove selected")),
                        )
                        .clicked()
                    {
                        self.rows.retain(|r| !r.selected);
                        self.focus = self.rows.first().map(|r| r.item.id);
                        self.cache.clear();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.checkbox(&mut self.show_preview, language.text("预览", "Preview"));
                        let failures = self
                            .rows
                            .iter()
                            .any(|r| r.selected && r.state == State::Failed);
                        if ui
                            .add_enabled(
                                !busy && failures,
                                egui::Button::new(language.text("重试失败项", "Retry failed")),
                            )
                            .clicked()
                        {
                            self.prepare(ctx, 2);
                        }
                    });
                });
                ui.add_space(2.0);
                let preview_height = if self.show_preview { 280.0 } else { 0.0 };
                let list_height = (ui.available_height() - preview_height - 45.0).max(110.0);
                egui::Frame::new()
                    .fill(Color32::from_rgb(246, 248, 249))
                    .inner_margin(egui::Margin::symmetric(12, 7))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                RichText::new(
                                    language.text("文件 / 本次输入", "File / Input for this step"),
                                )
                                .small()
                                .color(MUTED),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(
                                        language.text("输出体积 · 状态", "Output size · Status"),
                                    )
                                    .small()
                                    .color(MUTED),
                                );
                            });
                        });
                    });
                egui::ScrollArea::vertical()
                    .id_salt("image-list")
                    .max_height(list_height)
                    .min_scrolled_height(list_height)
                    .auto_shrink([false, false])
                    .show_rows(ui, 64.0, self.rows.len(), |ui, range| {
                        for index in range {
                            self.row(ui, index);
                        }
                    });
                ui.separator();
                if self.show_preview {
                    self.preview_panel(ui, ctx);
                } else {
                    self.compact_actions(ui, ctx);
                }
            });
    }

    fn row(&mut self, ui: &mut egui::Ui, index: usize) {
        let language = self.lang();
        let preparing = self.preparing();
        let row = &mut self.rows[index];
        let focused = self.focus == Some(row.item.id);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), Sense::click());
        if focused {
            ui.painter().rect_filled(rect, 5, PALE);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, 5, Color32::from_rgb(248, 250, 250));
        }
        if response.clicked() {
            self.focus = Some(row.item.id);
            self.cache.stop_animation();
        }
        let thumb = self.cache.texture(&row.item.input.path, false);
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(9.0, 8.0))),
            |ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled(!preparing, egui::Checkbox::new(&mut row.selected, ""));
                    if let Some(texture) = thumb {
                        ui.add(
                            egui::Image::new(&texture)
                                .fit_to_exact_size(egui::vec2(44.0, 44.0))
                                .corner_radius(4),
                        );
                    } else {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), Sense::hover());
                        ui.painter()
                            .rect_filled(r, 4, Color32::from_rgb(227, 234, 233));
                        ui.painter().text(
                            r.center(),
                            egui::Align2::CENTER_CENTER,
                            "IMG",
                            FontId::proportional(10.0),
                            MUTED,
                        );
                    }
                    let right_width = if ui.available_width() > 480.0 {
                        180.0
                    } else {
                        142.0
                    };
                    let middle = (ui.available_width() - right_width - 12.0).max(120.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(middle, 48.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            let name = row.item.relative_path.to_string_lossy();
                            ui.add(
                                egui::Label::new(RichText::new(name.as_ref()).strong()).truncate(),
                            )
                            .on_hover_text(format!(
                                "{}\n{}",
                                row.item.original.path.display(),
                                row.item.input.path.display()
                            ));
                            let metadata = if let Some(info) = &row.info {
                                let (w, h) = info.display_dimensions();
                                format!(
                                    "{}  ·  {w} × {h}  ·  {}",
                                    info.format,
                                    theme::bytes(info.bytes)
                                )
                            } else {
                                theme::bytes(row.item.input.fingerprint.len)
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{metadata}  ·  {}",
                                        if row.item.input_source == InputSource::Original {
                                            language.text("原图", "Original")
                                        } else {
                                            language.text("上次输出", "Previous output")
                                        }
                                    ))
                                    .small()
                                    .color(MUTED),
                                )
                                .truncate(),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(right_width, 48.0),
                        Layout::top_down(Align::Max),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            let (status, color) = status_label(language, row);
                            ui.label(RichText::new(status).small().color(color));
                            if let Some(TaskResult {
                                status: TaskStatus::Success { report, .. },
                                ..
                            }) = &row.result
                            {
                                let delta = (report.output_bytes as f64
                                    / report.input_bytes.max(1) as f64
                                    - 1.0)
                                    * 100.0;
                                ui.label(
                                    RichText::new(format!(
                                        "{}  ({delta:+.1}%)",
                                        theme::bytes(report.output_bytes)
                                    ))
                                    .small()
                                    .color(MUTED),
                                );
                            } else if !row.detail.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&row.detail).small().color(MUTED),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&row.detail);
                            }
                        },
                    );
                });
            },
        );
    }

    fn compact_actions(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let language = self.lang();
        let busy = self.busy();
        let selected = self.rows.iter().any(|r| r.selected);
        let successful = self
            .rows
            .iter()
            .any(|r| r.selected && r.result.as_ref().and_then(TaskResult::output).is_some());
        ui.horizontal(|ui| {
            if let Some(info) = self
                .rows
                .iter()
                .find(|r| Some(r.item.id) == self.focus)
                .and_then(|r| r.info.as_ref())
            {
                let (width, height) = info.display_dimensions();
                if let Ok((w, h)) = self.options().resize.dimensions(width, height) {
                    ui.label(
                        RichText::new(format!("{} {w} × {h}", language.text("预计", "Expected")))
                            .small()
                            .color(MUTED),
                    );
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.continuation_menu(ui, ctx, !busy && successful);
                if ui
                    .add_enabled(
                        !busy && selected,
                        egui::Button::new(language.text("重新调整", "Readjust originals")),
                    )
                    .clicked()
                {
                    self.prepare(ctx, 0);
                }
            });
        });
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let language = self.lang();
        let Some(index) = self.rows.iter().position(|r| Some(r.item.id) == self.focus) else {
            return;
        };
        let row = &self.rows[index];
        let original = row.item.original.path.clone();
        let input = row.item.input.path.clone();
        let output = row
            .result
            .as_ref()
            .and_then(TaskResult::output)
            .map(|s| s.path.clone());
        let continued = row.item.input_source == InputSource::PreviousOutput;
        let info = if self.preview_original {
            row.original_info.as_ref()
        } else {
            row.info.as_ref()
        };
        let before_dimensions = info.map(ImageInfo::display_dimensions);
        let animated = info.is_some_and(|i| i.animated);
        let output_info = match &row.result.as_ref().map(|r| &r.status) {
            Some(TaskStatus::Success { report, .. }) => {
                Some((report.width, report.height, report.format))
            }
            _ => None,
        };
        let detail = row.detail.clone();
        let expected = row.info.as_ref().and_then(|i| {
            let (w, h) = i.display_dimensions();
            self.options().resize.dimensions(w, h).ok()
        });
        let busy = self.busy();
        let selected_success = self
            .rows
            .iter()
            .any(|r| r.selected && r.result.as_ref().and_then(TaskResult::output).is_some());
        let selected = self.rows.iter().any(|r| r.selected);
        ui.horizontal(|ui| {
            ui.label(RichText::new(language.text("前后预览", "Before / After")).strong());
            if let Some((w, h)) = expected {
                ui.label(
                    RichText::new(format!("{} {w} × {h}", language.text("预计", "Expected")))
                        .small()
                        .color(MUTED),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.continuation_menu(ui, ctx, !busy && selected_success);
                if ui
                    .add_enabled(
                        !busy && selected,
                        egui::Button::new(language.text("重新调整", "Readjust originals")),
                    )
                    .on_hover_text(language.text(
                        "回到最初原图，载入此工具上次处理的参数供修改。",
                        "Return to the original import and restore this tool's last processing settings.",
                    ))
                    .clicked()
                {
                    self.prepare(ctx, 0);
                }
            });
        });
        if continued {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.preview_original,
                    false,
                    language.text("本次输入：上次输出", "Input: previous output"),
                );
                ui.selectable_value(
                    &mut self.preview_original,
                    true,
                    language.text("最初原图", "Original import"),
                );
            });
        }
        let before = if self.preview_original {
            original
        } else {
            input
        };
        let preview_header_height = ui.spacing().interact_size.y;
        let note_height = ui.text_style_height(&egui::TextStyle::Small);
        let image_height = (ui.available_height()
            - preview_header_height
            - note_height
            - ui.spacing().item_spacing.y * 3.0)
            .clamp(48.0, if continued { 145.0 } else { 170.0 });
        ui.columns(2, |columns| {
            self.preview_image(
                &mut columns[0],
                ctx,
                &before,
                language.text(
                    if self.preview_original {
                        "最初原图"
                    } else {
                        "本次输入"
                    },
                    if self.preview_original {
                        "Original"
                    } else {
                        "Input"
                    },
                ),
                before_dimensions,
                image_height,
                animated,
            );
            if let Some(path) = output {
                self.preview_image(
                    &mut columns[1],
                    ctx,
                    &path,
                    language.text("有效输出", "Valid output"),
                    output_info.map(|(w, h, _)| (w, h)),
                    image_height,
                    output_info.is_some_and(|(_, _, f)| f == Format::Gif),
                );
            } else {
                columns[1].horizontal(|ui| {
                    ui.set_min_height(preview_header_height);
                    ui.label(
                        RichText::new(language.text("输出预览", "Output preview"))
                            .small()
                            .color(MUTED),
                    );
                });
                let (rect, _) = columns[1].allocate_exact_size(
                    egui::vec2(columns[1].available_width(), image_height),
                    Sense::hover(),
                );
                columns[1]
                    .painter()
                    .rect_filled(rect, 6, Color32::from_rgb(245, 248, 248));
                columns[1].painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    language.text("处理完成后显示", "Available after processing"),
                    FontId::proportional(13.0),
                    MUTED,
                );
            }
        });
        if !detail.is_empty() {
            ui.add(egui::Label::new(RichText::new(detail.clone()).small().color(MUTED)).truncate())
                .on_hover_text(detail);
        } else {
            ui.label(
                RichText::new(language.text(
                    "预览经过尺寸限制；上方数字为真实像素尺寸。",
                    "Previews are scaled for display; dimensions above are actual pixels.",
                ))
                .small()
                .color(MUTED),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn preview_image(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        path: &Path,
        label: &str,
        dimensions: Option<(u32, u32)>,
        height: f32,
        animated: bool,
    ) {
        let language = self.lang();
        ui.horizontal(|ui| {
            ui.set_min_height(ui.spacing().interact_size.y);
            ui.label(
                RichText::new(match dimensions {
                    Some((w, h)) => format!("{label}  ·  {w} × {h}"),
                    None => label.into(),
                })
                .small()
                .color(MUTED),
            );
            if animated {
                let playing = self.cache.animation_path.as_deref() == Some(path);
                if ui
                    .add_enabled(
                        !self.running,
                        egui::Button::new(if playing {
                            language.text("停止", "Stop")
                        } else {
                            language.text("循环预览", "Loop preview")
                        })
                        .small(),
                    )
                    .clicked()
                {
                    if playing {
                        self.cache.stop_animation();
                    } else {
                        self.cache.play(path.to_owned(), ctx.clone());
                    }
                }
            }
        });
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6, Color32::from_rgb(244, 247, 247));
        let tile = 12.0;
        for y in 0..=(height / tile) as usize {
            for x in 0..=(rect.width() / tile) as usize {
                if (x + y) % 2 == 0 {
                    let r = egui::Rect::from_min_size(
                        rect.min + egui::vec2(x as f32 * tile, y as f32 * tile),
                        Vec2::splat(tile),
                    )
                    .intersect(rect);
                    painter.rect_filled(r, 0, Color32::from_rgb(232, 237, 237));
                }
            }
        }
        let texture = if self.cache.animation_path.as_deref() == Some(path) {
            self.cache
                .animation_texture
                .clone()
                .or_else(|| self.cache.texture(path, true))
        } else {
            self.cache.texture(path, true)
        };
        if let Some(texture) = texture {
            let size = texture.size_vec2();
            let scale = ((rect.width() - 12.0) / size.x)
                .min((height - 12.0) / size.y)
                .min(1.0);
            let dest = egui::Rect::from_center_size(rect.center(), size * scale);
            painter.image(
                texture.id(),
                dest,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            let text = if self.cache.error(path, true).is_some() {
                language.text("预览不可用", "Preview unavailable")
            } else if self.running {
                language.text("处理后生成预览", "Preview loads after processing")
            } else {
                language.text("正在生成预览…", "Loading preview…")
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                FontId::proportional(13.0),
                MUTED,
            );
        }
    }
}

impl ImageToolsApp {
    fn diagnostic_snapshot(&self, diagnostics: &Diagnostics, event: &str) {
        let active = self.active_tool();
        let rows: Vec<_> = self.rows.iter().filter(|_| active.is_some()).map(|row| {
            let output = row.result.as_ref().and_then(TaskResult::output).map(|output| &output.path);
            let report = row.result.as_ref().and_then(|result| match &result.status { TaskStatus::Success { report, .. } => Some(report), _ => None });
            serde_json::json!({ "id":row.item.id, "original":row.item.original.path, "input":row.item.input.path,
                "source":format!("{:?}",row.item.input_source), "state":format!("{:?}",row.state),
                "selected":row.selected,"input_info":row.info,"output":output,"report":report,"detail":row.detail })
        }).collect();
        let queues: serde_json::Map<_, _> = ToolKind::ALL
            .into_iter()
            .map(|tool| {
                let count = if tool == self.current_tool {
                    self.rows.len()
                } else {
                    self.sessions[tool.index()].rows.len()
                };
                (tool.id().to_owned(), serde_json::json!(count))
            })
            .collect();
        diagnostics.log(serde_json::json!({"event":event,"step":diagnostics.index,"page":if active.is_some() {"tool"} else {"home"},
            "tool":active,"pending_input_count":self.pending_paths.len(),"tool_queues":queues,
            "preparing":self.preparing(),"navigation_enabled":!self.navigation_busy(),
            "preparation_id":self.preparation.as_ref().map(|pending| pending.request.id),
            "settings":self.settings,"options":active.map(|tool| self.settings.options(tool)),"rows":rows,"message":self.message,
            "portable":self.portable(),"resolved_output_directory":self.output_directory().ok()}));
    }

    fn run_diagnostics(&mut self, ctx: &egui::Context) {
        let Some(mut diagnostics) = self.diagnostics.take() else {
            return;
        };
        let mut exit_requested = false;
        diagnostics.receive_screenshots(ctx);
        match diagnostics.phase {
            0 if !self.busy() && Instant::now() >= diagnostics.deadline => {
                self.diagnostic_snapshot(&diagnostics, "ready");
                diagnostics.log(serde_json::json!({"event":"ui-resources","font_atlas":ctx.fonts(|f|f.font_image_size()),"pixels_per_point":ctx.pixels_per_point()}));
                diagnostics.capture(ctx, "00-ready".into());
                diagnostics.phase = 1;
            }
            1 if diagnostics.screenshot_received => {
                if diagnostics.index >= diagnostics.config.steps.len() {
                    diagnostics.complete = true;
                    diagnostics.phase = 6;
                    diagnostics.log(serde_json::json!({"event":"complete",
                        "max_processing_update_ms":diagnostics.max_processing_update_ms,
                        "max_processing_frame_cpu_ms":diagnostics.max_processing_cpu_ms,
                        "max_processing_tick_gap_ms":diagnostics.max_processing_tick_gap_ms,
                        "cancel_accepted_ms":diagnostics.cancel_accepted_ms,
                        "cancel_closed_ms":diagnostics.cancel_closed_ms,
                        "measurement_note":"Update/CPU time and diagnostic frame gaps; not OS input-to-photon latency"}));
                    if diagnostics.config.exit_when_done {
                        exit_requested = true;
                    }
                } else {
                    let step = diagnostics.config.steps[diagnostics.index].clone();
                    match step.action.as_str() {
                        "home" => self.go_home(),
                        "continue" | "continue_ready" => {
                            self.continue_with(ctx, step.tool.unwrap_or(self.current_tool))
                        }
                        "readjust" => {
                            if let Some(tool) = step.tool {
                                self.switch_tool(tool, ctx);
                            }
                            self.prepare(ctx, 0);
                        }
                        _ => {
                            if let Some(tool) = step.tool {
                                self.switch_tool(tool, ctx);
                            }
                        }
                    }
                    diagnostics.phase = 2;
                }
            }
            2 if !self.busy() => {
                diagnostics.cancel_requested = None;
                diagnostics.cancel_accepted_ms = None;
                diagnostics.cancel_closed_ms = None;
                let step = &diagnostics.config.steps[diagnostics.index];
                if let Some(tool) = self.active_tool() {
                    step.apply(&mut self.settings, tool);
                }
                if matches!(step.action.as_str(), "home" | "open" | "continue_ready") {
                    diagnostics.deadline = Instant::now()
                        + std::time::Duration::from_millis(diagnostics.config.settle_ms);
                    diagnostics.phase = 4;
                } else {
                    self.diagnostic_snapshot(&diagnostics, "before-process");
                    self.start(ctx, None);
                    diagnostics.step_started = Instant::now();
                    diagnostics.phase = 3;
                }
            }
            3 => {
                if self.running
                    && !self.cancelled.load(Ordering::Relaxed)
                    && diagnostics.config.steps[diagnostics.index]
                        .cancel_after_ms
                        .is_some_and(|milliseconds| {
                            diagnostics.step_started.elapsed()
                                >= std::time::Duration::from_millis(milliseconds)
                        })
                {
                    diagnostics.cancel_requested = Some(Instant::now());
                    diagnostics.log(
                        serde_json::json!({"event":"cancel-requested","step":diagnostics.index}),
                    );
                    self.cancelled.store(true, Ordering::Release);
                }
                if !self.busy() {
                    diagnostics.deadline = Instant::now()
                        + std::time::Duration::from_millis(diagnostics.config.settle_ms);
                    diagnostics.phase = 4;
                }
            }
            4 if !self.busy() && Instant::now() >= diagnostics.deadline => {
                let action = &diagnostics.config.steps[diagnostics.index].action;
                self.diagnostic_snapshot(
                    &diagnostics,
                    if matches!(action.as_str(), "home" | "open" | "continue_ready") {
                        "after-navigation"
                    } else {
                        "after-process"
                    },
                );
                let name = format!("{:02}-{}", diagnostics.index + 1, action);
                diagnostics.capture(ctx, name);
                diagnostics.phase = 5;
            }
            5 if diagnostics.screenshot_received => {
                diagnostics.index += 1;
                diagnostics.phase = 1;
            }
            _ => {}
        }
        if !diagnostics.complete {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.diagnostics = Some(diagnostics);
        if exit_requested {
            self.request_close(ctx);
        }
    }
}

impl eframe::App for ImageToolsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let update_started = Instant::now();
        if let Some(paths) = self.initial_paths.take() {
            self.add_paths(paths, ctx);
        }
        self.poll(ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.request_close(ctx);
        }
        if self.close_approved && !self.running {
            return;
        }
        let dropped = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect::<Vec<_>>()
        });
        if !dropped.is_empty() {
            self.add_paths(dropped, ctx);
        }
        if !self.busy() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::O)) {
            self.choose_files(ctx, false);
        }
        if !self.busy() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F5)) {
            self.start(ctx, None);
        }
        if (self.running || self.preparing())
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            if self.preparing() {
                self.cancel_preparation();
            } else {
                self.cancelled.store(true, Ordering::Release);
            }
        }
        if !self.busy()
            && self.active_tool().is_some()
            && !ctx.wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::A))
        {
            for row in &mut self.rows {
                row.selected = true;
            }
        }
        self.header(ctx);
        match self.page {
            Page::Home => {
                if let Some(tool) = tool_gallery::home(
                    ctx,
                    self.lang(),
                    &mut self.tool_search,
                    self.pending_paths.len(),
                ) {
                    self.switch_tool(tool, ctx);
                }
            }
            Page::Tool(tool) if self.rows.is_empty() => {
                if !self.message.is_empty() || !self.issues.is_empty() {
                    let language = self.lang();
                    egui::TopBottomPanel::bottom("landing-status")
                        .frame(
                            egui::Frame::new()
                                .fill(Color32::WHITE)
                                .inner_margin(egui::Margin::symmetric(24, 14)),
                        )
                        .show(ctx, |ui| {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&self.message).small().color(MUTED),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&self.message);
                                if !self.issues.is_empty()
                                    && ui.link(language.text("查看提示", "View notes")).clicked()
                                {
                                    self.show_issues = true;
                                }
                            });
                        });
                }
                match tool_gallery::landing(
                    ctx,
                    self.lang(),
                    tool,
                    self.busy(),
                    &mut self.settings.recursive,
                ) {
                    LandingAction::ChooseImages => self.choose_files(ctx, false),
                    LandingAction::ChooseFolder => self.choose_files(ctx, true),
                    LandingAction::None => {}
                }
            }
            Page::Tool(_) => {
                self.footer(ctx);
                self.parameters(ctx);
                self.workspace(ctx);
            }
        }
        if self.show_issues {
            let language = self.lang();
            egui::Window::new(language.text("操作提示", "Notes"))
                .open(&mut self.show_issues)
                .default_width(650.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().max_height(420.0).show_rows(
                        ui,
                        22.0,
                        self.issues.len(),
                        |ui, range| {
                            for i in range {
                                ui.add(egui::Label::new(&self.issues[i]).truncate())
                                    .on_hover_text(&self.issues[i]);
                            }
                        },
                    );
                });
        }
        self.settings_error_dialog(ctx);
        if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop-overlay"),
            ));
            let rect = ctx.screen_rect();
            painter.rect_filled(rect, 0, Color32::from_rgba_unmultiplied(17, 57, 52, 220));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                self.lang()
                    .text("松开以添加图片或文件夹", "Release to add images or folders"),
                FontId::proportional(28.0),
                Color32::WHITE,
            );
        }
        // No continuous repaint when idle. Worker events and UI interactions wake the app.
        self.run_diagnostics(ctx);
        if let Some(diagnostics) = &mut self.diagnostics {
            if self.running {
                diagnostics.max_processing_update_ms = diagnostics
                    .max_processing_update_ms
                    .max(update_started.elapsed().as_secs_f64() * 1000.0);
                diagnostics.max_processing_tick_gap_ms = diagnostics
                    .max_processing_tick_gap_ms
                    .max(diagnostics.last_frame.elapsed().as_secs_f64() * 1000.0);
                if let Some(cpu) = _frame.info().cpu_usage {
                    diagnostics.max_processing_cpu_ms = diagnostics
                        .max_processing_cpu_ms
                        .max(f64::from(cpu) * 1000.0);
                }
            }
            diagnostics.last_frame = Instant::now();
        }
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cancel_preparation();
        self.cancelled.store(true, Ordering::Release);
    }
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value.into_owned()
    }
}
fn resize_label(language: Language, mode: u8) -> &'static str {
    match mode {
        1 => language.text("宽高边界内适配", "Fit within width / height"),
        2 => language.text("最长边", "Longest edge"),
        3 => language.text("百分比", "Percentage"),
        _ => language.text("保持像素尺寸", "Keep pixel dimensions"),
    }
}
fn format_label(format: Format) -> &'static str {
    match format {
        Format::Jpeg => "JPG / JPEG",
        Format::Png => "PNG",
        Format::WebP => "WebP",
        Format::Gif => "GIF",
    }
}
fn matte_picker(ui: &mut egui::Ui, language: Language, background: &mut [u8; 3]) {
    ui.horizontal(|ui| {
        ui.label(language.text("透明区域底色", "Transparency matte"));
        ui.color_edit_button_srgb(background);
    })
    .response
    .on_hover_text(language.text(
        "用于目标格式无法保留的透明区域。",
        "Used when the output format cannot preserve transparency.",
    ));
}
fn status_label(language: Language, row: &Row) -> (String, Color32) {
    let label = match row.state {
        State::Ready => language.text("待处理", "Ready"),
        State::Queued => language.text("排队中", "Queued"),
        State::Running => language.text("处理中", "Processing"),
        State::Skipped => language.text("已跳过", "Skipped"),
        State::Failed => language.text("失败", "Failed"),
        State::Cancelled => language.text("已取消", "Cancelled"),
        State::Success => match row.result.as_ref().map(|r| &r.status) {
            Some(TaskStatus::Success {
                change: SizeChange::Smaller,
                ..
            }) => language.text("已缩小", "Smaller"),
            Some(TaskStatus::Success {
                change: SizeChange::Larger,
                ..
            }) => language.text("体积增大", "Larger"),
            _ => language.text("未缩小", "Unchanged"),
        },
    };
    (
        label.into(),
        match row.state {
            State::Success | State::Running => ACCENT,
            State::Failed => Color32::from_rgb(182, 72, 59),
            _ => MUTED,
        },
    )
}
