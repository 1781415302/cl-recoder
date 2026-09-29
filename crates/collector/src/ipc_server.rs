//! pipe 控制服务端（PLAN §4.4 契约，S8）。
//!
//! - 管道：[`clrecoder_core::ipc::PIPE_NAME`]，字节模式（PIPE_TYPE_BYTE / PIPE_READMODE_BYTE）、
//!   PIPE_WAIT、`PIPE_REJECT_REMOTE_CLIENTS`；**显式 DACL**
//!   `ConvertStringSecurityDescriptorToSecurityDescriptorW("D:P(A;;GA;;;<当前用户SID>)")`——
//!   SID 运行时解析（进程令牌 → `ConvertSidToStringSidW`），防计划任务被误配为 SYSTEM 时 GUI 被拒。
//! - NDJSON：一行请求（UTF-8，≤ [`clrecoder_core::ipc::MAX_REQUEST_BYTES`] 含换行，
//!   **超长即断开且不响应**）→ 一行响应；一连接只处理一请求（GUI 每条命令新建探测连接）。
//! - 命令：`status` / `set_paused` / `shutdown`，置位 [`Flags`]；响应形状
//!   `{"ok":true,"data":{...}}` / `{"ok":true,"data":null}` / `{"ok":false,"error":"..."}`，
//!   逐字对齐 §4.4 线格式示例（未知命令恒回 `"unknown command"`）。
//! - 线程模型：单接受线程串行处理；`shutdown` 命令置位后服务线程在处理完当前连接后退出
//!   （main 线程 join 后退出进程，PLAN §5.1）。线程内禁止 panic（PLAN §9.4）。
//!
//! S8 集成单测（进程内）：起 server → 客户端三条请求往返逐字对齐 §4.4 示例。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clrecoder_core::day::now_local_rfc3339;
use clrecoder_core::ipc::{CtlRequest, CtlResponse, StatusData, MAX_REQUEST_BYTES, PIPE_NAME};
use windows::core::{Error as WError, HSTRING, HRESULT, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, E_FAIL, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_UNICODE_TRANSLATION, ERROR_PIPE_CONNECTED,
    HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, ReadFile, WriteFile, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 调试日志：仅在设置环境变量 `CLRECODER_DEBUG` 时输出到 stderr（release 默认静默，PLAN §9.2）。
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if std::env::var_os("CLRECODER_DEBUG").is_some() {
            eprintln!("[ipc_server] {}", format_args!($($arg)*));
        }
    };
}

/// 管道收发缓冲区大小（字节；请求行上限 8KB，见 [`MAX_REQUEST_BYTES`]）。
const PIPE_BUFFER_SIZE: u32 = 8192;

/// 采集进程运行开关（PLAN §4.6）：`paused`/`shutdown` 仅由 pipe 命令置位。
///
/// 构造后经 `Arc` 共享：ipc_server 写入、engine_loop（S9）读取。
/// 暂停状态不持久化（PLAN §6）——collector 重启即恢复统计。
#[derive(Debug, Default)]
pub struct Flags {
    /// true=暂停统计（aggregator 丢弃 Input 事件，Foreground 照常）
    pub paused: AtomicBool,
    /// true=请求采集进程优雅退出
    pub shutdown: AtomicBool,
}

/// 服务端要报告的运行时状态（[`StatusData`] 的数据来源）。
///
/// 由 main（S9）构造并 `Arc` 分享：engine_loop 在消费到输入事件时调 [`RuntimeStatus::record_event`]，
/// ipc_server 在 `status` 请求时读取快照。`version` 固定为编译期包版本。
#[derive(Debug)]
pub struct RuntimeStatus {
    /// collector 版本（如 "0.1.0"，main 传 `env!("CARGO_PKG_VERSION")`）
    pub version: String,
    /// 进程启动时刻（RFC 3339 本地时区，见 [`now_local_rfc3339`]）
    pub started_at: String,
    /// 最近一次输入事件时刻；尚无事件为 `None`
    pub last_event_at: Mutex<Option<String>>,
    /// 进程启动以来累计收到的事件数
    pub events_seen: AtomicU64,
}

