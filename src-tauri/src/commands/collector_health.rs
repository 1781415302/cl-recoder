//! collector_health —— 纯健康分类与进程/任务证据的有界探针（usability-runtime-v3 §4.2）。
//!
//! - **类型合同**：[`Evidence`]（present/absent/unknown）、[`CollectorHealth`]（线上
//!   snake_case）、[`PipeFailureKind`]、[`PipeProbe`]、稳定短码 [`DiagnosticCode`] 与
//!   纯函数 [`classify_health`]——分类只吃**类型化探测结果**，绝不解析中文错误文案
//!   （§4.2："不能通过中文字符串解析错误种类"）；
//! - **进程证据**（仅在 pipe 失败时补查，§4.2）：当前用户 SID（复用 collector
//!   ipc_server 的 Win32 Security API 字节/对齐合同）→ 有界 PowerShell + CIM 探针把
//!   候选镜像的 owner SID 分为 own/foreign/unknown 计数：own>0 → Present，无 own 有
//!   unknown → Unknown，确认全无本用户且无 unknown → Absent——**别账户同名进程不算
//!   本用户实例**。内部探针 JSON 固定 `{queried,ownCount,foreignCount,unknownCount}`；
//!   PowerShell 子进程限时 1500ms、超时只结束本次自建探针 child、stdout 32KiB 上限、
//!   stderr 不参与任何解析；证据 TTL 5 秒 + single-flight（禁止 500ms 一轮进程扫描）；
//! - **任务证据**：schtasks /Query 廉价探测（exit 0 = 确认存在），非 0 时再用有界
//!   PowerShell + COM 探针区分"确认不存在（0x80070002）/ 无法判定"；TTL 30 秒沿用
//!   既有缓存机制（uncached 供写路径回读校验）。
//!
//! 禁止（§2.1）：解析中文错误判断类别、结束 collector 进程——探针限时只杀探针 child。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 类型合同（§4.2）
// ---------------------------------------------------------------------------

/// 进程/任务证据强度（§4.2：serde snake_case；默认 Unknown——不以默认 false 先显示未运行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// 证据成立（进程：存在本用户 owner 实例；任务：schtasks 确认存在）
    Present,
    /// 确认不存在（进程：确认全无本用户且无未知归属；任务：0x80070002）
    Absent,
    /// 证据不足（探测竞态 / 补查不可用 / 归属未知）
    #[default]
    Unknown,
}

/// 采集器健康分类（§4.2：serde snake_case；默认 Unknown）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CollectorHealth {
    /// 本次成功取得有效 status 且未暂停
    Running,
    /// 本次成功取得有效 status 且处于暂停
    Paused,
    /// pipe NotFound 且确认本用户进程 Absent
    NotRunning,
    /// 任意 pipe 失败但确认本用户进程 Present（AccessDenied 优先于此）
    Unreachable,
    /// pipe 拒绝访问——不宣称未运行
    AccessDenied,
    /// 证据不足（默认；Timeout/Busy/Io/Protocol 与 Absent 组合亦归此——不能排除探测竞态）
    #[default]
    Unknown,
}

/// pipe 失败种类（由 collector_ctl 的 typed transport 按 Win32 码/失败阶段直接映射产生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeFailureKind {
    /// 管道不存在（ERROR_FILE_NOT_FOUND）
    NotFound,
    /// 拒绝访问（ERROR_ACCESS_DENIED）
    AccessDenied,
    /// 管道忙且等待超时（ERROR_PIPE_BUSY）
    Busy,
    /// 500ms 守护超时
    Timeout,
    /// 其它 I/O 失败（连接/读写/模式设置）
    Io,
    /// 响应不完整 / 超长 / 非 UTF-8 / JSON 解析失败
    Protocol,
    /// 守护 worker（线程）创建失败
    WorkerUnavailable,
}

/// pipe 探测结果（[`classify_health`] 的输入；typed transport 归 collector_ctl）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeProbe {
    /// 本次成功取得有效 status（paused 为采集器报告的暂停态）
    Status { paused: bool },
    /// pipe 失败（带种类）
    Failed(PipeFailureKind),
}

/// 稳定诊断短码（§4.2：诊断文字可改善，短码不改；serde snake_case 线上形状）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    PipeNotFound,
    PipeAccessDenied,
    PipeBusy,
    PipeTimeout,
    PipeIo,
    PipeProtocol,
    /// WorkerUnavailable（worker 不可用）对应的短码
    ProbeUnavailable,
}

