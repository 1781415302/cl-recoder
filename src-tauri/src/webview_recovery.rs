//! 本窗口的 WebView2 故障恢复；不重启 GUI 进程或采集器，不修改外部叠加层。
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use clrecoder_diagnostics::Level;
use tauri::{AppHandle, Manager, State, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;

const RETRY_WINDOW: Duration = Duration::from_secs(60);
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RETRIES: usize = 2;

#[derive(Default)]
struct Lifecycle {
    epoch: u64,
    broken: bool,
    busy: bool,
    blocked: bool,
    visible: bool,
    mounted: bool,
    watchdog: bool,
    exiting: bool,
    attempts: VecDeque<Instant>,
}

#[derive(Debug, PartialEq)]
enum Decision {
    Wait,
    Recreate(u64),
    Blocked,
}

impl Lifecycle {
    fn begin(&mut self, manual: bool, now: Instant) -> Decision {
        if self.exiting || self.busy || !self.broken || (self.blocked && !manual) {
            return Decision::Wait;
        }
        if manual && self.blocked {
            self.attempts.clear();
        }
        if !self.visible {
            return Decision::Wait;
        }
        self.attempts
            .retain(|time| now.duration_since(*time) < RETRY_WINDOW);
        if self.attempts.len() >= MAX_RETRIES {
            self.blocked = true;
            return Decision::Blocked;
        }
        self.attempts.push_back(now);
        self.epoch += 1;
        self.busy = true;
        self.broken = false;
        self.blocked = false;
        self.mounted = false;
        self.watchdog = false;
        Decision::Recreate(self.epoch)
    }

    fn fail(&mut self, epoch: u64) -> bool {
        if self.exiting || epoch != self.epoch {
            return false;
        }
        self.broken = true;
        true
    }
}

#[derive(Default)]
pub struct RecoveryState(Mutex<Lifecycle>);

impl RecoveryState {
    fn lock(&self) -> MutexGuard<'_, Lifecycle> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn log(level: Level, code: &str, message: &str) {
    crate::commands::diagnostics::record_gui_event(level, code, message);
}

/// setup 在管理业务状态之后调用，确保浏览器首次执行脚本时状态和 IPC 已存在。
pub fn create_initial(app: &AppHandle) -> tauri::Result<()> {
    let config = main_config(app);
    let window = build(app, &config, 0)?;
    bind_events(&window, 0);
    let open_requested = app.state::<RecoveryState>().lock().visible;
    if open_requested {
        show(app);
    }
    log(
        Level::Info,
        "webview.render_mode",
        "本窗口启用软件渲染兼容模式",
    );
    Ok(())
}

fn main_config(app: &AppHandle) -> tauri::utils::config::WindowConfig {
    app.config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .expect("配置必须包含 main 窗口")
        .clone()
}

fn build(
    app: &AppHandle,
    config: &tauri::utils::config::WindowConfig,
    epoch: u64,
) -> tauri::Result<WebviewWindow> {
    WebviewWindowBuilder::from_config(app, config)?
        .initialization_script(format!("window.__CL_RECODER_VIEW_EPOCH__ = {epoch};"))
        .build()
}

/// 托盘、二次启动及首启统一走这里；坏窗口先重建，正常窗口直接显示。
pub fn show(app: &AppHandle) {
    let (broken, busy) = {
        let state = app.state::<RecoveryState>();
        let mut life = state.lock();
        life.visible = true;
        (life.broken, life.busy)
    };
    // 重建期间只记住打开意图，不把正在销毁的旧窗口再次显示出来。
    if busy {
        return;
    }
    if broken {
        request(app, true);
    } else if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        crate::ui_activity::refresh(app);
        arm_watchdog(app);
    }
}

pub fn hidden(app: &AppHandle) {
    if let Some(state) = app.try_state::<RecoveryState>() {
        state.lock().visible = false;
    }
}

pub fn stopping(app: &AppHandle) {
    if let Some(state) = app.try_state::<RecoveryState>() {
        state.lock().exiting = true;
    }
}