impl RuntimeStatus {
    /// 以当前时刻为 `started_at` 构造（version 传 `env!("CARGO_PKG_VERSION")`）。
    #[must_use]
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            started_at: now_local_rfc3339(),
            last_event_at: Mutex::new(None),
            events_seen: AtomicU64::new(0),
        }
    }

    /// 记录一次输入事件（engine_loop 每消费到一个 `AggEvent::Input` 调一次）。
    pub fn record_event(&self) {
        self.events_seen.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut guard) = self.last_event_at.lock() {
            *guard = Some(now_local_rfc3339());
        }
    }

    /// 按当前 `paused` 值生成 [`StatusData`] 快照。
    fn snapshot(&self, paused: bool) -> StatusData {
        // Mutex 中毒不致命（持锁者 panic 过），取出内部值继续服务——服务端绝不 panic
        let last_event_at = match self.last_event_at.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        StatusData {
            paused,
            version: self.version.clone(),
            started_at: self.started_at.clone(),
            last_event_at,
            events_seen: self.events_seen.load(Ordering::Relaxed),
        }
    }
}

/// 启动 pipe 服务线程：先创建显式 DACL 的安全描述符（进程生命周期内一次），
/// 然后循环「创建监听实例 → 等待客户端 → 处理一请求一响应 → 断开」，
/// 直至 [`Flags::shutdown`] 置位。返回服务线程句柄，main（S9）join 后退出进程。
///
/// 安全描述符创建失败（SDDL 转换异常等）时线程立即退出——属于启动期致命错误，
/// main 可通过 join 感知（不 panic，PLAN §9.4）。
#[must_use = "服务线程必须被 join（main 退出前等待 shutdown 生效）"]
pub fn spawn(flags: Arc<Flags>, status: Arc<RuntimeStatus>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("ipc_server".into())
        .spawn(move || run_server(&flags, &status))
        .expect("ipc_server 线程创建失败")
}

/// 持有管道实例的安全属性；析构时归还 [`ConvertStringSecurityDescriptorToSecurityDescriptorW`]
/// 分配的安全描述符（LocalFree）。
struct OwnedSecurityAttributes {
    sa: SECURITY_ATTRIBUTES,
}

impl Drop for OwnedSecurityAttributes {
    fn drop(&mut self) {
        // SAFETY: lpSecurityDescriptor 由 ConvertStringSecurityDescriptorToSecurityDescriptorW
        // 以 LocalAlloc 语义分配，须以 LocalFree 归还；仅在 Drop 时释放一次。
        unsafe {
            LocalFree(Some(HLOCAL(self.sa.lpSecurityDescriptor)));
        }
    }
}