impl PipeFailureKind {
    /// pipe 失败短码（§4.2：WorkerUnavailable → probe_unavailable）。
    pub fn diagnostic_code(self) -> DiagnosticCode {
        match self {
            Self::NotFound => DiagnosticCode::PipeNotFound,
            Self::AccessDenied => DiagnosticCode::PipeAccessDenied,
            Self::Busy => DiagnosticCode::PipeBusy,
            Self::Timeout => DiagnosticCode::PipeTimeout,
            Self::Io => DiagnosticCode::PipeIo,
            Self::Protocol => DiagnosticCode::PipeProtocol,
            Self::WorkerUnavailable => DiagnosticCode::ProbeUnavailable,
        }
    }
}

/// 纯健康分类（§4.2 规则表）：
/// - 有效 status → running/paused（无需进程补查）；
/// - AccessDenied → access_denied（优先级最高，不宣称未运行）；
/// - 任意失败且本用户进程 Present → unreachable；
/// - NotFound 且确认 Absent → not_running；
/// - Timeout/Busy/Io/Protocol 与 Absent 组合 → unknown（不能排除探测竞态）；
/// - 进程证据 Unknown → unknown（AccessDenied 仍优先）。
pub fn classify_health(pipe: PipeProbe, process: Evidence) -> CollectorHealth {
    match pipe {
        PipeProbe::Status { paused } => {
            if paused {
                CollectorHealth::Paused
            } else {
                CollectorHealth::Running
            }
        }
        PipeProbe::Failed(PipeFailureKind::AccessDenied) => CollectorHealth::AccessDenied,
        PipeProbe::Failed(kind) => match process {
            Evidence::Present => CollectorHealth::Unreachable,
            Evidence::Absent => {
                if kind == PipeFailureKind::NotFound {
                    CollectorHealth::NotRunning
                } else {
                    CollectorHealth::Unknown
                }
            }
            Evidence::Unknown => CollectorHealth::Unknown,
        },
    }
}

// ---------------------------------------------------------------------------
// 当前用户 SID（复用 collector ipc_server 的 Win32 Security API 字节/对齐合同）
// ---------------------------------------------------------------------------

/// 从已打开的进程令牌提取用户 SID 字符串（形如 `S-1-5-21-…`）。
///
/// # Safety
/// `token` 必须是带 `TOKEN_QUERY` 权限的有效令牌句柄。
unsafe fn token_user_sid_string(
    token: windows::Win32::Foundation::HANDLE,
) -> windows::core::Result<String> {
    use windows::core::{HRESULT, PWSTR};
    use windows::Win32::Foundation::{
        LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_UNICODE_TRANSLATION, E_FAIL, HLOCAL,
    };
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_USER};

    let mut len = 0u32;
    // SAFETY: token 有效；首传空缓冲仅为探测所需长度（预期 ERROR_INSUFFICIENT_BUFFER 并回填 len）。
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
    }
    if len == 0 {
        return Err(windows::core::Error::from_hresult(HRESULT::from_win32(
            ERROR_INSUFFICIENT_BUFFER.0,
        )));
    }
    // 以 u64 分配保证 8 字节对齐（TOKEN_USER 含指针成员），避免对齐问题（ipc_server 同款合同）
    let mut buf = vec![0u64; len as usize / 8 + 1];
    // SAFETY: buf 按 len 分配且在调用期间存活；TOKEN_USER 由系统写入 buf。
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )?;
    }
    // SAFETY: buf 刚被 GetTokenInformation 以 TOKEN_USER 布局写入，且容量覆盖整个结构、对齐充分。
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut pwstr = PWSTR::null();
    // SAFETY: user.User.Sid 指向 buf 内的有效 SID；pwstr 由系统分配，下方读取后 LocalFree。
    unsafe {
        ConvertSidToStringSidW(user.User.Sid, &mut pwstr)?;
    }
    if pwstr.is_null() {
        return Err(windows::core::Error::from_hresult(E_FAIL));
    }
    // SAFETY: pwstr 由 ConvertSidToStringSidW 分配为合法以 NUL 结尾的 UTF-16 串。
    let sid = unsafe { pwstr.to_string() };
    // SAFETY: pwstr 是 ConvertSidToStringSidW 分配的 LocalAlloc 内存，须以 LocalFree 归还。
    unsafe {
        LocalFree(Some(HLOCAL(pwstr.0.cast())));
    }
    sid.map_err(|_| {
        windows::core::Error::from_hresult(HRESULT::from_win32(ERROR_NO_UNICODE_TRANSLATION.0))
    })
}

/// 取当前进程令牌的用户 SID 字符串（形如 `S-1-5-21-…`）；失败返回中文错误说明。
pub fn current_user_sid_string() -> Result<String, String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess 返回伪句柄；token 由系统写出，本轮调用内关闭。
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| format!("打开当前进程令牌失败: {e}"))?;
    }
    // SAFETY: token 刚由 OpenProcessToken 成功打开，本轮调用内有效。
    let result = unsafe { token_user_sid_string(token) };
    // SAFETY: token 在上方成功打开，此处关闭。
    unsafe {
        let _ = CloseHandle(token);
    }
    result.map_err(|e| format!("解析当前用户 SID 失败: {e}"))
}

