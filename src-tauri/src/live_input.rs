//! GUI 实时输入客户端（DEVPLAN-input-live-trigger §4.4）。
//!
//! - [`LiveInputClient`]：固定/测试端点、`snapshot` 在同一 Mutex 内串行实际 I/O，
//!   至多缓存一条连接；总预算 [`LIVE_IO_TIMEOUT_MS`]；超时必须取消原生操作。
//! - Drop：取消/关闭当前连接，不修改 collector。
//! - Windows：`FILE_FLAG_OVERLAPPED` + `CancelIoEx`，等待片段 ≤25ms。
//! - 测试：独立 fake pipe（名含 PID+fixture）或不依赖生产管道的抽象传输；
//!   不连接生产 `LIVE_PIPE_NAME` / control 管。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use clrecoder_core::live_input::{
    LiveError, LiveErrorCode, LiveSnapshot, LiveSnapshotReply, LiveSnapshotRequest, LiveSource,
    LIVE_IO_TIMEOUT_MS, LIVE_MAX_REQUEST_BYTES, LIVE_MAX_RESPONSE_BYTES, LIVE_PIPE_NAME,
    LIVE_PROTOCOL_VERSION,
};

/// 等待片段上限（§4.4：≤25ms）。
const WAIT_SLICE: Duration = Duration::from_millis(25);

/// GUI 实时管道客户端：共享连接锁，可取消 I/O。
pub struct LiveInputClient {
    pipe_name: String,
    inner: Mutex<ClientInner>,
    /// 测试接缝：注入抽象传输（不碰生产/真实命名管道）。
    #[cfg(test)]
    test_transport: Option<std::sync::Arc<dyn TestTransport>>,
}

struct ClientInner {
    /// 缓存的已连接句柄（成功往返后保留；失败/Drop 清空）。
    cached: Option<PipeConn>,
}

impl LiveInputClient {
    /// 生产端点 [`LIVE_PIPE_NAME`]。
    pub fn new() -> Self {
        Self::new_at(LIVE_PIPE_NAME.to_string())
    }

    /// 测试/自定义端点（名称须含 PID+fixture，不得用生产名）。
    pub fn new_at(pipe_name: String) -> Self {
        Self {
            pipe_name,
            inner: Mutex::new(ClientInner { cached: None }),
            #[cfg(test)]
            test_transport: None,
        }
    }

    /// 拉取一帧快照：正常 IPC 失败也返回结构化 [`LiveSnapshotReply`]（不 Err）。
    /// 测试用：当前端点名（不暴露为 Tauri 命令）。
    #[cfg(test)]
    pub(crate) fn pipe_name_for_test(&self) -> &str {
        &self.pipe_name
    }

    pub fn snapshot(&self) -> LiveSnapshotReply {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let budget_end = Instant::now() + Duration::from_millis(LIVE_IO_TIMEOUT_MS);
        self.snapshot_locked(&mut guard, budget_end)
    }

    fn snapshot_locked(&self, inner: &mut ClientInner, budget_end: Instant) -> LiveSnapshotReply {
        #[cfg(test)]
        if let Some(ref transport) = self.test_transport {
            return snapshot_with_transport(transport.as_ref(), inner, budget_end);
        }

        #[cfg(windows)]
        {
            win::snapshot_named_pipe(&self.pipe_name, inner, budget_end)
        }
        #[cfg(not(windows))]
        {
            let _ = (&self.pipe_name, inner, budget_end);
            err_reply(
                LiveErrorCode::NotRunning,
                "实时输入不可用，请更新并启动采集器",
            )
        }
    }
}

impl Default for LiveInputClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LiveInputClient {
    fn drop(&mut self) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut conn) = guard.cached.take() {
            conn.cancel_close();
        }
    }
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

fn encode_request_line() -> Result<Vec<u8>, Box<LiveSnapshotReply>> {
    let req = LiveSnapshotRequest {
        protocol_version: LIVE_PROTOCOL_VERSION,
    };
    let mut line = serde_json::to_vec(&req).map_err(|e| {
        Box::new(err_reply(
            LiveErrorCode::Protocol,
            format!("序列化实时请求失败: {e}"),
        ))
    })?;
    line.push(b'\n');
    if line.len() > LIVE_MAX_REQUEST_BYTES {
        return Err(Box::new(err_reply(
            LiveErrorCode::Protocol,
            format!(
                "实时请求超长（{} 字节 > {LIVE_MAX_REQUEST_BYTES}）",
                line.len()
            ),
        )));
    }
    Ok(line)
}

/// 解析并校验 NDJSON 响应行（不含截断；过大已在读侧判 too_large）。
fn parse_and_validate_reply(bytes: &[u8]) -> LiveSnapshotReply {
    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t.trim_end_matches(['\r', '\n']),
        Err(e) => {
            return err_reply(LiveErrorCode::Protocol, format!("响应非 UTF-8: {e}"));
        }
    };
    let reply: LiveSnapshotReply = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            return err_reply(LiveErrorCode::Protocol, format!("响应解析失败: {e}"));
        }
    };
    validate_reply(reply)
}

fn validate_reply(reply: LiveSnapshotReply) -> LiveSnapshotReply {
    if reply.protocol_version != LIVE_PROTOCOL_VERSION {
        return err_reply(
            LiveErrorCode::Protocol,
            format!(
                "协议版本不符（got {} want {LIVE_PROTOCOL_VERSION}）",
                reply.protocol_version
            ),
        );
    }
    match (&reply.snapshot, &reply.error) {
        (Some(_), None) => {
            if let Some(ref snap) = reply.snapshot {
                if let Err(msg) = validate_snapshot_analogs(snap) {
                    return err_reply(LiveErrorCode::Protocol, msg);
                }
            }
            reply
        }
        (None, Some(_)) => reply,
        (None, None) => err_reply(LiveErrorCode::Protocol, "响应缺少 snapshot 与 error"),
        (Some(_), Some(_)) => err_reply(LiveErrorCode::Protocol, "响应同时含 snapshot 与 error"),
    }
}

fn validate_snapshot_analogs(snap: &LiveSnapshot) -> Result<(), String> {
    for src in &snap.sources {
        if let LiveSource::Gamepad { analog: Some(a), .. } = src {
            for (name, v) in [
                ("leftStick.x", a.left_stick.x),
                ("leftStick.y", a.left_stick.y),
                ("rightStick.x", a.right_stick.x),
                ("rightStick.y", a.right_stick.y),
                ("leftTrigger", a.left_trigger),
                ("rightTrigger", a.right_trigger),
            ] {
                if !v.is_finite() {
                    return Err(format!("模拟量 {name} 非有限"));
                }
            }
        }
    }
    Ok(())
}

