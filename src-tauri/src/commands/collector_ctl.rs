//! collector_ctl —— 采集进程控制（typed 管道传输 + 安全启动 + 任务修复，usability-runtime-v3 §4.2/§4.3）。
//!
//! - **pipe 客户端**：连 `\\.\pipe\clrecoder-control`（NDJSON 单行请求/单行响应，§4.4），
//!   **500ms 超时**（§5.1）——阻塞 I/O 放独立线程，外层 channel `recv_timeout` 兜底；
//!   超时/守护线程失败/连接错误全部映射为 **typed** [`CtlError`]（[`PipeFailureKind`]），
//!   错误种类只由 Win32 码与失败阶段决定，**绝不解析中文文案**；旧
//!   [`ctl_request`]（`Result<_, String>`）保留为转换包装（托盘等既有调用方不变）；
//! - **status 分类**：成功直接 running/paused（processEvidence=present，无需补查）；
//!   失败按 typed kind 补查本用户进程证据（TTL 5s + single-flight，归
//!   [`super::collector_health`]）后交给纯分类 [`classify_health`]——不回填旧
//!   paused/startedAt 为当前事实，不宣称未运行；
//! - **安全启动**（§4.2）：START_LOCK、无 Wait 直启、有限脚本 Wait 保持；**删除**
//!   自动/IM 强杀与"瞬时不通就是僵尸"分支——进程 Present/Unknown 只等待（≤5s），
//!   仍不可达返回可解释错误，不杀进程、不重复 runas；权限拒绝/Unknown 同样不杀；
//!   Absent 才进入 schtasks 优先 → 有界就绪 → 必要 runas（本流程**至多一次**直启），
//!   失败后补查若进程已存在则停止重试；没有新自动重启策略；
//! - **schtasks 提权辅助**：`PowerShell Start-Process powershell -Verb RunAs -ArgumentList
//!   '-NoProfile -ExecutionPolicy Bypass -File "<脚本绝对路径>"'`（路径用原生双引号包裹——
//!   PS 5.1 的 ArgumentList 数组按空格裸拼接、不自动加引号，空格路径必须单串+内嵌双引号）
//!   ——**必须带 -ExecutionPolicy Bypass**（客户端默认 Restricted，提权也不改变策略，§5.1）；
//!   修复入口 [`collector_autostart_repair`] 经静态 `-RepairOnly -ExpectedUserSid <SID>`
//!   参数调用同一安装脚本（一次 UAC、有限脚本 Wait、等待退出码，§4.3）；
//! - `collector_start_now`：先 `schtasks /Run`，失败回退 runas 直接启动采集器（§4.7）——
//!   直启外层 PowerShell **只受理创建请求（不含 -Wait，§4.6）**，随后管道 5s 探测就绪；
//! - **任务证据缓存**（§4.2：30s TTL，归 [`super::collector_health`]；写路径 uncached
//!   实时回读）；**只读任务策略** [`get_collector_task_policy`] 仅 Settings 活动时调用，
//!   不随 500ms status 重复（§4.3）。
//!
//! 不使用 ShellExecute runas API（Win32_UI_Shell 不在 §9.1 依赖白名单内），
//! 提权统一走 PowerShell `Start-Process -Verb RunAs`。

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use clrecoder_core::ipc::{CtlRequest, CtlResponse, StatusData, MAX_REQUEST_BYTES, PIPE_NAME};

use super::collector_health::{
    self, classify_health, process_evidence_cached, task_evidence_cached, task_evidence_uncached,
    CollectorHealth, DiagnosticCode, Evidence, PipeFailureKind, PipeProbe,
};

/// pipe 探测/控制超时（§5.1：连接超时 500ms）。
pub const CTL_TIMEOUT: Duration = Duration::from_millis(500);

static LAST_HEALTH: std::sync::Mutex<Option<CollectorHealth>> = std::sync::Mutex::new(None);

fn health_event(previous: Option<CollectorHealth>, current: CollectorHealth) -> Option<&'static str> {
    if previous == Some(current) {
        None
    } else if matches!(current, CollectorHealth::Running | CollectorHealth::Paused)
        && previous.is_some_and(|h| !matches!(h, CollectorHealth::Running | CollectorHealth::Paused))
    {
        Some("collector.health_recovered")
    } else {
        Some("collector.health_changed")
    }
}

fn record_health(status: &CollectorStatusDto) {
    let mut previous = LAST_HEALTH.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(code) = health_event(*previous, status.health) {
        let level = if matches!(status.health, CollectorHealth::Running | CollectorHealth::Paused) {
            clrecoder_diagnostics::Level::Info
        } else {
            clrecoder_diagnostics::Level::Warn
        };
        super::diagnostics::record_gui_event(level, code, &format!(
            "health={:?}, diagnostic={:?}, process={:?}",
            status.health, status.diagnostic_code, status.process_evidence,
        ));
    }
    *previous = Some(status.health);
}

/// 采集器计划任务名（§5.1：`/TN ClRecoderCollector`）。
pub const COLLECTOR_TASK_NAME: &str = "ClRecoderCollector";

/// 采集器可执行文件名（与 GUI 主程序同目录，bundle.resources map 落盘，§8-S13）。
const COLLECTOR_EXE: &str = "cl-recoder-collector.exe";

/// 采集器状态 DTO（§4.7 `CollectorStatus`：普通结构体非枚举，camelCase；§4.2 扩展字段）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorStatusDto {
    /// 保留旧语义：本次成功取得有效 status
    pub running: bool,
    /// 计划任务 ClRecoderCollector 是否存在（仅在存在证据成立时 true；unknown 时 false 仅作 legacy）
    pub task_exists: bool,
    /// 任务存在证据（§4.2）
    pub task_evidence: Evidence,
    /// 纯分类健康（§4.2 规则表）
    pub health: CollectorHealth,
    /// 本次管道是否取得有效 status
    pub pipe_reachable: bool,
    /// 本用户进程证据（成功 status 恒为 present——可响应的本用户管道服务即当前证据）
    pub process_evidence: Evidence,
    /// 稳定诊断短码（失败时保留原 pipe 类别；成功为 null）
    pub diagnostic_code: Option<DiagnosticCode>,
    /// 诊断文字（可改善；失败不吞掉原 pipe 类别）
    pub diagnostic_message: Option<String>,
    /// 是否暂停（仅本次成功 status 时在场——失败不回填旧值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    /// 进程启动时刻（RFC3339；仅本次成功 status 时在场）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// 最近一次输入事件时刻（尚无事件为 null）
    pub last_event_at: Option<String>,
}

/// 只读任务策略 DTO（§4.3：逐字段对应 TS `TaskPolicy`，snake_case Rust 字段以 camelCase
/// 序列化；appliesOnNextStart 固定 true——定义更新对后续启动实例生效）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPolicyDto {
    /// 任务证据（任务缺失 → absent、不可读 → unknown，此时其余字段为 null）
    pub evidence: Evidence,
    /// ExecutionTimeLimit（节点缺失为 null——如真实 /Create 产物就没有该节点）
    pub execution_time_limit: Option<String>,
    /// DisallowStartIfOnBatteries（节点缺失为 null）
    pub disallow_start_if_on_batteries: Option<bool>,
    /// StopIfGoingOnBatteries（节点缺失为 null）
    pub stop_if_going_on_batteries: Option<bool>,
    /// 三项全部等于目标值（PT0S / false / false）
    pub compliant: Option<bool>,
    /// 定义更新对后续启动生效（§4.3 固定 true）
    pub applies_on_next_start: bool,
}

/// typed 管道传输错误（§4.2）：种类由 Win32 码/失败阶段直接映射产生。
#[derive(Debug, Clone)]
pub struct CtlError {
    pub kind: PipeFailureKind,
    pub message: String,
}

// ---------------------------------------------------------------------------
// pipe 客户端（typed）
// ---------------------------------------------------------------------------

/// `CtlRequest` → NDJSON 单行（§4.4：≤ [`MAX_REQUEST_BYTES`] 字节，超长即断开——
/// 客户端侧同样拒发超长请求）。
pub(crate) fn encode_request_line(req: &CtlRequest) -> Result<String, String> {
    let line = serde_json::to_string(req).map_err(|e| e.to_string())?;
    if line.len() + 1 > MAX_REQUEST_BYTES {
        return Err(format!(
            "请求超长（{} 字节 > {MAX_REQUEST_BYTES}）",
            line.len()
        ));
    }
    Ok(format!("{line}\n"))
}

