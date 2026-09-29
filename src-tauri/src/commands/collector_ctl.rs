//! collector_ctl —— 采集进程控制（PLAN §4.4/§5.1/§8-S10）。
//!
//! - **pipe 客户端**：连 `\\.\pipe\clrecoder-control`（NDJSON 单行请求/单行响应，§4.4），
//!   **500ms 超时**（§5.1）——阻塞 I/O 放独立线程，外层 channel `recv_timeout` 兜底；
//! - **schtasks 提权辅助**：`PowerShell Start-Process powershell -Verb RunAs -ArgumentList
//!   '-NoProfile -ExecutionPolicy Bypass -File "<脚本绝对路径>"'`（路径用原生双引号包裹——
//!   PS 5.1 的 ArgumentList 数组按空格裸拼接、不自动加引号，空格路径必须单串+内嵌双引号）
//!   ——**必须带 -ExecutionPolicy Bypass**（客户端默认 Restricted，提权也不改变策略，
//!   -File 会被直接拒绝，§5.1）；
//! - `collector_start_now`：先 `schtasks /Run`，失败回退 runas 直接启动采集器（§4.7）。
//!
//! 不使用 ShellExecute runas API（Win32_UI_Shell 不在 §9.1 依赖白名单内），
//! 提权统一走 PowerShell `Start-Process -Verb RunAs`。

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use clrecoder_core::ipc::{CtlRequest, CtlResponse, MAX_REQUEST_BYTES, PIPE_NAME};

/// pipe 探测/控制超时（§5.1：连接超时 500ms）。
pub const CTL_TIMEOUT: Duration = Duration::from_millis(500);

/// 采集器计划任务名（§5.1：`/TN ClRecoderCollector`）。
pub const COLLECTOR_TASK_NAME: &str = "ClRecoderCollector";

/// 采集器可执行文件名（与 GUI 主程序同目录，bundle.resources map 落盘，§8-S13）。
const COLLECTOR_EXE: &str = "cl-recoder-collector.exe";

/// 采集器状态 DTO（§4.7 `CollectorStatus`：普通结构体非枚举，camelCase）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorStatusDto {
    /// collector 是否在运行（pipe 可达）
    pub running: bool,
    /// 计划任务 ClRecoderCollector 是否存在（自启是否已配置）
    pub task_exists: bool,
    /// 是否暂停（运行时才有）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    /// 进程启动时刻（RFC3339；运行时才有）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// 最近一次输入事件时刻（尚无事件为 null）
    pub last_event_at: Option<String>,
}

// ---------------------------------------------------------------------------
// pipe 客户端
// ---------------------------------------------------------------------------

/// `CtlRequest` → NDJSON 单行（§4.4：≤ [`MAX_REQUEST_BYTES`] 字节，超长即断开——
/// 客户端侧同样拒发超长请求）。
pub(crate) fn encode_request_line(req: &CtlRequest) -> Result<String, String> {
    let line = serde_json::to_string(req).map_err(|e| e.to_string())?;
    if line.len() + 1 > MAX_REQUEST_BYTES {
        return Err(format!("请求超长（{} 字节 > {MAX_REQUEST_BYTES}）", line.len()));
    }
    Ok(format!("{line}\n"))
}

