//! Isolate Windows driver shader initialization in a short-lived process.
//! This module never creates the image application in the child process.
//! See docs/adr/0002-native-processing-and-memory.md for measurements and limits.
use crate::diagnostics::Diagnostics;
use eframe::{
    egui,
    glow::{self, HasContext},
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::json;
use std::{
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CHILD_MARKER: &str = "LIT_GLOW_PRIMER_CHILD";
const PARENT_TIMEOUT: Duration = Duration::from_secs(5);
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

pub fn is_child() -> bool {
    std::env::var_os(CHILD_MARKER).is_some()
}

fn log(diagnostics: Option<&Diagnostics>, value: serde_json::Value) {
    if let Some(diagnostics) = diagnostics {
        diagnostics.log(value);
    } else {
        eprintln!("{value}");
    }
}

/// Called before eframe creates any GL objects in the parent process.
pub fn run_before_graphics() {
    if is_child() {
        return;
    }
    let diagnostics = Diagnostics::from_environment();
    if std::env::var_os("LIT_GLOW_PRIMER_DISABLE").is_some() {
        log(
            diagnostics.as_ref(),
            json!({"event":"primer-parent-result","result":"disabled"}),
        );
        return;
    }
    let started = Instant::now();
    log(
        diagnostics.as_ref(),
        json!({"event":"primer-parent-start","timeout_ms":5000}),
    );
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            log(
                diagnostics.as_ref(),
                json!({"event":"primer-parent-result","result":"executable-error","error":error.to_string()}),
            );
            return;
        }
    };
    let mut command = Command::new(executable);
    // Inherit diagnostic configuration/cold constant, but never user arguments.
    // The marker takes the child-only branch before any recursive spawn.
    command.env(CHILD_MARKER, "1").stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console window.
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            log(
                diagnostics.as_ref(),
                json!({"event":"primer-parent-result","result":"spawn-error","error":error.to_string()}),
            );
            return;
        }
    };
    let child_pid = child.id();
    log(
        diagnostics.as_ref(),
        json!({"event":"primer-parent-spawned","child_pid":child_pid}),
    );
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                log(
                    diagnostics.as_ref(),
                    json!({
                        "event":"primer-parent-result", "child_pid":child_pid,
                        "result":if status.success() { "success" } else { "child-failed" },
                        "exit_code":status.code(), "child_exit_confirmed":true,
                        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0
                    }),
                );
                break;
            }
            Ok(None) if started.elapsed() < PARENT_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            pending => {
                let wait_error = pending.err().map(|error| error.to_string());
                // Only this explicitly spawned child is terminated. No unrelated
                // processes, memory trimming, or driver-cache manipulation.
                let kill_error = child.kill().err().map(|error| error.to_string());
                // TerminateProcess is asynchronous on Windows. Reap the child
                // before starting normal graphics whenever termination succeeds.
                // Bound this final wait too: a broken driver must not indefinitely
                // prevent the real application from opening.
                let termination_started = Instant::now();
                let mut termination_wait_error = None;
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if termination_started.elapsed() < TERMINATION_GRACE => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Ok(None) => break None,
                        Err(error) => {
                            termination_wait_error = Some(error.to_string());
                            break None;
                        }
                    }
                };
                log(
                    diagnostics.as_ref(),
                    json!({
                        "event":"primer-parent-result", "child_pid":child_pid,
                        "result":if wait_error.is_some() { "wait-error" } else { "timeout" },
                        "wait_error":wait_error,"kill_error":kill_error,
                        "termination_wait_error":termination_wait_error,
                        "child_exit_confirmed":status.is_some(),
                        "exit_code":status.and_then(|status|status.code()),
                        "fallback":"open-main-window",
                        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0
                    }),
                );
                break;
            }
        }
    }
}

#[derive(Default)]
struct PaintEvidence {
    last_frame: Option<u64>,
    painted_frames: usize,
    hidden_all_frames: bool,
    complete: bool,
    success: bool,
}