/// 服务线程主体：见 [`spawn`]。
fn run_server(flags: &Flags, status: &RuntimeStatus) {
    // SAFETY: 仅调用 Win32 API；返回的 SECURITY_ATTRIBUTES 内的 SD 指针由 guard 在线程退出时释放。
    let owned_sa = match unsafe { build_security_attributes() } {
        Ok(sa) => OwnedSecurityAttributes { sa },
        Err(err) => {
            debug_log!("创建安全描述符（显式 DACL）失败，pipe 服务未启动: {err}");
            return;
        }
    };
    let sa = &owned_sa.sa;
    let name = HSTRING::from(PIPE_NAME);
    // FILE_FLAG_FIRST_PIPE_INSTANCE 只能加在首次创建上（此后管道已存在），用于暴露重名实例。
    let mut first_instance = true;
    loop {
        if flags.shutdown.load(Ordering::Acquire) {
            break;
        }
        // SAFETY: sa 的 SD 在 guard 释放前始终有效；name 为本函数内存活的 HSTRING；
        // 成功创建的句柄由 serve_client 关闭，失败路径立即 CloseHandle。
        let handle = unsafe {
            let open_mode = if first_instance {
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                PIPE_ACCESS_DUPLEX
            };
            CreateNamedPipeW(
                &name,
                open_mode,
                // 字节模式 + PIPE_WAIT + 拒绝远程客户端（§4.4）
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                0,
                Some(sa),
            )
        };
        if handle.is_invalid() {
            debug_log!("CreateNamedPipeW 失败，1s 后重试");
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        first_instance = false;
        // SAFETY: handle 为上方成功创建的同步管道实例；阻塞等待直到客户端连入或出错。
        let connected = unsafe { ConnectNamedPipe(handle, None) };
        match connected {
            Ok(()) => {}
            // 客户端在 CreateNamedPipeW 与 ConnectNamedPipe 之间已连入，连接仍有效，继续服务
            Err(err) if err.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => {}
            Err(err) => {
                debug_log!("ConnectNamedPipe 失败: {err}");
                // SAFETY: handle 有效；释放后回到循环顶部重建监听实例。
                unsafe {
                    let _ = CloseHandle(handle);
                }
                continue;
            }
        }
        // SAFETY: handle 已连入客户端；serve_client 负责断开并关闭句柄。
        unsafe { serve_client(handle, flags, status) };
    }
}

/// 处理一个已连入的客户端：读一行请求 → 处理 → 回一行响应 → 断开并关闭。
/// 任何读失败 / 空连接 / 超长行都静默断开（不响应，§4.4「超长即断开」）。
///
/// # Safety
/// `pipe` 必须是已建立连接的有效管道句柄（字节模式、同步）；本函数结束时关闭该句柄，
/// 调用方不得再使用。
unsafe fn serve_client(pipe: HANDLE, flags: &Flags, status: &RuntimeStatus) {
    if let Some(line) = read_request_line(pipe) {
        let response = handle_request(&line, flags, status);
        let mut out = serde_json::to_vec(&response)
            .unwrap_or_else(|_| b"{\"ok\":false,\"error\":\"serialize failed\"}".to_vec());
        out.push(b'\n');
        let mut written = 0u32;
        // SAFETY: pipe 有效且处于已连接状态；同步句柄、阻塞写。
        let write_ok = unsafe { WriteFile(pipe, Some(&out), Some(&mut written), None) }.is_ok();
        if write_ok {
            // SAFETY: pipe 有效；flush 保证客户端在断开前读到完整响应。
            unsafe {
                let _ = FlushFileBuffers(pipe);
            }
        }
    }
    // SAFETY: pipe 有效；断开并关闭，回到服务循环。
    unsafe {
        let _ = DisconnectNamedPipe(pipe);
        let _ = CloseHandle(pipe);
    }
}

/// 从管道读一行 UTF-8 请求（以 `\n` 结束）。行长度（含换行）超过
/// [`MAX_REQUEST_BYTES`] 时返回 `None`（断开不响应）；客户端未发完整行就关闭亦返回 `None`。
///
/// # Safety
/// `pipe` 必须是已连接的有效管道句柄（字节模式、同步）。
unsafe fn read_request_line(pipe: HANDLE) -> Option<Vec<u8>> {
    let mut chunk = [0u8; 4096];
    let mut acc: Vec<u8> = Vec::with_capacity(512);
    loop {
        let mut read = 0u32;
        // SAFETY: pipe 有效；chunk 为本次读的目标缓冲，read 由系统写入。
        if unsafe { ReadFile(pipe, Some(&mut chunk), Some(&mut read), None) }.is_err() {
            return None; // 客户端异常断开 / 句柄错误
        }
        if read == 0 {
            return None; // 客户端关闭且未发送完整行
        }
        let chunk = &chunk[..read as usize];
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            acc.extend_from_slice(&chunk[..pos]);
            // 行（含换行）超过 8KB：断开不响应（§4.4）
            if acc.len() + 1 > MAX_REQUEST_BYTES {
                return None;
            }
            return Some(acc);
        }
        acc.extend_from_slice(chunk);
        if acc.len() + 1 > MAX_REQUEST_BYTES {
            return None; // 已超长且未见换行 → 断开
        }
    }
}

