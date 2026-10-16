//! update —— Windows 胶水：检查 / 下载 / 校验 / 停采集器 / 备份 / 提权拉起安装包（PLAN §4.5–4.7）。
//!
//! 纯逻辑（版本、Release、curl 参数、sha256、marker、DTO）在 `clrecoder-update`；
//! 本模块只跑系统 `curl.exe` / PowerShell、独占句柄、采集器控制与 GUI 退出。
//!
//! **沙箱无法编译 Windows 目标**：单测覆盖纯函数；完整 build/clippy/test 以 windows-latest CI 为准。

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use clrecoder_core::ipc::CtlRequest;
use tauri::Manager;
use clrecoder_update::{
    build_api_curl_args, build_download_curl_args, build_update_check, map_curl_exit, parse_latest_release,
    parse_marker, parse_version, serialize_marker, verify_installer, PendingMarker,
    UpdateApplied, UpdateCheck, UpdateErrorCode, UpdatePhase, UpdateStatus, VerifyError,
    LATEST_API_URL, MAX_API_BYTES,
};

use super::collector_ctl::{
    ctl_request_typed, find_script, ps_quote, run_scheduled_task_now, wait_collector_running,
    win_path_is_under,
};
use super::collector_health::PipeFailureKind;

/// GUI 侧更新运行态（`app.manage`；PLAN §6）。
pub struct UpdateRuntime {
    status: Mutex<UpdateStatus>,
    /// 单飞：流水线进行中为 true。
    in_flight: AtomicBool,
    /// 启动时消费标记得到的「更新已完成」提示（会话内只读）。
    applied: Mutex<Option<UpdateApplied>>,
}

impl Default for UpdateRuntime {
    fn default() -> Self {
        Self {
            status: Mutex::new(UpdateStatus::default()),
            in_flight: AtomicBool::new(false),
            applied: Mutex::new(None),
        }
    }
}

impl UpdateRuntime {
    pub fn snapshot(&self) -> UpdateStatus {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn applied_snapshot(&self) -> Option<UpdateApplied> {
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_applied(&self, v: Option<UpdateApplied>) {
        *self
            .applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = v;
    }

    fn set_status(&self, s: UpdateStatus) {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = s;
    }

    fn patch_status(&self, f: impl FnOnce(&mut UpdateStatus)) {
        let mut g = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut g);
    }
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `FILE_SHARE_READ`：他人只可读，不可写/删/改名（PLAN §4.5 verifying）。
const FILE_SHARE_READ: u32 = 1;

fn system_root() -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()))
}

fn curl_exe_path() -> PathBuf {
    system_root().join("System32").join("curl.exe")
}

fn powershell_exe_path() -> PathBuf {
    system_root()
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

fn updates_dir() -> PathBuf {
    crate::db::data_dir().join("updates")
}

fn pending_marker_path() -> PathBuf {
    crate::db::data_dir().join("update-pending.json")
}

fn current_version_str() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 安装版布局判定（纯函数；与 `query_legacy_install_hints` 一致：exe 在 `%ProgramFiles%` 下）。
#[must_use]
pub(crate) fn is_installed_layout(current_exe: &str, program_files: &str) -> bool {
    win_path_is_under(current_exe, program_files)
}

fn probe_installed_layout() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let program_files = std::env::var("ProgramFiles").unwrap_or_default();
    is_installed_layout(&exe.to_string_lossy(), &program_files)
}

/// 提权拉起 NSIS 安装包的外层 PowerShell 命令（纯函数；PLAN §4.6）。
///
/// 形态对齐 [`super::collector_ctl::ps_runas_collector_command`]：`-PassThru`、无 `-Wait`、
/// UAC 取消 / 启动失败 → `exit 4`。
#[must_use]
pub(crate) fn ps_runas_installer_command(exe: &str) -> String {
    format!(
        "try {{ $p = Start-Process -FilePath '{}' -ArgumentList '/P /R /UPDATE' -Verb RunAs -PassThru }} \
         catch {{ exit 4 }}; if ($null -eq $p) {{ exit 4 }}; exit 0",
        ps_quote(exe)
    )
}