struct PrimerApp {
    evidence: Arc<Mutex<PaintEvidence>>,
    diagnostics: Option<Arc<Diagnostics>>,
    native_window: Option<usize>,
    started: Instant,
    close_sent: bool,
}

impl PrimerApp {
    fn new(cc: &eframe::CreationContext<'_>, evidence: Arc<Mutex<PaintEvidence>>) -> Self {
        let diagnostics = Diagnostics::from_environment().map(Arc::new);
        crate::theme::install(&cc.egui_ctx);
        if let Some(diagnostics) = &diagnostics {
            crate::diagnostics::record_renderer(cc, diagnostics);
        }
        let native_window = cc
            .window_handle()
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as usize),
                _ => None,
            });
        // eframe will show the window after its first paint. Cloak it through
        // DWM before that point, so its ordinary visible-window event loop can
        // repaint and close while the compositor keeps it invisible.
        let cloak_setup = configure_cloaked_window(native_window);
        log(
            diagnostics.as_deref(),
            json!({"event":"primer-window-cloak","setup":cloak_setup,"window":window_visibility(native_window)}),
        );
        if !cloak_setup.success {
            // Do not allow a primer window to become visible on setup failure.
            // This graphics-only child owns no user data or persistent state.
            log(
                diagnostics.as_deref(),
                json!({"event":"primer-child-exit","success":false,"reason":"cloak-setup-failed","painted_frames":0}),
            );
            std::process::exit(3);
        }
        evidence.lock().expect("primer evidence").hidden_all_frames = true;
        log(
            diagnostics.as_deref(),
            json!({
                "event":"primer-child-start","window":window_visibility(native_window),
                "cold_shader":std::env::var("LIT_DIAGNOSTIC_COLD_SHADER").ok(),
                "image_application_created":false,"persist_window":false
            }),
        );
        Self {
            evidence,
            diagnostics,
            native_window,
            started: Instant::now(),
            close_sent: false,
        }
    }
}