// ---------------------------------------------------------------------------
// 有界探针基础设施（进程证据 + 任务证据二段探测共用）
// ---------------------------------------------------------------------------

/// 探针子进程限时（§4.2：1500ms；超时只结束本次自建探针 child）。
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// 探针 stdout 读取上限（§4.2：32KiB；超限输出不参与解析）。
pub(crate) const PROBE_STDOUT_CAP: usize = 32 * 1024;

/// 有界探针通用 runner（spawn 注入接缝——单测注入假命令，禁止真 CIM/COM 进单测矩阵）：
/// spawn → 限时等待退出 → stdout 上限读取。生成失败/超时/读取中断 → `None`；
/// stderr 直接遗弃（`Stdio::null()`，不参与语言解析）。
fn run_bounded_probe_with(
    spawn: impl FnOnce() -> std::io::Result<std::process::Child>,
    timeout: Duration,
) -> Option<String> {
    use std::io::Read;

    let mut child = spawn().ok()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return None;
    };
    // 读取线程：上限 32KiB，child 退出（管道 EOF）后必然返回；探针超时被杀时同样返回
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let reader = std::thread::Builder::new()
        .name("clrecoder-probe-read".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let mut limited = stdout.take(PROBE_STDOUT_CAP as u64);
            let _ = limited.read_to_end(&mut buf);
            let _ = tx.send(buf);
        })
        .ok()?;

    // 限时等待探针 child 自行退出（try_wait 轮询，不阻塞管道）
    let deadline = Instant::now() + timeout;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break false,
        }
    };
    if !exited {
        // 只结束本次自建探针 child（绝不触碰 collector 进程，§2.1）
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    // child 已退出：pipe 写端关闭，读取线程必达；2s 宽限兜底防悬挂
    let output = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    drop(reader);
    Some(String::from_utf8_lossy(&output).into_owned())
}

/// 生成有界探针 PowerShell 子进程（stderr 遗弃、stdin 关闭、-NonInteractive 防悬挂）。
fn spawn_powershell_probe(script: &str) -> std::io::Result<std::process::Child> {
    use std::process::{Command, Stdio};
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
}

// ---------------------------------------------------------------------------
// 进程证据（有界 CIM 探针 + TTL 5s + single-flight）
// ---------------------------------------------------------------------------

/// 进程探针内部 JSON（§4.2 固定形状 `{queried,ownCount,foreignCount,unknownCount}`；
/// foreign_count 是合同形状组成部分，分类本身不消费——仅测试断言其按位呈现）。
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(crate) struct ProcessProbeCounts {
    pub queried: bool,
    pub own_count: u32,
    pub foreign_count: u32,
    pub unknown_count: u32,
}

/// 解析探针 stdout（§4.2 固定 JSON；任何偏差 → None → 证据 Unknown）。
pub(crate) fn parse_process_probe_output(stdout: &str) -> Option<ProcessProbeCounts> {
    serde_json::from_str(stdout.trim()).ok()
}

/// 计数 → 证据（§4.2：有 own → Present；无 own 有未知 → Unknown；确认全无本用户
/// 且无未知 → Absent。别账户同名进程（foreign）不参与判定）。
pub(crate) fn evidence_from_counts(c: ProcessProbeCounts) -> Evidence {
    if !c.queried {
        return Evidence::Unknown;
    }
    if c.own_count > 0 {
        Evidence::Present
    } else if c.unknown_count > 0 {
        Evidence::Unknown
    } else {
        Evidence::Absent
    }
}

/// 进程证据 TTL（§4.2：5 秒）。
pub(crate) const PROCESS_EVIDENCE_TTL: Duration = Duration::from_secs(5);

static PROCESS_EVIDENCE_CACHE: Mutex<Option<(Instant, Evidence)>> = Mutex::new(None);
/// single-flight 门：并发补查者复用第一个探针的结果，不重复 spawn PowerShell。
static PROCESS_PROBE_GATE: Mutex<()> = Mutex::new(());

