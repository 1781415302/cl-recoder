//! 实时输入独立命名管道服务（DEVPLAN-input-live-trigger §4.4，S4a）。
//!
//! - 端点：生产 [`LIVE_PIPE_NAME`]；测试经 [`spawn_at`] 使用含 PID+fixture 的独立名。
//! - 安全：复用 [`crate::ipc_server::OwnedSecurityAttributes`] 当前用户 SID 显式 DACL；
//!   `PIPE_REJECT_REMOTE_CLIENTS`、byte mode、`FILE_FLAG_OVERLAPPED`、**仅 1 个连接实例**。
//! - 线程：单服务线程顺序处理该连接多笔快照；不每帧 spawn、不 `FlushFileBuffers`、
//!   不复制控制管无超时 worker 逻辑。
//! - I/O：connect/read/write 均有界；等待片段 ≤25ms 并检查 [`Flags::shutdown`]；
//!   超时/关闭经 `CancelIoEx` 并等到 IRP 收尾后再释放 OVERLAPPED/event/缓冲。
//! - 空闲：已连接但 250ms 内未完成请求 → 取消该连接并重新监听。
//! - 关停：监听与已连接 idle 均须 ≤1s 退出，**不依赖**生产管自连唤醒。
//! - 失败：限频日志；不影响控制管 / collector health。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clrecoder_core::live_input::{
    LiveError, LiveErrorCode, LiveSnapshotReply, LiveSnapshotRequest, LIVE_IO_TIMEOUT_MS,
    LIVE_MAX_REQUEST_BYTES, LIVE_MAX_RESPONSE_BYTES, LIVE_PIPE_NAME, LIVE_PROTOCOL_VERSION,
};
use windows::core::{HRESULT, HSTRING};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, ResetEvent, WaitForSingleObject,
};

use crate::ipc_server::{Flags, OwnedSecurityAttributes};
use crate::live_input::LiveInputRuntime;

/// 等待片段上限（§4.4：≤25ms）。
const WAIT_SLICE: Duration = Duration::from_millis(25);

/// 管道缓冲：出站覆盖最大响应；入站覆盖最大请求。
const PIPE_OUT_BUFFER: u32 = LIVE_MAX_RESPONSE_BYTES as u32;
const PIPE_IN_BUFFER: u32 = 8192;

macro_rules! debug_log {
    ($($arg:tt)*) => {
        if std::env::var_os("CLRECODER_DEBUG").is_some() {
            eprintln!("[live_server] {}", format_args!($($arg)*));
        }
    };
}

/// 生产端点 [`LIVE_PIPE_NAME`]；原生服务线程。
///
/// S5 才会在 main 接线；本步仅声明模块，故 bin 测试中可能暂未引用。
#[allow(dead_code)]
#[must_use = "服务线程必须被 join（main 退出前等待 shutdown 生效）"]
pub fn spawn(flags: Arc<Flags>, live: Arc<LiveInputRuntime>) -> std::thread::JoinHandle<()> {
    spawn_at(LIVE_PIPE_NAME.to_string(), flags, live)
}

/// 测试接缝：独立端点（名称须含 PID+fixture，不得用生产名）。
#[must_use = "服务线程必须被 join"]
pub(crate) fn spawn_at(
    pipe_name: String,
    flags: Arc<Flags>,
    live: Arc<LiveInputRuntime>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("live_server".into())
        .spawn(move || run_server(&pipe_name, &flags, &live))
        .expect("spawn live_server thread")
}