/// 平台连接句柄（Windows 真管道；测试抽象亦可装箱）。
enum PipeConn {
    #[cfg(windows)]
    Win(win::WinPipeConn),
    #[cfg(test)]
    Test(Box<dyn TestConn>),
}

impl PipeConn {
    fn cancel_close(&mut self) {
        match self {
            #[cfg(windows)]
            PipeConn::Win(c) => c.cancel_close(),
            #[cfg(test)]
            PipeConn::Test(c) => c.cancel_close(),
        }
    }
}

// ---------------------------------------------------------------------------
// 测试抽象传输（Linux/Windows 均可；不依赖生产 collector）
// ---------------------------------------------------------------------------

#[cfg(test)]
trait TestConn: Send {
    fn write_all_deadline(&mut self, buf: &[u8], deadline: Instant) -> Result<(), IoFail>;
    fn read_line_deadline(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, IoFail>;
    fn cancel_close(&mut self);
}

#[cfg(test)]
trait TestTransport: Send + Sync {
    fn connect(&self, deadline: Instant) -> Result<Box<dyn TestConn>, IoFail>;
}

#[cfg(test)]
#[derive(Debug)]
enum IoFail {
    NotRunning,
    AccessDenied,
    Busy,
    Timeout,
    Io(String),
    #[allow(dead_code)]
    Protocol(String),
    TooLarge,
}

#[cfg(test)]
impl IoFail {
    fn into_reply(self) -> LiveSnapshotReply {
        match self {
            IoFail::NotRunning => err_reply(
                LiveErrorCode::NotRunning,
                "实时输入不可用，请更新并启动采集器",
            ),
            IoFail::AccessDenied => err_reply(LiveErrorCode::AccessDenied, "实时管道访问被拒"),
            IoFail::Busy => err_reply(LiveErrorCode::Busy, "实时管道忙"),
            IoFail::Timeout => err_reply(LiveErrorCode::Timeout, "实时输入请求超时"),
            IoFail::Io(m) => err_reply(LiveErrorCode::Io, m),
            IoFail::Protocol(m) => err_reply(LiveErrorCode::Protocol, m),
            IoFail::TooLarge => err_reply(LiveErrorCode::TooLarge, "实时响应过大"),
        }
    }
}

#[cfg(test)]
fn snapshot_with_transport(
    transport: &dyn TestTransport,
    inner: &mut ClientInner,
    budget_end: Instant,
) -> LiveSnapshotReply {
    let line = match encode_request_line() {
        Ok(l) => l,
        Err(r) => return *r,
    };

    // 尝试复用缓存连接
    if let Some(mut conn) = inner.cached.take() {
        match exchange_test_conn(&mut conn, &line, budget_end) {
            Ok(reply) => {
                inner.cached = Some(conn);
                return reply;
            }
            Err(fail) => {
                conn.cancel_close();
                // 失效缓存：预算内重连一次
                if matches!(fail, IoFail::Timeout) || Instant::now() >= budget_end {
                    return fail.into_reply();
                }
                // 其它失败走重连
            }
        }
    }

    if Instant::now() >= budget_end {
        return IoFail::Timeout.into_reply();
    }

    let mut conn = match transport.connect(budget_end) {
        Ok(c) => PipeConn::Test(c),
        Err(e) => return e.into_reply(),
    };

    match exchange_test_conn(&mut conn, &line, budget_end) {
        Ok(reply) => {
            inner.cached = Some(conn);
            reply
        }
        Err(fail) => {
            conn.cancel_close();
            fail.into_reply()
        }
    }
}

#[cfg(test)]
fn exchange_test_conn(
    conn: &mut PipeConn,
    line: &[u8],
    budget_end: Instant,
) -> Result<LiveSnapshotReply, IoFail> {
    let PipeConn::Test(c) = conn else {
        return Err(IoFail::Io("测试连接类型不符".into()));
    };
    c.write_all_deadline(line, budget_end)?;
    let raw = c.read_line_deadline(LIVE_MAX_RESPONSE_BYTES, budget_end)?;
    Ok(parse_and_validate_reply(&raw))
}

// ---------------------------------------------------------------------------
// Windows 命名管道客户端
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{HRESULT, HSTRING};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_IO_INCOMPLETE,
        ERROR_IO_PENDING, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows::Win32::System::Pipes::{
        SetNamedPipeHandleState, WaitNamedPipeW, PIPE_READMODE_BYTE,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    pub(super) struct WinPipeConn {
        handle: HANDLE,
        event: HANDLE,
    }

    // SAFETY: HANDLE 在 Windows 上跨线程传递合法（同一进程内）；ClientInner 的 Mutex
    // 保证同一时刻只有一个线程对连接做 I/O。
    unsafe impl Send for WinPipeConn {}

    impl WinPipeConn {
        pub(super) fn cancel_close(&mut self) {
            if self.handle.is_invalid() {
                return;
            }
            // SAFETY: self.handle/self.event 为本连接独占句柄；调用方保证无栈上悬垂 OVERLAPPED（wait_overlapped 已收尾 IRP）后 CancelIoEx+CloseHandle。
            unsafe {
                // 取消该句柄上所有未决 IO，再关闭；调用方保证无栈上悬垂 OVERLAPPED
                // （wait_overlapped 返回前已等到 IRP 收尾）。
                let _ = CancelIoEx(self.handle, None);
                let _ = CloseHandle(self.event);
                let _ = CloseHandle(self.handle);
                self.event = HANDLE::default();
                self.handle = HANDLE::default();
            }
        }

        fn write_all_deadline(&mut self, buf: &[u8], deadline: Instant) -> Result<(), IoKind> {
            let mut offset = 0usize;
            while offset < buf.len() {
                if Instant::now() >= deadline {
                    return Err(IoKind::Timeout);
                }
                let chunk = &buf[offset..];
                let mut ov = OVERLAPPED {
                    hEvent: self.event,
                    ..Default::default()
                };
                let mut transferred = 0u32;
                // SAFETY: handle 有效；源缓冲在调用期间存活；若 OVERLAPPED 则 ov.hEvent 有效且直至结果取回。
                let submit = unsafe {
                    WriteFile(
                        self.handle,
                        Some(chunk),
                        Some(&mut transferred),
                        Some(&mut ov),
                    )
                };
                // wine：连接未就绪时偶发失败——预算内短暂重试。
                if submit.is_err() && Instant::now() + Duration::from_millis(30) < deadline {
                    std::thread::sleep(Duration::from_millis(15));
                    continue;
                }
                let n = match wait_overlapped(self.handle, &mut ov, deadline, submit) {
                    Ok(n) => n as usize,
                    Err(IoKind::Io(m)) => {
                        return Err(IoKind::Io(format!("write: {m}")));
                    }
                    Err(e) => return Err(e),
                };
                if n == 0 {
                    return Err(IoKind::Io("写入返回 0 字节".into()));
                }
                offset += n;
            }
            Ok(())
        }

        fn read_line_deadline(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, IoKind> {
            let mut out: Vec<u8> = Vec::with_capacity(256);
            let mut one = [0u8; 1];
            loop {
                if Instant::now() >= deadline {
                    return Err(IoKind::Timeout);
                }
                let mut ov = OVERLAPPED {
                    hEvent: self.event,
                    ..Default::default()
                };
                let mut transferred = 0u32;
                // SAFETY: handle 有效；目标缓冲在调用期间存活；若 OVERLAPPED 则 ov.hEvent 有效且直至结果取回。
                let submit = unsafe {
                    ReadFile(
                        self.handle,
                        Some(&mut one),
                        Some(&mut transferred),
                        Some(&mut ov),
                    )
                };
                let n = match wait_overlapped(self.handle, &mut ov, deadline, submit) {
                    Ok(n) => n as usize,
                    Err(IoKind::Io(m)) => {
                        return Err(IoKind::Io(format!("read: {m}")));
                    }
                    Err(e) => return Err(e),
                };
                if n == 0 {
                    return Err(IoKind::Io("管道在完整响应前关闭".into()));
                }
                out.push(one[0]);
                if one[0] == b'\n' {
                    break;
                }
                if out.len() > max {
                    return Err(IoKind::TooLarge);
                }
            }
            Ok(out)
        }
    }

    impl Drop for WinPipeConn {
        fn drop(&mut self) {
            if !self.handle.is_invalid() {
                self.cancel_close();
            }
        }
    }

    #[derive(Debug)]
    pub(super) enum IoKind {
        NotRunning,
        AccessDenied,
        Busy,
        Timeout,
        Io(String),
        TooLarge,
    }

    impl IoKind {
        fn into_reply(self) -> LiveSnapshotReply {
            match self {
                IoKind::NotRunning => err_reply(
                    LiveErrorCode::NotRunning,
                    "实时输入不可用，请更新并启动采集器",
                ),
                IoKind::AccessDenied => {
                    err_reply(LiveErrorCode::AccessDenied, "实时管道访问被拒")
                }
                IoKind::Busy => err_reply(LiveErrorCode::Busy, "实时管道忙"),
                IoKind::Timeout => err_reply(LiveErrorCode::Timeout, "实时输入请求超时"),
                IoKind::Io(m) => err_reply(LiveErrorCode::Io, m),
                IoKind::TooLarge => err_reply(LiveErrorCode::TooLarge, "实时响应过大"),
            }
        }
    }

    /// OVERLAPPED 等待：到期 CancelIoEx 并等到 IRP 收尾；迟到成功不采信。
    fn wait_overlapped(
        handle: HANDLE,
        ov: &mut OVERLAPPED,
        deadline: Instant,
        submit: windows::core::Result<()>,
    ) -> Result<u32, IoKind> {
        match submit {
            Ok(()) => {
                // 同步完成：transferred 已由 API 填好，再查一次确认
                let mut transferred = 0u32;
                // SAFETY: handle 与 ov 对应同一次未完成/已完成 IRP；ov.hEvent 仍有效；transferred 为本栈变量。
                unsafe { GetOverlappedResult(handle, ov, &mut transferred, false) }.map_err(
                    |e| IoKind::Io(format!("同步完成查询失败: {e}")),
                )?;
                return Ok(transferred);
            }
            Err(e) if e.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {}
            Err(e) => return Err(IoKind::Io(format!("提交 I/O 失败: {e}"))),
        }

        let mut cancelled = false;
        loop {
            let slice_ms = WAIT_SLICE
                .min(deadline.saturating_duration_since(Instant::now()))
                .as_millis() as u32;
            // 即使已到期也要进循环以便 CancelIoEx；slice=0 时仍轮询一次
            let wait_ms = if cancelled {
                WAIT_SLICE.as_millis() as u32
            } else if Instant::now() >= deadline {
                0
            } else {
                slice_ms.max(1)
            };
            // SAFETY: 等待对象为本路径持有的有效事件句柄；超时切片内句柄不被关闭。
            let _ = unsafe { WaitForSingleObject(ov.hEvent, wait_ms) };

            let mut transferred = 0u32;
            // SAFETY: handle 与 ov 对应同一次未完成/已完成 IRP；ov.hEvent 仍有效；transferred 为本栈变量。
            match unsafe { GetOverlappedResult(handle, ov, &mut transferred, false) } {
                Ok(()) => {
                    return if cancelled {
                        Err(IoKind::Timeout)
                    } else {
                        Ok(transferred)
                    };
                }
                Err(e) => {
                    if e.code() != HRESULT::from_win32(ERROR_IO_INCOMPLETE.0) {
                        return if cancelled {
                            Err(IoKind::Timeout)
                        } else {
                            Err(IoKind::Io(format!("I/O 失败: {e}")))
                        };
                    }
                }
            }

            if !cancelled && Instant::now() >= deadline {
                // SAFETY: handle 有效；ov 指向该句柄上未决 IRP（或 None 取消全部）；随后仍须 GetOverlappedResult/等待收尾。
                let _ = unsafe { CancelIoEx(handle, Some(ov)) };
                cancelled = true;
                continue;
            }
            if cancelled {
                // 取消后继续等到收尾（无业务期限）
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    fn open_pipe(name: &str, deadline: Instant) -> Result<WinPipeConn, IoKind> {
        if Instant::now() >= deadline {
            return Err(IoKind::Timeout);
        }
        let wide = HSTRING::from(name);
        // SAFETY: wide/name 在调用期间存活；成功句柄由本路径 CloseHandle 或移交包装类型。
        let open = || unsafe {
            CreateFileW(
                &wide,
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };

        let handle = match open() {
            Ok(h) => h,
            Err(e) => {
                let code = e.code();
                if code == ERROR_FILE_NOT_FOUND.to_hresult() {
                    return Err(IoKind::NotRunning);
                }
                if code == ERROR_ACCESS_DENIED.to_hresult() {
                    return Err(IoKind::AccessDenied);
                }
                if code == ERROR_PIPE_BUSY.to_hresult() {
                    let remain = deadline.saturating_duration_since(Instant::now());
                    if remain.is_zero() {
                        return Err(IoKind::Timeout);
                    }
                    let wait_ms = remain.as_millis().min(u32::MAX as u128) as u32;
                    // SAFETY: wide/name 在调用期间存活；仅等待现有实例，不持有返回句柄。
                    if !unsafe { WaitNamedPipeW(&wide, wait_ms) }.as_bool() {
                        return if Instant::now() >= deadline {
                            Err(IoKind::Timeout)
                        } else {
                            Err(IoKind::Busy)
                        };
                    }
                    if Instant::now() >= deadline {
                        return Err(IoKind::Timeout);
                    }
                    open().map_err(|e2| {
                        let c = e2.code();
                        if c == ERROR_FILE_NOT_FOUND.to_hresult() {
                            IoKind::NotRunning
                        } else if c == ERROR_ACCESS_DENIED.to_hresult() {
                            IoKind::AccessDenied
                        } else if c == ERROR_PIPE_BUSY.to_hresult() {
                            IoKind::Busy
                        } else {
                            IoKind::Io(format!("重连实时管道失败: {e2}"))
                        }
                    })?
                } else {
                    return Err(IoKind::Io(format!("连接实时管道失败: {e}")));
                }
            }
        };

        let mode = PIPE_READMODE_BYTE;
        // SAFETY: handle/token/event 为本路径拥有的有效内核句柄，关闭后不再使用。
        if let Err(e) = unsafe { SetNamedPipeHandleState(handle, Some(&mode), None, None) } {
            // SAFETY: handle/token/event 为本路径拥有的有效内核句柄，关闭后不再使用。
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(IoKind::Io(format!("设置管道模式失败: {e}")));
        }

        // SAFETY: handle/token/event 为本路径拥有的有效内核句柄，关闭后不再使用。
        let event = unsafe { CreateEventW(None, false, false, None) }.map_err(|e| {
            // SAFETY: handle/token/event 为本路径拥有的有效内核句柄，关闭后不再使用。
            unsafe {
                let _ = CloseHandle(handle);
            }
            IoKind::Io(format!("创建重叠事件失败: {e}"))
        })?;

        // wine/部分主机上 CreateFileW 返回后短时仍报 Pipe not connected；短暂让出。
        std::thread::sleep(Duration::from_millis(15));
        Ok(WinPipeConn { handle, event })
    }

    pub(super) fn snapshot_named_pipe(
        pipe_name: &str,
        inner: &mut ClientInner,
        budget_end: Instant,
    ) -> LiveSnapshotReply {
        let line = match encode_request_line() {
            Ok(l) => l,
            Err(r) => return *r,
        };

        if let Some(mut conn) = inner.cached.take() {
            match &mut conn {
                PipeConn::Win(w) => match exchange_win(w, &line, budget_end) {
                    Ok(reply) => {
                        inner.cached = Some(conn);
                        return reply;
                    }
                    Err(IoKind::Timeout) => {
                        conn.cancel_close();
                        return IoKind::Timeout.into_reply();
                    }
                    Err(_) => {
                        conn.cancel_close();
                        // 失效连接：预算内重连一次
                    }
                },
                #[cfg(test)]
                PipeConn::Test(_) => {
                    conn.cancel_close();
                }
            }
        }

        if Instant::now() >= budget_end {
            return IoKind::Timeout.into_reply();
        }

        let mut w = match open_pipe(pipe_name, budget_end) {
            Ok(c) => c,
            Err(e) => return e.into_reply(),
        };

        match exchange_win(&mut w, &line, budget_end) {
            Ok(reply) => {
                inner.cached = Some(PipeConn::Win(w));
                reply
            }
            Err(e) => {
                w.cancel_close();
                e.into_reply()
            }
        }
    }

    fn exchange_win(
        conn: &mut WinPipeConn,
        line: &[u8],
        budget_end: Instant,
    ) -> Result<LiveSnapshotReply, IoKind> {
        conn.write_all_deadline(line, budget_end)?;
        let raw = conn.read_line_deadline(LIVE_MAX_RESPONSE_BYTES, budget_end)?;
        Ok(parse_and_validate_reply(&raw))
    }

    /// 将 CreateFileW 风格错误码映射为种类（单测用）。
    #[cfg(test)]
    pub(super) fn map_open_hresult(code: HRESULT) -> IoKind {
        if code == ERROR_FILE_NOT_FOUND.to_hresult() {
            IoKind::NotRunning
        } else if code == ERROR_ACCESS_DENIED.to_hresult() {
            IoKind::AccessDenied
        } else if code == ERROR_PIPE_BUSY.to_hresult() {
            IoKind::Busy
        } else {
            IoKind::Io(format!("连接实时管道失败: {code}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// 成功空来源 NDJSON（含换行）。
    fn ok_empty_line() -> Vec<u8> {
        let s = r#"{"protocolVersion":1,"snapshot":{"sessionId":"42:1791970000000000","sampleSeq":"8","resetEpoch":"0","statisticsPaused":false,"captureState":"active","rawInputAgeMs":10,"sources":[]},"error":null}"#;
        format!("{s}\n").into_bytes()
    }

    // ----- 抽象 FakeConn -----

    struct ScriptedConn {
        /// 写成功后才允许读
        written: bool,
        response: Option<Vec<u8>>,
        /// 读时挂起直到 cancel 或 deadline
        hang_read: bool,
        cancelled: Arc<AtomicBool>,
        /// 读挂起结束后置位（证明原生/抽象 I/O 真正结束）
        io_finished: Arc<AtomicBool>,
        fail_write: Option<IoFail>,
    }

    impl TestConn for ScriptedConn {
        fn write_all_deadline(&mut self, _buf: &[u8], deadline: Instant) -> Result<(), IoFail> {
            if let Some(f) = self.fail_write.take() {
                return Err(f);
            }
            if Instant::now() >= deadline {
                return Err(IoFail::Timeout);
            }
            self.written = true;
            Ok(())
        }

        fn read_line_deadline(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, IoFail> {
            if self.hang_read {
                while Instant::now() < deadline {
                    if self.cancelled.load(Ordering::SeqCst) {
                        self.io_finished.store(true, Ordering::SeqCst);
                        return Err(IoFail::Timeout);
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                // 预算到期：模拟「取消后 I/O 结束」
                self.cancelled.store(true, Ordering::SeqCst);
                self.io_finished.store(true, Ordering::SeqCst);
                return Err(IoFail::Timeout);
            }
            let raw = self
                .response
                .take()
                .ok_or_else(|| IoFail::Io("无预设响应".into()))?;
            if raw.len() > max {
                return Err(IoFail::TooLarge);
            }
            self.io_finished.store(true, Ordering::SeqCst);
            Ok(raw)
        }

        fn cancel_close(&mut self) {
            self.cancelled.store(true, Ordering::SeqCst);
            self.io_finished.store(true, Ordering::SeqCst);
        }
    }

    struct ScriptedTransport {
        /// 每次 connect 弹出一个工厂结果
        connects: Mutex<Vec<Result<ScriptedConnFactory, IoFail>>>,
        connect_count: AtomicU32,
    }

    struct ScriptedConnFactory {
        response: Option<Vec<u8>>,
        hang_read: bool,
        cancelled: Arc<AtomicBool>,
        io_finished: Arc<AtomicBool>,
        fail_write: Option<IoFail>,
    }

    impl TestTransport for ScriptedTransport {
        fn connect(&self, deadline: Instant) -> Result<Box<dyn TestConn>, IoFail> {
            if Instant::now() >= deadline {
                return Err(IoFail::Timeout);
            }
            self.connect_count.fetch_add(1, Ordering::SeqCst);
            let next = self
                .connects
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop()
                .unwrap_or(Err(IoFail::NotRunning))?;
            // pop 从尾部，测试里 reverse push
            Ok(Box::new(ScriptedConn {
                written: false,
                response: next.response,
                hang_read: next.hang_read,
                cancelled: next.cancelled,
                io_finished: next.io_finished,
                fail_write: next.fail_write,
            }))
        }
    }

    fn client_with(transport: Arc<dyn TestTransport>) -> LiveInputClient {
        LiveInputClient {
            pipe_name: format!(
                r"\\.\pipe\clrecoder-live-test-{}-abstract",
                std::process::id()
            ),
            inner: Mutex::new(ClientInner { cached: None }),
            test_transport: Some(transport),
        }
    }

    fn push_ok(transport: &ScriptedTransport, response: Vec<u8>) {
        transport
            .connects
            .lock()
            .unwrap()
            .insert(
                0,
                Ok(ScriptedConnFactory {
                    response: Some(response),
                    hang_read: false,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    io_finished: Arc::new(AtomicBool::new(false)),
                    fail_write: None,
                }),
            );
    }

    #[test]
    fn live_input_not_running_when_connect_fails() {
        let t = Arc::new(ScriptedTransport {
            connects: Mutex::new(vec![Err(IoFail::NotRunning)]),
            connect_count: AtomicU32::new(0),
        });
        // pop 从尾部：先 push 的最后弹出——这里只有一个
        let client = client_with(t.clone());
        let reply = client.snapshot();
        assert!(reply.snapshot.is_none());
        assert_eq!(reply.error.as_ref().unwrap().code, LiveErrorCode::NotRunning);
        assert!(reply.error.as_ref().unwrap().message.contains("实时输入不可用"));
    }

    #[test]
    fn live_input_access_denied_and_busy() {
        for (fail, code) in [
            (IoFail::AccessDenied, LiveErrorCode::AccessDenied),
            (IoFail::Busy, LiveErrorCode::Busy),
        ] {
            let t = Arc::new(ScriptedTransport {
                connects: Mutex::new(vec![Err(fail)]),
                connect_count: AtomicU32::new(0),
            });
            let client = client_with(t);
            let reply = client.snapshot();
            assert_eq!(reply.error.as_ref().unwrap().code, code);
        }
    }

    #[test]
    fn live_input_protocol_and_too_large() {
        // protocol：非法 JSON
        let t = Arc::new(ScriptedTransport {
            connects: Mutex::new(Vec::new()),
            connect_count: AtomicU32::new(0),
        });
        push_ok(&t, b"{not-json}\n".to_vec());
        let client = client_with(t.clone());
        let reply = client.snapshot();
        assert_eq!(reply.error.as_ref().unwrap().code, LiveErrorCode::Protocol);

        // too_large：读侧发现超长（无换行且持续超限——用超长且带换行但 len>max 的预设）
        let t2 = Arc::new(ScriptedTransport {
            connects: Mutex::new(Vec::new()),
            connect_count: AtomicU32::new(0),
        });
        let mut huge = vec![b'A'; LIVE_MAX_RESPONSE_BYTES + 8];
        huge.push(b'\n');
        t2.connects.lock().unwrap().insert(
            0,
            Ok(ScriptedConnFactory {
                response: Some(huge),
                hang_read: false,
                cancelled: Arc::new(AtomicBool::new(false)),
                io_finished: Arc::new(AtomicBool::new(false)),
                fail_write: None,
            }),
        );
        let client2 = client_with(t2);
        let reply2 = client2.snapshot();
        assert_eq!(
            reply2.error.as_ref().unwrap().code,
            LiveErrorCode::TooLarge
        );
    }

    #[test]
    fn live_input_both_null_is_protocol() {
        let line = b"{\"protocolVersion\":1,\"snapshot\":null,\"error\":null}\n".to_vec();
        let t = Arc::new(ScriptedTransport {
            connects: Mutex::new(Vec::new()),
            connect_count: AtomicU32::new(0),
        });
        push_ok(&t, line);
        let client = client_with(t);
        let reply = client.snapshot();
        assert_eq!(reply.error.as_ref().unwrap().code, LiveErrorCode::Protocol);
    }

    #[test]
    fn live_input_reuse_cached_connection() {
        let t = Arc::new(ScriptedTransport {
            connects: Mutex::new(Vec::new()),
            connect_count: AtomicU32::new(0),
        });
        // 两次成功响应，但只应 connect 一次（第二次复用缓存）
        // connect 从 Vec 尾部 pop：先 insert(0) 的是先被用的
        push_ok(&t, ok_empty_line());
        // 缓存复用时不会再 connect；但 ScriptedConn 的 response 是 take 掉的，
        // 所以复用路径需要同一连接能再次响应——ScriptedConn 只支持一次。
        // 改为：自定义可多次读的连接。
        struct ReuseConn {
            reads: u32,
        }
        impl TestConn for ReuseConn {
            fn write_all_deadline(&mut self, _: &[u8], _: Instant) -> Result<(), IoFail> {
                Ok(())
            }
            fn read_line_deadline(&mut self, _: usize, _: Instant) -> Result<Vec<u8>, IoFail> {
                self.reads += 1;
                Ok(ok_empty_line())
            }
            fn cancel_close(&mut self) {}
        }
        struct ReuseTransport {
            connects: AtomicU32,
        }
        impl TestTransport for ReuseTransport {
            fn connect(&self, _: Instant) -> Result<Box<dyn TestConn>, IoFail> {
                self.connects.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(ReuseConn { reads: 0 }))
            }
        }
        let rt = Arc::new(ReuseTransport {
            connects: AtomicU32::new(0),
        });
        let client = client_with(rt.clone());
        let r1 = client.snapshot();
        let r2 = client.snapshot();
        assert!(r1.snapshot.is_some() && r1.error.is_none());
        assert!(r2.snapshot.is_some() && r2.error.is_none());
        assert_eq!(rt.connects.load(Ordering::SeqCst), 1, "应复用同一连接");
    }

    #[test]
    fn live_input_reconnect_after_stale_cache() {
        struct FlakyConn {
            id: u32,
            writes: u32,
        }
        impl TestConn for FlakyConn {
            fn write_all_deadline(&mut self, _: &[u8], _: Instant) -> Result<(), IoFail> {
                self.writes += 1;
                if self.id == 1 && self.writes == 1 {
                    Ok(())
                } else if self.id == 1 {
                    Err(IoFail::Io("broken".into()))
                } else {
                    Ok(())
                }
            }
            fn read_line_deadline(&mut self, _: usize, _: Instant) -> Result<Vec<u8>, IoFail> {
                let _ = self.id;
                Ok(ok_empty_line())
            }
            fn cancel_close(&mut self) {}
        }
        struct FlakyTransport {
            n: AtomicU32,
        }
        impl TestTransport for FlakyTransport {
            fn connect(&self, _: Instant) -> Result<Box<dyn TestConn>, IoFail> {
                let id = self.n.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(Box::new(FlakyConn { id, writes: 0 }))
            }
        }
        let ft = Arc::new(FlakyTransport {
            n: AtomicU32::new(0),
        });
        let client = client_with(ft.clone());
        assert!(client.snapshot().snapshot.is_some());
        // 第二次：缓存写失败 → 重连 → 成功
        let r2 = client.snapshot();
        assert!(r2.snapshot.is_some(), "{r2:?}");
        assert_eq!(ft.n.load(Ordering::SeqCst), 2, "应重连一次");
    }

    #[test]
    fn live_input_timeout_cancels_actual_io() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let io_finished = Arc::new(AtomicBool::new(false));
        let t = Arc::new(ScriptedTransport {
            connects: Mutex::new(Vec::new()),
            connect_count: AtomicU32::new(0),
        });
        t.connects.lock().unwrap().insert(
            0,
            Ok(ScriptedConnFactory {
                response: None,
                hang_read: true,
                cancelled: cancelled.clone(),
                io_finished: io_finished.clone(),
                fail_write: None,
            }),
        );
        let client = client_with(t);
        let started = Instant::now();
        let reply = client.snapshot();
        let elapsed = started.elapsed();
        assert_eq!(reply.error.as_ref().unwrap().code, LiveErrorCode::Timeout);
        assert!(
            elapsed >= Duration::from_millis(LIVE_IO_TIMEOUT_MS.saturating_sub(30)),
            "应接近 250ms 预算，实际 {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(LIVE_IO_TIMEOUT_MS + 200),
            "不应远超预算，实际 {elapsed:?}"
        );
        assert!(
            io_finished.load(Ordering::SeqCst),
            "超时后实际 I/O 必须结束（不可 detach）"
        );
    }

    #[test]
    fn live_input_drop_closes_cached_connection() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let io_finished = Arc::new(AtomicBool::new(false));
        struct DropProbeTransport {
            cancelled: Arc<AtomicBool>,
            io_finished: Arc<AtomicBool>,
        }
        impl TestTransport for DropProbeTransport {
            fn connect(&self, _: Instant) -> Result<Box<dyn TestConn>, IoFail> {
                Ok(Box::new(ScriptedConn {
                    written: false,
                    response: Some(ok_empty_line()),
                    hang_read: false,
                    cancelled: self.cancelled.clone(),
                    io_finished: self.io_finished.clone(),
                    fail_write: None,
                }))
            }
        }
        let t = Arc::new(DropProbeTransport {
            cancelled: cancelled.clone(),
            io_finished: io_finished.clone(),
        });
        let client = client_with(t);
        assert!(client.snapshot().snapshot.is_some());
        drop(client);
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(io_finished.load(Ordering::SeqCst));
    }

    #[test]
    fn live_input_success_empty_sources() {
        let t = Arc::new(ScriptedTransport {
            connects: Mutex::new(Vec::new()),
            connect_count: AtomicU32::new(0),
        });
        push_ok(&t, ok_empty_line());
        let client = client_with(t);
        let reply = client.snapshot();
        assert!(reply.error.is_none());
        let snap = reply.snapshot.unwrap();
        assert_eq!(snap.session_id, "42:1791970000000000");
        assert!(snap.sources.is_empty());
    }

    #[test]
    fn live_input_new_defaults_to_production_name() {
        let c = LiveInputClient::new();
        assert_eq!(c.pipe_name, LIVE_PIPE_NAME);
        let c2 = LiveInputClient::new_at(r"\\.\pipe\clrecoder-live-test-1-x".into());
        assert!(c2.pipe_name.contains("test"));
    }

    #[cfg(windows)]
    mod win_pipe_tests {
        use super::*;
        use windows::core::HSTRING;
        use windows::Win32::Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING,
            ERROR_PIPE_BUSY, HANDLE,
        };

        /// 测试内把 HANDLE 位型拷到 isize 再还原（跨线程交接；独占使用）。
        fn handle_to_bits(h: HANDLE) -> isize {
            h.0 as isize
        }
        fn bits_to_handle(bits: isize) -> HANDLE {
            HANDLE(bits as *mut core::ffi::c_void)
        }
        use windows::Win32::Storage::FileSystem::{
            ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
            PIPE_ACCESS_DUPLEX,
        };
        use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
        };
        use windows::Win32::System::Threading::CreateEventW;
        use windows::core::HRESULT;

        fn test_pipe_name(fixture: &str) -> String {
            format!(
                r"\\.\pipe\clrecoder-live-test-{}-{}",
                std::process::id(),
                fixture
            )
        }

        fn open_server(name: &str) -> HANDLE {
            let wide = HSTRING::from(name);
            // SAFETY: name/HSTRING 与 SECURITY_ATTRIBUTES 在调用期间存活；返回句柄由本路径 Disconnect/CloseHandle 释放。
            let server = unsafe {
                CreateNamedPipeW(
                    &wide,
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    65536,
                    65536,
                    0,
                    None,
                )
            };
            assert!(!server.is_invalid(), "创建测试管道失败");
            server
        }

        fn accept_one(server: HANDLE, accepting: Option<&AtomicBool>) {
            use windows::Win32::Foundation::ERROR_PIPE_CONNECTED;
            // SAFETY: handle 为已创建的管道实例；ov.hEvent 为本连接专用且在 Wait/GetOverlappedResult 期间有效。
            let event = unsafe { CreateEventW(None, false, false, None) }.unwrap();
            let mut ov = OVERLAPPED {
                hEvent: event,
                ..Default::default()
            };
            // 先标记「已提交 listen」，再等连接完成——客户端须在 pending 后才 CreateFile。
            // SAFETY: handle 为已创建的管道实例；ov.hEvent 为本连接专用且在 Wait/GetOverlappedResult 期间有效。
            let submitted = unsafe { ConnectNamedPipe(server, Some(&mut ov)) };
            if let Some(flag) = accepting {
                // 复用同一 flag：false→pending 已提交；测试用第二 flag 更清晰，见 wait_listening
                flag.store(true, Ordering::SeqCst);
            }
            match submitted {
                Ok(()) => {}
                Err(e) => {
                    let code = e.code();
                    if code == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) {
                        // 客户端已连上
                    } else if code == HRESULT::from_win32(ERROR_IO_PENDING.0) {
                        let mut transferred = 0u32;
                        let deadline = Instant::now() + Duration::from_secs(2);
                        loop {
                            // SAFETY: handle 与 ov 对应同一次未完成/已完成 IRP；ov.hEvent 仍有效；transferred 为本栈变量。
                            let r = unsafe {
                                GetOverlappedResult(server, &ov, &mut transferred, false)
                            };
                            if r.is_ok() {
                                break;
                            }
                            let incomplete = HRESULT::from_win32(
                                windows::Win32::Foundation::ERROR_IO_INCOMPLETE.0,
                            );
                            if r.as_ref().err().map(|err| err.code() != incomplete).unwrap_or(true)
                            {
                                // 终态（含失败）：视为 accept 结束
                                break;
                            }
                            if Instant::now() >= deadline {
                                panic!("accept 超时");
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                    } else {
                        panic!("ConnectNamedPipe 失败: {e}");
                    }
                }
            }
            // SAFETY: handle/token/event 为本路径拥有的有效内核句柄，关闭后不再使用。
            unsafe {
                let _ = CloseHandle(event);
            }
        }

        fn wait_listening(flag: &AtomicBool) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !flag.load(Ordering::SeqCst) {
                if Instant::now() >= deadline {
                    panic!("服务端未进入 listen");
                }
                thread::sleep(Duration::from_millis(5));
            }
            thread::sleep(Duration::from_millis(20));
        }

        /// 独立 fake pipe 往返成功（写响应后保持连接直到客户端读完）。
        #[test]
        fn live_input_native_fake_pipe_round_trip() {
            let name = test_pipe_name("timeout-ok");
            let server_bits = handle_to_bits(open_server(&name));
            let accepting = Arc::new(AtomicBool::new(false));
            let accepting2 = accepting.clone();
            let client_done = Arc::new(AtomicBool::new(false));
            let client_done2 = client_done.clone();
            let responder = thread::spawn(move || {
                let server = bits_to_handle(server_bits);
                accept_one(server, Some(&accepting2));
                // SAFETY: 匿名手动/自动重置事件；成功句柄由本结构/函数在退出路径 CloseHandle。
                let event = unsafe { CreateEventW(None, false, false, None) }.unwrap();
                let mut transferred = 0u32;
                let mut req_buf = vec![0u8; 256];
                let mut ov_req = OVERLAPPED {
                    hEvent: event,
                    ..Default::default()
                };
                // SAFETY: handle 有效；目标缓冲在调用期间存活；若 OVERLAPPED 则 ov.hEvent 有效且直至结果取回。
                let _ = unsafe {
                    ReadFile(
                        server,
                        Some(&mut req_buf),
                        Some(&mut transferred),
                        Some(&mut ov_req),
                    )
                };
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let mut n = 0u32;
                    // SAFETY: handle 与 ov 对应同一次未完成/已完成 IRP；ov.hEvent 仍有效；transferred 为本栈变量。
                    if unsafe { GetOverlappedResult(server, &ov_req, &mut n, false) }.is_ok() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                let reply = {
                    let s = r#"{"protocolVersion":1,"snapshot":{"sessionId":"42:1791970000000000","sampleSeq":"8","resetEpoch":"0","statisticsPaused":false,"captureState":"active","rawInputAgeMs":10,"sources":[]},"error":null}"#;
                    format!("{s}\n").into_bytes()
                };
                let mut ovw = OVERLAPPED {
                    hEvent: event,
                    ..Default::default()
                };
                let mut wtrans = 0u32;
                // SAFETY: handle 有效；源缓冲在调用期间存活；若 OVERLAPPED 则 ov.hEvent 有效且直至结果取回。
                let _ = unsafe {
                    WriteFile(server, Some(&reply), Some(&mut wtrans), Some(&mut ovw))
                };
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let mut wn = 0u32;
                    // SAFETY: handle 与 ov 对应同一次未完成/已完成 IRP；ov.hEvent 仍有效；transferred 为本栈变量。
                    if unsafe { GetOverlappedResult(server, &ovw, &mut wn, false) }.is_ok() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                // 切勿在客户端读完前 Disconnect——否则 ReadFile 报 Pipe not connected。
                let wait_deadline = Instant::now() + Duration::from_secs(2);
                while !client_done2.load(Ordering::SeqCst) {
                    if Instant::now() >= wait_deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                // SAFETY: handle/server 为本路径持有的管道实例；断开后关闭关联 event 与句柄，之后不再使用。
                unsafe {
                    let _ = DisconnectNamedPipe(server);
                    let _ = CloseHandle(event);
                    let _ = CloseHandle(server);
                }
            });

            wait_listening(&accepting);
            let client = LiveInputClient::new_at(name);
            let reply = client.snapshot();
            client_done.store(true, Ordering::SeqCst);
            assert!(
                reply.snapshot.is_some() && reply.error.is_none(),
                "{reply:?}"
            );
            let _ = responder.join();
        }

        /// 服务端接受后不响应 → 客户端超时，且 CancelIoEx 后 I/O 收尾。
        #[test]
        fn live_input_native_timeout_cancels_io() {
            let name = test_pipe_name("timeout");
            let server_bits = handle_to_bits(open_server(&name));
            let finished = Arc::new(AtomicBool::new(false));
            let finished2 = finished.clone();
            let accepting = Arc::new(AtomicBool::new(false));
            let accepting2 = accepting.clone();
            let responder = thread::spawn(move || {
                let server = bits_to_handle(server_bits);
                accept_one(server, Some(&accepting2));
                // 读挂起：客户端写完后我们故意不回复，并阻塞在读下一笔（或空转）
                // 直到客户端因超时关闭连接。
                // SAFETY: 匿名手动/自动重置事件；成功句柄由本结构/函数在退出路径 CloseHandle。
                let event = unsafe { CreateEventW(None, false, false, None) }.unwrap();
                let mut buf = vec![0u8; 1];
                let mut ov = OVERLAPPED {
                    hEvent: event,
                    ..Default::default()
                };
                let mut transferred = 0u32;
                // 先读完请求（客户端会写一行）
                let mut req_buf = vec![0u8; 256];
                let mut ov_req = OVERLAPPED {
                    hEvent: event,
                    ..Default::default()
                };
                // SAFETY: handle 有效；目标缓冲在调用期间存活；若 OVERLAPPED 则 ov.hEvent 有效且直至结果取回。
                let _ = unsafe {
                    ReadFile(
                        server,
                        Some(&mut req_buf),
                        Some(&mut transferred),
                        Some(&mut ov_req),
                    )
                };
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let mut n = 0u32;
                    // SAFETY: handle 与 ov 对应同一次未完成/已完成 IRP；ov.hEvent 仍有效；transferred 为本栈变量。
                    if unsafe { GetOverlappedResult(server, &ov_req, &mut n, false) }.is_ok() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                // 不写响应；再挂起一次读，等待客户端关连接导致失败
                // SAFETY: handle 有效；目标缓冲在调用期间存活；若 OVERLAPPED 则 ov.hEvent 有效且直至结果取回。
                let _ = unsafe {
                    ReadFile(
                        server,
                        Some(&mut buf),
                        Some(&mut transferred),
                        Some(&mut ov),
                    )
                };
                let wait_deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    let mut n = 0u32;
                    // SAFETY: handle 与 ov 对应同一次未完成/已完成 IRP；ov.hEvent 仍有效；transferred 为本栈变量。
                    let r = unsafe { GetOverlappedResult(server, &ov, &mut n, false) };
                    if r.is_ok()
                        || r
                            .as_ref()
                            .err()
                            .map(|e| e.code() != HRESULT::from_win32(windows::Win32::Foundation::ERROR_IO_INCOMPLETE.0))
                            .unwrap_or(true)
                    {
                        break;
                    }
                    if Instant::now() >= wait_deadline {
                        // SAFETY: handle 有效；ov 指向该句柄上未决 IRP（或 None 取消全部）；随后仍须 GetOverlappedResult/等待收尾。
                        let _ = unsafe {
                            windows::Win32::System::IO::CancelIoEx(server, Some(&ov))
                        };
                        // 再等到收尾
                        loop {
                            let mut n2 = 0u32;
                            let r2 =
                                // SAFETY: handle 与 ov 对应同一次未完成/已完成 IRP；ov.hEvent 仍有效；transferred 为本栈变量。
                                unsafe { GetOverlappedResult(server, &ov, &mut n2, false) };
                            if r2.is_ok()
                                || r2.as_ref().err().map(|e| {
                                    e.code()
                                        != HRESULT::from_win32(
                                            windows::Win32::Foundation::ERROR_IO_INCOMPLETE.0,
                                        )
                                }).unwrap_or(true)
                            {
                                break;
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                finished2.store(true, Ordering::SeqCst);
                // SAFETY: handle/server 为本路径持有的管道实例；断开后关闭关联 event 与句柄，之后不再使用。
                unsafe {
                    let _ = DisconnectNamedPipe(server);
                    let _ = CloseHandle(event);
                    let _ = CloseHandle(server);
                }
            });

            wait_listening(&accepting);
            let client = LiveInputClient::new_at(name);
            let started = Instant::now();
            let reply = client.snapshot();
            let elapsed = started.elapsed();
            assert_eq!(
                reply.error.as_ref().map(|e| e.code),
                Some(LiveErrorCode::Timeout),
                "{reply:?}"
            );
            assert!(
                elapsed < Duration::from_millis(LIVE_IO_TIMEOUT_MS + 500),
                "超时后应结束，实际 {elapsed:?}"
            );
            // 关闭客户端连接以唤醒服务端挂起读
            drop(client);
            let _ = responder.join();
            assert!(
                finished.load(Ordering::SeqCst),
                "服务端挂起 I/O 须在客户端取消/关闭后收尾"
            );
        }

        #[test]
        fn live_input_open_error_kind_maps_win32() {
            use windows::core::HRESULT;
            assert!(matches!(
                win::map_open_hresult(ERROR_FILE_NOT_FOUND.to_hresult()),
                win::IoKind::NotRunning
            ));
            assert!(matches!(
                win::map_open_hresult(ERROR_ACCESS_DENIED.to_hresult()),
                win::IoKind::AccessDenied
            ));
            assert!(matches!(
                win::map_open_hresult(ERROR_PIPE_BUSY.to_hresult()),
                win::IoKind::Busy
            ));
            let _ = HRESULT::from_win32(12345); // 其它 → Io
            assert!(matches!(
                win::map_open_hresult(HRESULT::from_win32(12345)),
                win::IoKind::Io(_)
            ));
        }
    }
}