fn failed(app: &AppHandle, epoch: u64, reason: &str) {
    let state = app.state::<RecoveryState>();
    let read_visibility = {
        let mut life = state.lock();
        if !life.fail(epoch) {
            return;
        }
        !life.busy
    };
    if read_visibility {
        // 不持有状态锁调用窗口 API，避免消息处理重入时等待同一把锁。
        let visible = app.get_webview_window("main").is_some_and(|window| {
            window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
        });
        let mut life = state.lock();
        if life.epoch == epoch && !life.busy {
            life.visible = visible;
        }
    }
    log(
        Level::Error,
        "webview.process_failed",
        &format!("epoch={epoch}: {reason}"),
    );
    request(app, false);
}

fn request(app: &AppHandle, manual: bool) {
    let decision = app
        .state::<RecoveryState>()
        .lock()
        .begin(manual, Instant::now());
    match decision {
        Decision::Wait => {}
        Decision::Blocked => report_blocked(app),
        Decision::Recreate(epoch) => {
            log(
                Level::Warn,
                "webview.recovery_started",
                &format!("重建界面 epoch={epoch}"),
            );
            let worker_app = app.clone();
            // Windows 不允许在同步事件/COM 回调中创建 WebView2；始终交给独立线程。
            if let Err(error) = std::thread::Builder::new()
                .name("webview-recovery".into())
                .spawn(move || recreate(&worker_app, epoch))
            {
                recovery_error(app, epoch, &error.to_string());
            }
        }
    }
}

fn recreate(app: &AppHandle, epoch: u64) {
    let mut config = main_config(app);
    config.visible = false;
    if let Some(window) = app.get_webview_window("main") {
        if let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) {
            // 最小化窗口的客户区可能为 0，不能把零尺寸带入替代窗口。
            config.width = (f64::from(size.width) / scale).max(config.min_width.unwrap_or(1.0));
            config.height = (f64::from(size.height) / scale).max(config.min_height.unwrap_or(1.0));
        }
        config.maximized = window.is_maximized().unwrap_or(false);
        if let Err(error) = window.destroy() {
            recovery_error(app, epoch, &error.to_string());
            return;
        }
    }
    // destroy 是异步消息：等旧 main 从 Tauri 管理表移除后才复用同一 label。
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.get_webview_window("main").is_some() {
        if Instant::now() >= deadline {
            recovery_error(app, epoch, "旧窗口未能释放");
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if app.state::<RecoveryState>().lock().exiting {
        return;
    }
    match build(app, &config, epoch) {
        Ok(window) => {
            bind_events(&window, epoch);
            let (visible, broken) = {
                let state = app.state::<RecoveryState>();
                let mut life = state.lock();
                if life.exiting || life.epoch != epoch {
                    return;
                }
                life.busy = false;
                (life.visible, life.broken)
            };
            if broken {
                request(app, false);
            } else if visible {
                show(app);
            }
            log(
                Level::Info,
                "webview.recreated",
                &format!("窗口重建完成 epoch={epoch}"),
            );
        }
        Err(error) => recovery_error(app, epoch, &error.to_string()),
    }
}

fn recovery_error(app: &AppHandle, epoch: u64, error: &str) {
    let state = app.state::<RecoveryState>();
    {
        let mut life = state.lock();
        if life.exiting || life.epoch != epoch {
            return;
        }
        life.busy = false;
        life.broken = true;
        life.blocked = true;
    }
    log(Level::Error, "webview.recovery_failed", error);
    report_blocked(app);
}

fn report_blocked(app: &AppHandle) {
    log(
        Level::Error,
        "webview.recovery_blocked",
        "界面恢复失败，停止自动重试",
    );
    if !app.state::<RecoveryState>().lock().visible {
        return;
    }
    app.dialog()
        .message("界面加载失败，已停止自动重试。可稍后从托盘重新打开界面。后台采集仍会继续运行。")
        .title("CL Recoder")
        .show(|_| {});
}

fn arm_watchdog(app: &AppHandle) {
    let epoch = {
        let state = app.state::<RecoveryState>();
        let mut life = state.lock();
        if life.exiting || life.mounted || life.watchdog || life.busy {
            return;
        }
        life.watchdog = true;
        life.epoch
    };
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("webview-load-check".into())
        .spawn(move || {
            std::thread::sleep(LOAD_TIMEOUT);
            let expired = {
                let state = app.state::<RecoveryState>();
                let mut life = state.lock();
                if life.epoch != epoch || life.exiting {
                    return;
                }
                life.watchdog = false;
                life.visible && !life.mounted && !life.busy
            };
            if expired {
                failed(&app, epoch, "前端在可见窗口中未能完成挂载");
            }
        });
}

#[tauri::command]
pub fn frontend_ready(epoch: u64, state: State<'_, RecoveryState>) {
    let mut life = state.lock();
    if life.exiting || epoch != life.epoch || life.mounted {
        return;
    }
    life.mounted = true;
    drop(life);
    log(
        Level::Info,
        if epoch == 0 {
            "webview.initial_ready"
        } else {
            "webview.recovery_ready"
        },
        &format!("前端完成挂载 epoch={epoch}"),
    );
}

#[cfg(windows)]
fn bind_events(window: &WebviewWindow, epoch: u64) {
    use webview2_com::{
        Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
            COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
        },
        ProcessFailedEventHandler,
    };
    let app = window.app_handle().clone();
    let register_app = app.clone();
    if let Err(error) = window.with_webview(move |platform| {
        let callback_app = register_app.clone();
        let handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED;
                // COM 回调参数只在此线程读取；向恢复线程仅传递数字/字符串和 AppHandle。
                unsafe {
                    args.ProcessFailedKind(&mut kind)?;
                }
                if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
                    || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                {
                    failed(
                        &callback_app,
                        epoch,
                        &format!("WebView2 ProcessFailed kind={}", kind.0),
                    );
                } else {
                    log(
                        Level::Warn,
                        "webview.auxiliary_failure",
                        &format!("WebView2 kind={}，由运行时处理", kind.0),
                    );
                }
            }
            Ok(())
        }));
        let mut token = 0;
        // WebView2 持有注册后的回调；销毁 controller 时随窗口一起释放。
        let result = unsafe {
            platform
                .controller()
                .CoreWebView2()
                .and_then(|view| view.add_ProcessFailed(&handler, &mut token))
        };
        if let Err(error) = result {
            failed(&register_app, epoch, &format!("注册故障监听失败：{error}"));
        }
    }) {
        failed(&app, epoch, &error.to_string());
    }
}