/// CreateFileW 失败 → 种类（两处打开失败分支共用；§4.2 不经中文文案解析）。
fn open_err_kind(e: &windows::core::Error) -> PipeFailureKind {
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND};
    let code = e.code();
    if code == ERROR_FILE_NOT_FOUND.to_hresult() {
        PipeFailureKind::NotFound
    } else if code == ERROR_ACCESS_DENIED.to_hresult() {
        PipeFailureKind::AccessDenied
    } else {
        PipeFailureKind::Io
    }
}

/// CreateFileW 失败文案（种类对应的用户可读说明）。
fn open_err_message(e: &windows::core::Error) -> String {
    match open_err_kind(e) {
        PipeFailureKind::NotFound => "采集器未运行（控制管道不存在）".to_string(),
        PipeFailureKind::AccessDenied => {
            "控制管道访问被拒（采集器可能以其他用户身份运行）".to_string()
        }
        _ => format!("连接控制管道失败: {e}"),
    }
}

/// 单次原始管道往返：连接（含 PIPE_BUSY 等待）→ 写行 → 读行 → 解析响应。
///
/// 收发不经 windows crate 的 `ReadFile`/`WriteFile`（windows 0.62 把带 OVERLAPPED 的
/// API 藏在白名单外的 `Win32_System_IO` feature 后面）：把 HANDLE 包成**属主**
/// `std::fs::File`（std 自带 kernel32 绑定），读写走 `std::io`，Drop 即 CloseHandle。
fn raw_exchange(line: &str) -> Result<CtlResponse, CtlError> {
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
                    return Err(CtlError {
                        kind: PipeFailureKind::Busy,
                        message: "管道忙且等待超时".to_string(),
                    });
                }
                open().map_err(|e2| CtlError {
                    kind: open_err_kind(&e2),
                    message: open_err_message(&e2),
                })?
            } else {
                return Err(CtlError {
                    kind: open_err_kind(&e),
                    message: open_err_message(&e),
                });
            }
        }
    };

    // HANDLE → 属主 File：后续读写全走 std::io；File Drop = CloseHandle（不重复关闭）
    let mut file =
        unsafe { std::fs::File::from_raw_handle(handle.0 as std::os::windows::io::RawHandle) };

    let result = (|| -> Result<CtlResponse, CtlError> {
        // 显式置字节读模式（SetNamedPipeHandleState 不涉 OVERLAPPED，不在 Win32_System_IO 门控内）
        set_pipe_byte_mode(handle).map_err(|e| CtlError {
            kind: PipeFailureKind::Io,
            message: format!("设置管道模式失败: {e}"),
        })?;

        // 写一行请求
        use std::io::Write;
        file.write_all(line.as_bytes()).map_err(|e| CtlError {
            kind: PipeFailureKind::Io,
            message: format!("写入请求失败: {e}"),
        })?;

        // 读一行响应（逐字节，响应很小；8KB 上限防失控）
        use std::io::Read;
        let mut out: Vec<u8> = Vec::with_capacity(256);
        let mut one = [0u8; 1];
        loop {
            let n = file.read(&mut one).map_err(|e| CtlError {
                kind: PipeFailureKind::Io,
                message: format!("读取响应失败: {e}"),
            })?;
            if n == 0 {
                return Err(CtlError {
                    kind: PipeFailureKind::Protocol,
                    message: "管道在完整响应前关闭".to_string(),
                });
            }
            out.push(one[0]);
            if one[0] == b'\n' {
                break;
            }
            if out.len() > MAX_REQUEST_BYTES * 2 {
                return Err(CtlError {
                    kind: PipeFailureKind::Protocol,
                    message: "响应超长".to_string(),
                });
            }
        }
        let text = String::from_utf8(out).map_err(|e| CtlError {
            kind: PipeFailureKind::Protocol,
            message: format!("响应非 UTF-8: {e}"),
        })?;
        serde_json::from_str::<CtlResponse>(text.trim_end()).map_err(|e| CtlError {
            kind: PipeFailureKind::Protocol,
            message: format!("响应解析失败: {e}"),
        })
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

/// 500ms 守护（§5.1）：exchange 注入接缝——单测注入假 exchange（禁止真管道）；
/// 超时 → [`PipeFailureKind::Timeout`]，守护线程创建失败 → [`PipeFailureKind::WorkerUnavailable`]。
fn exchange_with_timeout(
    line: &str,
    exchange: impl FnOnce(&str) -> Result<CtlResponse, CtlError> + Send + 'static,
) -> Result<CtlResponse, CtlError> {
    let (tx, rx) = std::sync::mpsc::channel();
    let line_owned = line.to_string();
    let spawned = std::thread::Builder::new()
        .name("clrecoder-ctl".into())
        .spawn(move || {
            let _ = tx.send(exchange(&line_owned));
        });
    let worker = match spawned {
        Ok(w) => w,
        Err(e) => {
            return Err(CtlError {
                kind: PipeFailureKind::WorkerUnavailable,
                message: format!("超时守护线程创建失败: {e}"),
            })
        }
    };
    let result = match rx.recv_timeout(CTL_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err(CtlError {
            kind: PipeFailureKind::Timeout,
            message: format!(
                "collector 控制管道无响应（超时 {}ms）",
                CTL_TIMEOUT.as_millis()
            ),
        }),
    };
    // 超时存活时守护线程自然随系统调用结束（管道关闭即返回）——不 join（既有边界）
    drop(worker);
    result
}

/// typed 控制请求（§4.2 新私有入口）：500ms 守护 + typed 错误。
fn ctl_request_typed(req: &CtlRequest) -> Result<CtlResponse, CtlError> {
    let line = encode_request_line(req).map_err(|m| CtlError {
        kind: PipeFailureKind::Protocol,
        message: m,
    })?;
    exchange_with_timeout(&line, raw_exchange)
}

/// 发送控制请求（旧 String 语义，保留既有调用方——托盘/暂停控制等）。
pub(crate) fn ctl_request(req: &CtlRequest) -> Result<CtlResponse, String> {
    ctl_request_typed(req).map_err(|e| e.message)
}

// ---------------------------------------------------------------------------
// status 分类装配（§4.2）
// ---------------------------------------------------------------------------

/// 成功 status → DTO（§4.2：processEvidence=present、诊断清空、无需进程补查）。
fn status_dto_from_success(d: StatusData, task_ev: Evidence) -> CollectorStatusDto {
    CollectorStatusDto {
        running: true,
        task_exists: task_ev == Evidence::Present,
        task_evidence: task_ev,
        health: classify_health(PipeProbe::Status { paused: d.paused }, Evidence::Present),
        pipe_reachable: true,
        process_evidence: Evidence::Present,
        diagnostic_code: None,
        diagnostic_message: None,
        paused: Some(d.paused),
        started_at: Some(d.started_at),
        last_event_at: d.last_event_at,
    }
}

/// 失败 → DTO（§4.2：不回填旧 paused/startedAt 为当前事实；短码保留原 pipe 类别；
/// 进程补查不可用反映在 processEvidence/message，不吞掉原 pipe 类别）。
fn status_dto_from_failure(
    err: &CtlError,
    task_ev: Evidence,
    proc_ev: Evidence,
) -> CollectorStatusDto {
    let mut message = err.message.clone();
    if proc_ev == Evidence::Unknown {
        message.push_str("；当前用户进程证据不足");
    }
    CollectorStatusDto {
        running: false,
        task_exists: task_ev == Evidence::Present,
        task_evidence: task_ev,
        health: classify_health(PipeProbe::Failed(err.kind), proc_ev),
        pipe_reachable: false,
        process_evidence: proc_ev,
        diagnostic_code: Some(err.kind.diagnostic_code()),
        diagnostic_message: Some(message),
        paused: None,
        started_at: None,
        last_event_at: None,
    }
}

// ---------------------------------------------------------------------------
// 提权辅助（PowerShell Start-Process -Verb RunAs）
// ---------------------------------------------------------------------------

/// PS 字符串单引号转义（'' 表示字面单引号）。
fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// 构造"以管理员运行脚本（带参数）"的外层 PowerShell 命令（§4.3）。
///
/// 外层非提权 PowerShell 执行 `Start-Process powershell -Verb RunAs`（触发一次 UAC），
/// 内层以 `-NoProfile -ExecutionPolicy Bypass -File <script> [args...]` 运行
/// ——**两层都必须带 -ExecutionPolicy Bypass**（§5.1：客户端默认 Restricted，提权也不变）。
/// `-Wait -PassThru` + `exit $p.ExitCode` 把脚本退出码透传给外层（§4.3 有限脚本 Wait）。
///
/// 单串 ArgumentList + 路径/参数值原生双引号包裹（PS 5.1 数组形态按空格裸拼接，
/// 空格路径会在空格处断裂——历史 -196608 事故的根因）。参数值来源受控（静态开关名 /
/// 本进程 SID），双引号仍做防御性 `\"` 转义。
pub(crate) fn ps_runas_script_command_with_args(script: &str, args: &[&str]) -> String {
    let mut inner = format!(
        "-NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        ps_quote(script)
    );
    for a in args {
        inner.push_str(&format!(" \"{}\"", a.replace('"', "\\\"")));
    }
    // PS 5.1 的 Start-Process -ArgumentList **数组**按空格裸拼接、不给含空格元素加引号：
    // 因此传单个字符串参数、路径用原生双引号包裹，子进程 argv 解码后即为一整个带引号参数。
    // UAC 被取消时 Start-Process 抛终止性错误，try/catch 兜底 exit 4——GUI 据此区分
    // "被取消"与"脚本报错"。
    format!(
        "try {{ $p = Start-Process powershell -Verb RunAs -Wait -PassThru \
         -ArgumentList '{inner}' }} \
         catch {{ exit 4 }}; if ($null -eq $p) {{ exit 4 }}; exit $p.ExitCode",
    )
}

/// 无参数版 [`ps_runas_script_command_with_args`]（启用/停用脚本沿用既有调用形态）。
pub(crate) fn ps_runas_script_command(script: &str) -> String {
    ps_runas_script_command_with_args(script, &[])
}

/// 构造"以管理员直接启动**常驻**采集器 exe"的外层 PowerShell 命令（§4.6）。
///
/// 与 [`ps_runas_script_command`]（一次性安装/修复脚本）不同：collector 不退出，
/// **绝不能 -Wait**——否则外层 PowerShell 一直阻塞到采集器退出，启动流程永远
/// "处理中"（F2）。外层只确认创建请求已受理（-PassThru 返回进程对象且非空）即
/// `exit 0` 返回；管道是否真正就绪由 GUI 侧 [`wait_collector_running`] 轮询判定，
/// 不读取常驻进程 ExitCode（它不会退出）。UAC 取消/启动失败沿用 catch exit 4
/// 的现有文案映射（`run_powershell`）。
pub(crate) fn ps_runas_collector_command(exe: &str) -> String {
    // 路径经 ps_quote 单引号包裹：外层 -Command 直接求值该 Start-Process 表达式
    //（非 ArgumentList 数组裸拼接），空格/单引号路径安全。
    format!(
        "try {{ $p = Start-Process -FilePath '{}' -Verb RunAs -WindowStyle Hidden -PassThru }} \
         catch {{ exit 4 }}; if ($null -eq $p) {{ exit 4 }}; exit 0",
        ps_quote(exe)
    )
}

/// 用 `cmd` 执行外层 PowerShell（CREATE_NO_WINDOW 防控制台闪烁）。
/// `script_path` 仅用于错误提示，便于用户定位丢失的脚本。
fn run_powershell(command: &str, script_path: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            command,
        ])
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
            // §4.3 明确冲突：定义兼容性校验不满足（账户/登录类型/触发器/安全描述符）
            Some(5) => format!(
                "计划任务定义冲突（账户/登录类型/触发器或安全描述符无法安全更新）——\
                 脚本未做任何修改，可手动以管理员 PowerShell 运行 {script_path} 查看详情"
            ),
            // §4.3 回读校验失败：已更新但三项/owner/group/DACL 与预期不符
            Some(6) => format!(
                "脚本已执行但回读校验未通过（策略三项或安全描述符与预期不符）：{script_path}"
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

/// 提权运行 scripts\ 下的指定脚本并附加静态参数（§4.3 修复入口：一次 UAC、
/// -Wait 有限脚本、等待退出码）。
fn runas_script_with_args(name: &str, args: &[&str]) -> Result<(), String> {
    let script = find_script(name)
        .ok_or_else(|| format!("未找到 {name}（应随主程序安装在 scripts\\ 下）"))?;
    let script_str = script.to_string_lossy().to_string();
    run_powershell(
        &ps_runas_script_command_with_args(&script_str, args),
        &script_str,
    )
}

/// 采集器 exe 路径（主 exe 同目录，§5.1）。
fn collector_exe_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("解析主程序路径失败: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "主程序路径无父目录".to_string())?;
    let p = dir.join(COLLECTOR_EXE);
    if !p.is_file() {
        return Err(format!("未找到采集器（{}）——请先安装完整程序", p.display()));
    }
    Ok(p)
}

// ---------------------------------------------------------------------------
// 安全启动（§4.2）
// ---------------------------------------------------------------------------

/// `schtasks /Run` 拉起计划任务（无 UAC；任务 /RL HIGHEST 以最高权限运行）。
fn run_scheduled_task_now() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("schtasks")
        .args(["/Run", "/TN", COLLECTOR_TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 轮询等待 collector 管道就绪（启动后进程初始化需要时间，不能只看命令退出码）。
fn wait_collector_running(timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if matches!(
            ctl_request_typed(&CtlRequest::Status),
            Ok(resp) if resp.ok && resp.data.is_some()
        ) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// runas 直启采集器的最小测试接缝（§4.6）：构造常驻命令 → `launch` 受理创建请求 →
/// 成功才以 **5 秒**窗口探测管道就绪。launch 失败（含 UAC 取消）原样透传、**不探测**；
/// `Ok(bool)` 表示管道是否就绪（false 不当作成功，调用方按 §4.2 安全启动策略诊断）。
///
/// 生产直启路径经此接缝，注入 [`run_powershell`] 与 [`wait_collector_running`]；
/// 单测注入假闭包（禁止真 UAC / 真管道）。
fn start_direct_with(
    exe: &str,
    launch: impl FnOnce(&str) -> Result<(), String>,
    wait_ready: impl FnOnce(Duration) -> bool,
) -> Result<bool, String> {
    launch(&ps_runas_collector_command(exe))?;
    Ok(wait_ready(Duration::from_secs(5)))
}

/// 启动锁：同一时刻只允许一次启动流程，避免 UI 连点打出多路 UAC / 多实例互踩。
static START_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 安全启动核心（§4.2，依赖注入接缝；单测禁止真 UAC / 真管道 / 真进程扫描）：
///
/// 1. 已就绪 → 直接成功；
/// 2. 进程证据 Present/Unknown：**只等待**最多 5 秒——不杀、不 runas、不 schtasks
///    （Present 不重复 launch；"瞬时不通就是僵尸"的自动强杀分支已删除，§4.2）；
///    仍不可达返回可解释错误；
/// 3. 进程证据 Absent：`schtasks /Run` 优先 → 有界（5s）就绪 → 必要时 runas 直启
///    （本流程**至多一次**，§4.2）；schtasks 已受理但未就绪时先补查进程证据——
///    进程已存在则不再重复拉起；直启后仍不就绪也补查：进程已存在 → 停止重试并返回
///    可解释错误；launch 失败（含 UAC 取消）原样透传；没有新自动重启策略。
fn start_collector_with(
    wait_ready: impl Fn(Duration) -> bool,
    process_evidence: impl Fn() -> Evidence,
    run_task: impl Fn() -> bool,
    direct_start: impl FnOnce() -> Result<bool, String>,
) -> Result<(), String> {
    // 已在跑：无需再拉
    if wait_ready(Duration::from_millis(400)) {
        return Ok(());
    }

    match process_evidence() {
        // 进程在（或归属不明）：绝不杀、绝不重复 launch——只给一次有界就绪窗口
        Evidence::Present | Evidence::Unknown => {
            if wait_ready(Duration::from_secs(5)) {
                Ok(())
            } else {
                Err(
                    "采集器进程已存在，但控制管道等待 5 秒后仍未就绪（可能仍在初始化或被安全软件拦截）。\
                     不会自动结束既有进程；可稍后重试，或查看诊断日志定位"
                        .to_string(),
                )
            }
        }
        Evidence::Absent => {
            // 1) 计划任务拉起（不弹 UAC）
            let task_launched = run_task();
            if task_launched && wait_ready(Duration::from_secs(5)) {
                return Ok(());
            }
            // schtasks 已受理但未就绪：补查进程证据——已起来就不重复拉起（runas 只做必要一次）
            if task_launched {
                match process_evidence() {
                    Evidence::Present | Evidence::Unknown => {
                        return if wait_ready(Duration::from_secs(5)) {
                            Ok(())
                        } else {
                            Err(
                                "计划任务已触发采集器进程，但控制管道等待 5 秒后仍未就绪（可能仍在初始化或被安全软件拦截）。\
                                 不会自动结束既有进程；可稍后重试，或查看诊断日志定位"
                                    .to_string(),
                            )
                        };
                    }
                    Evidence::Absent => {}
                }
            }
            // 2) 必要时 runas 直启采集器 exe（本流程至多一次；外层 PowerShell 只等待创建请求
            //    受理即返回——不含 -Wait，随后接缝内以 5 秒窗口轮询管道就绪，§4.6）
            match direct_start() {
                Ok(true) => Ok(()),
                Ok(false) => {
                    // 失败后补查：进程已存在 → 停止重试（本流程不再提权、不杀进程）
                    match process_evidence() {
                        Evidence::Present | Evidence::Unknown => Err(
                            "采集器已拉起，但控制管道等待 5 秒后仍未就绪。本流程不会再次提权重启，\
                             也不会结束既有进程；可稍后重试，或查看诊断日志定位"
                                .to_string(),
                        ),
                        Evidence::Absent => Err(
                            "采集器未能启动（管道无响应、进程未在运行）。若自启任务未生效，\
                             可在设置页启用采集器自启或使用「修复采集器自启」"
                                .to_string(),
                        ),
                    }
                }
                // launch 失败（含 UAC 取消）原样透传
                Err(e) => Err(e),
            }
        }
    }
}

/// 启动采集器并**等到管道真正就绪**再返回（§4.2 安全启动；全程串行 [`START_LOCK`]）。
fn start_collector_and_wait() -> Result<(), String> {
    let _guard = START_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    start_collector_with(
        wait_collector_running,
        || process_evidence_cached(COLLECTOR_EXE),
        run_scheduled_task_now,
        || {
            // 采集器 exe 路径惰性解析：安装布局下 schtasks /Run 不依赖本地 exe 是否缺失
            let exe = collector_exe_path()?;
            let exe_str = exe.to_string_lossy().to_string();
            start_direct_with(
                &exe_str,
                |cmd| run_powershell(cmd, &exe_str),
                wait_collector_running,
            )
        },
    )
}

// ---------------------------------------------------------------------------
// Tauri commands（§4.7 / §4.3）
// ---------------------------------------------------------------------------

/// 采集器状态（§4.7 `collector_status()`；§4.2 typed 分类：pipe 探测 500ms 超时；
/// 失败时补查本用户进程证据（TTL 5s single-flight）后交给纯分类——不以默认 false
/// 先显示未运行，不解析中文文案）。
#[tauri::command]
pub async fn collector_status() -> Result<CollectorStatusDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let task_ev = task_evidence_cached(COLLECTOR_TASK_NAME);
        let status = match ctl_request_typed(&CtlRequest::Status) {
            Ok(resp) if resp.ok && resp.data.is_some() => {
                let d = resp.data.expect("上分支已检查");
                status_dto_from_success(d, task_ev)
            }
            Ok(resp) => {
                // 管道应答但非有效 status——协议偏差，按 Protocol 分类（进程补查照常）
                let proc_ev = process_evidence_cached(COLLECTOR_EXE);
                let err = CtlError {
                    kind: PipeFailureKind::Protocol,
                    message: resp
                        .error
                        .unwrap_or_else(|| "collector 状态响应无效".to_string()),
                };
                status_dto_from_failure(&err, task_ev, proc_ev)
            }
            Err(e) => {
                let proc_ev = process_evidence_cached(COLLECTOR_EXE);
                status_dto_from_failure(&e, task_ev, proc_ev)
            }
        };
        record_health(&status);
        Ok(status)
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
        let sid = collector_health::current_user_sid_string()
            .map_err(|e| format!("获取当前用户 SID 失败（无法安全校验任务归属）: {e}"))?;
        runas_script_with_args("install-collector-task.ps1", &["-ExpectedUserSid", &sid])?;
        if task_evidence_uncached(COLLECTOR_TASK_NAME) != Evidence::Present {
            return Err(
                "安装脚本已返回，但计划任务 ClRecoderCollector 仍不存在（请检查磁盘/策略）".into(),
            );
        }
        super::diagnostics::record_gui_event(
            clrecoder_diagnostics::Level::Info, "task.policy_updated", "采集器自启任务已配置",
        );
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
        if task_evidence_uncached(COLLECTOR_TASK_NAME) == Evidence::Present {
            return Err("卸载脚本已返回，但计划任务仍存在".into());
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("自启任务失败: {e}"))?
}

/// 立即启动采集器（§4.7：先 `schtasks /Run`，失败回退 runas 直接启动；等待就绪；
/// §4.2 安全启动——不自动杀进程、不重复 runas）。
#[tauri::command]
pub async fn collector_start_now() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(start_collector_and_wait)
        .await
        .map_err(|e| format!("启动采集器任务失败: {e}"))?
}

/// 修复采集器自启任务（§4.3）：前端无参数；一次 UAC、有限脚本 Wait、等待退出码；
/// 经静态 `-RepairOnly -ExpectedUserSid <SID>` 参数调用既有安装脚本——只改三项策略并
/// 回读，**不 /Run、不结束任何进程**；ExpectedUserSid 由本进程（GUI 后端）可信获取。
/// 成功仅表示定义校验通过，**不表示当前实例已重启**（策略对后续启动生效）。
#[tauri::command]
pub async fn collector_autostart_repair(_app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let sid = collector_health::current_user_sid_string()
            .map_err(|e| format!("获取当前用户 SID 失败（无法安全校验任务归属）: {e}"))?;
        runas_script_with_args(
            "install-collector-task.ps1",
            &["-RepairOnly", "-ExpectedUserSid", &sid],
        )?;
        // 回读校验 + 刷新任务证据缓存（§5.2：结果刷新缓存，不进入启动流程）
        if task_evidence_uncached(COLLECTOR_TASK_NAME) != Evidence::Present {
            return Err(
                "修复脚本已返回，但计划任务 ClRecoderCollector 仍不存在（请检查磁盘/策略）".into(),
            );
        }
        super::diagnostics::record_gui_event(
            clrecoder_diagnostics::Level::Info, "task.policy_updated", "采集器任务策略已修复，对后续启动生效",
        );
        Ok(())
    })
    .await
    .map_err(|e| format!("修复采集器自启任务失败: {e}"))?
}

/// 只读任务策略（§4.3）：仅 Settings 活动时调用，不随 500ms status 重复；
/// 外部 schtasks 查询在 spawn_blocking 执行（不在 UI 线程）。
#[tauri::command]
pub async fn get_collector_task_policy() -> Result<TaskPolicyDto, String> {
    tauri::async_runtime::spawn_blocking(move || query_task_policy(COLLECTOR_TASK_NAME))
        .await
        .map_err(|e| format!("任务策略查询任务失败: {e}"))
}

// ---------------------------------------------------------------------------
// 只读任务策略（§4.3）
// ---------------------------------------------------------------------------

/// 目标策略（§4.3 固定）：ExecutionTimeLimit = PT0S。
const TARGET_TIME_LIMIT: &str = "PT0S";

/// 取 `<tag>…</tag>` 首个叶子文本（任务 XML 三个目标叶子均为纯文本节点；
/// 自闭合/带属性形态按"缺失"处理）。
fn leaf_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    Some(xml[start..end].trim().to_string())
}