impl eframe::App for PrimerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (completed, painted_frames) = {
            let evidence = self.evidence.lock().expect("primer evidence");
            (evidence.complete, evidence.painted_frames)
        };
        if completed || self.started.elapsed() >= Duration::from_secs(4) {
            if !self.close_sent {
                log(
                    self.diagnostics.as_deref(),
                    json!({
                        "event":"primer-child-close","completed":completed,
                        "painted_frames":painted_frames,"window":window_visibility(self.native_window)
                    }),
                );
                self.close_sent = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }

        let expected = if painted_frames % 2 == 0 {
            [36_u8, 104, 172, 255]
        } else {
            [209_u8, 115, 53, 255]
        };
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("本地图像工具 · Local Image Tools · shader primer");
            let area = ui.max_rect();
            let rect = egui::Rect::from_center_size(area.center(), egui::vec2(100.0,100.0));
            ui.painter().rect_filled(rect,0.0,egui::Color32::from_rgba_unmultiplied(expected[0],expected[1],expected[2],expected[3]));
            let evidence = self.evidence.clone();
            let diagnostics = self.diagnostics.clone();
            let native_window = self.native_window;
            let frame_nr = ctx.cumulative_frame_nr();
            let center = rect.center();
            let repaint_context = ctx.clone();
            // This callback runs inside Glow's actual paint_primitives, after
            // the preceding egui mesh has been drawn. Layout/update calls do
            // not increment this evidence counter.
            ui.painter().add(egui::PaintCallback {
                rect: area,
                callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |info,painter| {
                    let mut evidence = evidence.lock().expect("primer evidence");
                    if evidence.complete || evidence.last_frame == Some(frame_nr) { return; }
                    evidence.last_frame = Some(frame_nr);
                    evidence.painted_frames += 1;
                    let visibility = window_visibility(native_window);
                    let hidden = visibility.as_ref().is_some_and(WindowVisibility::hidden);
                    evidence.hidden_all_frames &= hidden;
                    let gl = painter.gl();
                    let prior_error = unsafe { gl.get_error() };
                    if evidence.painted_frames < 2 {
                        log(diagnostics.as_deref(),json!({"event":"primer-painted","frame_nr":frame_nr,"painted_frames":evidence.painted_frames,"window_hidden":hidden,"window":visibility,"gl_error":prior_error}));
                        repaint_context.request_repaint();
                        return;
                    }
                    // Exactly one explicit finish, only in the short-lived child.
                    unsafe { gl.finish(); }
                    let finish_error = unsafe { gl.get_error() };
                    let x = (center.x*info.pixels_per_point).round() as i32;
                    let y = info.screen_size_px[1] as i32 - (center.y*info.pixels_per_point).round() as i32 - 1;
                    let mut pixel = [0_u8;4];
                    unsafe { gl.read_pixels(x,y,1,1,glow::RGBA,glow::UNSIGNED_BYTE,glow::PixelPackData::Slice(Some(&mut pixel))); }
                    let readback_error = unsafe { gl.get_error() };
                    let matches = pixel.iter().zip(expected).all(|(&actual,expected)|actual.abs_diff(expected)<=8);
                    evidence.complete = true;
                    let cloak_verified = visibility.as_ref().is_some_and(WindowVisibility::cloaked);
                    evidence.success = evidence.hidden_all_frames && cloak_verified && prior_error == glow::NO_ERROR
                        && finish_error == glow::NO_ERROR && readback_error == glow::NO_ERROR && matches;
                    log(diagnostics.as_deref(),json!({
                        "event":"primer-painted","frame_nr":frame_nr,"painted_frames":evidence.painted_frames,
                        "window_hidden":hidden,"window":visibility,"cloak_verified":cloak_verified,"all_frames_hidden":evidence.hidden_all_frames,
                        "gl_error":prior_error,"finish_gl_error":finish_error,"readback_gl_error":readback_error,
                        "readback_pixel":pixel,"expected_pixel":expected,"pixel_matches":matches,"success":evidence.success
                    }));
                    // This repaint is requested after actual painting completed,
                    // not only during the previous App::update/layout pass.
                    repaint_context.request_repaint();
                })),
            });
        });
        ctx.request_repaint();
    }

    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        let evidence = self.evidence.lock().expect("primer evidence");
        log(
            self.diagnostics.as_deref(),
            json!({
                "event":"primer-child-on-exit","close_sent":self.close_sent,
                "painted_frames":evidence.painted_frames,"success":evidence.success,
                "window":window_visibility(self.native_window)
            }),
        );
    }
}

#[derive(serde::Serialize)]
struct WindowVisibility {
    win32_visible: bool,
    dwm_cloaked: u32,
    dwm_get_hresult: i32,
    extended_style: isize,
}
impl WindowVisibility {
    fn cloaked(&self) -> bool {
        self.dwm_get_hresult >= 0 && self.dwm_cloaked & 1 != 0
    }
    fn hidden(&self) -> bool {
        !self.win32_visible || self.cloaked()
    }
}

#[derive(Default, serde::Serialize)]
struct CloakSetup {
    success: bool,
    set_style_error: Option<u32>,
    set_cloak_hresult: Option<i32>,
    frame_changed: bool,
    tool_window: bool,
    no_activate: bool,
    app_window: bool,
}

#[cfg(windows)]
mod native {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn IsWindowVisible(hwnd: *mut c_void) -> i32;
        pub fn GetWindowLongPtrW(hwnd: *mut c_void, index: i32) -> isize;
        pub fn SetWindowLongPtrW(hwnd: *mut c_void, index: i32, value: isize) -> isize;
        pub fn SetWindowPos(
            hwnd: *mut c_void,
            insert_after: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn SetLastError(error: u32);
        pub fn GetLastError() -> u32;
    }
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        pub fn DwmSetWindowAttribute(
            hwnd: *mut c_void,
            attribute: u32,
            value: *const c_void,
            size: u32,
        ) -> i32;
        pub fn DwmGetWindowAttribute(
            hwnd: *mut c_void,
            attribute: u32,
            value: *mut c_void,
            size: u32,
        ) -> i32;
    }
}