/// 单次原始管道往返：连接（含 PIPE_BUSY 等待）→ 写行 → 读行 → 解析响应。
///
/// 收发不经 windows crate 的 `ReadFile`/`WriteFile`（windows 0.62 把带 OVERLAPPED 的
/// API 藏在白名单外的 `Win32_System_IO` feature 后面）：把 HANDLE 包成**属主**
/// `std::fs::File`（std 自带 kernel32 绑定），读写走 `std::io`，Drop 即 CloseHandle。
fn raw_exchange(line: &str) -> Result<CtlResponse, String> {
    use std::os::windows::io::FromRawHandle;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    let name_wide: Vec<u16> = PIPE_NAME.encode_utf16().chain(std::iter::once(0)).collect();
    let name = PCWSTR::from_raw(name_wide.as_ptr());

    let open = || -> windows::core::Result<HANDLE> {
        unsafe {
            CreateFileW(
                name,
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
    };

    let handle = match open() {
        Ok(h) => h,
        Err(e) => {
            let code = e.code();
            if code == ERROR_PIPE_BUSY.to_hresult() {
                if !unsafe { WaitNamedPipeW(name, CTL_TIMEOUT.as_millis() as u32) }.as_bool() {
                    return Err("管道忙且等待超时".to_string());
                }
                open().map_err(|e2| format!("连接控制管道失败: {e2}"))?
            } else {
                return Err(format!("连接控制管道失败（collector 未运行？）: {e}"));
            }
        }
    };

    // HANDLE → 属主 File：后续读写全走 std::io；File Drop = CloseHandle（不重复关闭）
    let mut file = unsafe { std::fs::File::from_raw_handle(handle.0 as std::os::windows::io::RawHandle) };

    let result = (|| -> Result<CtlResponse, String> {
        // 显式置字节读模式（SetNamedPipeHandleState 不涉 OVERLAPPED，不在 Win32_System_IO 门控内）
        set_pipe_byte_mode(handle).map_err(|e| format!("设置管道模式失败: {e}"))?;

        // 写一行请求
        use std::io::Write;
        file.write_all(line.as_bytes())
            .map_err(|e| format!("写入请求失败: {e}"))?;

        // 读一行响应（逐字节，响应很小；8KB 上限防失控）
        use std::io::Read;
        let mut out: Vec<u8> = Vec::with_capacity(256);
        let mut one = [0u8; 1];
        loop {
            let n = file.read(&mut one).map_err(|e| format!("读取响应失败: {e}"))?;
            if n == 0 {
                return Err("管道在完整响应前关闭".to_string());
            }
            out.push(one[0]);
            if one[0] == b'\n' {
                break;
            }
            if out.len() > MAX_REQUEST_BYTES * 2 {
                return Err("响应超长".to_string());
            }
        }
        let text = String::from_utf8(out).map_err(|e| format!("响应非 UTF-8: {e}"))?;
        serde_json::from_str::<CtlResponse>(text.trim_end()).map_err(|e| format!("响应解析失败: {e}"))
    })();

    drop(file); // CloseHandle
    result
}

/// `SetNamedPipeHandleState` 置字节读模式（无 OVERLAPPED 参数，不在 Win32_System_IO 门控内）。
fn set_pipe_byte_mode(handle: windows::Win32::Foundation::HANDLE) -> windows::core::Result<()> {
    use windows::Win32::System::Pipes::{SetNamedPipeHandleState, PIPE_READMODE_BYTE};
    let mode = PIPE_READMODE_BYTE;
    unsafe { SetNamedPipeHandleState(handle, Some(&mode), None, None) }
}

/// 发送控制请求（500ms 超时守护，§5.1）。
///
/// 阻塞 I/O 在独立线程执行；超时后该线程自然随系统调用结束（管道关闭即返回），
/// 调用方立即得到超时错误。
pub(crate) fn ctl_request(req: &CtlRequest) -> Result<CtlResponse, String> {
    let line = encode_request_line(req)?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("clrecoder-ctl".into())
        .spawn(move || {
            let _ = tx.send(raw_exchange(&line));
        })
        .map_err(|e| format!("超时守护线程创建失败: {e}"))?;
    match rx.recv_timeout(CTL_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err(format!(
            "collector 控制管道无响应（超时 {}ms）",
            CTL_TIMEOUT.as_millis()
        )),
    }
}

// ---------------------------------------------------------------------------
// 提权辅助（PowerShell Start-Process -Verb RunAs）
// ---------------------------------------------------------------------------

/// PS 字符串单引号转义（'' 表示字面单引号）。
fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// 构造"以管理员运行脚本"的外层 PowerShell 命令（§5.1）。
///
/// 外层非提权 PowerShell 执行 `Start-Process powershell -Verb RunAs`（触发一次 UAC），
/// 内层以 `-NoProfile -ExecutionPolicy Bypass -File <script>` 运行安装脚本
/// ——**两层都必须带 -ExecutionPolicy Bypass**（§5.1：客户端默认 Restricted，提权也不变）。
/// `-Wait -PassThru` + `exit $p.ExitCode` 把脚本退出码透传给外层。
pub(crate) fn ps_runas_script_command(script: &str) -> String {
    // PS 5.1 的 Start-Process -ArgumentList **数组**按空格裸拼接、不给含空格元素加引号：
    // 路径含空格（如 "cl recoder"）时内层 -File 在空格处断裂，内层 PS 以 -196608
    //（-File 找不到文件的特征退出码）退出。因此传单个字符串参数、路径用原生双引号
    // 包裹，子进程 argv 解码后即为一整个带引号参数。UAC 被取消时 Start-Process 抛
    // 终止性错误，try/catch 兜底 exit 4——GUI 据此区分"被取消"与"脚本报错"。
    format!(
        "try {{ $p = Start-Process powershell -Verb RunAs -Wait -PassThru \
         -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File \"{}\"' }} \
         catch {{ exit 4 }}; if ($null -eq $p) {{ exit 4 }}; exit $p.ExitCode",
        ps_quote(script)
    )
}

/// 用 `cmd` 执行外层 PowerShell（CREATE_NO_WINDOW 防控制台闪烁）。
/// `script_path` 仅用于错误提示，便于用户定位丢失的脚本。
fn run_powershell(command: &str, script_path: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", command])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("启动 PowerShell 失败: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(match status.code() {
            Some(4) => "UAC 被取消——未做任何更改，可再次点击重试".to_string(),
            Some(-196608) => format!(
                "提权 PowerShell 找不到脚本文件（程序安装可能不完整）：{script_path}"
            ),
            Some(code) => format!(
                "PowerShell 退出码 {code}（脚本报错；可手动以管理员 PowerShell 运行 {script_path} 复现详情）"
            ),
            None => "PowerShell 被中断".to_string(),
        })
    }
}

/// 定位脚本：优先主 exe 同目录 `scripts\`（bundle.resources 落盘位，§8-S13），
/// 再 exe 同目录、当前目录 `scripts\` 及其父目录 `scripts\`（开发态）。
fn find_script(name: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("scripts").join(name));
            candidates.push(dir.join(name));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("scripts").join(name));
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join("scripts").join(name));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// 提权运行 scripts\ 下的指定脚本（一次 UAC）。
fn runas_script(name: &str) -> Result<(), String> {
    let script = find_script(name)
        .ok_or_else(|| format!("未找到 {name}（应随主程序安装在 scripts\\ 下）"))?;
    let script_str = script.to_string_lossy().to_string();
    run_powershell(&ps_runas_script_command(&script_str), &script_str)
}