/// 进程探针脚本模板（ASCII；`__SID__`/`__IMAGE__` 由受控来源填充——SID 来自
/// ConvertSidToStringSidW、镜像名为固定常量，均无引号注入面）。
const PROCESS_PROBE_TEMPLATE: &str = r#"
$ErrorActionPreference = 'Stop'
$sid = '__SID__'
$own = 0; $foreign = 0; $unknown = 0; $ok = $false
try {
  $procs = @(Get-CimInstance -ClassName Win32_Process -Filter "Name='__IMAGE__'" -ErrorAction Stop)
  foreach ($p in $procs) {
    $s = $null
    try {
      $r = Invoke-CimMethod -InputObject $p -MethodName GetOwnerSid -ErrorAction Stop
      if ($r) { $s = $r.Sid }
    } catch { $s = $null }
    if ([string]::IsNullOrEmpty($s)) { $unknown++ }
    elseif ($s -eq $sid) { $own++ }
    else { $foreign++ }
  }
  $ok = $true
} catch { $ok = $false }
if ($ok) {
  Write-Output ('{"queried":true,"ownCount":' + $own + ',"foreignCount":' + $foreign + ',"unknownCount":' + $unknown + '}')
} else {
  Write-Output '{"queried":false,"ownCount":0,"foreignCount":0,"unknownCount":0}'
}
"#;

fn process_probe_script(sid: &str, image: &str) -> String {
    PROCESS_PROBE_TEMPLATE
        .replace("__SID__", sid)
        .replace("__IMAGE__", image)
}

/// 生产进程探针：当前用户 SID → CIM owner 计数 → 证据；任何环节不可用 → Unknown。
fn process_evidence_probe(image: &str) -> Evidence {
    let Ok(sid) = current_user_sid_string() else {
        return Evidence::Unknown;
    };
    // 防注入面（来源受控，此处仅为纵深防御）：SID 仅允许字母数字与 '-'
    if !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Evidence::Unknown;
    }
    let script = process_probe_script(&sid, image);
    match run_bounded_probe_with(|| spawn_powershell_probe(&script), PROBE_TIMEOUT) {
        Some(out) => parse_process_probe_output(&out)
            .map(evidence_from_counts)
            .unwrap_or(Evidence::Unknown),
        None => Evidence::Unknown,
    }
}

/// TTL 窗口内的缓存证据。
fn fresh_cached(cache: &Mutex<Option<(Instant, Evidence)>>, ttl: Duration) -> Option<Evidence> {
    let guard = cache.lock().unwrap_or_else(|p| p.into_inner());
    match *guard {
        Some((t, e)) if t.elapsed() < ttl => Some(e),
        _ => None,
    }
}

/// 缓存 + single-flight 通用核心（探针注入，单测注入假探针——hermetic）。
fn cached_evidence_with(
    cache: &Mutex<Option<(Instant, Evidence)>>,
    gate: &Mutex<()>,
    ttl: Duration,
    probe: impl FnOnce() -> Evidence,
) -> Evidence {
    if let Some(e) = fresh_cached(cache, ttl) {
        return e;
    }
    // single-flight：拿到门锁后再查一次缓存（并发等待者复用刚写入的证据）
    let _gate = gate.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(e) = fresh_cached(cache, ttl) {
        return e;
    }
    let e = probe();
    *cache.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), e));
    e
}

/// 进程证据（TTL 5 秒 + single-flight；§4.2：只在 pipe 失败时调用——禁止 500ms 一轮扫描）。
pub(crate) fn process_evidence_cached_with(
    image: &str,
    probe: impl FnOnce(&str) -> Evidence,
) -> Evidence {
    cached_evidence_with(
        &PROCESS_EVIDENCE_CACHE,
        &PROCESS_PROBE_GATE,
        PROCESS_EVIDENCE_TTL,
        || probe(image),
    )
}

/// 生产封装：有界 CIM 探针。
pub(crate) fn process_evidence_cached(image: &str) -> Evidence {
    process_evidence_cached_with(image, process_evidence_probe)
}

// ---------------------------------------------------------------------------
// 任务证据（schtasks 廉价探测 + 失败二段 COM 探针 + TTL 30s 缓存）
// ---------------------------------------------------------------------------

/// 任务证据 TTL（§4.2：30 秒，沿用既有缓存机制）。
pub(crate) const TASK_EVIDENCE_TTL: Duration = Duration::from_secs(30);

static TASK_EVIDENCE_CACHE: Mutex<Option<(Instant, Evidence)>> = Mutex::new(None);
static TASK_PROBE_GATE: Mutex<()> = Mutex::new(());

/// 任务探针内部 JSON（queried=false 表示连"存在与否"都无法判定）。
#[derive(Debug, Clone, Copy, Deserialize)]
struct TaskProbeJson {
    queried: bool,
    exists: bool,
}

/// 任务二段探针 stdout → 证据（任何偏差 → Unknown）。
pub(crate) fn task_evidence_from_probe_output(stdout: &str) -> Evidence {
    match serde_json::from_str::<TaskProbeJson>(stdout.trim()) {
        Ok(j) if j.queried && j.exists => Evidence::Present,
        Ok(j) if j.queried => Evidence::Absent,
        _ => Evidence::Unknown,
    }
}