fn fail_status(
    code: UpdateErrorCode,
    message: impl Into<String>,
    collector_restored: Option<bool>,
) -> UpdateStatus {
    UpdateStatus {
        phase: UpdatePhase::Failed,
        received_bytes: 0,
        total_bytes: None,
        error_code: Some(code),
        message: Some(message.into()),
        collector_restored,
    }
}

fn rollback_collector_message(restored: bool) -> (bool, String) {
    if restored {
        (
            true,
            "已尝试恢复采集器。".to_string(),
        )
    } else {
        (
            false,
            "采集器未能自动恢复，请到设置页点击‘立即启动采集器’。".to_string(),
        )
    }
}

fn rollback_collector_if_needed(was_running: bool) -> Option<bool> {
    if !was_running {
        return None;
    }
    let _ = run_scheduled_task_now();
    Some(wait_collector_running(Duration::from_secs(5)))
}

fn clear_updates_dir() -> Result<(), String> {
    let dir = updates_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建更新目录失败: {e}"))?;
    // 只清 backup 以外的内容，保留 updates\backup（P1-1）
    for entry in fs::read_dir(&dir).map_err(|e| format!("读取更新目录失败: {e}"))? {
        let entry = entry.map_err(|e| format!("读取更新目录项失败: {e}"))?;
        let name = entry.file_name();
        if name == *"backup" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|e| format!("清理 {} 失败: {e}", path.display()))?;
        } else {
            fs::remove_file(&path).map_err(|e| format!("清理 {} 失败: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// 下载并轮询 `.part` 大小；成功返回 ().
fn download_with_progress(
    args: &[String],
    part_path: &Path,
    rt: &UpdateRuntime,
    total: u64,
) -> Result<(), (UpdateErrorCode, String)> {
    use std::os::windows::process::CommandExt;
    let curl = curl_exe_path();
    if !curl.is_file() {
        return Err((
            UpdateErrorCode::CurlMissing,
            "未找到系统 curl.exe，请前往发布页手动下载安装包。".into(),
        ));
    }
    let mut child = std::process::Command::new(&curl)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| (UpdateErrorCode::Network, format!("启动 curl 失败: {e}")))?;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code().unwrap_or(-1);
                if let Some(mapped) = map_curl_exit(code) {
                    let _ = fs::remove_file(part_path);
                    return Err((mapped.code, mapped.message));
                }
                if let Ok(meta) = fs::metadata(part_path) {
                    rt.patch_status(|s| s.received_bytes = meta.len());
                }
                return Ok(());
            }
            Ok(None) => {
                if let Ok(meta) = fs::metadata(part_path) {
                    rt.patch_status(|s| {
                        s.phase = UpdatePhase::Downloading;
                        s.received_bytes = meta.len();
                        s.total_bytes = Some(total);
                    });
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                let _ = fs::remove_file(part_path);
                return Err((UpdateErrorCode::Network, format!("等待 curl 失败: {e}")));
            }
        }
    }
}

fn fetch_latest_json() -> Result<String, (UpdateErrorCode, String)> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    let curl = curl_exe_path();
    if !curl.is_file() {
        return Err((
            UpdateErrorCode::CurlMissing,
            "未找到系统 curl.exe，请前往发布页手动下载安装包。".into(),
        ));
    }
    let args = build_api_curl_args(LATEST_API_URL, current_version_str())
        .map_err(|m| (UpdateErrorCode::Network, m.to_string()))?;
    let output = std::process::Command::new(&curl)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| (UpdateErrorCode::Network, format!("启动 curl 失败: {e}")))?;
    let code = output.status.code().unwrap_or(-1);
    if let Some(mapped) = map_curl_exit(code) {
        return Err((mapped.code, mapped.message));
    }
    if output.stdout.len() as u64 > MAX_API_BYTES {
        return Err((
            UpdateErrorCode::Network,
            "Release JSON 超过大小上限。".into(),
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| {
        (
            UpdateErrorCode::Network,
            format!("Release JSON 不是合法 UTF-8: {e}"),
        )
    })
}

fn open_installer_locked(path: &Path) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .map_err(|e| format!("锁定安装包失败: {e}"))
}