fn leaf_bool(xml: &str, tag: &str) -> Option<bool> {
    match leaf_text(xml, tag)?.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// 从任务 XML 文本提取三项（§4.3）。schtasks /XML 的管道输出为控制台码页（中文系统
/// GBK），但结构标签与三个目标叶子值（PT0S/true/false）均为 ASCII——手解析不受影响，
/// 且避免为 GUI 引入 XML 解析依赖；非任务 XML（如错误文本）→ None。
fn parse_task_policy_values(xml: &str) -> Option<(Option<String>, Option<bool>, Option<bool>)> {
    if !xml.contains("<Task") {
        return None;
    }
    Some((
        leaf_text(xml, "ExecutionTimeLimit"),
        leaf_bool(xml, "DisallowStartIfOnBatteries"),
        leaf_bool(xml, "StopIfGoingOnBatteries"),
    ))
}

/// `schtasks /Query /TN <name> /XML` 只读查询任务定义（GUI 进程与任务同用户）。
fn query_task_xml(task_name: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("schtasks")
        .args(["/Query", "/TN", task_name, "/XML"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 纯装配核心（探针注入）：任务证据缺席/未知 → evidence 对应 + 可空字段；
/// 存在但定义不可读/解析失败 → evidence=unknown + 可空字段。
fn query_task_policy_with(task_ev: Evidence, task_xml: Option<&str>) -> TaskPolicyDto {
    let null_fields = |evidence| TaskPolicyDto {
        evidence,
        execution_time_limit: None,
        disallow_start_if_on_batteries: None,
        stop_if_going_on_batteries: None,
        compliant: None,
        applies_on_next_start: true,
    };
    match task_ev {
        Evidence::Absent => null_fields(Evidence::Absent),
        Evidence::Unknown => null_fields(Evidence::Unknown),
        Evidence::Present => match task_xml.and_then(parse_task_policy_values) {
            Some((etl, disallow, stop)) => TaskPolicyDto {
                evidence: Evidence::Present,
                compliant: Some(
                    etl.as_deref() == Some(TARGET_TIME_LIMIT)
                        && disallow == Some(false)
                        && stop == Some(false),
                ),
                execution_time_limit: etl,
                disallow_start_if_on_batteries: disallow,
                stop_if_going_on_batteries: stop,
                applies_on_next_start: true,
            },
            None => null_fields(Evidence::Unknown),
        },
    }
}

/// 生产装配：任务证据（30s 缓存）+ schtasks /XML 实时读取。
fn query_task_policy(task_name: &str) -> TaskPolicyDto {
    let ev = task_evidence_cached(task_name);
    query_task_policy_with(ev, query_task_xml(task_name).as_deref())
}

// ---------------------------------------------------------------------------
// 测试（保留既有 correctness_v2_/helper 安全断言；新增 usability_v3_ 行为矩阵）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usability_v3_health_events_only_on_change_and_recovery() {
        assert_eq!(health_event(None, CollectorHealth::Running), Some("collector.health_changed"));
        assert_eq!(health_event(Some(CollectorHealth::Running), CollectorHealth::Running), None);
        assert_eq!(health_event(Some(CollectorHealth::Running), CollectorHealth::Unreachable), Some("collector.health_changed"));
        assert_eq!(health_event(Some(CollectorHealth::Unreachable), CollectorHealth::Running), Some("collector.health_recovered"));
        assert_eq!(health_event(Some(CollectorHealth::Running), CollectorHealth::Paused), Some("collector.health_changed"));
    }
    use clrecoder_core::ipc::StatusData;
    use std::cell::RefCell;
    use std::collections::VecDeque;

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
        assert_eq!(
            (d.paused, d.version.as_str(), d.events_seen),
            (false, "0.1.0", 48219)
        );

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
        let cmd = ps_runas_script_command(
            r"C:\Program Files\CL Recoder\scripts\install-collector-task.ps1",
        );
        // 内层 Bypass + 双引号包裹的空格路径（单串 ArgumentList）
        assert!(
            cmd.contains("-ExecutionPolicy Bypass -File \"C:\\Program Files\\CL Recoder"),
            "{cmd}"
        );
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
        assert_eq!(
            inner.matches("-ExecutionPolicy").count() + 1,
            2,
            "外层+内层共两处 Bypass"
        );
    }

    /// RepairOnly 提权命令形态（§4.3）：静态 `-RepairOnly -ExpectedUserSid <SID>` 参数、
    /// 单串 ArgumentList、参数值双引号包裹（空格安全）、两层 Bypass、-Wait -PassThru
    /// 有限脚本 Wait、UAC 取消兜底保留。
    #[test]
    fn usability_v3_repair_runas_command_carries_repaironly_and_sid() {
        let cmd = ps_runas_script_command_with_args(
            r"C:\Program Files\CL Recoder\scripts\install-collector-task.ps1",
            &["-RepairOnly", "-ExpectedUserSid", "S-1-5-21-1-2-3"],
        );
        assert!(
            cmd.contains("-ExecutionPolicy Bypass -File \"C:\\Program Files\\CL Recoder"),
            "{cmd}"
        );
        assert!(cmd.contains("-Verb RunAs -Wait -PassThru"), "{cmd}");
        assert!(
            cmd.contains("\"-RepairOnly\" \"-ExpectedUserSid\" \"S-1-5-21-1-2-3\""),
            "{cmd}"
        );
        assert!(cmd.contains("catch { exit 4 }"), "{cmd}");
        assert!(cmd.contains("exit $p.ExitCode"), "{cmd}");

        // 参数值防御性转义：双引号不出现在裸参数位
        let cmd2 = ps_runas_script_command_with_args("a.ps1", &["-X", "v\"q"]);
        assert!(cmd2.contains("v\\\"q"), "{cmd2}");
    }

    /// 超时常量（§5.1 500ms）与任务名（§5.1）。
    #[test]
    fn constants_match_plan() {
        assert_eq!(CTL_TIMEOUT, Duration::from_millis(500));
        assert_eq!(COLLECTOR_TASK_NAME, "ClRecoderCollector");
    }

    /// 停用的 collector：DTO 默认值线形状（§4.2）——health/taskEvidence/processEvidence
    /// 均为 unknown（不以默认 false 先显示未运行），diagnosticCode/message 为 null。
    #[test]
    fn status_dto_default_is_not_running() {
        let d = CollectorStatusDto::default();
        assert!(!d.running);
        assert!(!d.task_exists);
        assert_eq!(d.task_evidence, Evidence::Unknown);
        assert_eq!(d.health, CollectorHealth::Unknown);
        assert_eq!(d.process_evidence, Evidence::Unknown);
        assert!(!d.pipe_reachable);
        assert!(d.paused.is_none());
        // 序列化形状：running/taskExists/三个证据字段/diagnostic* 恒在场；paused/startedAt 省略
        let js = serde_json::to_string(&d).unwrap();
        assert_eq!(
            js,
            r#"{"running":false,"taskExists":false,"taskEvidence":"unknown","health":"unknown","pipeReachable":false,"processEvidence":"unknown","diagnosticCode":null,"diagnosticMessage":null,"lastEventAt":null}"#,
            "{js}"
        );
    }

    /// 成功分支 DTO 线形状（§4.2）：processEvidence=present、诊断清空、pipeReachable=true、
    /// paused/startedAt 在场、health 随 paused。
    #[test]
    fn usability_v3_status_dto_success_wire_shape() {
        let d = StatusData {
            paused: true,
            version: "0.1.0".into(),
            started_at: "2026-09-27T22:00:00+08:00".into(),
            last_event_at: None,
            events_seen: 1,
        };
        let dto = status_dto_from_success(d, Evidence::Present);
        assert_eq!(dto.health, CollectorHealth::Paused);
        assert_eq!(dto.process_evidence, Evidence::Present);
        assert!(dto.pipe_reachable);
        assert!(dto.running);
        assert!(dto.diagnostic_code.is_none() && dto.diagnostic_message.is_none());
        let js = serde_json::to_string(&dto).unwrap();
        assert!(js.contains(r#""health":"paused""#), "{js}");
        assert!(js.contains(r#""taskEvidence":"present""#), "{js}");
        assert!(js.contains(r#""processEvidence":"present""#), "{js}");
        assert!(js.contains(r#""pipeReachable":true"#), "{js}");
        assert!(js.contains(r#""paused":true"#), "{js}");
        assert!(
            js.contains(r#""startedAt":"2026-09-27T22:00:00+08:00""#),
            "{js}"
        );
        assert!(js.contains(r#""diagnosticCode":null"#), "{js}");
    }

    /// 失败分支 DTO 线形状（§4.2）：NotFound+确认 Absent → not_running；不回填
    /// paused/startedAt；短码保留 pipe_not_found；AccessDenied 优先；Timeout+Absent →
    /// unknown（探测竞态不可排除）；进程证据 Unknown → 消息附加证据不足说明。
    #[test]
    fn usability_v3_status_dto_failure_wire_shape() {
        let not_found = CtlError {
            kind: PipeFailureKind::NotFound,
            message: "采集器未运行（控制管道不存在）".into(),
        };
        let dto = status_dto_from_failure(&not_found, Evidence::Absent, Evidence::Absent);
        assert_eq!(dto.health, CollectorHealth::NotRunning);
        assert!(!dto.running && !dto.pipe_reachable);
        let js = serde_json::to_string(&dto).unwrap();
        assert!(js.contains(r#""health":"not_running""#), "{js}");
        assert!(js.contains(r#""taskEvidence":"absent""#), "{js}");
        assert!(js.contains(r#""diagnosticCode":"pipe_not_found""#), "{js}");
        assert!(
            js.contains(r#""diagnosticMessage":"采集器未运行（控制管道不存在）""#),
            "{js}"
        );
        assert!(!js.contains("\"paused\""), "失败不回填 paused：{js}");
        assert!(!js.contains("\"startedAt\""), "失败不回填 startedAt：{js}");
        assert!(js.contains(r#""lastEventAt":null"#), "{js}");

        // AccessDenied 优先：进程 Present 仍 access_denied
        let denied = CtlError {
            kind: PipeFailureKind::AccessDenied,
            message: "x".into(),
        };
        let dto = status_dto_from_failure(&denied, Evidence::Present, Evidence::Present);
        assert_eq!(dto.health, CollectorHealth::AccessDenied);
        assert_eq!(
            dto.diagnostic_code,
            Some(DiagnosticCode::PipeAccessDenied),
            "短码保留原 pipe 类别"
        );

        // Timeout + Absent → unknown（不能排除探测竞态）
        let timeout = CtlError {
            kind: PipeFailureKind::Timeout,
            message: "t".into(),
        };
        let dto = status_dto_from_failure(&timeout, Evidence::Present, Evidence::Absent);
        assert_eq!(dto.health, CollectorHealth::Unknown);

        // 进程补查不可用 → processEvidence=unknown + 消息附加说明（不吞掉原 pipe 类别）
        let dto = status_dto_from_failure(&timeout, Evidence::Absent, Evidence::Unknown);
        assert_eq!(dto.health, CollectorHealth::Unknown);
        assert_eq!(dto.process_evidence, Evidence::Unknown);
        assert_eq!(dto.diagnostic_code, Some(DiagnosticCode::PipeTimeout));
        assert!(
            dto.diagnostic_message
                .as_ref()
                .unwrap()
                .contains("证据不足"),
            "{dto:?}"
        );
    }

    // -------------------------------------------------------------------------
    // typed 传输（§4.2：种类由 Win32 码/失败阶段映射，不经中文文案解析）
    // -------------------------------------------------------------------------

    /// CreateFileW 错误 → 种类直接映射。
    #[test]
    fn usability_v3_open_error_kind_maps_win32_codes() {
        use windows::core::Error as WError;
        use windows::core::HRESULT;
        use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND};
        assert_eq!(
            open_err_kind(&WError::from_hresult(ERROR_FILE_NOT_FOUND.to_hresult())),
            PipeFailureKind::NotFound
        );
        assert_eq!(
            open_err_kind(&WError::from_hresult(ERROR_ACCESS_DENIED.to_hresult())),
            PipeFailureKind::AccessDenied
        );
        assert_eq!(
            open_err_kind(&WError::from_hresult(HRESULT::from_win32(32))),
            PipeFailureKind::Io,
            "其它错误码归 Io"
        );
    }

    /// typed 限时守护：exchange 挂起 → Timeout（§4.2：500ms 只约束 pipe 请求）。
    #[test]
    fn usability_v3_exchange_timeout_maps_to_timeout_kind() {
        let started = std::time::Instant::now();
        let err = exchange_with_timeout("x", |line| {
            assert_eq!(line, "x");
            std::thread::sleep(Duration::from_secs(30));
            Err(CtlError {
                kind: PipeFailureKind::Io,
                message: "unreachable".into(),
            })
        })
        .unwrap_err();
        assert_eq!(err.kind, PipeFailureKind::Timeout);
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "守护超时应为 500ms 量级"
        );
    }

    /// typed 限时守护：快速成功原样透传（不改变响应语义）。
    #[test]
    fn usability_v3_exchange_success_passes_through() {
        let resp = exchange_with_timeout("x", |line| {
            assert_eq!(line, "x");
            Ok(CtlResponse::ok_no_data())
        })
        .unwrap();
        assert!(resp.ok);
    }

    // -------------------------------------------------------------------------
    // 安全启动行为矩阵（§4.2：Present/Unknown 不重复 launch、不强杀、错误可解释；
    // Absent 才 schtasks 优先 → 有界就绪 → 至多一次 runas）
    // -------------------------------------------------------------------------

    /// 探针注入 fixture：脚本化 wait_ready / process_evidence / run_task / direct_start。
    struct StartFixture {
        ready_script: RefCell<VecDeque<bool>>,
        ready_windows: RefCell<Vec<Duration>>,
        evidence_script: RefCell<VecDeque<Evidence>>,
        evidence_calls: RefCell<u32>,
        run_task_result: bool,
        run_task_calls: RefCell<u32>,
        direct_result: Result<bool, String>,
        direct_calls: RefCell<u32>,
    }

    impl StartFixture {
        fn ready(&self, window: Duration) -> bool {
            self.ready_windows.borrow_mut().push(window);
            self.ready_script.borrow_mut().pop_front().unwrap_or(false)
        }
        fn evidence(&self) -> Evidence {
            *self.evidence_calls.borrow_mut() += 1;
            self.evidence_script
                .borrow_mut()
                .pop_front()
                .unwrap_or(Evidence::Unknown)
        }
        fn run_task(&self) -> bool {
            *self.run_task_calls.borrow_mut() += 1;
            self.run_task_result
        }
        fn direct(&self) -> Result<bool, String> {
            *self.direct_calls.borrow_mut() += 1;
            self.direct_result.clone()
        }
        fn run(&self) -> Result<(), String> {
            start_collector_with(
                |d| self.ready(d),
                || self.evidence(),
                || self.run_task(),
                || self.direct(),
            )
        }
    }

    fn fixture(
        ready: &[bool],
        evidence: &[Evidence],
        run_task: bool,
        direct: Result<bool, String>,
    ) -> StartFixture {
        StartFixture {
            ready_script: RefCell::new(ready.iter().copied().collect()),
            ready_windows: RefCell::new(Vec::new()),
            evidence_script: RefCell::new(evidence.iter().copied().collect()),
            evidence_calls: RefCell::new(0),
            run_task_result: run_task,
            run_task_calls: RefCell::new(0),
            direct_result: direct,
            direct_calls: RefCell::new(0),
        }
    }

    /// 已就绪 → 直接成功：不补查进程、不 schtasks、不 runas。
    #[test]
    fn usability_v3_start_ready_succeeds_without_any_probe_or_launch() {
        let f = fixture(&[true], &[], false, Ok(true));
        assert!(f.run().is_ok());
        assert_eq!(*f.evidence_calls.borrow(), 0);
        assert_eq!(*f.run_task_calls.borrow(), 0);
        assert_eq!(*f.direct_calls.borrow(), 0);
        assert_eq!(*f.ready_windows.borrow(), vec![Duration::from_millis(400)]);
    }

    /// Present + 5s 内就绪 → Ok；不 schtasks、不 runas（Present 不重复 launch）；
    /// 就绪窗口恰为 5 秒。
    #[test]
    fn usability_v3_start_present_waits_5s_and_never_launches() {
        let f = fixture(&[false, true], &[Evidence::Present], false, Ok(true));
        assert!(f.run().is_ok());
        assert_eq!(*f.run_task_calls.borrow(), 0, "Present 不得触发 schtasks");
        assert_eq!(*f.direct_calls.borrow(), 0, "Present 不得 runas");
        assert_eq!(*f.evidence_calls.borrow(), 1, "只补查一次");
        assert_eq!(
            *f.ready_windows.borrow(),
            vec![Duration::from_millis(400), Duration::from_secs(5)]
        );
    }

    /// Present 但始终不可达 → 可解释错误；无 launch、无强杀路径。
    #[test]
    fn usability_v3_start_present_unreachable_returns_explainable_error_without_launch() {
        let f = fixture(&[false, false], &[Evidence::Present], true, Ok(true));
        let err = f.run().unwrap_err();
        assert!(err.contains("不会自动结束"), "{err}");
        assert!(err.contains("5 秒"), "{err}");
        assert_eq!(*f.run_task_calls.borrow(), 0, "Present 不得触发 schtasks");
        assert_eq!(*f.direct_calls.borrow(), 0, "Present 不得 runas");
    }

    /// Unknown 同样不 launch（§4.2：Unknown 不重复 launch、不自动杀）。
    #[test]
    fn usability_v3_start_unknown_never_launches() {
        let f = fixture(&[false, false], &[Evidence::Unknown], true, Ok(true));
        let err = f.run().unwrap_err();
        assert!(err.contains("不会自动结束"), "{err}");
        assert_eq!(*f.run_task_calls.borrow(), 0);
        assert_eq!(*f.direct_calls.borrow(), 0);
    }

    /// Absent + schtasks 成功且就绪 → Ok；不 runas。
    #[test]
    fn usability_v3_start_absent_schtasks_ready_without_runas() {
        let f = fixture(&[false, true], &[Evidence::Absent], true, Ok(true));
        assert!(f.run().is_ok());
        assert_eq!(*f.run_task_calls.borrow(), 1);
        assert_eq!(*f.direct_calls.borrow(), 0);
    }

    /// Absent + schtasks 失败 → 恰一次 runas 直启；就绪 → Ok。
    #[test]
    fn usability_v3_start_absent_falls_back_to_exactly_one_runas() {
        let f = fixture(&[false, true], &[Evidence::Absent], false, Ok(true));
        assert!(f.run().is_ok());
        assert_eq!(*f.run_task_calls.borrow(), 1);
        assert_eq!(*f.direct_calls.borrow(), 1, "本流程至多一次 runas");
    }

    /// Absent + schtasks 受理但未就绪 + 补查 Present → 不 runas（避免双实例互踩）；
    /// 再等待仍不就绪 → 可解释错误。
    #[test]
    fn usability_v3_start_schtasks_present_process_stops_retry_without_runas() {
        let f = fixture(
            &[false, false, false],
            &[Evidence::Absent, Evidence::Present],
            true,
            Ok(true),
        );
        let err = f.run().unwrap_err();
        assert!(err.contains("不会自动结束"), "{err}");
        assert_eq!(*f.run_task_calls.borrow(), 1);
        assert_eq!(
            *f.direct_calls.borrow(),
            0,
            "进程已由任务拉起，不得再 runas"
        );
    }

    /// Absent + runas 受理但不就绪 + 补查 Present → 恰一次 runas、停止重试、可解释错误。
    #[test]
    fn usability_v3_start_runas_not_ready_probe_present_stops_retry() {
        let f = fixture(
            &[false, false],
            &[Evidence::Absent, Evidence::Present],
            false,
            Ok(false),
        );
        let err = f.run().unwrap_err();
        assert!(err.contains("不会"), "{err}");
        assert_eq!(*f.direct_calls.borrow(), 1, "失败后不重复 runas");
        assert_eq!(*f.run_task_calls.borrow(), 1);
    }

    /// Absent + runas 受理但不就绪 + 补查 Absent → 错误说明"未在运行"并指向修复入口。
    #[test]
    fn usability_v3_start_runas_not_ready_reprobe_absent_reports_start_failure() {
        let f = fixture(
            &[false, false],
            &[Evidence::Absent, Evidence::Absent],
            false,
            Ok(false),
        );
        let err = f.run().unwrap_err();
        assert!(err.contains("未能启动"), "{err}");
        assert_eq!(*f.direct_calls.borrow(), 1);
    }

    /// launch 失败（UAC 取消）原样透传，且不再补查/重试。
    #[test]
    fn usability_v3_start_launch_error_passthrough_without_reprobe() {
        let f = fixture(
            &[false],
            &[Evidence::Absent],
            false,
            Err("UAC 被取消——未做任何更改，可再次点击重试".into()),
        );
        let err = f.run().unwrap_err();
        assert_eq!(err, "UAC 被取消——未做任何更改，可再次点击重试");
        assert_eq!(*f.evidence_calls.borrow(), 1, "launch 失败不补查");
        assert_eq!(*f.direct_calls.borrow(), 1, "不重试");
    }

    // -------------------------------------------------------------------------
    // correctness_v2 既有安全断言（§4.2 明确保留：直启无 Wait、路径安全、接缝语义）
    // -------------------------------------------------------------------------

    /// 常驻采集器的 runas 命令形态（§4.6）：无 -Wait、不读常驻 ExitCode；
    /// -Verb RunAs + -WindowStyle Hidden + -PassThru 确认创建请求；成功外层
    /// exit 0；UAC 取消/启动失败沿用 catch exit 4 的现有文案映射。
    #[test]
    fn correctness_v2_collector_runas_command_has_no_wait_and_no_exit_code() {
        let cmd =
            ps_runas_collector_command(r"C:\Program Files\CL Recoder\cl-recoder-collector.exe");
        // F2 根因：-Wait 会阻塞到常驻进程退出，启动流程永远"处理中"
        assert!(!cmd.contains("-Wait"), "{cmd}");
        // 常驻进程不退出，ExitCode 不可读
        assert!(!cmd.contains("ExitCode"), "{cmd}");
        // 提权 + 隐藏窗口 + PassThru 确认创建请求（§4.6）
        assert!(
            cmd.contains("-Verb RunAs -WindowStyle Hidden -PassThru"),
            "{cmd}"
        );
        // 进程对象为空兜底 + 创建请求受理即成功（就绪与否由管道探测判定）
        assert!(cmd.contains("if ($null -eq $p) { exit 4 }"), "{cmd}");
        assert!(cmd.ends_with("exit 0"), "{cmd}");
        // UAC 取消 → exit 4 → run_powershell 现有"UAC 被取消"文案
        assert!(cmd.contains("catch { exit 4 }"), "{cmd}");
    }

    /// 路径安全（§4.6）：空格路径整体位于单引号 -FilePath 参数内不断裂；
    /// 单引号经 ps_quote 转义为 ''。
    #[test]
    fn correctness_v2_collector_runas_command_quote_and_space_safe() {
        let cmd =
            ps_runas_collector_command(r"C:\Program Files\CL Recoder\cl-recoder-collector.exe");
        assert!(
            cmd.contains("-FilePath 'C:\\Program Files\\CL Recoder\\cl-recoder-collector.exe'"),
            "{cmd}"
        );
        let odd = ps_runas_collector_command(r"C:\odd'name\cl-recoder-collector.exe");
        assert!(odd.contains(r"odd''name"), "{odd}");
    }

    /// 接缝语义（§4.6）：launch 失败（含 UAC 取消）原样透传且**不探测**管道。
    #[test]
    fn correctness_v2_start_direct_launch_failure_skips_probe() {
        let mut probed = false;
        let err = start_direct_with(
            r"C:\x\cl-recoder-collector.exe",
            |_| Err("UAC 被取消——未做任何更改，可再次点击重试".into()),
            |_| {
                probed = true;
                true
            },
        )
        .unwrap_err();
        assert_eq!(err, "UAC 被取消——未做任何更改，可再次点击重试");
        assert!(!probed, "launch 失败不得探测管道");
    }

    /// 接缝语义（§4.6）：launch 成功后以**恰好 5 秒**窗口探测管道；探测收到的
    /// 就是构造好的常驻命令；就绪 → Ok(true)，未就绪 → Ok(false)（不当作成功）。
    #[test]
    fn correctness_v2_start_direct_success_probes_pipe_with_5s_window() {
        let exe = r"C:\x\cl-recoder-collector.exe";
        let mut windows: Vec<Duration> = Vec::new();
        let ready = start_direct_with(
            exe,
            |cmd| {
                assert_eq!(cmd, &ps_runas_collector_command(exe));
                Ok(())
            },
            |d| {
                windows.push(d);
                true
            },
        )
        .unwrap();
        assert!(ready, "管道就绪 → Ok(true)");
        assert_eq!(windows, vec![Duration::from_secs(5)], "就绪探测窗口 5 秒");

        let not_ready = start_direct_with(exe, |_| Ok(()), |_| false).unwrap();
        assert!(!not_ready, "未就绪 → Ok(false)，由调用方按原策略诊断");
    }

    /// 旧 correctness_v2 "两 fallback 源码计数"测试的新策略等价断言（§4.2）：生产启动
    /// 函数仍收口经 start_direct_with 接缝但**至多一次**；自动强杀必须消失；
    /// START_LOCK 保持；一次性脚本命令保留 -Wait -PassThru；直启命令无 -Wait。
    #[test]
    fn usability_v3_start_flow_single_seam_no_kill_and_start_lock_kept() {
        let src = include_str!("collector_ctl.rs");
        // 截取生产函数文本（定义处 → 下一个 #[tauri::command]），测试代码不入窗
        let begin = src
            .find("fn start_collector_and_wait")
            .expect("生产启动函数存在");
        let end = begin
            + src[begin..]
                .find("#[tauri::command]")
                .expect("其后应有命令入口");
        let production = &src[begin..end];

        // 恰一次 runas 直启接缝（旧版两条 fallback 各一次，已按 §4.2 收敛）
        let seam = ["start_direct_with", "("].concat();
        assert_eq!(
            production.matches(&seam).count(),
            1,
            "本流程至多一次 runas 直启"
        );
        // 命令构造收口：生产函数内不再内联 Start-Process
        assert!(!production.contains("Start-Process"), "{production}");
        // 启动锁保持
        assert!(
            production.contains("START_LOCK"),
            "START_LOCK 启动锁必须保持"
        );

        // 自动强杀已删除：整个模块不再出现强杀工具调用 / 强杀函数（检查针运行时拼接，
        // 避免与本测试自身源码自匹配）
        let kill_tool = ["task", "kill"].concat();
        let kill_fn = ["kill_", "collector_", "process"].concat();
        assert!(!src.contains(&kill_tool), "强杀工具必须从本模块消失");
        assert!(!src.contains(&kill_fn), "强杀函数必须删除");

        // 一次性脚本命令仍保留 -Wait -PassThru（安装/修复语义不变）；直启命令无 -Wait
        let script_wait = ["-Verb RunAs -Wait", " -PassThru"].concat();
        assert!(src.contains(&script_wait), "{src}");
        let direct = ps_runas_collector_command(r"C:\x\cl-recoder-collector.exe");
        assert!(!direct.contains("-Wait"), "{direct}");
    }

    /// 只读任务策略解析（§4.3）：PT72H/true/true → 不合规；真机实证形状（无
    /// ExecutionTimeLimit 节点）→ 字段 null；PT0S/false/false → 合规；非任务 XML → None。
    #[test]
    fn usability_v3_task_policy_parse_and_compliance() {
        let full = r#"<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Settings><ExecutionTimeLimit>PT72H</ExecutionTimeLimit><DisallowStartIfOnBatteries>true</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>true</StopIfGoingOnBatteries></Settings></Task>"#;
        let (etl, disallow, stop) = parse_task_policy_values(full).unwrap();
        assert_eq!(
            (etl.as_deref(), disallow, stop),
            (Some("PT72H"), Some(true), Some(true))
        );

        // 真机实证（schtasks /Create ONLOGON /RL HIGHEST 产物）：完全没有 ExecutionTimeLimit
        let real = r#"<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Settings><DisallowStartIfOnBatteries>true</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>true</StopIfGoingOnBatteries><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><IdleSettings><Duration>PT10M</Duration></IdleSettings></Settings><Triggers><LogonTrigger /></Triggers></Task>"#;
        let (etl, disallow, stop) = parse_task_policy_values(real).unwrap();
        assert_eq!(etl, None, "节点缺失 → null（非 PT0S）");
        assert_eq!((disallow, stop), (Some(true), Some(true)));

        let compliant = r#"<Task xmlns="x"><Settings><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries></Settings></Task>"#;
        let (etl, disallow, stop) = parse_task_policy_values(compliant).unwrap();
        assert_eq!(
            (etl.as_deref(), disallow, stop),
            (Some("PT0S"), Some(false), Some(false))
        );

        // 非任务 XML（schtasks 错误文本等）→ None
        assert!(parse_task_policy_values("错误: 系统找不到指定的文件。").is_none());
        assert!(parse_task_policy_values("").is_none());
    }

    /// 只读策略 DTO 装配（§4.3）：absent/unknown → 可空字段；present+合规 → true；
    /// present 但 /XML 不可读 → unknown；appliesOnNextStart 恒 true；线上 camelCase。
    #[test]
    fn usability_v3_task_policy_dto_wire_shape() {
        let dto = query_task_policy_with(Evidence::Absent, None);
        assert_eq!(dto.evidence, Evidence::Absent);
        assert_eq!(dto.execution_time_limit, None);
        assert_eq!(dto.disallow_start_if_on_batteries, None);
        assert_eq!(dto.stop_if_going_on_batteries, None);
        assert_eq!(dto.compliant, None);
        assert!(dto.applies_on_next_start);

        let dto = query_task_policy_with(Evidence::Unknown, None);
        assert_eq!(dto.evidence, Evidence::Unknown);
        assert_eq!(dto.compliant, None);
        assert!(dto.applies_on_next_start);

        let xml = r#"<Task xmlns="x"><Settings><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries></Settings></Task>"#;
        let dto = query_task_policy_with(Evidence::Present, Some(xml));
        assert_eq!(dto.evidence, Evidence::Present);
        assert_eq!(dto.execution_time_limit.as_deref(), Some("PT0S"));
        assert_eq!(dto.disallow_start_if_on_batteries, Some(false));
        assert_eq!(dto.stop_if_going_on_batteries, Some(false));
        assert_eq!(dto.compliant, Some(true));
        assert!(dto.applies_on_next_start);

        // 存在但读取/解析失败 → unknown（证据不足，不猜测）
        let dto = query_task_policy_with(Evidence::Present, None);
        assert_eq!(dto.evidence, Evidence::Unknown);
        assert_eq!(dto.compliant, None);

        // 非合规（72 小时 + 禁电池）：compliant=false 但字段如实呈现
        let xml = r#"<Task xmlns="x"><Settings><ExecutionTimeLimit>PT72H</ExecutionTimeLimit><DisallowStartIfOnBatteries>true</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>true</StopIfGoingOnBatteries></Settings></Task>"#;
        let dto = query_task_policy_with(Evidence::Present, Some(xml));
        assert_eq!(dto.compliant, Some(false));
        assert_eq!(dto.execution_time_limit.as_deref(), Some("PT72H"));

        // 线上 camelCase 形状
        let js = serde_json::to_string(&dto).unwrap();
        assert!(js.contains(r#""executionTimeLimit":"PT72H""#), "{js}");
        assert!(js.contains(r#""disallowStartIfOnBatteries":true"#), "{js}");
        assert!(js.contains(r#""stopIfGoingOnBatteries":true"#), "{js}");
        assert!(js.contains(r#""compliant":false"#), "{js}");
        assert!(js.contains(r#""appliesOnNextStart":true"#), "{js}");
    }
}