#[cfg(windows)]
fn configure_cloaked_window(window: Option<usize>) -> CloakSetup {
    let Some(window) = window else {
        return CloakSetup::default();
    };
    let hwnd = window as *mut std::ffi::c_void;
    unsafe {
        let original_style = native::GetWindowLongPtrW(hwnd, -20);
        // TOOLWINDOW + NOACTIVATE, without APPWINDOW: no taskbar button or focus.
        let desired_style = (original_style | 0x0000_0080 | 0x0800_0000) & !0x0004_0000;
        native::SetLastError(0);
        let previous = native::SetWindowLongPtrW(hwnd, -20, desired_style);
        let set_style_error = if previous == 0 {
            native::GetLastError()
        } else {
            0
        };
        let frame_changed =
            native::SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, 0, 0, 0x0037) != 0;
        let cloak = 1_i32;
        let set_cloak_hresult =
            native::DwmSetWindowAttribute(hwnd, 13, (&cloak as *const i32).cast(), 4);
        let style = native::GetWindowLongPtrW(hwnd, -20);
        let tool_window = style & 0x0000_0080 != 0;
        let no_activate = style & 0x0800_0000 != 0;
        let app_window = style & 0x0004_0000 != 0;
        CloakSetup {
            success: set_style_error == 0
                && frame_changed
                && set_cloak_hresult >= 0
                && tool_window
                && no_activate
                && !app_window,
            set_style_error: Some(set_style_error),
            set_cloak_hresult: Some(set_cloak_hresult),
            frame_changed,
            tool_window,
            no_activate,
            app_window,
        }
    }
}

#[cfg(not(windows))]
fn configure_cloaked_window(_window: Option<usize>) -> CloakSetup {
    CloakSetup::default()
}

#[cfg(windows)]
fn window_visibility(window: Option<usize>) -> Option<WindowVisibility> {
    window.map(|window| unsafe {
        let hwnd = window as *mut std::ffi::c_void;
        let mut cloaked = 0_u32;
        let hresult = native::DwmGetWindowAttribute(hwnd, 14, (&mut cloaked as *mut u32).cast(), 4);
        WindowVisibility {
            win32_visible: native::IsWindowVisible(hwnd) != 0,
            dwm_cloaked: cloaked,
            dwm_get_hresult: hresult,
            extended_style: native::GetWindowLongPtrW(hwnd, -20),
        }
    })
}

#[cfg(not(windows))]
fn window_visibility(_window: Option<usize>) -> Option<WindowVisibility> {
    None
}

pub fn run_child(mut options: eframe::NativeOptions) -> eframe::Result {
    options.viewport = options
        .viewport
        .with_visible(false)
        .with_active(false)
        .with_taskbar(false);
    options.persist_window = false;
    let evidence = Arc::new(Mutex::new(PaintEvidence::default()));
    let child_evidence = evidence.clone();
    let result = eframe::run_native(
        "Local Image Tools Shader Primer",
        options,
        Box::new(move |creation| {
            crate::graphics::initialize(creation);
            Ok(Box::new(PrimerApp::new(creation, child_evidence)))
        }),
    );
    let evidence = evidence.lock().expect("primer evidence");
    let success = result.is_ok() && evidence.complete && evidence.success;
    log(
        Diagnostics::from_environment().as_ref(),
        json!({
            "event":"primer-child-exit","success":success,"painted_frames":evidence.painted_frames,
            "all_frames_hidden":evidence.hidden_all_frames,"error":result.as_ref().err().map(|error|error.to_string())
        }),
    );
    if result.is_ok() && !success {
        std::process::exit(2);
    }
    result
}
