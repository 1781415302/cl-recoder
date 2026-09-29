//! 前台应用跟踪（PLAN §4.6 apps.rs 契约，S7）。
//!
//! 专用消息循环线程 + `SetWinEventHook(EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT)`：
//!
//! ```text
//! WinEvent 回调（本线程）→ HWND → GetWindowThreadProcessId → pid
//!   → OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) → QueryFullProcessImageNameW
//!   → 完整镜像路径 → 小写 basename（纯函数 lowercase_basename）
//!   → ApplicationFrameHost.exe 特判：EnumChildWindows 找类名 "Windows.UI.Core.CoreWindow"
//!     的子窗口取真实 PID，穿透失败归 "UWP 应用"
//!   → 任何一步失败归 "unknown"
//! ```
//!
//! 对外契约（§4.6 逐字）：
//! - [`FgState`]：`{ exe, since }`，exe/since **仅由本模块的 apps 线程写入**，aggregator
//!   （engine_loop，S9）读取做前台秒数归账；
//! - [`spawn`]：钩子注册后立即 `GetForegroundWindow` 解析一次并发送**初始** `Foreground`
//!   （`EVENT_SYSTEM_FOREGROUND` 不会对注册时已处于前台的窗口补发）；此后仅当 exe 变化时
//!   更新 [`FgState`] 并 `tx.send(AggEvent::Foreground{ exe })`——exe 未变化不重置 `since`
//!   （前台时长连续累计，事件只代表"切换"语义，见 core::event::AggEvent）。
//!
//! 解析决策集中在纯函数 [`exe_label_for`] / [`lowercase_basename`]，可脱离 OS 单测（S7 验收点）。

use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;

use clrecoder_core::event::AggEvent;
use crossbeam_channel::Sender;
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumChildWindows, GetClassNameW, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, TranslateMessage, EVENT_SYSTEM_FOREGROUND, MSG, OBJID_WINDOW,
    WINEVENT_OUTOFCONTEXT,
};

/// 归属 exe 兜底值：解析失败（PLAN §4.6 "解析失败 exe=\"unknown\""）。
pub const EXE_UNKNOWN: &str = "unknown";
/// UWP 宿主穿透失败的归属值（PLAN §4.6 "失败用 \"UWP 应用\""）。
pub const EXE_UWP: &str = "UWP 应用";
/// UWP 宿主进程（ApplicationFrameHost.exe）：前台窗口属于宿主，真实 exe 要穿透到
/// CoreWindow 子窗口所在进程取（小写 basename，§4.6）。
const UWP_FRAME_HOST: &str = "applicationframehost.exe";
/// UWP 内容子窗口类名（§4.6 逐字）。
const UWP_CORE_WINDOW_CLASS: &str = "Windows.UI.Core.CoreWindow";

/// 前台状态（PLAN §4.6 逐字契约）。
///
/// exe/since **仅由 apps 线程写入**；engine_loop（S9）按 `(now - since)` 归账秒数到
/// `app_daily(day, exe)`，跨天/暂停的切分也在 engine_loop 完成。
///
/// 初始值由调用方（S9 main）构造，建议 `exe: "unknown".into(), since: Instant::now()`；
/// 本线程在首条事件前若解析失败不会把它改写为别的值，语义自洽。
pub struct FgState {
    /// 当前前台进程 exe 的小写 basename（"unknown"/"UWP 应用" 兜底值见模块文档）
    pub exe: String,
    /// 当前前台自该时刻起算（exe 变化时重置）
    pub since: Instant,
}

/// 回调上下文：WinEvent 回调是 `extern "system"` 无上下文参数，经进程级单例转交。
/// apps 线程唯一，`spawn` 只允许调用一次（重复调用记录错误并直接退出该线程）。
struct HookCtx {
    tx: Sender<AggEvent>,
    fg: Arc<Mutex<FgState>>,
}

static HOOK_CTX: OnceLock<HookCtx> = OnceLock::new();