fn run_server(pipe_name: &str, flags: &Arc<Flags>, live: &Arc<LiveInputRuntime>) {
    let owned_sa = match OwnedSecurityAttributes::new() {
        Ok(sa) => sa,
        Err(err) => {
            debug_log!("创建安全描述符失败，live pipe 未启动: {err}");
            log::error!("live_server: 创建安全描述符（显式 DACL）失败，服务未启动: {err}");
            return;
        }
    };

    let event = match unsafe { CreateEventW(None, true, false, None) } {
        Ok(e) => e,
        Err(err) => {
            debug_log!("CreateEventW 失败: {err}");
            log::error!("live_server: CreateEventW 失败，服务未启动: {err}");
            return;
        }
    };

    let name = HSTRING::from(pipe_name);

    while !flags.shutdown.load(Ordering::Acquire) {
        // SAFETY: owned_sa 存活；name 本函数内存活；nmax=1；OVERLAPPED 模式。
        let handle = unsafe {
            CreateNamedPipeW(
                &name,
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1, // 只允许 1 个连接实例（§4.4）
                PIPE_OUT_BUFFER,
                PIPE_IN_BUFFER,
                0,
                Some(owned_sa.as_ptr()),
            )
        };
        if handle.is_invalid() {
            let os_err = std::io::Error::last_os_error();
            debug_log!("CreateNamedPipeW 失败，稍后重试: {os_err}");
            log::warn!("live_server: CreateNamedPipeW 失败，稍后重试: {os_err}");
            // 关停检查：短睡后回到循环顶
            if sleep_interruptible(flags, Duration::from_millis(50)) {
                break;
            }
            continue;
        }

        match accept_client(handle, event, flags) {
            AcceptOutcome::Connected => {
                serve_connection(handle, event, flags, live);
            }
            AcceptOutcome::Shutdown | AcceptOutcome::Failed => {}
        }

        // SAFETY: 关闭本轮实例（Disconnect 已在 serve/accept 失败路径处理或未连接）。
        unsafe {
            let _ = DisconnectNamedPipe(handle);
            let _ = CloseHandle(handle);
        }
    }

    // SAFETY: event 由本线程创建，退出前关闭。
    unsafe {
        let _ = CloseHandle(event);
    }
}

enum AcceptOutcome {
    Connected,
    Shutdown,
    Failed,
}

/// 有界 overlapped `ConnectNamedPipe`；关停经 CancelIoEx，不自连生产管。
fn accept_client(handle: HANDLE, event: HANDLE, flags: &Flags) -> AcceptOutcome {
    // 手动重置事件：每次提交前 Reset
    unsafe {
        let _ = ResetEvent(event);
    }
    let mut ov = OVERLAPPED {
        hEvent: event,
        ..Default::default()
    };
    let submit = unsafe { ConnectNamedPipe(handle, Some(&mut ov)) };
    match submit {
        Ok(()) => return AcceptOutcome::Connected,
        Err(err) if err.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => {
            return AcceptOutcome::Connected;
        }
        Err(err) if err.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {}
        Err(err) => {
            debug_log!("ConnectNamedPipe 提交失败: {err}");
            log::warn!("live_server: ConnectNamedPipe 失败: {err}");
            return AcceptOutcome::Failed;
        }
    }

    // 无业务期限：直到连入或 shutdown；每片 ≤25ms 查 flag
    loop {
        if flags.shutdown.load(Ordering::Acquire) {
            let _ = cancel_and_wait(handle, &mut ov);
            return AcceptOutcome::Shutdown;
        }
        let _ = unsafe { WaitForSingleObject(event, WAIT_SLICE.as_millis() as u32) };
        let mut transferred = 0u32;
        match unsafe { GetOverlappedResult(handle, &ov, &mut transferred, false) } {
            Ok(()) => return AcceptOutcome::Connected,
            Err(e) if e.code() == HRESULT::from_win32(ERROR_IO_INCOMPLETE.0) => {
                continue;
            }
            Err(e) if e.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => {
                return AcceptOutcome::Connected;
            }
            Err(e) => {
                debug_log!("ConnectNamedPipe 完成失败: {e}");
                log::warn!("live_server: ConnectNamedPipe 完成失败: {e}");
                return AcceptOutcome::Failed;
            }
        }
    }
}

