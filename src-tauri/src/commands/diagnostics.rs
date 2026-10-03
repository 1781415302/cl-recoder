//! diagnostics —— GUI 侧诊断日志（usability-runtime-v3 §4.1，S1）。
//!
//! - [`init_gui_diagnostics`]：setup 内（single-instance 插件已成立、构造 AppState **前**）
//!   调用一次——开 `gui.log` 角色 sink、安装受控 facade adapter；失败只降级不阻塞启动，
//!   防 GUI 同角色两进程争轮转（single-instance 先行保证）。
//! - [`record_gui_message`]：旧 [`crate::gui_log!`] 宏的持久化适配入口（stderr 习惯保留在
//!   宏内）：按既有 `INFO:`/`WARN:`/`ERROR:` 前缀识别受控级别，去前缀后落固定 gui 事件码
//!   （`gui.info`/`gui.warn`/`gui.error`），未识别文本按 Info 原文；仍受单条 2048B 与
//!   60s/code 限频约束。不解析健康错误种类，不按文案生成 code。
//! - [`get_diagnostics_info`] / [`open_diagnostics_directory`]：前端两个无参命令；
//!   DTO 对前端 camelCase。目录由后端固定 `%LOCALAPPDATA%\ClRecoder\logs`，用既有
//!   opener 插件 Rust 接口打开，不接受任意前端路径。
//!
//! 本模块全部 Info 语义只描述 **GUI sink**；collector sink 属另一进程，状态不在此暴露，
//! 亦不得据此宣称 collector sink 必可写。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde::Serialize;

use clrecoder_diagnostics::{DiagnosticLog, Level, LogConfig, Role, MAX_TOTAL_BYTES};

/// 进程内 GUI sink（宏路径用：state/db 等无 AppHandle 的调用点拿不到 Tauri State）。
/// setup 诊断初始化成功后才有值；此前的 gui_log! 仅走 stderr（原诊断习惯）。
static GUI_SINK: OnceLock<Arc<DiagnosticLog>> = OnceLock::new();

/// GUI 诊断状态（§4.1）：setup 阶段 [`init_gui_diagnostics`] 构造并由 Tauri manage，
/// [`get_diagnostics_info`] 经 `State` 注入。不序列化、不进 invoke 参数。
#[derive(Debug, Clone)]
pub struct GuiDiagnosticsState {
    /// GUI 角色 sink（打开失败为 None——stderr-only 降级）
    pub log: Option<Arc<DiagnosticLog>>,
    /// 日志目录（无法定位 %LOCALAPPDATA% 时为 None）
    pub directory: Option<PathBuf>,
    /// 初始化/adapter 安装失败原因（供 DTO `lastError`）
    pub init_error: Option<String>,
}

/// 诊断日志目录（§4.1 后端固定）：`%LOCALAPPDATA%\ClRecoder\logs`；无法定位时 `None`。
pub fn log_directory() -> Option<PathBuf> {
    dirs::data_local_dir().map(|base| base.join("ClRecoder").join("logs"))
}

/// 初始化 GUI 诊断日志（§4.1）：single-instance 插件成立后的 setup 内调用一次，
/// **先于** `AppState` 构造。打开 gui.log 并安装 facade adapter（每进程一次）。
///
/// 本函数不失败：sink 打开失败 / adapter 安装失败都只降级并在返回值里报告
/// （`init_error`），GUI 照常启动——日志不成为业务失败来源。
#[must_use]
pub fn init_gui_diagnostics() -> GuiDiagnosticsState {
    let Some(dir) = log_directory() else {
        let msg = "无法定位 %LOCALAPPDATA%，GUI 持久诊断日志未启用（stderr-only 降级）".to_string();
        eprintln!("[cl-recoder] {msg}");
        return GuiDiagnosticsState {
            log: None,
            directory: None,
            init_error: Some(msg),
        };
    };
    let sink = match DiagnosticLog::open(LogConfig {
        directory: dir.clone(),
        role: Role::Gui,
    }) {
        Ok(sink) => Arc::new(sink),
        Err(e) => {
            let msg = format!("打开 GUI 诊断日志失败，本进程 stderr-only 降级: {e}");
            eprintln!("[cl-recoder] {msg}");
            return GuiDiagnosticsState {
                log: None,
                directory: Some(dir),
                init_error: Some(msg),
            };
        }
    };
    // stderr debug 沿用原诊断习惯（CLRECODER_DEBUG）；持久日志是否启用与它无关
    let debug_stderr = std::env::var_os("CLRECODER_DEBUG").is_some();
    let mut init_error = None;
    if let Err(e) =
        clrecoder_diagnostics::install_project_log_adapter(Arc::clone(&sink), debug_stderr)
    {
        // 不忽略 SetLoggerError：报告不可用 + 直接 record 落盘证据（不经 facade）
        let msg = format!("GUI 诊断日志 facade adapter 安装失败（facade 持久日志不可用）: {e}");
        eprintln!("[cl-recoder] {msg}");
        let _ = sink.record(Level::Error, "log.adapter_install_failed", &msg);
        init_error = Some(msg);
    }
    // 宏路径注册（二次 set 静默保持首个——init 只应被调用一次）
    let _ = GUI_SINK.set(Arc::clone(&sink));
    sink.record(Level::Info, "service.started", "GUI 已启动");
    GuiDiagnosticsState {
        log: Some(sink),
        directory: Some(dir),
        init_error,
    }
}