/// 启动前台应用跟踪线程（PLAN §4.6 逐字契约）。
///
/// - `tx`：聚合事件通道，本线程仅发送 [`AggEvent::Foreground`]；
/// - `fg`：与 engine_loop（S9）共享的前台状态，本线程是唯一写方。
///
/// 线程行为：注册 `EVENT_SYSTEM_FOREGROUND` 钩子（WINEVENT_OUTOFCONTEXT，钩子注册线程
/// 自建消息循环接收回调）→ 发送初始 Foreground → 消息循环直至线程退出（WM_QUIT/错误/
/// 进程退出），退出前反注册钩子。钩子注册失败：记录错误日志后线程退出（防御性兜底，
/// 绝不 panic、不影响其它采集线程，§1 设计原则 3）。
pub fn spawn(tx: Sender<AggEvent>, fg: Arc<Mutex<FgState>>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("clrecoder-apps".into())
        .spawn(move || {
            if HOOK_CTX.set(HookCtx { tx, fg }).is_err() {
                // 契约上 spawn 只被调用一次；重复调用会与首个钩子竞争回调上下文，直接退出。
                log::error!("apps 线程被重复 spawn，忽略第二次调用");
                return;
            }
            run_message_loop();
        })
        .expect("apps 前台跟踪线程创建失败")
}

/// 注册钩子并发送初始快照，然后驱动消息循环（仅 apps 线程调用一次）。
fn run_message_loop() {
    // SAFETY：仅在 apps 线程调用一次；钩子回调与本消息循环同线程（WINEVENT_OUTOFCONTEXT
    // + hmod=None 时事件经注册线程的消息队列投递）。
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None, // hmod：回调为进程内函数，无需 DLL 句柄
            Some(fg_callback),
            0, // idprocess：全部进程
            0, // idthread：全部线程
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if hook.is_invalid() {
        log::error!("SetWinEventHook(EVENT_SYSTEM_FOREGROUND) 注册失败，前台应用不再跟踪");
        // 仍发送一次初始快照：aggregator 至少有启动时刻的归属，避免整段时间挂在初始值上。
        emit_initial_foreground();
        return;
    }

    emit_initial_foreground();

    // 消息循环：钩子回调经 GetMessageW 分派；线程随进程退出/WM_QUIT 结束。
    let mut msg = MSG::default();
    loop {
        // GetMessageW 返回值：0=WM_QUIT，-1=错误；禁止用 as_bool() 判断（-1 会被当真值死循环）。
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            if r.0 < 0 {
                log::warn!("apps 消息循环 GetMessageW 失败，前台跟踪线程退出");
            }
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // SAFETY：hook 由本线程刚注册成功且尚未被反注册。
    let _ = unsafe { UnhookWinEvent(hook) };
}

/// 初始前台快照（PLAN §4.6：EVENT_SYSTEM_FOREGROUND 不会对注册时已前台的窗口补发）。
///
/// 解析成功与否都**无条件**上报一次（force=true）：调用方（S9）构造的 FgState 初始 exe
/// 只是占位，aggregator 需要这条事件把归属时间轴正式启动。
fn emit_initial_foreground() {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        log::debug!("启动时无前台窗口（如无交互会话），等待首个 EVENT_SYSTEM_FOREGROUND");
        return;
    }
    let exe = unsafe { resolve_foreground_exe(hwnd) };
    let Some(ctx) = HOOK_CTX.get() else { return };
    apply_foreground(&ctx.fg, exe, true);
}

/// WinEvent 回调：仅处理窗口级前台事件，解析并按"exe 变化"上报。
unsafe extern "system" fn fg_callback(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    idobject: i32,
    _idchild: i32,
    _ideventthread: u32,
    _dwmseventtime: u32,
) {
    // OBJID_WINDOW(0)：EVENT_SYSTEM_FOREGROUND 偶发对非窗口对象触发，忽略。
    if idobject != OBJID_WINDOW.0 || hwnd.is_invalid() {
        return;
    }
    let Some(ctx) = HOOK_CTX.get() else { return };
    let exe = resolve_foreground_exe(hwnd);
    // 同 exe 不上报也不重置 since：前台时长连续累计（切到同 exe 的另一窗口不算切换）。
    apply_foreground(&ctx.fg, exe, false);
}