/// 任务二段探针脚本模板：0x80070002（-2147024894）→ 确认不存在；其它 → 无法判定。
const TASK_PROBE_TEMPLATE: &str = r#"
$ErrorActionPreference = 'Stop'
$name = '__TASK__'
$queried = $false; $exists = $false; $miss = $false
try {
  $svc = New-Object -ComObject Schedule.Service
  $svc.Connect()
  [void]$svc.GetFolder('\').GetTask($name)
  $queried = $true; $exists = $true
} catch {
  $e = $_.Exception
  while ($null -ne $e) {
    if ($e.HResult -eq -2147024894) { $miss = $true; break }
    $e = $e.InnerException
  }
  if ($miss) { $queried = $true; $exists = $false }
}
if ($exists) { Write-Output '{"queried":true,"exists":true}' }
elseif ($queried) { Write-Output '{"queried":true,"exists":false}' }
else { Write-Output '{"queried":false,"exists":false}' }
"#;

fn task_probe_script(task_name: &str) -> String {
    TASK_PROBE_TEMPLATE.replace("__TASK__", task_name)
}

/// `schtasks /Query /TN <name>` 退出码（实测：任务缺失 exit 1，与其它失败同码——
/// 因此非 0 时需要二段探针区分"确认不存在 / 无法判定"）。spawn 失败 → None。
fn scheduled_task_query_exit(task_name: &str) -> Option<i32> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("schtasks")
        .args(["/Query", "/TN", task_name])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .ok()
        .and_then(|s| s.code())
}

/// 任务证据探针：exit 0 → Present；非 0 → 有界 PS+COM 二段探针（§2.1 有界探针归本模块）。
fn task_evidence_probe(task_name: &str) -> Evidence {
    match scheduled_task_query_exit(task_name) {
        Some(0) => return Evidence::Present,
        Some(_) => {}
        None => return Evidence::Unknown,
    }
    let script = task_probe_script(task_name);
    match run_bounded_probe_with(|| spawn_powershell_probe(&script), PROBE_TIMEOUT) {
        Some(out) => task_evidence_from_probe_output(&out),
        None => Evidence::Unknown,
    }
}

/// 任务证据（TTL 30 秒 + single-flight；status 轮询路径）。
pub(crate) fn task_evidence_cached_with(
    task_name: &str,
    probe: impl FnOnce(&str) -> Evidence,
) -> Evidence {
    cached_evidence_with(
        &TASK_EVIDENCE_CACHE,
        &TASK_PROBE_GATE,
        TASK_EVIDENCE_TTL,
        || probe(task_name),
    )
}

/// 生产封装。
pub(crate) fn task_evidence_cached(task_name: &str) -> Evidence {
    task_evidence_cached_with(task_name, task_evidence_probe)
}

/// 写路径接缝：实时探针 + 无条件回写缓存（单测注入假探针——hermetic）。
pub(crate) fn task_evidence_uncached_with(
    task_name: &str,
    probe: impl FnOnce(&str) -> Evidence,
) -> Evidence {
    let e = probe(task_name);
    *TASK_EVIDENCE_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), e));
    e
}

/// 写路径专用：实时查询 + 无条件回写缓存（自启启停/修复后的回读校验）。
pub(crate) fn task_evidence_uncached(task_name: &str) -> Evidence {
    task_evidence_uncached_with(task_name, task_evidence_probe)
}