fn verify_locked_file(
    file: &mut File,
    expected_size: u64,
    expected_sha: &str,
) -> Result<(), (UpdateErrorCode, String)> {
    let meta = file
        .metadata()
        .map_err(|e| (UpdateErrorCode::Disk, format!("读取安装包元数据失败: {e}")))?;
    let actual_size = meta.len();
    if actual_size != expected_size {
        return Err((
            UpdateErrorCode::SizeMismatch,
            format!("安装包大小不符（期望 {expected_size}，实际 {actual_size}）。"),
        ));
    }
    let mut buf = Vec::with_capacity(actual_size as usize);
    file.read_to_end(&mut buf)
        .map_err(|e| (UpdateErrorCode::Disk, format!("读取安装包失败: {e}")))?;
    // 回到文件头，避免后续若再读；校验用内存副本
    match verify_installer(&buf, expected_size, expected_sha) {
        Ok(()) => Ok(()),
        Err(VerifyError::SizeMismatch { .. }) => Err((
            UpdateErrorCode::SizeMismatch,
            "安装包大小不符。".into(),
        )),
        Err(VerifyError::DigestMismatch { .. }) => Err((
            UpdateErrorCode::DigestMismatch,
            "安装包校验失败（摘要不符），已删除下载文件。".into(),
        )),
    }
}

fn backup_stats_db() -> Result<(), String> {
    let backup_dir = updates_dir().join("backup");
    fs::create_dir_all(&backup_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    // 复制前先清空 backup 内旧文件，避免残留旧的 -wal/-shm（P1-1）
    for entry in fs::read_dir(&backup_dir).map_err(|e| format!("读取备份目录失败: {e}"))? {
        let entry = entry.map_err(|e| format!("读取备份目录项失败: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|e| format!("清理旧备份 {} 失败: {e}", path.display()))?;
        } else {
            fs::remove_file(&path).map_err(|e| format!("清理旧备份 {} 失败: {e}", path.display()))?;
        }
    }
    let src = crate::db::stats_db_path();
    let copy_one = |name: &str| -> Result<(), String> {
        let from = if name.is_empty() {
            src.clone()
        } else {
            PathBuf::from(format!("{}{}", src.display(), name))
        };
        if !from.is_file() {
            return Ok(());
        }
        let file_name = from
            .file_name()
            .ok_or_else(|| "备份源文件名无效".to_string())?;
        let to = backup_dir.join(file_name);
        fs::copy(&from, &to).map_err(|e| format!("备份 {} 失败: {e}", from.display()))?;
        Ok(())
    };
    copy_one("")?;
    copy_one("-wal")?;
    copy_one("-shm")?;
    Ok(())
}

fn write_pending_marker(from: &str, to: &str, collector_was_running: bool) -> Result<(), String> {
    let created_at = chrono::Utc::now().to_rfc3339();
    let marker = PendingMarker {
        from_version: from.to_string(),
        to_version: to.to_string(),
        collector_was_running,
        created_at,
    };
    let text = serialize_marker(&marker).map_err(|e| format!("序列化更新标记失败: {e}"))?;
    let path = pending_marker_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建数据目录失败: {e}"))?;
    }
    fs::write(&path, text).map_err(|e| format!("写入更新标记失败: {e}"))
}

fn delete_pending_marker() {
    let _ = fs::remove_file(pending_marker_path());
}

fn run_installer_elevated(installer: &Path) -> Result<(), (UpdateErrorCode, String)> {
    use std::os::windows::process::CommandExt;
    let ps = powershell_exe_path();
    if !ps.is_file() {
        return Err((
            UpdateErrorCode::LaunchFailed,
            "未找到系统 PowerShell，无法提权启动安装包。".into(),
        ));
    }
    let cmd = ps_runas_installer_command(&installer.to_string_lossy());
    let status = std::process::Command::new(&ps)
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &cmd,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| (UpdateErrorCode::LaunchFailed, format!("启动 PowerShell 失败: {e}")))?;
    let code = status.code().unwrap_or(-1);
    if code == 0 {
        Ok(())
    } else if code == 4 {
        Err((
            UpdateErrorCode::UacCancelled,
            "已取消管理员授权，更新未继续。".into(),
        ))
    } else {
        Err((
            UpdateErrorCode::LaunchFailed,
            format!("提权启动安装包失败（退出码 {code}）。"),
        ))
    }
}