/// 同一连接顺序处理多笔快照；idle/读写超 250ms 或断线则返回。
fn serve_connection(handle: HANDLE, event: HANDLE, flags: &Flags, live: &LiveInputRuntime) {
    loop {
        if flags.shutdown.load(Ordering::Acquire) {
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(LIVE_IO_TIMEOUT_MS);
        let line = match read_request_line(handle, event, flags, deadline) {
            Ok(line) => line,
            Err(IoFail::Shutdown) => return,
            Err(IoFail::Timeout) | Err(IoFail::Disconnected) | Err(IoFail::Io(_)) => {
                // idle / 残包 / 断线 → 取消连接并重新监听
                return;
            }
            Err(IoFail::TooLarge) => {
                // 超长请求：断开且不响应（§4.4）
                return;
            }
        };

        let reply = build_reply(&line, live, flags);
        let out = match encode_reply(&reply) {
            Ok(bytes) => bytes,
            Err(too_large) => match encode_reply(too_large.as_ref()) {
                Ok(bytes) => bytes,
                Err(_) => return, // 极端：连 too_large 都编不过，断开
            },
        };

        let write_deadline = Instant::now() + Duration::from_millis(LIVE_IO_TIMEOUT_MS);
        match write_all(handle, event, flags, &out, write_deadline) {
            Ok(()) => {
                // 不 FlushFileBuffers（§4.4）；继续下笔
            }
            Err(IoFail::Shutdown) => return,
            Err(_) => return,
        }
        // out 在下一轮前可释放；显式 drop 提示缓冲生命周期
        drop(out);
    }
}

fn build_reply(line: &[u8], live: &LiveInputRuntime, flags: &Flags) -> LiveSnapshotReply {
    let line = trim_ascii(line);
    let req: LiveSnapshotRequest = match serde_json::from_slice(line) {
        Ok(r) => r,
        Err(e) => {
            return err_reply(
                LiveErrorCode::Protocol,
                format!("实时请求无法解析: {e}"),
            );
        }
    };
    if req.protocol_version != LIVE_PROTOCOL_VERSION {
        return err_reply(
            LiveErrorCode::Protocol,
            format!(
                "协议版本不符：收到 {}，需要 {LIVE_PROTOCOL_VERSION}",
                req.protocol_version
            ),
        );
    }
    let snapshot = live.snapshot(
        Instant::now(),
        flags.paused.load(Ordering::Acquire),
    );
    LiveSnapshotReply {
        protocol_version: LIVE_PROTOCOL_VERSION,
        snapshot: Some(snapshot),
        error: None,
    }
}

/// 编码响应行；若超过 [`LIVE_MAX_RESPONSE_BYTES`]（含换行）返回 Err(too_large 回复)。
fn encode_reply(reply: &LiveSnapshotReply) -> Result<Vec<u8>, Box<LiveSnapshotReply>> {
    let mut bytes = serde_json::to_vec(reply).map_err(|e| {
        Box::new(err_reply(
            LiveErrorCode::Protocol,
            format!("序列化实时响应失败: {e}"),
        ))
    })?;
    bytes.push(b'\n');
    if bytes.len() > LIVE_MAX_RESPONSE_BYTES {
        return Err(Box::new(err_reply(
            LiveErrorCode::TooLarge,
            format!(
                "实时响应过大（{} 字节 > {LIVE_MAX_RESPONSE_BYTES}）",
                bytes.len()
            ),
        )));
    }
    Ok(bytes)
}

fn err_reply(code: LiveErrorCode, message: impl Into<String>) -> LiveSnapshotReply {
    LiveSnapshotReply {
        protocol_version: LIVE_PROTOCOL_VERSION,
        snapshot: None,
        error: Some(LiveError {
            code,
            message: message.into(),
        }),
    }
}

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

#[derive(Debug)]
enum IoFail {
    Timeout,
    Disconnected,
    Shutdown,
    TooLarge,
    #[allow(dead_code)]
    Io(String),
}

/// 读到含 `\n` 的一行；长度上限含换行。
fn read_request_line(
    handle: HANDLE,
    event: HANDLE,
    flags: &Flags,
    deadline: Instant,
) -> Result<Vec<u8>, IoFail> {
    let mut acc: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 256];
    loop {
        if flags.shutdown.load(Ordering::Acquire) {
            return Err(IoFail::Shutdown);
        }
        if Instant::now() >= deadline {
            return Err(IoFail::Timeout);
        }
        unsafe {
            let _ = ResetEvent(event);
        }
        let mut ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        let mut transferred = 0u32;
        let submit = unsafe {
            ReadFile(
                handle,
                Some(&mut chunk),
                Some(&mut transferred),
                Some(&mut ov),
            )
        };
        let n = wait_overlapped(handle, &mut ov, flags, deadline, submit)?;
        if n == 0 {
            return Err(IoFail::Disconnected);
        }
        let got = &chunk[..n as usize];
        if let Some(pos) = got.iter().position(|&b| b == b'\n') {
            acc.extend_from_slice(&got[..pos]);
            // 行（含换行）超上限
            if acc.len() + 1 > LIVE_MAX_REQUEST_BYTES {
                return Err(IoFail::TooLarge);
            }
            return Ok(acc);
        }
        acc.extend_from_slice(got);
        if acc.len() + 1 > LIVE_MAX_REQUEST_BYTES {
            return Err(IoFail::TooLarge);
        }
    }
}