// ---------------------------------------------------------------------------
// 测试（usability_v3_ 前缀；hermetic：真探针只允许 powershell 假命令，禁止
// schtasks/COM/collector 实体，禁止 UAC / taskkill）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// 分类规则表全矩阵（§4.2）。
    #[test]
    fn usability_v3_classify_health_matrix_per_contract() {
        use PipeFailureKind as K;
        // 有效 status：paused=false/true → running/paused，无需进程补查
        for proc in [Evidence::Present, Evidence::Absent, Evidence::Unknown] {
            assert_eq!(
                classify_health(PipeProbe::Status { paused: false }, proc),
                CollectorHealth::Running
            );
            assert_eq!(
                classify_health(PipeProbe::Status { paused: true }, proc),
                CollectorHealth::Paused
            );
        }
        // AccessDenied 优先：不宣称未运行（任意进程证据）
        for proc in [Evidence::Present, Evidence::Absent, Evidence::Unknown] {
            assert_eq!(
                classify_health(PipeProbe::Failed(K::AccessDenied), proc),
                CollectorHealth::AccessDenied
            );
        }
        // 任意失败 + 本用户进程 Present → unreachable
        for kind in [
            K::NotFound,
            K::Busy,
            K::Timeout,
            K::Io,
            K::Protocol,
            K::WorkerUnavailable,
        ] {
            assert_eq!(
                classify_health(PipeProbe::Failed(kind), Evidence::Present),
                CollectorHealth::Unreachable,
                "{kind:?}"
            );
        }
        // NotFound + 确认 Absent → not_running
        assert_eq!(
            classify_health(PipeProbe::Failed(K::NotFound), Evidence::Absent),
            CollectorHealth::NotRunning
        );
        // Timeout/Busy/Io/Protocol 与 Absent 组合 → unknown（不能排除探测竞态）
        for kind in [
            K::Busy,
            K::Timeout,
            K::Io,
            K::Protocol,
            K::WorkerUnavailable,
        ] {
            assert_eq!(
                classify_health(PipeProbe::Failed(kind), Evidence::Absent),
                CollectorHealth::Unknown,
                "{kind:?}"
            );
        }
        // 进程证据 Unknown → unknown（AccessDenied 已在上面优先）
        for kind in [
            K::NotFound,
            K::Busy,
            K::Timeout,
            K::Io,
            K::Protocol,
            K::WorkerUnavailable,
        ] {
            assert_eq!(
                classify_health(PipeProbe::Failed(kind), Evidence::Unknown),
                CollectorHealth::Unknown,
                "{kind:?}"
            );
        }
    }

    /// serde 线上形状：Evidence / CollectorHealth / DiagnosticCode 全部 snake_case 短码；
    /// WorkerUnavailable → probe_unavailable；默认值为 unknown。
    #[test]
    fn usability_v3_evidence_health_diagnostic_code_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&Evidence::Present).unwrap(),
            r#""present""#
        );
        assert_eq!(
            serde_json::to_string(&Evidence::Absent).unwrap(),
            r#""absent""#
        );
        assert_eq!(
            serde_json::to_string(&Evidence::Unknown).unwrap(),
            r#""unknown""#
        );
        assert_eq!(
            serde_json::to_string(&CollectorHealth::Running).unwrap(),
            r#""running""#
        );
        assert_eq!(
            serde_json::to_string(&CollectorHealth::Paused).unwrap(),
            r#""paused""#
        );
        assert_eq!(
            serde_json::to_string(&CollectorHealth::NotRunning).unwrap(),
            r#""not_running""#
        );
        assert_eq!(
            serde_json::to_string(&CollectorHealth::Unreachable).unwrap(),
            r#""unreachable""#
        );
        assert_eq!(
            serde_json::to_string(&CollectorHealth::AccessDenied).unwrap(),
            r#""access_denied""#
        );
        assert_eq!(
            serde_json::to_string(&CollectorHealth::Unknown).unwrap(),
            r#""unknown""#
        );
        assert_eq!(Evidence::default(), Evidence::Unknown);
        assert_eq!(CollectorHealth::default(), CollectorHealth::Unknown);

        let expect = [
            (PipeFailureKind::NotFound, r#""pipe_not_found""#),
            (PipeFailureKind::AccessDenied, r#""pipe_access_denied""#),
            (PipeFailureKind::Busy, r#""pipe_busy""#),
            (PipeFailureKind::Timeout, r#""pipe_timeout""#),
            (PipeFailureKind::Io, r#""pipe_io""#),
            (PipeFailureKind::Protocol, r#""pipe_protocol""#),
            (PipeFailureKind::WorkerUnavailable, r#""probe_unavailable""#),
        ];
        for (kind, wire) in expect {
            assert_eq!(
                serde_json::to_string(&kind.diagnostic_code()).unwrap(),
                wire,
                "{kind:?}"
            );
        }
    }

    /// 探针 JSON 解析 + 计数→证据映射矩阵（§4.2 固定形状，别账户同名进程不算本用户实例）。
    #[test]
    fn usability_v3_process_probe_output_parse_and_evidence_mapping() {
        let c = parse_process_probe_output(
            r#"{"queried":true,"ownCount":1,"foreignCount":2,"unknownCount":0}"#,
        )
        .unwrap();
        assert_eq!((c.own_count, c.foreign_count, c.unknown_count), (1, 2, 0));
        // own>0 → Present（无关 foreign）
        assert_eq!(evidence_from_counts(c), Evidence::Present);
        // 确认全无本用户且无未知 → Absent
        assert_eq!(
            evidence_from_counts(
                parse_process_probe_output(
                    r#"{"queried":true,"ownCount":0,"foreignCount":3,"unknownCount":0}"#
                )
                .unwrap()
            ),
            Evidence::Absent
        );
        // 无 own 但有未知归属 → Unknown
        assert_eq!(
            evidence_from_counts(
                parse_process_probe_output(
                    r#"{"queried":true,"ownCount":0,"foreignCount":1,"unknownCount":2}"#
                )
                .unwrap()
            ),
            Evidence::Unknown
        );
        // CIM 查询本身失败 → Unknown
        assert_eq!(
            evidence_from_counts(
                parse_process_probe_output(
                    r#"{"queried":false,"ownCount":0,"foreignCount":0,"unknownCount":0}"#
                )
                .unwrap()
            ),
            Evidence::Unknown
        );
        // 非 JSON / 空输出 → None（调用方折为 Unknown）
        assert!(parse_process_probe_output("not json").is_none());
        assert!(parse_process_probe_output("").is_none());
    }

    /// 任务二段探针 stdout → 证据：0x80070002 → Absent；存在 → Present；
    /// 无法判定/垃圾输出 → Unknown。
    #[test]
    fn usability_v3_task_probe_output_parse_and_evidence() {
        assert_eq!(
            task_evidence_from_probe_output(r#"{"queried":true,"exists":true}"#),
            Evidence::Present
        );
        assert_eq!(
            task_evidence_from_probe_output(r#"{"queried":true,"exists":false}"#),
            Evidence::Absent
        );
        assert_eq!(
            task_evidence_from_probe_output(r#"{"queried":false,"exists":false}"#),
            Evidence::Unknown
        );
        assert_eq!(
            task_evidence_from_probe_output("garbage"),
            Evidence::Unknown
        );
    }

    /// 探针脚本模板：SID 与镜像名按位填充、无占位符残留。
    #[test]
    fn usability_v3_probe_scripts_fill_placeholders() {
        let ps = process_probe_script("S-1-5-21-1-2-3", "cl-recoder-collector.exe");
        assert!(ps.contains("$sid = 'S-1-5-21-1-2-3'"), "{ps}");
        assert!(ps.contains("Name='cl-recoder-collector.exe'"), "{ps}");
        assert!(!ps.contains("__SID__") && !ps.contains("__IMAGE__"), "{ps}");
        assert!(ps.contains("ownCount"), "{ps}");

        let ts = task_probe_script("ClRecoderCollector");
        assert!(ts.contains("$name = 'ClRecoderCollector'"), "{ts}");
        assert!(!ts.contains("__TASK__"), "{ts}");
        assert!(
            ts.contains("-2147024894"),
            "二段探针按 0x80070002 判定确认不存在"
        );
    }

    /// 有界探针：进程 1500ms 超时只结束本次探针 child（标记文件永不落盘）且证据 Unknown。
    #[test]
    fn usability_v3_process_probe_timeout_kills_only_probe_child() {
        let dir = std::env::temp_dir().join(format!(
            "clrec-health-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("marker.txt");
        let marker_ps = marker.to_string_lossy().replace('\\', "/");
        let script = format!("Start-Sleep -Seconds 20; Set-Content -Path '{marker_ps}' -Value 'x'");
        let started = Instant::now();
        let out = run_bounded_probe_with(
            || spawn_powershell_probe(&script),
            Duration::from_millis(400),
        );
        assert!(out.is_none(), "超时必须返回 None（证据 Unknown）");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "探针限时生效，不得等满 child 时长"
        );
        // 标记文件不得出现：探针 child 被 kill，Start-Sleep 之后的语句永不执行
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!marker.exists(), "超时后探针 child 必须已被结束");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 有界探针：stdout 超 32KiB 上限 → 视为不可信输出（None → Unknown），限时生效。
    #[test]
    fn usability_v3_process_probe_stdout_cap_bounds_output() {
        let script = "$b = 'x' * 40960; Write-Output $b";
        let started = Instant::now();
        let out = run_bounded_probe_with(
            || spawn_powershell_probe(script),
            Duration::from_millis(1500),
        );
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "32KiB 上限不应导致无界读取"
        );
        // child 因管道停止消费而阻塞 → 限时杀掉 → None；若机器过快则至少不超上限
        if let Some(text) = out {
            assert!(
                text.len() <= PROBE_STDOUT_CAP + 2,
                "stdout 读取必须封顶 32KiB"
            );
        }
    }

    /// 有界探针端到端（注入假 CIM 输出命令）：快速 JSON → 对应证据。
    #[test]
    fn usability_v3_process_probe_end_to_end_with_fake_cim_output() {
        let echo = |json: &str| format!("Write-Output '{}'", json.replace('\'', "''"));
        let present = run_bounded_probe_with(
            || {
                spawn_powershell_probe(&echo(
                    r#"{"queried":true,"ownCount":1,"foreignCount":0,"unknownCount":0}"#,
                ))
            },
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(
            parse_process_probe_output(&present).map(evidence_from_counts),
            Some(Evidence::Present)
        );
        let absent = run_bounded_probe_with(
            || {
                spawn_powershell_probe(&echo(
                    r#"{"queried":true,"ownCount":0,"foreignCount":0,"unknownCount":0}"#,
                ))
            },
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(
            parse_process_probe_output(&absent).map(evidence_from_counts),
            Some(Evidence::Absent)
        );
    }

    /// 进程证据缓存：TTL 内命中不重探；白盒平移时间戳模拟过期后重探并回写；
    /// single-flight——4 线程并发只触发一次探针。
    #[test]
    fn usability_v3_process_evidence_cache_ttl_and_single_flight() {
        static TEST_LOCK: Mutex<()> = Mutex::new(());
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        *PROCESS_EVIDENCE_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;

        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = Arc::clone(&calls);
        let e1 = process_evidence_cached_with("cl-recoder-collector.exe", |_| {
            calls2.fetch_add(1, Ordering::SeqCst);
            Evidence::Present
        });
        assert_eq!(e1, Evidence::Present);
        // TTL 内命中：探针不再被调用
        let calls3 = Arc::clone(&calls);
        let e2 = process_evidence_cached_with("cl-recoder-collector.exe", |_| {
            calls3.fetch_add(1, Ordering::SeqCst);
            Evidence::Absent
        });
        assert_eq!(e2, Evidence::Present, "TTL 内命中旧值");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 白盒平移：把缓存时间戳挪到 TTL+1s 前 → 过期 → 重新探针并回写
        {
            let mut guard = PROCESS_EVIDENCE_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let expired = Instant::now()
                .checked_sub(PROCESS_EVIDENCE_TTL + Duration::from_secs(1))
                .expect("系统运行时长应足以让 TTL 过期点落在过去");
            *guard = Some((expired, Evidence::Unknown));
        }
        let calls4 = Arc::clone(&calls);
        let e3 = process_evidence_cached_with("cl-recoder-collector.exe", |_| {
            calls4.fetch_add(1, Ordering::SeqCst);
            Evidence::Absent
        });
        assert_eq!(e3, Evidence::Absent, "过期后重新探针");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // single-flight：并发等待者复用同一份新证据，探针只跑一次
        *PROCESS_EVIDENCE_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        let calls5 = Arc::new(AtomicU32::new(0));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let c = Arc::clone(&calls5);
                std::thread::spawn(move || {
                    process_evidence_cached_with("cl-recoder-collector.exe", |_| {
                        std::thread::sleep(Duration::from_millis(60));
                        c.fetch_add(1, Ordering::SeqCst);
                        Evidence::Present
                    })
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), Evidence::Present);
        }
        assert_eq!(
            calls5.load(Ordering::SeqCst),
            1,
            "single-flight 下探针只跑一次"
        );

        *PROCESS_EVIDENCE_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// 任务证据缓存：TTL 30s 沿用（命中不重探 / uncached 实时回写后命中新值）。
    #[test]
    fn usability_v3_task_evidence_cache_ttl_30s() {
        static TEST_LOCK: Mutex<()> = Mutex::new(());
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        *TASK_EVIDENCE_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;

        assert_eq!(
            TASK_EVIDENCE_TTL,
            Duration::from_secs(30),
            "任务证据 TTL 30 秒"
        );

        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = Arc::clone(&calls);
        let v = task_evidence_cached_with("ClRecoderCollector", |_| {
            calls2.fetch_add(1, Ordering::SeqCst);
            Evidence::Present
        });
        assert_eq!(v, Evidence::Present);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let calls3 = Arc::clone(&calls);
        let v2 = task_evidence_cached_with("ClRecoderCollector", |_| {
            calls3.fetch_add(1, Ordering::SeqCst);
            Evidence::Absent
        });
        assert_eq!(v2, Evidence::Present, "TTL 内命中缓存");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // uncached：实时探针 + 无条件回写（下一次 cached 命中新值且不再探针）
        let calls4 = Arc::clone(&calls);
        let live = task_evidence_uncached_with("ClRecoderCollector", |_| {
            calls4.fetch_add(1, Ordering::SeqCst);
            Evidence::Absent
        });
        assert_eq!(live, Evidence::Absent);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let v3 = task_evidence_cached_with("ClRecoderCollector", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Evidence::Present
        });
        assert_eq!(v3, Evidence::Absent, "uncached 回写后下一次命中新值");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "命中缓存不调用探针");

        *TASK_EVIDENCE_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// 当前用户 SID（真实本机令牌，只读）：S-1-x(-y)+ 形状。
    #[test]
    fn usability_v3_current_user_sid_shape() {
        let sid = current_user_sid_string().expect("当前进程令牌 SID 必须可解析");
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert!(
            sid[4..].bytes().all(|b| b.is_ascii_digit() || b == b'-') && sid.len() > 6,
            "{sid}"
        );
        assert!(
            sid.split('-').count() >= 4,
            "至少含 S/1/authority/rid 四段: {sid}"
        );
    }
}