/// 一次 Status 探测的可达性分类（P0-3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CollectorReachability {
    /// Ok 且 resp.ok —— 采集器在跑
    Running,
    /// Err(NotFound) —— 管道不存在；跳过停止（不证明进程已退出）
    Gone,
    /// 其它一切（Busy/Timeout/Io/Protocol/AccessDenied、Ok 但 !ok 等）
    Unknown,
}

/// 将一次 `ctl_request_typed(Status)` 的抽象结果分类（纯函数，可单测）。
///
/// `Ok(true)` = Running；`Err(NotFound)` = Gone；其余 = Unknown。
#[must_use]
pub(crate) fn classify_collector_status(
    result: Result<bool, PipeFailureKind>,
) -> CollectorReachability {
    match result {
        Ok(true) => CollectorReachability::Running,
        Err(PipeFailureKind::NotFound) => CollectorReachability::Gone,
        Ok(false) | Err(_) => CollectorReachability::Unknown,
    }
}

fn probe_collector_status() -> CollectorReachability {
    classify_collector_status(match ctl_request_typed(&CtlRequest::Status) {
        Ok(resp) => Ok(resp.ok),
        Err(e) => Err(e.kind),
    })
}

/// 运行 `stop-collector.ps1` 的 PowerShell 参数（纯函数；顺序固定）。
///
/// `-File` 后路径作为独立参数传入，由 CreateProcess 负责引号，故空格/单引号路径保持原样。
#[must_use]
pub(crate) fn stop_collector_ps_args(script_path: &str) -> Vec<String> {
    vec![
        "-NoProfile".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        script_path.to_string(),
    ]
}

const STOP_COLLECTOR_SCRIPT: &str = "stop-collector.ps1";
const STOP_COLLECTOR_TIMEOUT: Duration = Duration::from_secs(25);

/// 以系统 PowerShell 运行打包的 stop-collector.ps1；仅退出码 0 视为进程已退出。
///
/// 管道消失不等于进程退出（Writer/WAL 收尾更晚）；脚本内 WaitForExit 才是依据。
fn run_stop_collector_script(script: &Path) -> Result<(), (UpdateErrorCode, String)> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    let ps = powershell_exe_path();
    if !ps.is_file() {
        return Err((
            UpdateErrorCode::CollectorStopFailed,
            "未找到系统 PowerShell，无法停止采集器，更新已中止。".into(),
        ));
    }
    let args = stop_collector_ps_args(&script.to_string_lossy());
    let mut child = std::process::Command::new(&ps)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| {
            (
                UpdateErrorCode::CollectorStopFailed,
                format!("启动 stop-collector.ps1 失败: {e}。更新已中止。"),
            )
        })?;

    let deadline = Instant::now() + STOP_COLLECTOR_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let code = status.code().unwrap_or(-1);
                return Err((
                    UpdateErrorCode::CollectorStopFailed,
                    format!("停止采集器失败（退出码 {code}），更新已中止。"),
                ));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err((
                        UpdateErrorCode::CollectorStopFailed,
                        "等待采集器退出超时（25 秒），更新已中止。".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => {
                let _ = child.kill();
                return Err((
                    UpdateErrorCode::CollectorStopFailed,
                    format!("等待 stop-collector.ps1 失败: {e}。更新已中止。"),
                ));
            }
        }
    }
}