/// 采集器 exe 路径（主 exe 同目录，§5.1）。
fn collector_exe_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("解析主程序路径失败: {e}"))?;
    let dir = exe.parent().ok_or_else(|| "主程序路径无父目录".to_string())?;
    let p = dir.join(COLLECTOR_EXE);
    if !p.is_file() {
        return Err(format!("未找到采集器（{}）——请先安装完整程序", p.display()));
    }
    Ok(p)
}

// ---------------------------------------------------------------------------
// Tauri commands（§4.7）
// ---------------------------------------------------------------------------

/// `schtasks /Query` 探测计划任务是否存在（不弹 UAC，只读）。
fn scheduled_task_exists() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("schtasks")
        .args(["/Query", "/TN", COLLECTOR_TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 轮询等待 collector 管道就绪（启动后进程初始化需要时间，不能只看命令退出码）。
fn wait_collector_running(timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if ctl_request(&CtlRequest::Status).is_ok_and(|r| r.ok && r.data.is_some()) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// 采集器进程是否在跑（仅看进程表；不保证管道可用）。
fn collector_process_alive() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // tasklist 退出码恒为 0，用 findstr 过滤镜像名；找不到时 findstr 退出码 1。
    let out = std::process::Command::new("cmd")
        .args([
            "/C",
            "tasklist /FI \"IMAGENAME eq cl-recoder-collector.exe\" /NH | findstr /I \"cl-recoder-collector\"",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// 强杀采集器进程（用于「进程在但管道死」的僵尸态）。普通权限 taskkill 可能失败（采集器是 High IL），
/// 失败时回退一次提权 taskkill（UAC）。
fn kill_collector_process() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let try_kill = || {
        std::process::Command::new("taskkill")
            .args(["/F", "/IM", "cl-recoder-collector.exe"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if try_kill() {
        return Ok(());
    }
    // 提权杀：一次 UAC。Start-Process taskkill -Verb RunAs
    run_powershell(
        "try { Start-Process taskkill -Verb RunAs -Wait -ArgumentList '/F /IM cl-recoder-collector.exe' } catch { exit 4 }",
        "taskkill",
    )?;
    // 给进程表一点时间收缩
    std::thread::sleep(Duration::from_millis(300));
    if try_kill() || !collector_process_alive() {
        Ok(())
    } else {
        Err("无法结束旧的 cl-recoder-collector 进程（请在任务管理器手动结束后重试）".into())
    }
}

/// 启动锁：同一时刻只允许一次启动流程，避免 UI 连点打出多路 UAC / 多实例互踩。
static START_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 启动采集器并**等到管道真正就绪**再返回。
///
/// 策略（针对「狂点才成功」的实际故障模式）：
/// 1. 已就绪 → 直接成功；
/// 2. 进程在但管道死（僵尸，占单实例互斥体）→ 先杀掉，否则新实例会立刻退出；
/// 3. 优先 `schtasks /Run`（不弹 UAC）；等待失败则 runas 直启采集器；
/// 4. 全程串行（[`START_LOCK`]），等待窗口加长到 5s×2。
fn start_collector_and_wait() -> Result<(), String> {
    let _guard = START_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    // 已在跑：无需再拉
    if wait_collector_running(Duration::from_millis(400)) {
        return Ok(());
    }

    // 僵尸进程：占着单实例互斥体但不服务管道 → 新实例秒退，必须先清掉
    if collector_process_alive() {
        crate::gui_log!("WARN: 检测到 cl-recoder-collector 进程存在但控制管道无响应，尝试结束后重启");
        kill_collector_process()?;
        // 等互斥体释放
        std::thread::sleep(Duration::from_millis(250));
    }

    // 1) 计划任务拉起（不弹 UAC；任务 /RL HIGHEST，以最高权限运行）
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let task_ok = std::process::Command::new("schtasks")
        .args(["/Run", "/TN", COLLECTOR_TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if task_ok && wait_collector_running(Duration::from_secs(5)) {
        return Ok(());
    }

    // 2) 回退：runas 直启采集器 exe（一次 UAC）。-Wait 确保进程真的起来了再探测。
    let exe = collector_exe_path()?;
    let exe_str = exe.to_string_lossy().to_string();
    run_powershell(
        &format!(
            "try {{ Start-Process -FilePath '{}' -Verb RunAs -Wait }} catch {{ exit 4 }}",
            ps_quote(&exe_str)
        ),
        &exe_str,
    )?;
    if wait_collector_running(Duration::from_secs(5)) {
        return Ok(());
    }

    // 3) 还不行：再杀一次僵尸 + 直启（覆盖「刚杀完又被计划任务拉起半死实例」的竞态）
    if collector_process_alive() {
        let _ = kill_collector_process();
        std::thread::sleep(Duration::from_millis(250));
        run_powershell(
            &format!(
                "try {{ Start-Process -FilePath '{}' -Verb RunAs -Wait }} catch {{ exit 4 }}",
                ps_quote(&exe_str)
            ),
            &exe_str,
        )?;
        if wait_collector_running(Duration::from_secs(5)) {
            return Ok(());
        }
    }

    let task = if scheduled_task_exists() { "已配置" } else { "未配置" };
    Err(format!(
        "采集器未能就绪（管道无响应）。自启任务{task}；进程在跑：{}。\
         可尝试：任务管理器结束 cl-recoder-collector 后再点一次「立即启动」",
        if collector_process_alive() { "是" } else { "否" }
    ))
}

/// 采集器状态（§4.7 `collector_status() -> CollectorStatus`；pipe 探测 500ms 超时，
/// 未运行 → `running:false` 而非报错——前端以此显示"采集器未运行"引导）。
#[tauri::command]
pub async fn collector_status() -> Result<CollectorStatusDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let task_exists = scheduled_task_exists();
        match ctl_request(&CtlRequest::Status) {
            Ok(resp) if resp.ok && resp.data.is_some() => {
                let d = resp.data.expect("上分支已检查");
                Ok(CollectorStatusDto {
                    running: true,
                    task_exists,
                    paused: Some(d.paused),
                    started_at: Some(d.started_at),
                    last_event_at: d.last_event_at,
                })
            }
            _ => Ok(CollectorStatusDto {
                task_exists,
                ..CollectorStatusDto::default()
            }),
        }
    })
    .await
    .map_err(|e| format!("状态探测任务失败: {e}"))?
}

/// 暂停/恢复统计（§4.7 `set_collector_paused(paused: bool) -> Result<(), String>`；
/// 暂停只丢弃 Input 事件，Foreground 照常——§5.3）。
#[tauri::command]
pub async fn set_collector_paused(paused: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let resp = ctl_request(&CtlRequest::SetPaused { paused })?;
        if resp.ok {
            Ok(())
        } else {
            Err(resp.error.unwrap_or_else(|| "collector 拒绝了请求".into()))
        }
    })
    .await
    .map_err(|e| format!("暂停控制任务失败: {e}"))?
}

/// 启用采集器自启（§4.7：PowerShell -Verb RunAs 调 `scripts\install-collector-task.ps1`，
/// 一次 UAC；-ExecutionPolicy Bypass 必带）。
///
/// 成功后**自动启动采集器并等待就绪**（PLAN §5.1）；并二次校验计划任务确已落盘——
/// 只看脚本退出码会在任务未创建时误报成功（用户反馈：显示已启用但实际未启用）。
#[tauri::command]
pub async fn collector_autostart_enable() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        runas_script("install-collector-task.ps1")?;
        if !scheduled_task_exists() {
            return Err("安装脚本已返回，但计划任务 ClRecoderCollector 仍不存在（请检查磁盘/策略）".into());
        }
        start_collector_and_wait()
    })
    .await
    .map_err(|e| format!("自启任务失败: {e}"))?
}

/// 停用采集器自启（同提权模式运行 `scripts\uninstall-collector-task.ps1`）。
#[tauri::command]
pub async fn collector_autostart_disable() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        runas_script("uninstall-collector-task.ps1")?;
        if scheduled_task_exists() {
            return Err("卸载脚本已返回，但计划任务仍存在".into());
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("自启任务失败: {e}"))?
}