/// 解析并执行一条请求（NDJSON 行，UTF-8），返回响应。
///
/// 错误语义（§4.4 示例逐字对齐）：
/// - 非法 JSON / `cmd` 缺失或类型错 / 已知命令缺参数 → `"invalid request"`；
/// - `cmd` 是字符串但不属三个合法命令 → `"unknown command"`。
fn handle_request(line: &[u8], flags: &Flags, status: &RuntimeStatus) -> CtlResponse {
    // 行尾可能带 \r（客户端用 CRLF）或空白，先去除再解析
    let line = trim_ascii(line);
    let value: serde_json::Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return CtlResponse::err("invalid request"),
    };
    // 合法命令集合须与 clrecoder_core::ipc::CtlRequest 的 serde tag 值一致（§4.4）
    let unknown_cmd = value
        .get("cmd")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|cmd| !matches!(cmd, "status" | "set_paused" | "shutdown"));
    match serde_json::from_value::<CtlRequest>(value) {
        Ok(CtlRequest::Status) => {
            CtlResponse::ok_with(status.snapshot(flags.paused.load(Ordering::Acquire)))
        }
        Ok(CtlRequest::SetPaused { paused }) => {
            flags.paused.store(paused, Ordering::Release);
            CtlResponse::ok_no_data()
        }
        Ok(CtlRequest::Shutdown) => {
            flags.shutdown.store(true, Ordering::Release);
            CtlResponse::ok_no_data()
        }
        Err(_) if unknown_cmd => CtlResponse::err("unknown command"),
        Err(_) => CtlResponse::err("invalid request"),
    }
}

/// 去除字节行首尾的 ASCII 空白（含 `\r`、空格）。
fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |p| p + 1);
    &bytes[start..end]
}

/// 构造限当前用户 SID 的 [`SECURITY_ATTRIBUTES`]（显式 DACL，§4.4）。
/// SDDL：`D:P(A;;GA;;;<SID>)` = 受保护 DACL + 仅允许当前用户 Generic All。
///
/// # Safety
/// 仅调用 Win32 API；SD 指针的释放责任转移给调用方（见 [`OwnedSecurityAttributes`]）。
unsafe fn build_security_attributes() -> windows::core::Result<SECURITY_ATTRIBUTES> {
    // 运行时解析当前用户 SID（进程令牌），拼出 SDDL
    let sid = current_user_sid_string()?;
    let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})"));
    let mut psd = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
    // SAFETY: sddl 在调用期间存活；psd 由系统分配，成功后由调用方 LocalFree。
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut psd, None)?;
    }
    if psd.0.is_null() {
        return Err(WError::from_hresult(E_FAIL));
    }
    Ok(SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: false.into(),
    })
}

/// 取当前进程令牌的用户 SID 字符串（形如 `S-1-5-21-…`）。
///
/// # Safety
/// 仅调用 Win32 API；令牌与缓冲区生命周期在本函数内闭合。
unsafe fn current_user_sid_string() -> windows::core::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess 返回伪句柄；token 由系统写出，下方关闭。
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
    }
    // SAFETY: token 刚由 OpenProcessToken 成功打开，本轮调用内有效。
    let result = token_user_sid_string(token);
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