/// 停止采集器：Gone 跳过；Unknown 重试后中止；Running 则跑 stop-collector.ps1。
///
/// `Ok(was_running)`；`Err((code, msg, rollback_needed))` —— 已尝试脚本停止时
/// `rollback_needed=true`（采集器可能已停，需走回滚路径）。
fn stop_collector_gracefully() -> Result<bool, (UpdateErrorCode, String, bool)> {
    // 先探测：Unknown 间隔 300ms 重试，最多 5 次
    let mut reach = CollectorReachability::Unknown;
    for attempt in 0..5 {
        reach = probe_collector_status();
        match reach {
            CollectorReachability::Running | CollectorReachability::Gone => break,
            CollectorReachability::Unknown => {
                if attempt + 1 < 5 {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }
    match reach {
        CollectorReachability::Gone => return Ok(false),
        CollectorReachability::Unknown => {
            return Err((
                UpdateErrorCode::CollectorStopFailed,
                "无法确认采集器状态，更新已中止".into(),
                false,
            ));
        }
        CollectorReachability::Running => {}
    }

    let script = match find_script(STOP_COLLECTOR_SCRIPT) {
        Some(p) => p,
        None => {
            return Err((
                UpdateErrorCode::CollectorStopFailed,
                "未找到 stop-collector.ps1（应随主程序安装在 scripts\\ 下），更新已中止。".into(),
                false,
            ));
        }
    };
    // 已 Running：脚本失败时采集器可能已收到 shutdown，需回滚
    run_stop_collector_script(&script).map_err(|(c, m)| (c, m, true))?;
    Ok(true)
}

/// 启动时消费 `update-pending.json`（PLAN §4.7）：读完即删；损坏忽略。
pub(crate) fn process_startup_marker(rt: &UpdateRuntime) {
    let path = pending_marker_path();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return,
    };
    let _ = fs::remove_file(&path);
    let marker = match parse_marker(&text) {
        Ok(m) => m,
        Err(_) => return,
    };
    let current = current_version_str();
    if marker.from_version == current {
        // 版本未变 → 不算「已更新」（可能安装失败/取消后残留）
        return;
    }
    let mut collector_restored = true;
    if marker.collector_was_running {
        let reachable = matches!(
            ctl_request_typed(&CtlRequest::Status),
            Ok(resp) if resp.ok
        );
        if !reachable {
            let _ = run_scheduled_task_now();
            collector_restored = wait_collector_running(Duration::from_secs(5));
        }
    }
    rt.set_applied(Some(UpdateApplied {
        from_version: marker.from_version,
        to_version: marker.to_version,
        collector_was_running: marker.collector_was_running,
        collector_restored,
    }));
}

/// 检查更新（PLAN §4.5 `update_check`）。
#[tauri::command]
pub async fn update_check() -> Result<UpdateCheck, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let json = fetch_latest_json().map_err(|(_, m)| m)?;
        let release = parse_latest_release(&json).map_err(|e| e.to_string())?;
        let current = parse_version(current_version_str())
            .ok_or_else(|| "当前版本号无法解析".to_string())?;
        let portable = !probe_installed_layout();
        Ok(build_update_check(current, &release, portable))
    })
    .await
    .map_err(|e| format!("检查更新任务失败: {e}"))?
}

/// 开始安装流水线（立即返回；后台线程执行，PLAN §4.5）。
#[tauri::command]
pub async fn update_install(
    app: tauri::AppHandle,
    version: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || update_install_start(app, version))
        .await
        .map_err(|e| format!("启动更新任务失败: {e}"))?
}