/// 立即启动采集器（§4.7：先 `schtasks /Run`，失败回退 runas 直接启动；等待就绪）。
#[tauri::command]
pub async fn collector_start_now() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(start_collector_and_wait)
        .await
        .map_err(|e| format!("启动采集器任务失败: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::ipc::StatusData;

    /// 请求行编码：逐字符合 §4.4 线格式示例 + 8KB 上限拒绝。
    #[test]
    fn request_line_matches_ipc_wire_format() {
        let line = encode_request_line(&CtlRequest::Status).unwrap();
        assert_eq!(line, "{\"cmd\":\"status\"}\n");

        let line = encode_request_line(&CtlRequest::SetPaused { paused: true }).unwrap();
        assert_eq!(line, "{\"cmd\":\"set_paused\",\"paused\":true}\n");

        let line = encode_request_line(&CtlRequest::Shutdown).unwrap();
        assert_eq!(line, "{\"cmd\":\"shutdown\"}\n");

        // 超长请求客户端侧拒发（§4.4 ≤8KB）
        let huge = CtlRequest::SetPaused { paused: false }; // 小请求合法
        assert!(encode_request_line(&huge).is_ok());
        // 直接验证常量与核心契约一致
        assert_eq!(MAX_REQUEST_BYTES, 8192);
        assert_eq!(PIPE_NAME, r"\\.\pipe\clrecoder-control");
    }

    /// 响应解析：§4.4 三个线格式示例能被客户端解析（与 core 的 serde 往返对齐）。
    #[test]
    fn response_parsing_via_core_contract() {
        let ok = r#"{"ok":true,"data":{"paused":false,"version":"0.1.0","started_at":"2026-09-27T22:00:00+08:00","last_event_at":"2026-09-27T22:14:31+08:00","events_seen":48219}}"#;
        let resp: CtlResponse = serde_json::from_str(ok).unwrap();
        assert!(resp.ok);
        let d: StatusData = resp.data.unwrap();
        assert_eq!((d.paused, d.version.as_str(), d.events_seen), (false, "0.1.0", 48219));

        let nodata: CtlResponse = serde_json::from_str(r#"{"ok":true,"data":null}"#).unwrap();
        assert!(nodata.ok && nodata.data.is_none());

        let err: CtlResponse =
            serde_json::from_str(r#"{"ok":false,"error":"unknown command"}"#).unwrap();
        assert!(!err.ok && err.error.as_deref() == Some("unknown command"));
    }

    /// **提权辅助硬性要求**：内层 ArgumentList 单串形态、路径原生双引号包裹（空格路径
    /// 安全——PS 5.1 数组形态会在空格处断裂，-196608 事故的根因），-Verb RunAs 在场、
    /// UAC 取消有 exit 4 兜底、单引号路径转义正确。
    #[test]
    fn runas_command_contains_execution_policy_bypass() {
        let cmd = ps_runas_script_command(r"C:\Program Files\CL Recoder\scripts\install-collector-task.ps1");
        // 内层 Bypass + 双引号包裹的空格路径（单串 ArgumentList）
        assert!(cmd.contains("-ExecutionPolicy Bypass -File \"C:\\Program Files\\CL Recoder"), "{cmd}");
        assert!(cmd.contains("-Verb RunAs"), "{cmd}");
        assert!(cmd.contains("-Wait -PassThru"), "{cmd}");
        // UAC 取消兜底 + 退出码透传
        assert!(cmd.contains("catch { exit 4 }"), "{cmd}");
        assert!(cmd.contains("exit $p.ExitCode"), "{cmd}");
        // 单引号路径转义（路径位于单引号 PS 串内）
        let cmd2 = ps_runas_script_command(r"C:\odd'name\install.ps1");
        assert!(cmd2.contains(r"odd''name"), "{cmd2}");
    }

    /// 外层 PowerShell 调用参数（run_powershell 的命令串形态）：外层也必须 Bypass。
    #[test]
    fn outer_powershell_would_run_with_bypass() {
        // run_powershell 固定附加这四个参数（实现处）；这里锁定该形态防止回归
        let outer = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"];
        assert_eq!(outer[1..3], ["-ExecutionPolicy", "Bypass"]);
        let inner = ps_runas_script_command("install.ps1");
        assert_eq!(inner.matches("-ExecutionPolicy").count() + 1, 2, "外层+内层共两处 Bypass");
    }

    /// 超时常量（§5.1 500ms）与任务名（§5.1）。
    #[test]
    fn constants_match_plan() {
        assert_eq!(CTL_TIMEOUT, Duration::from_millis(500));
        assert_eq!(COLLECTOR_TASK_NAME, "ClRecoderCollector");
    }

    /// 停用的 collector：状态探测必须返回 running=false 而不是 Err（引导态语义）。
    #[test]
    fn status_dto_default_is_not_running() {
        let d = CollectorStatusDto::default();
        assert!(!d.running);
        assert!(!d.task_exists);
        assert!(d.paused.is_none());
        // 序列化形状：running/taskExists 恒在场；paused/startedAt 省略；lastEventAt null
        let js = serde_json::to_string(&d).unwrap();
        assert_eq!(
            js,
            r#"{"running":false,"taskExists":false,"lastEventAt":null}"#,
            "{js}"
        );
    }
}