/// 从已打开的进程令牌提取用户 SID 字符串。
///
/// # Safety
/// `token` 必须是带 `TOKEN_QUERY` 权限的有效令牌句柄。
unsafe fn token_user_sid_string(token: HANDLE) -> windows::core::Result<String> {
    let mut len = 0u32;
    // SAFETY: token 有效；首传空缓冲仅为探测所需长度（预期 ERROR_INSUFFICIENT_BUFFER 并回填 len）。
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
    }
    if len == 0 {
        return Err(WError::from_hresult(HRESULT::from_win32(
            ERROR_INSUFFICIENT_BUFFER.0,
        )));
    }
    // 以 u64 分配保证 8 字节对齐（TOKEN_USER 含指针成员），避免对齐问题
    let mut buf = vec![0u64; len as usize / 8 + 1];
    // SAFETY: buf 按 len 分配且在调用期间存活；TOKEN_USER 由系统写入 buf。
    unsafe {
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len)?;
    }
    // SAFETY: buf 刚被 GetTokenInformation 以 TOKEN_USER 布局写入，且容量覆盖整个结构、对齐充分。
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut pwstr = PWSTR::null();
    // SAFETY: user.User.Sid 指向 buf 内的有效 SID；pwstr 由系统分配，下方读取后 LocalFree。
    unsafe {
        ConvertSidToStringSidW(user.User.Sid, &mut pwstr)?;
    }
    if pwstr.is_null() {
        return Err(WError::from_hresult(E_FAIL));
    }
    // SAFETY: pwstr 由 ConvertSidToStringSidW 分配为合法以 NUL 结尾的 UTF-16 串。
    let sid = unsafe { pwstr.to_string() };
    // SAFETY: pwstr 是 ConvertSidToStringSidW 分配的 LocalAlloc 内存，须以 LocalFree 归还。
    unsafe {
        LocalFree(Some(HLOCAL(pwstr.0.cast())));
    }
    sid.map_err(|_| WError::from_hresult(HRESULT::from_win32(ERROR_NO_UNICODE_TRANSLATION.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorControl, ACL,
        ACCESS_ALLOWED_ACE, PSID, SE_DACL_PROTECTED,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    /// pipe 名是全局唯一的：同一时刻只允许一个测试持有服务端，避免实例互相干扰。
    static SERVER_LOCK: Mutex<()> = Mutex::new(());

    fn test_status() -> RuntimeStatus {
        // 字段值取 §4.4 示例原文，使 status 响应可与示例逐字比对
        RuntimeStatus {
            version: "0.1.0".into(),
            started_at: "2026-09-27T22:00:00+08:00".into(),
            last_event_at: Mutex::new(Some("2026-09-27T22:14:31+08:00".into())),
            events_seen: AtomicU64::new(48_219),
        }
    }

    /// 客户端连入（重试直至服务端就绪或超时），返回已连接的管道客户端句柄。
    unsafe fn open_client(deadline: Instant) -> HANDLE {
        let name = HSTRING::from(PIPE_NAME);
        loop {
            if Instant::now() >= deadline {
                panic!("连接管道超时（服务端未就绪）");
            }
            // SAFETY: 同步打开命名管道客户端端；name 为本循环内存活的 HSTRING。
            match unsafe {
                CreateFileW(
                    &name,
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            } {
                Ok(h) => return h,
                Err(err) if err.code() == HRESULT::from_win32(ERROR_PIPE_BUSY.0) => {
                    // SAFETY: name 有效；管道忙时让出 100ms 再试。
                    unsafe {
                        let _ = WaitNamedPipeW(&name, 100);
                    }
                }
                Err(err) if err.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
                    std::thread::sleep(Duration::from_millis(10)); // 服务端尚未建实例
                }
                Err(err) => panic!("CreateFileW 意外失败: {err}"),
            }
        }
    }

    /// 客户端：连入管道（重试直至服务端就绪），发送一行请求并读取一行响应。
    ///
    /// `Err` 返回值用于「写/读中途断开」的场景（如超长行被服务端断开）。
    unsafe fn transact(request: &str, deadline: Instant) -> windows::core::Result<String> {
        let handle = unsafe { open_client(deadline) };
        let result = (|| {
            let mut request = request.as_bytes().to_vec();
            request.push(b'\n');
            let mut written = 0u32;
            // SAFETY: handle 有效且已连接。
            unsafe {
                WriteFile(handle, Some(&request), Some(&mut written), None)?;
            }
            // 读一行响应（服务端 Flush 后才断开，读到 \n 即完整）
            let mut acc: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let mut read = 0u32;
                // SAFETY: handle 有效。
                unsafe {
                    ReadFile(handle, Some(&mut chunk), Some(&mut read), None)?;
                }
                if read == 0 {
                    break; // 服务端断开且无响应（超长行场景）
                }
                let chunk = &chunk[..read as usize];
                if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
                    acc.extend_from_slice(&chunk[..pos]);
                    break;
                }
                acc.extend_from_slice(chunk);
            }
            Ok(String::from_utf8(acc).expect("响应必须是 UTF-8"))
        })();
        // SAFETY: handle 为本轮打开的客户端句柄。
        unsafe {
            let _ = CloseHandle(handle);
        }
        result
    }

    /// §4.4 三条示例请求逐字往返 + set_paused/shutdown 置位 + 服务线程随 shutdown 退出。
    #[test]
    fn three_wire_examples_roundtrip() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let flags = Arc::new(Flags::default());
        let status = Arc::new(test_status());
        let server = spawn(flags.clone(), status.clone());
        let deadline = Instant::now() + Duration::from_secs(10);

        // → {"cmd":"status"}
        // ← {"ok":true,"data":{...}} —— 与 §4.4 示例逐字一致（值由 test_status 固定）
        let response = unsafe { transact(r#"{"cmd":"status"}"#, deadline) }.expect("status 往返");
        assert_eq!(
            response,
            r#"{"ok":true,"data":{"paused":false,"version":"0.1.0","started_at":"2026-09-27T22:00:00+08:00","last_event_at":"2026-09-27T22:14:31+08:00","events_seen":48219}}"#
        );

        // → {"cmd":"set_paused","paused":true}  ← {"ok":true,"data":null}
        let response = unsafe { transact(r#"{"cmd":"set_paused","paused":true}"#, deadline) }
            .expect("set_paused 往返");
        assert_eq!(response, r#"{"ok":true,"data":null}"#);
        assert!(flags.paused.load(Ordering::Acquire), "set_paused 必须置位 Flags.paused");

        // → {"cmd":"bogus"}  ← {"ok":false,"error":"unknown command"}
        let response = unsafe { transact(r#"{"cmd":"bogus"}"#, deadline) }.expect("bogus 往返");
        assert_eq!(response, r#"{"ok":false,"error":"unknown command"}"#);

        // → {"cmd":"shutdown"}  ← {"ok":true,"data":null}，随后服务线程退出
        let response = unsafe { transact(r#"{"cmd":"shutdown"}"#, deadline) }.expect("shutdown 往返");
        assert_eq!(response, r#"{"ok":true,"data":null}"#);
        assert!(flags.shutdown.load(Ordering::Acquire), "shutdown 必须置位 Flags.shutdown");
        server.join().expect("服务线程应随 shutdown 正常退出");
    }

    /// 暂停状态必须反映到 status 响应；shutdown 后管道不再接受连接。
    #[test]
    fn paused_visible_in_status_and_shutdown_stops_accepting() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let flags = Arc::new(Flags::default());
        let status = Arc::new(RuntimeStatus::new("0.1.0"));
        let server = spawn(flags.clone(), status.clone());
        let deadline = Instant::now() + Duration::from_secs(10);

        let response = unsafe { transact(r#"{"cmd":"set_paused","paused":true}"#, deadline) }
            .expect("set_paused 往返");
        assert_eq!(response, r#"{"ok":true,"data":null}"#);

        let response = unsafe { transact(r#"{"cmd":"status"}"#, deadline) }.expect("status 往返");
        assert!(response.contains(r#""paused":true"#), "status 需反映暂停状态: {response}");
        assert!(response.contains(r#""events_seen":0"#), "尚无事件: {response}");
        assert!(response.contains(r#""last_event_at":null"#), "尚无事件时间: {response}");

        // record_event 后计数与时间戳出现（S9 engine_loop 的调用面）
        status.record_event();
        let response = unsafe { transact(r#"{"cmd":"status"}"#, deadline) }.expect("status 往返");
        assert!(response.contains(r#""events_seen":1"#), "record_event 后计数 +1: {response}");
        assert!(!response.contains(r#""last_event_at":null"#), "record_event 后有时间戳: {response}");

        // 恢复统计
        let response = unsafe { transact(r#"{"cmd":"set_paused","paused":false}"#, deadline) }
            .expect("set_paused(false) 往返");
        assert_eq!(response, r#"{"ok":true,"data":null}"#);
        let response = unsafe { transact(r#"{"cmd":"status"}"#, deadline) }.expect("status 往返");
        assert!(response.contains(r#""paused":false"#), "恢复统计: {response}");

        let response = unsafe { transact(r#"{"cmd":"shutdown"}"#, deadline) }.expect("shutdown 往返");
        assert_eq!(response, r#"{"ok":true,"data":null}"#);
        server.join().expect("服务线程应随 shutdown 正常退出");

        // 服务线程退出后所有实例已关闭：管道名消失（重试窗口吸收关闭时延）
        let name = HSTRING::from(PIPE_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: 探测性打开；打开成功即说明管道仍在（测试失败），失败取错误码判断。
            let err = match unsafe {
                CreateFileW(
                    &name,
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            } {
                Ok(h) => {
                    // SAFETY: 关闭探测句柄。
                    unsafe {
                        let _ = CloseHandle(h);
                    }
                    panic!("shutdown 后管道不应再接受连接");
                }
                Err(err) => err,
            };
            if err.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) {
                break;
            }
            assert!(Instant::now() < deadline, "shutdown 后管道未消失: {err}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 非法 JSON → `{"ok":false,"error":"invalid request"}`；缺参数的已知命令同；
    /// 超长行（>8KB 无换行）→ 断开不响应，且服务端随后仍能正常服务。
    #[test]
    fn malformed_requests_and_overlong_line() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let flags = Arc::new(Flags::default());
        let status = Arc::new(test_status());
        let server = spawn(flags.clone(), status.clone());
        let deadline = Instant::now() + Duration::from_secs(10);

        let response = unsafe { transact("not json", deadline) }.expect("非法 JSON 往返");
        assert_eq!(response, r#"{"ok":false,"error":"invalid request"}"#);

        // 已知命令但缺参数 → invalid request（不是 unknown command）
        let response = unsafe { transact(r#"{"cmd":"set_paused"}"#, deadline) }
            .expect("缺参 set_paused 往返");
        assert_eq!(response, r#"{"ok":false,"error":"invalid request"}"#);

        // 超长行：8KB + 1 字节无换行 → 断开且无响应（读端得到空串或连接错误）
        let overlong = "x".repeat(MAX_REQUEST_BYTES + 1);
        let response = unsafe { transact(&overlong, deadline) }.unwrap_or_default();
        assert_eq!(response, "", "超长行必须断开且不响应");

        // 服务端仍活着：正常请求可继续往返
        let response = unsafe { transact(r#"{"cmd":"status"}"#, deadline) }.expect("恢复后 status");
        assert!(response.contains(r#""ok":true"#), "超长断开后服务端须继续服务: {response}");

        let response = unsafe { transact(r#"{"cmd":"shutdown"}"#, deadline) }.expect("shutdown 往返");
        assert_eq!(response, r#"{"ok":true,"data":null}"#);
        server.join().expect("服务线程应随 shutdown 正常退出");
    }

    /// 当前进程令牌的用户 SID 原始字节（自相对 SID 结构拷贝）。
    unsafe fn current_user_sid_bytes() -> Vec<u8> {
        let mut token = HANDLE::default();
        // SAFETY: token 由系统写出，函数结束前关闭。
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).expect("OpenProcessToken");
        }
        let mut len = 0u32;
        // SAFETY: token 有效；空缓冲探测长度。
        unsafe {
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        }
        let mut buf = vec![0u64; len as usize / 8 + 1];
        // SAFETY: buf 对齐且容量覆盖 len 字节。
        unsafe {
            GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len)
                .expect("GetTokenInformation");
        }
        // SAFETY: token 已查询完毕，关闭。
        unsafe {
            let _ = CloseHandle(token);
        }
        // SAFETY: buf 刚被写入 TOKEN_USER。
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        // SAFETY: user.User.Sid 是 buf 内的有效 SID。
        let sid_len = unsafe { GetLengthSid(user.User.Sid) } as usize;
        // SAFETY: 从有效 SID 起始处读取 GetLengthSid 个字节。
        let bytes = unsafe { std::slice::from_raw_parts(user.User.Sid.0 as *const u8, sid_len) }.to_vec();
        bytes
    }

    /// 显式 DACL 实证：管道对象的安全描述符必须是
    /// 「受保护 DACL（P）+ 恰一条允许型 ACE + 该 ACE 的 SID == 当前用户 SID」。
    #[test]
    fn dacl_is_protected_and_limited_to_current_user() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let flags = Arc::new(Flags::default());
        let status = Arc::new(RuntimeStatus::new("0.1.0"));
        let server = spawn(flags.clone(), status.clone());

        let client = unsafe { open_client(Instant::now() + Duration::from_secs(10)) };
        // SAFETY: client 已连入；查询其内核对象 DACL。
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
        let err = unsafe {
            GetSecurityInfo(
                client,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&mut dacl),
                None,
                Some(&mut sd),
            )
        };
        assert_eq!(err, WIN32_ERROR(0), "GetSecurityInfo 失败: {}", err.0);
        assert!(!dacl.is_null(), "必须有 DACL");

        // 恰一条 ACE（受保护 DACL 不允许继承项混入）
        let ace_count = unsafe { (*dacl).AceCount };
        assert_eq!(ace_count, 1, "显式 DACL 必须恰含一条 ACE，实际 {ace_count} 条");
        let mut ace_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: dacl 有效，索引 0 < AceCount。
        unsafe {
            GetAce(dacl, 0, &mut ace_ptr).expect("GetAce");
        }
        // SAFETY: GetAce 返回指向 ACL 内 ACCESS_ALLOWED_ACE 的指针。
        let ace = unsafe { &*(ace_ptr as *const ACCESS_ALLOWED_ACE) };
        // winnt.h 的 ACCESS_ALLOWED_ACE_TYPE == 0（常量位于 windows crate 未启用 feature 的
        // Win32::System::SystemServices，按 ABI 字面值比较）
        assert_eq!(ace.Header.AceType, 0, "必须为允许型（ACCESS_ALLOWED_ACE）");

        // ACE SID 必须等于当前用户 SID（运行时解析的正是同一来源：进程令牌）
        let current = unsafe { current_user_sid_bytes() };
        assert!(!current.is_empty(), "当前用户 SID 必须可解析");
        // SAFETY: current 持有完整 SID 结构；ace.SidStart 的地址即 ACE 内 SID 的起始。
        unsafe {
            EqualSid(
                PSID(&ace.SidStart as *const u32 as *mut core::ffi::c_void),
                PSID(current.as_ptr() as *mut core::ffi::c_void),
            )
            .expect("ACE 的 SID 必须等于当前用户 SID");
        }

        // DACL 受保护（SDDL 中的 P 标志）
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: sd 由 GetSecurityInfo 返回且在断言期间有效。
        unsafe {
            GetSecurityDescriptorControl(sd, &mut control, &mut revision).expect("GetSecurityDescriptorControl");
        }
        assert_ne!(control & SE_DACL_PROTECTED.0, 0, "DACL 必须受保护（P 标志）");

        // SAFETY: 结束探测连接；随后 shutdown 收尾。
        unsafe {
            let _ = CloseHandle(client);
        }
        let response = unsafe { transact(r#"{"cmd":"shutdown"}"#, Instant::now() + Duration::from_secs(10)) }
            .expect("shutdown 往返");
        assert_eq!(response, r#"{"ok":true,"data":null}"#);
        server.join().expect("服务线程应随 shutdown 正常退出");
    }
}