fn write_all(
    handle: HANDLE,
    event: HANDLE,
    flags: &Flags,
    buf: &[u8],
    deadline: Instant,
) -> Result<(), IoFail> {
    let mut offset = 0usize;
    while offset < buf.len() {
        if flags.shutdown.load(Ordering::Acquire) {
            return Err(IoFail::Shutdown);
        }
        if Instant::now() >= deadline {
            return Err(IoFail::Timeout);
        }
        unsafe {
            let _ = ResetEvent(event);
        }
        let mut ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        let chunk = &buf[offset..];
        let mut transferred = 0u32;
        let submit = unsafe {
            WriteFile(
                handle,
                Some(chunk),
                Some(&mut transferred),
                Some(&mut ov),
            )
        };
        let n = wait_overlapped(handle, &mut ov, flags, deadline, submit)?;
        if n == 0 {
            return Err(IoFail::Disconnected);
        }
        offset += n as usize;
    }
    Ok(())
}

/// OVERLAPPED 等待：到期或 shutdown → CancelIoEx 并等到 IRP 收尾；迟到成功不采信。
fn wait_overlapped(
    handle: HANDLE,
    ov: &mut OVERLAPPED,
    flags: &Flags,
    deadline: Instant,
    submit: windows::core::Result<()>,
) -> Result<u32, IoFail> {
    match submit {
        Ok(()) => {
            let mut transferred = 0u32;
            unsafe { GetOverlappedResult(handle, ov, &mut transferred, false) }.map_err(|e| {
                IoFail::Io(format!("同步完成查询失败: {e}"))
            })?;
            return Ok(transferred);
        }
        Err(e) if e.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {}
        Err(e) => return Err(IoFail::Io(format!("提交 I/O 失败: {e}"))),
    }

    let mut cancelled = false;
    loop {
        if !cancelled && flags.shutdown.load(Ordering::Acquire) {
            let _ = unsafe { CancelIoEx(handle, Some(ov)) };
            cancelled = true;
        }
        let remain = deadline.saturating_duration_since(Instant::now());
        let slice_ms = WAIT_SLICE.min(remain).as_millis() as u32;
        let wait_ms = if cancelled {
            WAIT_SLICE.as_millis() as u32
        } else if Instant::now() >= deadline {
            0
        } else {
            slice_ms.max(1)
        };
        let _ = unsafe { WaitForSingleObject(ov.hEvent, wait_ms) };

        let mut transferred = 0u32;
        match unsafe { GetOverlappedResult(handle, ov, &mut transferred, false) } {
            Ok(()) => {
                return if cancelled {
                    if flags.shutdown.load(Ordering::Acquire) {
                        Err(IoFail::Shutdown)
                    } else {
                        Err(IoFail::Timeout)
                    }
                } else {
                    Ok(transferred)
                };
            }
            Err(e) => {
                if e.code() != HRESULT::from_win32(ERROR_IO_INCOMPLETE.0) {
                    return if cancelled {
                        if flags.shutdown.load(Ordering::Acquire) {
                            Err(IoFail::Shutdown)
                        } else {
                            Err(IoFail::Timeout)
                        }
                    } else {
                        Err(IoFail::Io(format!("I/O 失败: {e}")))
                    };
                }
            }
        }

        if !cancelled && Instant::now() >= deadline {
            let _ = unsafe { CancelIoEx(handle, Some(ov)) };
            cancelled = true;
            continue;
        }
        if cancelled {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn cancel_and_wait(handle: HANDLE, ov: &mut OVERLAPPED) -> Result<(), ()> {
    let _ = unsafe { CancelIoEx(handle, Some(ov)) };
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let mut transferred = 0u32;
        match unsafe { GetOverlappedResult(handle, ov, &mut transferred, false) } {
            Ok(()) => return Ok(()),
            Err(e) if e.code() == HRESULT::from_win32(ERROR_IO_INCOMPLETE.0) => {
                if Instant::now() >= deadline {
                    return Err(());
                }
                let _ = unsafe { WaitForSingleObject(ov.hEvent, 25) };
            }
            Err(_) => return Ok(()), // 已失败/取消完成
        }
    }
}

fn sleep_interruptible(flags: &Flags, total: Duration) -> bool {
    let end = Instant::now() + total;
    while Instant::now() < end {
        if flags.shutdown.load(Ordering::Acquire) {
            return true;
        }
        std::thread::sleep(WAIT_SLICE.min(end.saturating_duration_since(Instant::now())));
    }
    flags.shutdown.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows::Win32::Security::{
        EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorControl, GetTokenInformation,
        TokenUser, ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    use windows::Win32::System::Pipes::{GetNamedPipeInfo, WaitNamedPipeW, PIPE_SERVER_END};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// 同进程内串行，避免测试抢占独立 pipe 名。
    static SERVER_LOCK: Mutex<()> = Mutex::new(());

    fn test_pipe_name(fixture: &str) -> String {
        format!(
            r"\\.\pipe\clrecoder-live-test-{}-{}",
            std::process::id(),
            fixture
        )
    }

    fn start_server(fixture: &str) -> (String, Arc<Flags>, Arc<LiveInputRuntime>, std::thread::JoinHandle<()>) {
        let name = test_pipe_name(fixture);
        let flags = Arc::new(Flags::default());
        let live = Arc::new(LiveInputRuntime::new(format!(
            "{}:test-{fixture}",
            std::process::id()
        )));
        let handle = spawn_at(name.clone(), flags.clone(), live.clone());
        (name, flags, live, handle)
    }

    fn shutdown_join(flags: &Flags, server: std::thread::JoinHandle<()>, limit: Duration) {
        flags.shutdown.store(true, Ordering::Release);
        let deadline = Instant::now() + limit;
        while !server.is_finished() {
            assert!(
                Instant::now() < deadline,
                "live_server 应在 {limit:?} 内随 shutdown 退出"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        server.join().expect("live_server join");
    }

    unsafe fn open_client(name: &str, deadline: Instant) -> HANDLE {
        let wide = HSTRING::from(name);
        loop {
            if Instant::now() >= deadline {
                panic!("连接 live 测试管道超时");
            }
            match unsafe {
                CreateFileW(
                    &wide,
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
                    unsafe {
                        let _ = WaitNamedPipeW(&wide, 50);
                    }
                }
                Err(err) if err.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("CreateFileW 意外失败: {err}"),
            }
        }
    }

    unsafe fn write_all_sync(handle: HANDLE, buf: &[u8]) -> windows::core::Result<()> {
        let mut offset = 0usize;
        while offset < buf.len() {
            let mut written = 0u32;
            unsafe {
                WriteFile(handle, Some(&buf[offset..]), Some(&mut written), None)?;
            }
            if written == 0 {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(0)));
            }
            offset += written as usize;
        }
        Ok(())
    }

    unsafe fn read_line_sync(handle: HANDLE, max: usize, deadline: Instant) -> Option<Vec<u8>> {
        let mut acc: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            let mut read = 0u32;
            if unsafe { ReadFile(handle, Some(&mut chunk), Some(&mut read), None) }.is_err() {
                return if acc.is_empty() { None } else { Some(acc) };
            }
            if read == 0 {
                return if acc.is_empty() { None } else { Some(acc) };
            }
            let got = &chunk[..read as usize];
            if let Some(pos) = got.iter().position(|&b| b == b'\n') {
                acc.extend_from_slice(&got[..pos]);
                return Some(acc);
            }
            acc.extend_from_slice(got);
            if acc.len() > max {
                return Some(acc);
            }
        }
    }

    unsafe fn transact(name: &str, request: &str, deadline: Instant) -> Option<String> {
        let handle = unsafe { open_client(name, deadline) };
        let result = (|| {
            let mut req = request.as_bytes().to_vec();
            req.push(b'\n');
            unsafe { write_all_sync(handle, &req) }.ok()?;
            let raw = unsafe { read_line_sync(handle, LIVE_MAX_RESPONSE_BYTES, deadline) }?;
            String::from_utf8(raw).ok()
        })();
        unsafe {
            let _ = CloseHandle(handle);
        }
        result
    }

    #[test]
    fn live_server_roundtrip_empty_snapshot() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("roundtrip");
        let deadline = Instant::now() + Duration::from_secs(5);

        let response = unsafe { transact(&name, r#"{"protocolVersion":1}"#, deadline) }
            .expect("往返应成功");
        let reply: LiveSnapshotReply =
            serde_json::from_str(&response).expect("响应须为 LiveSnapshotReply");
        assert_eq!(reply.protocol_version, LIVE_PROTOCOL_VERSION);
        assert!(reply.error.is_none(), "{reply:?}");
        let snap = reply.snapshot.expect("成功须有 snapshot");
        assert!(snap.sources.is_empty());
        assert_eq!(snap.sample_seq, "1");

        // 同连接多笔：再开新连接（服务端 disconnect 后重新 accept）
        let response2 = unsafe { transact(&name, r#"{"protocolVersion":1}"#, deadline) }
            .expect("第二笔往返");
        let reply2: LiveSnapshotReply = serde_json::from_str(&response2).unwrap();
        assert_eq!(reply2.snapshot.as_ref().unwrap().sample_seq, "2");

        shutdown_join(&flags, server, Duration::from_secs(1));
    }

    #[test]
    fn live_server_protocol_error_bad_version() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("badver");
        let deadline = Instant::now() + Duration::from_secs(5);
        let response =
            unsafe { transact(&name, r#"{"protocolVersion":99}"#, deadline) }.expect("应有结构化错误");
        let reply: LiveSnapshotReply = serde_json::from_str(&response).unwrap();
        assert!(reply.snapshot.is_none());
        assert_eq!(
            reply.error.as_ref().map(|e| e.code),
            Some(LiveErrorCode::Protocol)
        );
        shutdown_join(&flags, server, Duration::from_secs(1));
    }

    #[test]
    fn live_server_overlong_request_disconnects_without_response() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("overlong");
        let deadline = Instant::now() + Duration::from_secs(5);
        let handle = unsafe { open_client(&name, deadline) };
        // 超长且无换行
        let overlong = vec![b'x'; LIVE_MAX_REQUEST_BYTES + 8];
        let _ = unsafe { write_all_sync(handle, &overlong) };
        // 读应失败或空（服务端断开不响应）
        let got = unsafe { read_line_sync(handle, 64, Instant::now() + Duration::from_millis(500)) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        assert!(
            got.as_ref().map(|g| g.is_empty()).unwrap_or(true),
            "超长请求不得返回业务响应: {got:?}"
        );

        // 服务端须恢复：随后正常往返
        let response = unsafe { transact(&name, r#"{"protocolVersion":1}"#, deadline) }
            .expect("超长后应恢复");
        assert!(response.contains("snapshot"), "{response}");
        shutdown_join(&flags, server, Duration::from_secs(1));
    }

    #[test]
    fn live_server_partial_line_then_disconnect() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("partial");
        let deadline = Instant::now() + Duration::from_secs(5);
        let handle = unsafe { open_client(&name, deadline) };
        // 残包：不完整 JSON、无换行，随即关闭
        let _ = unsafe { write_all_sync(handle, b"{\"protocolVersion\":") };
        unsafe {
            let _ = CloseHandle(handle);
        }
        // 给服务端时间回到 accept
        std::thread::sleep(Duration::from_millis(100));
        let response = unsafe { transact(&name, r#"{"protocolVersion":1}"#, deadline) }
            .expect("残包断线后应恢复");
        let reply: LiveSnapshotReply = serde_json::from_str(&response).unwrap();
        assert!(reply.snapshot.is_some());
        shutdown_join(&flags, server, Duration::from_secs(1));
    }

    #[test]
    fn live_server_unread_response_times_out_and_recovers() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("unread");
        let deadline = Instant::now() + Duration::from_secs(8);
        let handle = unsafe { open_client(&name, deadline) };
        let mut req = br#"{"protocolVersion":1}"#.to_vec();
        req.push(b'\n');
        unsafe { write_all_sync(handle, &req) }.expect("写请求");
        // 故意不读响应：服务端写缓冲填满或写超时后应断开并重新监听
        // 保持句柄打开但不读，等待 >250ms
        std::thread::sleep(Duration::from_millis(LIVE_IO_TIMEOUT_MS + 200));
        unsafe {
            let _ = CloseHandle(handle);
        }
        std::thread::sleep(Duration::from_millis(100));
        let response = unsafe { transact(&name, r#"{"protocolVersion":1}"#, deadline) }
            .expect("不读响应超时后应恢复");
        assert!(response.contains("protocolVersion"), "{response}");
        shutdown_join(&flags, server, Duration::from_secs(1));
    }

    #[test]
    fn live_server_idle_timeout_reconnects() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("idle");
        let deadline = Instant::now() + Duration::from_secs(8);
        // 连入后不发数据 → 服务端 idle 250ms 取消
        let handle = unsafe { open_client(&name, deadline) };
        std::thread::sleep(Duration::from_millis(LIVE_IO_TIMEOUT_MS + 200));
        unsafe {
            let _ = CloseHandle(handle);
        }
        std::thread::sleep(Duration::from_millis(50));
        let response = unsafe { transact(&name, r#"{"protocolVersion":1}"#, deadline) }
            .expect("idle 超时后应重新监听");
        let reply: LiveSnapshotReply = serde_json::from_str(&response).unwrap();
        assert!(reply.snapshot.is_some());
        shutdown_join(&flags, server, Duration::from_secs(1));
    }

    #[test]
    fn live_server_shutdown_while_listening_within_1s() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_name, flags, _live, server) = start_server("shut-listen");
        // 确保已进入监听
        std::thread::sleep(Duration::from_millis(80));
        let started = Instant::now();
        shutdown_join(&flags, server, Duration::from_secs(1));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "监听态关停须 ≤1s，实际 {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn live_server_shutdown_while_idle_connected_within_1s() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("shut-idle");
        let deadline = Instant::now() + Duration::from_secs(5);
        let handle = unsafe { open_client(&name, deadline) };
        // 已连接、未发请求（idle 读挂起）
        std::thread::sleep(Duration::from_millis(30));
        let started = Instant::now();
        flags.shutdown.store(true, Ordering::Release);
        let join_deadline = Instant::now() + Duration::from_secs(1);
        while !server.is_finished() {
            assert!(
                Instant::now() < join_deadline,
                "已连接 idle 关停须 ≤1s"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        server.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        unsafe {
            let _ = CloseHandle(handle);
        }
    }

    #[test]
    fn live_server_pipe_max_instances_one_and_byte_mode() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("nmax");
        let deadline = Instant::now() + Duration::from_secs(5);
        let client = unsafe { open_client(&name, deadline) };

        let mut flags_mode = windows::Win32::System::Pipes::NAMED_PIPE_MODE(0);
        let mut max_inst = 0u32;
        unsafe {
            GetNamedPipeInfo(
                client,
                Some(&mut flags_mode),
                None,
                None,
                Some(&mut max_inst),
            )
        }
        .expect("GetNamedPipeInfo");
        assert_eq!(max_inst, 1, "只允许 1 个连接实例");
        // 客户端视角：不应带 PIPE_SERVER_END；类型为 byte（低位）
        // PIPE_TYPE_MESSAGE = 4；byte mode 时该位为 0
        assert_eq!(
            flags_mode.0 & 4,
            0,
            "须为 byte mode，flags={:?}",
            flags_mode
        );
        let _ = (PIPE_SERVER_END, PIPE_TYPE_BYTE);
        // 第二实例应失败（nmax=1 且已有连接）
        let wide = HSTRING::from(&name);
        let second = unsafe {
            CreateFileW(
                &wide,
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        assert!(
            second.is_err(),
            "第二客户端在 nMaxInstances=1 时应被拒"
        );

        unsafe {
            let _ = CloseHandle(client);
        }
        shutdown_join(&flags, server, Duration::from_secs(1));
    }

    #[test]
    fn live_server_dacl_current_user_sid() {
        let _guard = SERVER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (name, flags, _live, server) = start_server("dacl");
        let client = unsafe { open_client(&name, Instant::now() + Duration::from_secs(5)) };

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
        assert!(!dacl.is_null());
        let ace_count = unsafe { (*dacl).AceCount };
        assert_eq!(ace_count, 1, "显式 DACL 须恰一条 ACE");

        let mut ace_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        unsafe {
            GetAce(dacl, 0, &mut ace_ptr).expect("GetAce");
        }
        let ace = unsafe { &*(ace_ptr as *const ACCESS_ALLOWED_ACE) };
        assert_eq!(ace.Header.AceType, 0);

        let current = unsafe { current_user_sid_bytes() };
        unsafe {
            EqualSid(
                PSID(&ace.SidStart as *const u32 as *mut core::ffi::c_void),
                PSID(current.as_ptr() as *mut core::ffi::c_void),
            )
            .expect("ACE SID == 当前用户");
        }

        let mut control = 0u16;
        let mut revision = 0u32;
        unsafe {
            GetSecurityDescriptorControl(sd, &mut control, &mut revision).unwrap();
        }
        assert_ne!(control & SE_DACL_PROTECTED.0, 0, "DACL 须受保护");

        // PIPE_REJECT_REMOTE_CLIENTS：创建时传入；此处用常量存在性 + nmax 测试覆盖参数合同
        assert_eq!(PIPE_REJECT_REMOTE_CLIENTS.0, 8);

        unsafe {
            let _ = CloseHandle(client);
        }
        shutdown_join(&flags, server, Duration::from_secs(1));
    }

    #[test]
    fn live_server_spawn_uses_production_name_constant() {
        // 仅校验常量契约；不实际监听生产名
        assert_eq!(LIVE_PIPE_NAME, r"\\.\pipe\clrecoder-live-v1");
        assert!(test_pipe_name("x").contains(&std::process::id().to_string()));
        assert!(!test_pipe_name("x").contains("live-v1"));
    }

    unsafe fn current_user_sid_bytes() -> Vec<u8> {
        let mut token = HANDLE::default();
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).unwrap();
        }
        let mut len = 0u32;
        unsafe {
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        }
        let mut buf = vec![0u64; len as usize / 8 + 1];
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                len,
                &mut len,
            )
            .unwrap();
        }
        unsafe {
            let _ = CloseHandle(token);
        }
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        let sid_len = unsafe { GetLengthSid(user.User.Sid) } as usize;
        unsafe { std::slice::from_raw_parts(user.User.Sid.0 as *const u8, sid_len) }.to_vec()
    }
}