/// 把解析结果落到共享状态并按需发送事件（仅 apps 线程调用）。
///
/// `force=true` 用于初始快照（无条件更新+上报）；`false` 用于前台切换（exe 未变化则整体跳过）。
fn apply_foreground(fg: &Mutex<FgState>, exe: String, force: bool) {
    let mut st = match fg.lock() {
        Ok(g) => g,
        // 锁毒化不阻塞统计：读到原值继续（§1 设计原则 3：绝不 crash）。
        Err(poisoned) => poisoned.into_inner(),
    };
    if !force && st.exe == exe {
        return;
    }
    st.exe = exe.clone();
    st.since = Instant::now();
    drop(st); // 先落状态再发事件：engine_loop 收到事件时 FgState 已一致
              // 通道关闭（进程退出中）时丢弃事件，绝不 panic（§1 设计原则 3）。
    if let Some(ctx) = HOOK_CTX.get() {
        let _ = ctx.tx.send(AggEvent::Foreground { exe });
    }
}

/// 解析一个前台 HWND 的归属 exe（PLAN §4.6 解析链 + UWP 特判）。
///
/// 任何一步失败最终都落到 [`exe_label_for`] 的兜底分支（"unknown"/"UWP 应用"）。
///
/// # Safety
/// `hwnd` 必须是有效的窗口句柄；本函数仅做只读系统查询。
unsafe fn resolve_foreground_exe(hwnd: HWND) -> String {
    let mut pid = 0u32;
    // SAFETY：hwnd 有效性已由调用方保证。
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let top_image = if pid == 0 {
        None
    } else {
        query_process_image(pid)
    };

    // ApplicationFrameHost.exe 特判：UWP 窗口的宿主是固定进程，真实 exe 需穿透到
    // CoreWindow 子窗口所在进程（§4.6）。
    let is_frame_host = top_image
        .as_deref()
        .and_then(lowercase_basename)
        .is_some_and(|b| b == UWP_FRAME_HOST);
    let child_image = if is_frame_host {
        pid_of_core_window_child(hwnd).and_then(|pid| unsafe { query_process_image(pid) })
    } else {
        None
    };

    exe_label_for(top_image, child_image)
}

/// 归属 exe 决策（纯函数，单测锚点）。
///
/// - 顶层镜像缺失/无 basename → [`EXE_UNKNOWN`]；
/// - 非 UWP 宿主 → 顶层镜像的小写 basename；
/// - UWP 宿主且 CoreWindow 穿透成功 → 子窗口进程镜像的小写 basename；
/// - UWP 宿主且穿透失败 → [`EXE_UWP`]。
fn exe_label_for(top_image: Option<String>, core_child_image: Option<String>) -> String {
    let Some(top) = top_image.as_deref().and_then(lowercase_basename) else {
        return EXE_UNKNOWN.to_string();
    };
    if top != UWP_FRAME_HOST {
        return top;
    }
    match core_child_image.as_deref().and_then(lowercase_basename) {
        Some(real) => real,
        None => EXE_UWP.to_string(),
    }
}

/// 完整镜像路径 → 小写 basename（纯函数，单测锚点）。
///
/// 兼容 `\` 与 `/` 分隔符、`\\?\` 设备前缀、裸文件名；空路径或路径以分隔符结尾
/// （无 basename）返回 `None`，由调用方归 "unknown"。Unicode（中文路径）安全。
fn lowercase_basename(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let start = path.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let base = &path[start..];
    if base.is_empty() {
        None
    } else {
        Some(base.to_lowercase())
    }
}