#[cfg(not(windows))]
fn bind_events(_: &WebviewWindow, _: u64) {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_failure_waits_for_open_and_stale_epochs_are_ignored() {
        let mut life = Lifecycle::default();
        assert!(life.fail(0));
        assert_eq!(life.begin(false, Instant::now()), Decision::Wait);
        life.visible = true;
        assert_eq!(life.begin(true, Instant::now()), Decision::Recreate(1));
        assert!(!life.fail(0));
        assert!(!life.broken);
    }
    #[test]
    fn duplicate_events_coalesce_and_new_window_failure_is_not_lost() {
        let mut life = Lifecycle {
            visible: true,
            broken: true,
            ..Default::default()
        };
        let now = Instant::now();
        assert_eq!(life.begin(false, now), Decision::Recreate(1));
        assert!(life.fail(1));
        assert_eq!(life.begin(false, now), Decision::Wait);
        life.busy = false;
        assert_eq!(life.begin(false, now), Decision::Recreate(2));
    }
    #[test]
    fn automatic_retries_are_bounded_but_explicit_open_can_try_again() {
        let mut life = Lifecycle {
            visible: true,
            broken: true,
            ..Default::default()
        };
        let now = Instant::now();
        for epoch in 1..=2 {
            assert_eq!(life.begin(false, now), Decision::Recreate(epoch));
            life.busy = false;
            life.fail(epoch);
        }
        assert_eq!(life.begin(false, now), Decision::Blocked);
        assert_eq!(life.begin(false, now), Decision::Wait);
        assert_eq!(life.begin(true, now), Decision::Recreate(3));
    }
    #[test]
    fn retry_window_expires_and_quitting_prevents_recreation() {
        let mut life = Lifecycle {
            visible: true,
            broken: true,
            ..Default::default()
        };
        let now = Instant::now();
        life.attempts.extend([now, now]);
        assert_eq!(life.begin(false, now + RETRY_WINDOW), Decision::Recreate(1));
        life.busy = false;
        life.broken = true;
        life.exiting = true;
        assert_eq!(life.begin(true, now + RETRY_WINDOW), Decision::Wait);
        assert!(!life.fail(1));
    }
}