fn update_install_start(app: tauri::AppHandle, version: String) -> Result<(), String> {
    let runtime = app.state::<UpdateRuntime>();
    if runtime
        .in_flight
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("更新正在进行中，请稍候。".into());
    }
    runtime.set_status(UpdateStatus {
        phase: UpdatePhase::Downloading,
        received_bytes: 0,
        total_bytes: None,
        error_code: None,
        message: Some("准备下载…".into()),
        collector_restored: None,
    });

    let app2 = app.clone();
    std::thread::Builder::new()
        .name("clrecoder-update".into())
        .spawn(move || {
            let rt = app2.state::<UpdateRuntime>();
            run_install_pipeline(app2.clone(), &rt, version);
        })
        .map_err(|e| {
            app.state::<UpdateRuntime>()
                .in_flight
                .store(false, Ordering::SeqCst);
            format!("无法启动更新线程: {e}")
        })?;
    Ok(())
}

fn run_install_pipeline(app: tauri::AppHandle, rt: &UpdateRuntime, want_version: String) {

    let mark_fail = |rt: &UpdateRuntime, code: UpdateErrorCode, mut msg: String, restored: Option<bool>| {
        if restored == Some(false) && !msg.contains("立即启动采集器") {
            let (_, hint) = rollback_collector_message(false);
            msg = format!("{msg} {hint}");
        }
        rt.set_status(fail_status(code, msg, restored));
        rt.in_flight.store(false, Ordering::SeqCst);
    };

    if !probe_installed_layout() {
        mark_fail(
            rt,
            UpdateErrorCode::NotInstalledLayout,
            "当前不是安装版布局，无法自动更新。请前往发布页手动下载。".into(),
            None,
        );
        return;
    }
    if let Err(e) = clear_updates_dir() {
        mark_fail(rt, UpdateErrorCode::Disk, e, None);
        return;
    }

    let json = match fetch_latest_json() {
        Ok(j) => j,
        Err((code, msg)) => {
            mark_fail(rt, code, msg, None);
            return;
        }
    };
    let release = match parse_latest_release(&json) {
        Ok(r) => r,
        Err(e) => {
            mark_fail(rt, UpdateErrorCode::Network, format!("解析 Release 失败: {e}"), None);
            return;
        }
    };
    let current = match parse_version(current_version_str()) {
        Some(v) => v,
        None => {
            mark_fail(rt, UpdateErrorCode::VersionChanged, "当前版本号无法解析。".into(), None);
            return;
        }
    };
    let want = match parse_version(&want_version) {
        Some(v) => v,
        None => {
            mark_fail(
                rt,
                UpdateErrorCode::VersionChanged,
                "目标版本无效或已变化，请重新检查更新。".into(),
                None,
            );
            return;
        }
    };
    if release.version != want || !(release.version > current) {
        mark_fail(
            rt,
            UpdateErrorCode::VersionChanged,
            "远端版本已变化，请重新检查更新。".into(),
            None,
        );
        return;
    }
    let release_ver = release.version;
    let asset = match release.asset {
        Ok(a) => a,
        Err(_) => {
            mark_fail(
                rt,
                UpdateErrorCode::VersionChanged,
                "安装包资产不可用，请前往发布页手动下载。".into(),
                None,
            );
            return;
        }
    };

    rt.set_status(UpdateStatus {
        phase: UpdatePhase::Downloading,
        received_bytes: 0,
        total_bytes: Some(asset.size),
        error_code: None,
        message: Some("正在下载更新…".into()),
        collector_restored: None,
    });
    let part_path = updates_dir().join(format!("{}.part", asset.name));
    let final_path = updates_dir().join(&asset.name);
    let dl_args = match build_download_curl_args(
        &asset.download_url,
        &part_path.to_string_lossy(),
        asset.size.saturating_add(1),
        current_version_str(),
    ) {
        Ok(a) => a,
        Err(m) => {
            mark_fail(rt, UpdateErrorCode::Network, m.to_string(), None);
            return;
        }
    };
    if let Err((code, msg)) = download_with_progress(&dl_args, &part_path, rt, asset.size) {
        mark_fail(rt, code, msg, None);
        return;
    }
    if final_path.exists() {
        let _ = fs::remove_file(&final_path);
    }
    if let Err(e) = fs::rename(&part_path, &final_path) {
        let _ = fs::remove_file(&part_path);
        mark_fail(rt, UpdateErrorCode::Disk, format!("保存安装包失败: {e}"), None);
        return;
    }

    rt.patch_status(|s| {
        s.phase = UpdatePhase::Verifying;
        s.message = Some("正在校验安装包…".into());
    });
    let mut locked = match open_installer_locked(&final_path) {
        Ok(f) => f,
        Err(e) => {
            let _ = fs::remove_file(&final_path);
            mark_fail(rt, UpdateErrorCode::Disk, e, None);
            return;
        }
    };
    if let Err((code, msg)) = verify_locked_file(&mut locked, asset.size, &asset.sha256_hex) {
        drop(locked);
        let _ = fs::remove_file(&final_path);
        mark_fail(rt, code, msg, None);
        return;
    }

    rt.patch_status(|s| {
        s.phase = UpdatePhase::StoppingCollector;
        s.message = Some("正在停止采集器…".into());
    });
    let collector_was_running = match stop_collector_gracefully() {
        Ok(v) => v,
        Err((code, msg, rollback)) => {
            let restored = rollback_collector_if_needed(rollback);
            drop(locked);
            mark_fail(rt, code, msg, restored);
            return;
        }
    };

    rt.patch_status(|s| {
        s.phase = UpdatePhase::BackingUp;
        s.message = Some("正在备份数据库…".into());
    });
    if let Err(e) = backup_stats_db() {
        let restored = rollback_collector_if_needed(collector_was_running);
        drop(locked);
        mark_fail(rt, UpdateErrorCode::BackupFailed, e, restored);
        return;
    }

    if let Err(e) = write_pending_marker(
        &current.to_string_no_v(),
        &release_ver.to_string_no_v(),
        collector_was_running,
    ) {
        let restored = rollback_collector_if_needed(collector_was_running);
        drop(locked);
        mark_fail(rt, UpdateErrorCode::Disk, e, restored);
        return;
    }

    rt.patch_status(|s| {
        s.phase = UpdatePhase::Launching;
        s.message = Some("正在请求管理员权限以安装更新…".into());
    });
    let launch_result = run_installer_elevated(&final_path);
    drop(locked);

    match launch_result {
        Ok(()) => {
            rt.set_status(UpdateStatus {
                phase: UpdatePhase::Launched,
                received_bytes: asset.size,
                total_bytes: Some(asset.size),
                error_code: None,
                message: Some("正在交给安装程序，应用即将关闭…".into()),
                collector_restored: None,
            });
            std::thread::sleep(Duration::from_millis(800));
            rt.in_flight.store(false, Ordering::SeqCst);
            crate::quit_gui_for_update(&app);
        }
        Err((UpdateErrorCode::UacCancelled, msg)) => {
            delete_pending_marker();
            let restored = rollback_collector_if_needed(collector_was_running);
            mark_fail(rt, UpdateErrorCode::UacCancelled, msg, restored);
        }
        Err((code, msg)) => {
            delete_pending_marker();
            let restored = rollback_collector_if_needed(collector_was_running);
            mark_fail(rt, code, msg, restored);
        }
    }
}