/// 按 pid 打开进程并查询完整镜像路径（Win32 链：OpenProcess → QueryFullProcessImageNameW）。
///
/// 进程已退出/访问受限/缓冲不足等一律返回 `None`（归 "unknown"，绝不 panic）。
///
/// # Safety
/// 仅做只读系统查询；`pid` 可为任意进程 id（无有效性前提）。
unsafe fn query_process_image(pid: u32) -> Option<String> {
    // SAFETY：只读查询；句柄在查询后立即关闭。
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut buf = [0u16; 2048]; // 远大于常规可执行路径，溢出按失败归 "unknown"
    let mut len = buf.len() as u32;
    // SAFETY：buf 为调用方栈上缓冲，len 为其容量（字符数）。
    let r = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    if r.is_err() {
        return None;
    }
    let len = (len as usize).min(buf.len());
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// 在 UWP 宿主窗口下找类名为 [`UWP_CORE_WINDOW_CLASS`] 的子窗口并返回其进程 id。
///
/// 枚举回调把结果写进栈上输出槽；`EnumChildWindows` 的返回值不区分"自然枚举完"与
/// "回调提前终止"，一律以输出槽为准。找不到/子窗口 pid 为 0 返回 `None`。
///
/// # Safety
/// `host` 必须是有效的窗口句柄。
unsafe fn pid_of_core_window_child(host: HWND) -> Option<u32> {
    let mut pid: u32 = 0;
    // SAFETY：回调仅读取类名/进程 id 并写入本函数栈上的输出槽。
    let _ = unsafe {
        EnumChildWindows(
            Some(host),
            Some(enum_core_child_proc),
            LPARAM(&mut pid as *mut u32 as isize),
        )
    };
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

/// 子窗口枚举回调：命中 CoreWindow 类名且 pid 可用则记录并终止枚举。
unsafe extern "system" fn enum_core_child_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let slot = lparam.0 as *mut u32;
    if slot.is_null() {
        return BOOL(0);
    }
    let mut buf = [0u16; 256]; // 窗口类名上限 256
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == UWP_CORE_WINDOW_CLASS {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != 0 {
            *slot = pid;
            return BOOL(0); // 找到即停
        }
    }
    BOOL(1) // 继续
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- lowercase_basename：路径 → 小写 basename 归一化 ----------

    #[test]
    fn basename_normalizes_case_and_separators() {
        assert_eq!(
            lowercase_basename(r"C:\Windows\explorer.exe").as_deref(),
            Some("explorer.exe")
        );
        // 大小写归一化（§4.6 "小写 basename"）
        assert_eq!(
            lowercase_basename(r"C:\WINDOWS\System32\CMD.EXE").as_deref(),
            Some("cmd.exe")
        );
        // 正斜杠（防御性兼容）
        assert_eq!(
            lowercase_basename("C:/Program Files/App/Notepad++.exe").as_deref(),
            Some("notepad++.exe")
        );
        // 无分隔符的裸文件名
        assert_eq!(
            lowercase_basename("explorer.exe").as_deref(),
            Some("explorer.exe")
        );
        // \\?\ 设备前缀
        assert_eq!(
            lowercase_basename(r"\\?\C:\Users\u\a\tool.exe").as_deref(),
            Some("tool.exe")
        );
        // 中文路径（Unicode 安全）
        assert_eq!(
            lowercase_basename(r"C:\工具\记事本.EXE").as_deref(),
            Some("记事本.exe")
        );
    }

    #[test]
    fn basename_rejects_empty_or_root_paths() {
        assert_eq!(lowercase_basename(""), None);
        assert_eq!(lowercase_basename("   "), None);
        // 盘符根/以分隔符结尾：没有 basename
        assert_eq!(lowercase_basename(r"C:\"), None);
        assert_eq!(lowercase_basename(r"C:\dir\"), None);
    }

    // ---------- exe_label_for：解析决策表（含 UWP 特判与两级兜底） ----------

    #[test]
    fn label_normal_process_uses_lowercase_basename() {
        let exe = exe_label_for(
            Some(r"C:\Windows\explorer.EXE".to_string()),
            None, // 非 UWP 宿主时子窗口结果恒为 None
        );
        assert_eq!(exe, "explorer.exe");
    }

    #[test]
    fn label_uwp_frame_host_pierces_to_core_window_process() {
        let exe = exe_label_for(
            Some(r"C:\Windows\SystemApps\MicrosoftWindows.Client.Core_cbsw5\ApplicationFrameHost.exe".to_string()),
            Some(r"C:\Program Files\WindowsApps\Microsoft.WindowsCalculator_11.2210.0.0_x64__8wekyb3d8bbwe\CalculatorApp.exe".to_string()),
        );
        assert_eq!(exe, "calculatorapp.exe");
    }

    #[test]
    fn label_uwp_frame_host_falls_back_to_uwp_when_pierce_fails() {
        // 子窗口未找到（None）或子窗口镜像解析失败（空串）→ "UWP 应用"（§4.6）
        let host = Some(
            r"C:\Windows\SystemApps\MicrosoftWindows.Client.Core_cbsw5\ApplicationFrameHost.exe"
                .to_string(),
        );
        assert_eq!(exe_label_for(host.clone(), None), EXE_UWP);
        assert_eq!(exe_label_for(host, Some(String::new())), EXE_UWP);
    }

    #[test]
    fn label_unknown_on_unresolvable_image() {
        // 顶层镜像缺失（OpenProcess/GetWindowThreadProcessId 失败链路）→ "unknown"
        assert_eq!(exe_label_for(None, None), EXE_UNKNOWN);
        // 空镜像路径同理
        assert_eq!(exe_label_for(Some(String::new()), None), EXE_UNKNOWN);
        // 大小写不敏感地识别宿主：即使路径给出大写形式也走 UWP 分支
        assert_eq!(
            exe_label_for(Some(r"C:\X\APPLICATIONFRAMEHOST.EXE".to_string()), None),
            EXE_UWP
        );
    }

    // ---------- 契约文本锚点：兜底值与 PLAN §4.6 逐字一致 ----------

    #[test]
    fn fallback_labels_match_plan_contract() {
        assert_eq!(EXE_UNKNOWN, "unknown");
        assert_eq!(EXE_UWP, "UWP 应用");
        assert_eq!(UWP_CORE_WINDOW_CLASS, "Windows.UI.Core.CoreWindow");
        assert_eq!(UWP_FRAME_HOST, "applicationframehost.exe");
    }

    #[test]
    fn fg_state_is_initializeable_by_caller() {
        // S9 main 构造初始值 → apps 线程为唯一写方的语义成立（字段可读可写）。
        let fg = FgState {
            exe: EXE_UNKNOWN.to_string(),
            since: Instant::now(),
        };
        assert_eq!(fg.exe, "unknown");
        assert!(fg.since.elapsed() < std::time::Duration::from_secs(1));
    }

    // ---------- 真实 Win32 链冒烟：OpenProcess → QueryFullProcessImageNameW → basename ----------
    // （钩子/消息循环/EnumChildWindows 部分依赖交互会话，按 §8-S7 归入 S9/S12 人工切窗冒烟。）

    #[test]
    fn query_own_process_image_smoke() {
        // 查询自身进程：OpenProcess+QueryFullProcessImageNameW 链路在本机真实可用，
        // 且结果经 lowercase_basename 归一化后与 std::env::current_exe 的 basename 一致。
        let img = unsafe { query_process_image(std::process::id()) }.expect("查询自身镜像路径失败");
        let base = lowercase_basename(&img).expect("自身镜像路径应含 basename");
        let exe = std::env::current_exe().expect("current_exe 不可用");
        let exe_base =
            lowercase_basename(exe.to_str().unwrap_or("")).expect("exe 路径应含 basename");
        assert_eq!(
            base, exe_base,
            "Win32 查询的镜像 basename 应与 current_exe 一致"
        );
    }
}