/// 旧 [`crate::gui_log!`] 宏的持久化适配入口（§4.1）。识别既有 `INFO:`/`WARN:`/`ERROR:`
/// 前缀得受控级别，去前缀后按固定 gui 事件码落盘；未识别文本按 Info 原文。
/// sink 未初始化（极早期调用 / 二次启动进程 setup 未跑）或被限频抑制时静默丢弃。
pub fn record_gui_message(args: std::fmt::Arguments<'_>) {
    let text = args.to_string();
    let (level, code, message) = classify_gui_message(&text);
    if let Some(sink) = GUI_SINK.get() {
        sink.record(level, code, message);
    }
}

pub(crate) fn record_gui_event(level: Level, code: &str, message: &str) {
    if let Some(sink) = GUI_SINK.get() {
        sink.record(level, code, message);
    }
}

/// 前缀 →（受控级别, 固定事件码, 去前缀消息）。事件码按级别固定（限频 key 稳定），
/// 不从文案生成；未识别文本按 Info 原文。`pub(crate)` 供单测。
pub(crate) fn classify_gui_message(text: &str) -> (Level, &'static str, &str) {
    for (prefix, level, code) in [
        ("INFO:", Level::Info, "gui.info"),
        ("WARN:", Level::Warn, "gui.warn"),
        ("ERROR:", Level::Error, "gui.error"),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return (level, code, rest.trim_start());
        }
    }
    (Level::Info, "gui.info", text)
}

/// 前端 DTO（§4.1 camelCase）。`guiLoggingAvailable`/`lastError` 仅描述 **GUI sink**。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsInfoDto {
    /// 日志目录（后端固定；无法定位 %LOCALAPPDATA% 时为 null）
    pub log_directory: Option<String>,
    /// GUI sink 是否可写（打开失败 / 轮转或写入失败停用后为 false）
    pub gui_logging_available: bool,
    /// 最近一次初始化或写入错误（仅 GUI sink）
    pub last_error: Option<String>,
    /// 两角色日志文件合计上限（约 1 MiB）
    pub max_total_bytes: u64,
}

/// 前端无参命令（§4.1）：GUI sink 状态。State 只由 Tauri 注入，不进 invoke 参数。
#[tauri::command]
pub async fn get_diagnostics_info(
    state: tauri::State<'_, GuiDiagnosticsState>,
) -> Result<DiagnosticsInfoDto, String> {
    Ok(diagnostics_info(state.inner()))
}

fn diagnostics_info(st: &GuiDiagnosticsState) -> DiagnosticsInfoDto {
    let (available, sink_error) = match &st.log {
        Some(sink) => {
            let s = sink.state();
            (s.available, s.last_error)
        }
        None => (false, None),
    };
    DiagnosticsInfoDto {
        log_directory: st.directory.as_ref().map(|p| p.display().to_string()),
        gui_logging_available: available && st.init_error.is_none(),
        // sink 运行期错误优先；初始化失败（log=None）时回落到 init_error
        last_error: sink_error.or_else(|| st.init_error.clone()),
        max_total_bytes: MAX_TOTAL_BYTES,
    }
}

/// 前端无参命令（§4.1）：用既有 opener 插件 Rust 接口打开**后端固定**的日志目录
/// （不接受任意前端路径）。目录尚不存在时先创建（GUI sink 降级且 collector 未运行过）。
#[tauri::command]
pub async fn open_diagnostics_directory(app: tauri::AppHandle) -> Result<(), String> {
    let Some(dir) = log_directory() else {
        return Err("无法定位 %LOCALAPPDATA%，日志目录未知".to_string());
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建日志目录 {} 失败: {e}", dir.display()))?;
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(dir.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| format!("打开日志目录失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usability_v3_adapter_failure_is_reported_unavailable() {
        let dir = std::env::temp_dir().join(format!("clrec-gui-diag-{}", std::process::id()));
        let sink = Arc::new(DiagnosticLog::open(LogConfig {
            directory: dir.clone(), role: Role::Gui,
        }).unwrap());
        let mut state = GuiDiagnosticsState {
            log: Some(sink), directory: Some(dir.clone()), init_error: None,
        };
        assert!(diagnostics_info(&state).gui_logging_available);
        state.init_error = Some("adapter already installed".into());
        let info = diagnostics_info(&state);
        assert!(!info.gui_logging_available);
        assert_eq!(info.last_error.as_deref(), Some("adapter already installed"));
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 既有 gui_log! 文案前缀 → 受控级别 + 固定事件码 + 去前缀消息；
    /// 未识别文本按 Info 原文（§4.1 record_gui_message 适配合同）。
    #[test]
    fn usability_v3_gui_message_prefix_classification() {
        assert_eq!(
            classify_gui_message("INFO: 首启引导——已弹出主窗口"),
            (Level::Info, "gui.info", "首启引导——已弹出主窗口")
        );
        assert_eq!(
            classify_gui_message("WARN: settings.json 损坏，使用默认值重建"),
            (
                Level::Warn,
                "gui.warn",
                "settings.json 损坏，使用默认值重建"
            )
        );
        assert_eq!(
            classify_gui_message("ERROR: 不可恢复错误"),
            (Level::Error, "gui.error", "不可恢复错误")
        );
        // 去前缀后多余空白收掉
        assert_eq!(
            classify_gui_message("WARN:   缩进文案"),
            (Level::Warn, "gui.warn", "缩进文案")
        );
        // 未识别文本按 Info 原文（不吞字符）
        assert_eq!(
            classify_gui_message("无前缀文本"),
            (Level::Info, "gui.info", "无前缀文本")
        );
        assert_eq!(
            classify_gui_message("WARNING: 非既有前缀"),
            (Level::Info, "gui.info", "WARNING: 非既有前缀")
        );
        // 事件码与文案无关（限频 key 稳定）
        let (l1, c1, _) = classify_gui_message("WARN: A");
        let (l2, c2, _) = classify_gui_message("WARN: 完全不同的文案");
        assert_eq!((l1, c1), (l2, c2));
    }
}