/// 只读状态（PLAN §4.5 `update_status`）。
#[tauri::command]
pub async fn update_status(rt: tauri::State<'_, UpdateRuntime>) -> Result<UpdateStatus, String> {
    Ok(rt.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_runas_installer_command_no_wait_exit4_and_args() {
        let cmd = ps_runas_installer_command(r"C:\Program Files\CL Recoder\setup.exe");
        assert!(!cmd.contains("-Wait"), "{cmd}");
        assert!(cmd.contains("catch { exit 4 }"), "{cmd}");
        assert!(cmd.contains("-ArgumentList '/P /R /UPDATE'"), "{cmd}");
        assert!(cmd.contains("-Verb RunAs -PassThru"), "{cmd}");
        assert!(
            cmd.contains("-FilePath 'C:\\Program Files\\CL Recoder\\setup.exe'"),
            "{cmd}"
        );
        assert!(cmd.contains("if ($null -eq $p) { exit 4 }"), "{cmd}");
        assert!(cmd.ends_with("exit 0"), "{cmd}");
    }

    #[test]
    fn ps_runas_installer_quotes_space_and_apostrophe() {
        let space = ps_runas_installer_command(r"C:\Program Files\CL Recoder\a.exe");
        assert!(
            space.contains("-FilePath 'C:\\Program Files\\CL Recoder\\a.exe'"),
            "{space}"
        );
        let odd = ps_runas_installer_command(r"C:\odd'name\setup.exe");
        assert!(odd.contains(r"odd''name"), "{odd}");
        assert!(!odd.contains("-Wait"), "{odd}");
    }

    #[test]
    fn installed_layout_matches_program_files_boundary() {
        assert!(is_installed_layout(
            r"C:\Program Files\CL Recoder\cl-recoder.exe",
            r"C:\Program Files",
        ));
        assert!(!is_installed_layout(
            r"C:\Users\x\AppData\Local\ClRecoder\cl-recoder.exe",
            r"C:\Program Files",
        ));
        assert!(!is_installed_layout(
            r"C:\Program FilesExtra\x.exe",
            r"C:\Program Files",
        ));
        assert!(!is_installed_layout(r"C:\Program Files", r"C:\Program Files"));
    }

    #[test]
    fn update_status_dto_keys_camel_case() {
        let st = UpdateStatus {
            phase: UpdatePhase::BackingUp,
            received_bytes: 10,
            total_bytes: Some(20),
            error_code: Some(UpdateErrorCode::BackupFailed),
            message: Some("备份失败".into()),
            collector_restored: Some(false),
        };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["phase"], "backing_up");
        assert_eq!(v["errorCode"], "backup_failed");
        assert_eq!(v["receivedBytes"], 10);
        assert_eq!(v["collectorRestored"], false);
        assert!(v.get("received_bytes").is_none());
    }

    #[test]
    fn classify_collector_status_running_gone_unknown() {
        assert_eq!(
            classify_collector_status(Ok(true)),
            CollectorReachability::Running
        );
        assert_eq!(
            classify_collector_status(Err(PipeFailureKind::NotFound)),
            CollectorReachability::Gone
        );
        // Ok 但 !resp.ok → Unknown
        assert_eq!(
            classify_collector_status(Ok(false)),
            CollectorReachability::Unknown
        );
        for kind in [
            PipeFailureKind::Busy,
            PipeFailureKind::Timeout,
            PipeFailureKind::Io,
            PipeFailureKind::Protocol,
            PipeFailureKind::AccessDenied,
        ] {
            assert_eq!(
                classify_collector_status(Err(kind)),
                CollectorReachability::Unknown,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn stop_collector_ps_args_order_and_paths() {
        let space = stop_collector_ps_args(r"C:\Program Files\CL Recoder\scripts\stop-collector.ps1");
        assert_eq!(
            space,
            vec![
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                r"C:\Program Files\CL Recoder\scripts\stop-collector.ps1",
            ]
        );
        let odd = stop_collector_ps_args(r"C:\odd'name\scripts\stop-collector.ps1");
        assert_eq!(odd[0], "-NoProfile");
        assert_eq!(odd[1], "-ExecutionPolicy");
        assert_eq!(odd[2], "Bypass");
        assert_eq!(odd[3], "-File");
        assert_eq!(odd[4], r"C:\odd'name\scripts\stop-collector.ps1");
        assert_eq!(odd.len(), 5);
        // 路径保持原样（含空格与单引号），不做 PowerShell 转义
        assert!(odd[4].contains('\''));
        assert!(space[4].contains(' '));
    }
}
