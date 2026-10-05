//! 键鼠 Raw Input 采集（PLAN §4.6 raw_input.rs 契约 + correctness-v2 §4.3 物理来源合同
//! + motion-dpi §4.3 相对移动 counts）。
//!
//! 流程：message-only 窗口 + 三组 `RIDEV_INPUTSINK|RIDEV_DEVNOTIFY` 注册（keyboard 1/6、
//! mouse 1/2、Consumer Control 0x0C/1——音量/播放等媒体键按键盘进统计，§5.3）→
//! `WM_INPUT` 消息循环 → RAWKEYBOARD/RAWMOUSE 解析 → `AggEvent::Input(RawEvent::…)` 经
//! crossbeam channel 送 aggregator。
//!
//! 解析规则（§4.1/§4.2/§4.6）：
//! - 键盘：MakeCode==0 且 VKey!=0 先 `MapVirtualKeyW(VK, MAPVK_VK_TO_VSC_EX)` 预处理
//!   （扩展键前缀在 VSC_EX 高字节），再交 `core::codes::normalize_scancode`
//!   （0xFF 溢出码丢弃、Pause E1 归一等）；down/up 边沿均投递（Engine 需按下状态表
//!   去重自动重复/判定组合键）；事件携带来源注册器分配的连接 ID（§4.3）；
//! - 鼠标按键/滚轮：`ButtonFlags` 提取按下边沿（抬起不投递，§5.2 仅物理按下计数）；
//!   `RI_MOUSE_WHEEL/HWHEEL` 的 i16 delta **按来源独立累计**（F5：两鼠标零头互不合并），
//!   刻度折算为 WheelUp/Down/Left/Right 鼠标按键事件投递；
//! - 鼠标运动 counts（motion-dpi §4.3）：每个**相对** RAWMOUSE 包只计算一次
//!   `hypot(dx,dy)`，绝对输入不混成 counts；桶按 连接×捕获本地日×EffectiveDpi（×暂停
//!   epoch）分隔——DPI 快照/日期/暂停态变化先发旧有效桶再换新桶；每 25ms WM_TIMER
//!   批发有内容的桶，断连/正常退出排出尾数（不双计）。paused 包不累计；捕获 control
//!   快照与包一一对应，不跨 epoch 混桶。真实生产分支停止发送旧 `RawEvent::MouseMove`
//!   （该变体保留给旧合成 fixture，§4.1）；
//! - 设备：hDevice → `crate::device::DeviceResolver`（句柄缓存；hDevice==0/非 HID 归
//!   "未知/虚拟设备"桶，照常计数）；来源注册器按 `(原生句柄, kind)` 注册连接 ID——
//!   有效句柄即使型号解析失败也独立来源，null 句柄（0）降级为该 kind 的一个共享未知来源。
//!
//! 运动来源注册（motion-dpi §4.3）：注册 Raw Input 后枚举当前鼠标预注册（无需移动
//! 即出现 DPI 入口），首条输入懒注册兜底；接口路径/ContainerID 只在注册或重连查询
//! （resolver 缓存）。连接代际经 [`MotionRuntime::allocate_connection`] 进程内单调分配，
//! 断连重连生成新值。
//!
//! 生命周期（§4.3/§5.2/§4.3.1）：
//! - 每轮 message_loop 开始先经同一 sender 发 `KeyboardSourcesReset`（FIFO 位于本轮输入
//!   之前），进程级来源序号不随 loop 重建归零；
//! - `RIDEV_DEVNOTIFY` 使系统以 `WM_INPUT_DEVICE_CHANGE` 通知设备到达/移除；收到
//!   GIDC_REMOVAL：排出发动来源的运动尾桶（不双计）、发布断连状态、删来源映射与
//!   鼠标滚轮累计、resolver 缓存失效（句柄复用防护）、逐个发 `SourceRemoved`；
//! - loop 退出（注册失败/WM_QUIT/unwind）经 [`WindowGuard`] 在本线程 `DestroyWindow`，
//!   旧窗口不再在 1s 重试后继续投递旧来源事件；WM_QUIT（[`RawInputRunner::stop_and_join`]
//!   停止唤醒，只发本进程自有消息线程）先排出已捕获尾桶再退出；
//! - 注册失败/重试均检查 `producer_stop`（§4.3.1：经 [`MotionRuntime::stop_requested`]）。
//!
//! 窗口实现说明：windows 0.62 将 `WNDCLASSW`/`WNDCLASSEXW` gate 在 `Win32_Graphics_Gdi`
//! feature 之后（§9.1 依赖白名单未含该 feature），故复用 user32 系统全局类 `STATIC` 创建
//! message-only 窗口（父窗口 `HWND_MESSAGE`），再以 `GWLP_WNDPROC` 子类化挂接窗口过程——
//! 消息循环与 WM_INPUT 投递行为与自注册类完全一致。
//!
//! 健壮性（§1/§9.4）：窗口过程跨 FFI 边界（panic 即 abort），内部 catch_unwind 只丢当条
//! 事件（WM_INPUT/WM_INPUT_DEVICE_CHANGE/WM_TIMER 分支均受保护）；线程体 catch_unwind +
//! 1s 重试（限频日志）；tx 断开静默丢弃；状态盒有意泄漏（窗口随线程销毁时仍可能触发
//! 窗口过程，回收即悬垂）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::motion_runtime::MotionRuntime;
use clrecoder_core::codes::{normalize_scancode, DeviceKind, MouseButton};
use clrecoder_core::day;
use clrecoder_core::event::{AggEvent, DeviceKey, InputSourceId, RawEvent};
use clrecoder_core::motion::{
    local_day_from_unix_us, EffectiveDpi, MotionConnectionId, MotionControlSnapshot,
    MouseSourceDescriptor, MouseTravelDelta,
};
use crossbeam_channel::Sender;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC_EX};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceList, RegisterRawInputDevices, HRAWINPUT, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RAWKEYBOARD, RAWMOUSE, RIDEV_DEVNOTIFY,
    RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, PostThreadMessageW, SetTimer, SetWindowLongPtrW, TranslateMessage,
    GIDC_ARRIVAL, GIDC_REMOVAL, GWLP_USERDATA, GWLP_WNDPROC, HWND_MESSAGE, MSG, RI_KEY_BREAK,
    RI_KEY_E0, RI_KEY_E1, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_HWHEEL,
    RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_DOWN,
    RI_MOUSE_WHEEL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_INPUT_DEVICE_CHANGE, WM_QUIT,
    WM_TIMER,
};

/// RAWMOUSE 按下边沿位掩码（左/右/中/X1/X2 的 down 标志；up 标志与滚轮标志不在此列）。
const MOUSE_BUTTON_DOWN_MASK: u16 = (RI_MOUSE_LEFT_BUTTON_DOWN
    | RI_MOUSE_RIGHT_BUTTON_DOWN
    | RI_MOUSE_MIDDLE_BUTTON_DOWN
    | RI_MOUSE_BUTTON_4_DOWN
    | RI_MOUSE_BUTTON_5_DOWN) as u16;

/// 复用输入缓冲字数（64 字节 ≥ 键盘/鼠标事件 RAWINPUT 上限 48 字节；HID 未注册不到达）。
const BUF_WORDS: usize = 8;

/// 移动桶批发周期（motion-dpi §4.3：每 25ms WM_TIMER 批发有内容的桶）。
const TRAVEL_FLUSH_MS: u32 = 25;
/// 本窗口自建的移动桶批发定时器 ID（WM_TIMER wParam 锚定）。
const TRAVEL_FLUSH_TIMER_ID: usize = 1;

/// raw_input 采集线程句柄（motion-dpi §4.3：线程及本线程消息唤醒信息）。
///
/// [`RawInputRunner::stop_and_join`] 向自有消息线程投递 `WM_QUIT`（只发本进程自有
/// 消息线程，不改系统输入，§4.3.1）——窗口线程随后排出已捕获的尾桶并退出，join 返回。
pub struct RawInputRunner {
    handle: std::thread::JoinHandle<()>,
    /// 采集线程自报的 OS 线程 ID（PostThreadMessageW 目标；0=尚未登记）
    thread_id: Arc<AtomicU32>,
}

impl RawInputRunner {
    /// 停止并 join（motion-dpi §4.3.1）：先等线程登记其消息线程 ID（启动竞态窗口
    /// 极短，有界等待防 join 永挂），投递 `WM_QUIT` 后 join。线程体 panic 时 join
    /// 返回 Err——调用方写诊断并进入收尾，不因此永远等待。
    pub fn stop_and_join(self) -> std::thread::Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.thread_id.load(Ordering::Acquire) == 0
            && !self.handle.is_finished()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let tid = self.thread_id.load(Ordering::Acquire);
        if tid != 0 {
            // SAFETY: tid 为本进程 raw-input 线程启动时自报的 ID；WM_QUIT 使
            // GetMessageW 返回 0，消息循环在排出尾桶后退出。结果仅区分投递成败，
            // 失败（线程恰好退出）无害——join 自会收敛。
            let _ = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
        self.handle.join()
    }
}

/// 启动 raw_input 采集线程（PLAN §4.6 + motion-dpi §4.3）：`spawn(tx, motion) -> RawInputRunner`。
/// 事件经 crossbeam channel 送 aggregator；线程体 panic / 初始化失败自动重启
/// （1s 间隔、限频日志，§9.4 catch_unwind 兜底记录后继续）；重启前检查 `producer_stop`
/// （§4.3.1），停机请求后不再重建。
pub fn spawn(tx: Sender<AggEvent>, motion: Arc<MotionRuntime>) -> RawInputRunner {
    let thread_id = Arc::new(AtomicU32::new(0));
    let handle = {
        let thread_id = Arc::clone(&thread_id);
        std::thread::Builder::new()
            .name("raw-input".to_string())
            .spawn(move || {
                // SAFETY: 仅读取当前线程 ID（无副作用），用于 stop_and_join 的 WM_QUIT 定向投递。
                thread_id.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
                run(tx, motion);
            })
            .expect("raw_input 线程创建失败（内存耗尽等进程级错误）")
    };
    RawInputRunner { handle, thread_id }
}

/// 线程主体：消息循环 panic / 初始化失败 → 限频日志 + 1s 后重建窗口重试；
/// 收到 WM_QUIT（[`RawInputRunner::stop_and_join`]）或 `producer_stop`（§4.3.1）才退出。
fn run(tx: Sender<AggEvent>, motion: Arc<MotionRuntime>) {
    let mut failures: u64 = 0;
    loop {
        // §4.3.1：注册失败/重试均须检查 producer_stop——停机请求后不再重建采集窗口
        if motion.stop_requested() {
            return;
        }
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| message_loop(&tx, &motion)));
        match outcome {
            Ok(Ok(())) => return,
            Ok(Err(e)) => log_retry(
                &mut failures,
                &format!("raw_input 初始化/消息循环失败：{e}"),
            ),
            Err(_) => log_retry(
                &mut failures,
                "raw_input 线程 panic（已捕获，不影响其他采集线程）",
            ),
        }
        // 1s 重试等待切成小片，stop 请求 ≤100ms 内被观察到（§4.3.1）
        for _ in 0..10 {
            if motion.stop_requested() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// 重试日志限频：首次必记，此后每 60 次（≈1 分钟）记一次，避免热循环刷日志（§9.2）。
fn log_retry(failures: &mut u64, message: &str) {
    if *failures == 0 || (*failures).is_multiple_of(60) {
        log::error!(
            "{message}（第 {} 次，1s 后重试；每 60 次记录一次）",
            *failures + 1
        );
    }
    *failures += 1;
}

/// 建 message-only 窗口 → 注册三组 RIDEV_INPUTSINK|RIDEV_DEVNOTIFY → 枚举当前鼠标
/// 预注册运动来源 → 25ms 批发定时器 → GetMessageW 消息循环；WM_QUIT 退出前排尾桶。
fn message_loop(tx: &Sender<AggEvent>, motion: &Arc<MotionRuntime>) -> Result<(), String> {
    // 状态盒有意泄漏：窗口随线程退出被系统销毁时仍可能触发窗口过程，回收即悬垂；
    // 状态体量仅设备缓存 + 累计器（KB 级），进程驻留全程成本可忽略。
    let state = Box::into_raw(Box::new(RawInputState::new(tx, Arc::clone(motion))));
    let hwnd = unsafe { create_message_only_window(state) }
        .map_err(|e| format!("创建 message-only 窗口失败：{e}"))?;
    // 窗口退出清理守卫（§4.3）：注册失败 / WM_QUIT 正常退出 / unwind 三条路径都经 Drop
    // 在本线程 DestroyWindow——旧窗口在 1s 重试重建后不再继续投递旧来源事件。
    let _window_guard = WindowGuard(hwnd);
    unsafe { register_devices(hwnd) }.map_err(|e| format!("注册 Raw Input 设备失败：{e}"))?;
    // motion-dpi §4.3：注册后枚举当前鼠标预注册运动来源（无需移动即出现 DPI 入口）；
    // 枚举/解析失败仅跳过——首条 WM_INPUT 懒注册兜底。
    enumerate_current_mice(unsafe { &mut *state });
    // SAFETY: hwnd 为本线程刚创建的合法窗口；定时器随窗口销毁自动移除。
    unsafe { SetTimer(Some(hwnd), TRAVEL_FLUSH_TIMER_ID, TRAVEL_FLUSH_MS, None) };
    log::info!(
        "raw_input 采集线程就绪（keyboard+mouse+consumer，RIDEV_INPUTSINK|RIDEV_DEVNOTIFY）"
    );
    // §4.3/§5.2-6：本轮 loop 的任何输入之前，先经同一 sender 发 KeyboardSourcesReset
    // （此时尚未进入泵，队列中的 WM_INPUT 会在 reset 之后派发，FIFO 顺序由此保证）；
    // 进程级来源序号不随 loop 重建归零——旧来源状态由 reset 在 aggregator 侧清空。
    let _ = tx.send(AggEvent::KeyboardSourcesReset);
    let mut msg = MSG::default();
    loop {
        // GetMessageW 返回 0=WM_QUIT（进程关停）；-1=错误（如句柄失效）
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 == 0 {
            log::info!("raw_input 收到 WM_QUIT，消息循环退出");
            // motion-dpi §4.3.1：窗口线程排出已捕获的尾桶后退出（不双计——桶即清零）
            unsafe { &mut *state }.drain_all_travel();
            return Ok(());
        }
        if got.0 == -1 {
            let err = unsafe { windows::Win32::Foundation::GetLastError() };
            return Err(format!("GetMessageW 失败（Win32 错误码 {}）", err.0));
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 注册 Raw Input 后枚举当前鼠标（RIM_TYPEMOUSE）并预注册运动来源
/// （motion-dpi §4.3）。只读枚举；失败静默跳过（懒注册兜底）。
fn enumerate_current_mice(state: &mut RawInputState) {
    unsafe {
        // SAFETY: 只读枚举；首次调用 pvRIDI=None 探测数量，cbSize 取本 crate 类型布局。
        let cb_size = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
        let mut count: u32 = 0;
        let n = GetRawInputDeviceList(None, &mut count, cb_size);
        if n == u32::MAX || count == 0 {
            return;
        }
        let mut devices = vec![RAWINPUTDEVICELIST::default(); count as usize];
        // SAFETY: devices 按 count 分配，API 期间不释放/移动。
        let n = GetRawInputDeviceList(Some(devices.as_mut_ptr()), &mut count, cb_size);
        if n == u32::MAX {
            return;
        }
        devices.truncate(count as usize);
        let mice: Vec<HANDLE> = devices
            .iter()
            .filter(|d| d.dwType == RIM_TYPEMOUSE)
            .map(|d| d.hDevice)
            .collect();
        let mouse_count = mice.len();
        for h in &mice {
            state.register_motion_source_by_handle(*h);
        }
        log::info!("raw_input 已预注册 {mouse_count} 个当前鼠标的运动来源（懒注册兜底后续到达）");
    }
}

/// message-only 窗口的退出清理守卫（§4.3 重建合同）：无论 [`message_loop`] 以注册失败、
/// WM_QUIT 正常返回还是 panic unwind 收场，Drop 都在本线程调用 `DestroyWindow`，
/// 确保 1s 重试重建后旧窗口彻底停止投递（`DestroyWindow` 只能销毁调用线程的窗口，
/// 而本守卫只会在创建线程内被 Drop）。
struct WindowGuard(HWND);

impl Drop for WindowGuard {
    fn drop(&mut self) {
        // SAFETY: hwnd 由本线程 create_message_only_window 创建；Drop（正常结束或 unwind）
        // 都发生在创建线程内，满足 DestroyWindow 的线程归属约束。失败仅丢弃（重试路径
        // 自会再建新窗口）。
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

/// 创建 message-only 窗口（父窗口 `HWND_MESSAGE`）并挂接窗口过程。
///
/// windows 0.62 将 `WNDCLASSW`/`WNDCLASSEXW` gate 在 `Win32_Graphics_Gdi` feature 后
/// （§9.1 白名单未含），故复用 user32 系统全局类 `STATIC`，再以 `GWLP_WNDPROC` 子类化
/// 挂接 [`raw_input_wndproc`]——WM_INPUT 投递与消息循环行为与自注册类完全一致。
///
/// # Safety
/// `state` 必须是 `Box::into_raw` 产出的有效指针，且在本线程生命周期内不得回收。
unsafe fn create_message_only_window(state: *mut RawInputState) -> windows::core::Result<HWND> {
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        PCWSTR::null(),
        WINDOW_STYLE(0),
        0,
        0,
        0,
        0,
        Some(HWND_MESSAGE), // message-only：不可见、不参与 Z 序
        None,
        None,
        None,
    )?;
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
    let proc_fn =
        raw_input_wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc_fn as usize as isize);
    Ok(hwnd)
}

/// WM_INPUT / WM_INPUT_DEVICE_CHANGE / WM_TIMER 窗口过程（`GWLP_WNDPROC` 子类化挂接）。
/// 其余消息一律交 `DefWindowProcW`——WM_INPUT 契约要求调用 DefWindowProc 以便系统清理。
/// 本过程跨 FFI 边界，panic 即 abort 进程（§9.4）：内部 catch_unwind 只丢当条事件
/// （输入/设备生命周期/批发定时器分支均受保护，panic 不得跨 extern 边界）。
unsafe extern "system" fn raw_input_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RawInputState;
        if !ptr.is_null() {
            // Safety: 指针由 message_loop 以 Box::into_raw 存入且不回收，本线程独占访问
            let state = &mut *ptr;
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.on_wm_input(lparam)))
                .is_err()
            {
                log::error!("WM_INPUT 处理 panic，丢弃本条事件");
            }
        }
    } else if msg == WM_INPUT_DEVICE_CHANGE {
        // §4.3：RIDEV_DEVNOTIFY 注册后，wParam=GIDC_ARRIVAL/GIDC_REMOVAL、lParam=设备句柄。
        // 只处理移除；到达无需处理（首条 WM_INPUT 懒注册来源）。
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RawInputState;
        if !ptr.is_null() {
            // Safety: 指针由 message_loop 以 Box::into_raw 存入且不回收，本线程独占访问
            let state = &mut *ptr;
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state.on_device_change(wparam, lparam)
            }))
            .is_err()
            {
                log::error!("WM_INPUT_DEVICE_CHANGE 处理 panic，丢弃本条设备通知");
            }
        }
    } else if msg == WM_TIMER && wparam.0 == TRAVEL_FLUSH_TIMER_ID {
        // motion-dpi §4.3：每 25ms 批发有内容的移动桶
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RawInputState;
        if !ptr.is_null() {
            // Safety: 指针由 message_loop 以 Box::into_raw 存入且不回收，本线程独占访问
            let state = &mut *ptr;
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.on_flush_tick()))
                .is_err()
            {
                log::error!("WM_TIMER 批发 panic，本轮批发跳过");
            }
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// §4.6：注册 keyboard(UsagePage 1, Usage 6) + mouse(1, 2) + Consumer Control(0x0C, 1)
/// 三组目标设备，`RIDEV_INPUTSINK`（后台接收、无需窗口焦点）| `RIDEV_DEVNOTIFY`
///（设备到达/移除通知，§4.3 生命周期处理依赖）。
unsafe fn register_devices(hwnd: HWND) -> windows::core::Result<()> {
    let devices = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x0C,
            usUsage: 0x01,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        },
    ];
    RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
}

/// raw_input 线程状态：channel、来源注册器、设备解析缓存、按来源的鼠标累计、
/// 运动来源表、复用输入缓冲。
struct RawInputState {
    tx: Sender<AggEvent>,
    /// `(原生句柄, kind)` → 连接 ID（§4.3 来源注册器）
    sources: SourceRegistry,
    devices: crate::device::DeviceResolver,
    /// 鼠标按来源累计状态（F5：滚轮/水平滚轮各自独立于其他来源；移除时整项丢弃）
    mouse: HashMap<InputSourceId, MouseAccumulator>,
    /// 运动运行时（时钟/连接代际/DPI/控制快照，motion-dpi §4.3）
    motion: Arc<MotionRuntime>,
    /// 原生句柄 → 鼠标运动来源（描述/连接代际/当前移动桶）
    motion_sources: HashMap<isize, MouseMotionSource>,
    buf: Vec<u64>,
}

impl RawInputState {
    fn new(tx: &Sender<AggEvent>, motion: Arc<MotionRuntime>) -> Self {
        Self {
            tx: tx.clone(),
            sources: SourceRegistry::new(),
            devices: crate::device::DeviceResolver::new(),
            mouse: HashMap::new(),
            motion,
            motion_sources: HashMap::new(),
            buf: Vec::new(),
        }
    }

    /// 处理一条 WM_INPUT（lparam = HRAWINPUT）。任何失败仅丢弃当条事件。
    fn on_wm_input(&mut self, lparam: LPARAM) {
        let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let hraw = HRAWINPUT(lparam.0 as *mut core::ffi::c_void);
        if self.buf.is_empty() {
            self.buf.resize(BUF_WORDS, 0);
        }
        let mut copied = (self.buf.len() * std::mem::size_of::<u64>()) as u32;
        // 常规路径：u64 对齐复用缓冲一次取回（键盘/鼠标 RAWINPUT ≤ 48 字节）
        let mut n = unsafe {
            GetRawInputData(
                hraw,
                RID_INPUT,
                Some(self.buf.as_mut_ptr().cast::<core::ffi::c_void>()),
                &mut copied,
                header_size,
            )
        };
        if n == u32::MAX {
            // 防御兜底（理论不发生）：缓冲不足时先查实际大小、扩容后重试一次
            let mut needed = 0u32;
            let q = unsafe { GetRawInputData(hraw, RID_INPUT, None, &mut needed, header_size) };
            if q == u32::MAX || needed == 0 {
                return;
            }
            let words = needed.div_ceil(8) as usize;
            if words > self.buf.len() {
                self.buf.resize(words, 0);
                copied = (self.buf.len() * std::mem::size_of::<u64>()) as u32;
            }
            n = unsafe {
                GetRawInputData(
                    hraw,
                    RID_INPUT,
                    Some(self.buf.as_mut_ptr().cast::<core::ffi::c_void>()),
                    &mut copied,
                    header_size,
                )
            };
            if n == u32::MAX {
                return;
            }
        }
        if n < header_size {
            return;
        }
        // u64 对齐缓冲上重建 RAWINPUT 视图；仅读取与 dwType 匹配的联合体成员，
        // 保证读取范围不超过本次拷回的字节数
        let raw = unsafe { &*(self.buf.as_ptr().cast::<RAWINPUT>()) };
        match raw.header.dwType {
            t if t == RIM_TYPEKEYBOARD.0 => {
                if n < header_size + std::mem::size_of::<RAWKEYBOARD>() as u32 {
                    return;
                }
                // Safety: 仅读取与 dwType 匹配的成员，且 n 已验证覆盖 header+RAWKEYBOARD
                let kb = unsafe { &raw.data.keyboard };
                self.handle_keyboard(raw.header.hDevice, kb);
            }
            t if t == RIM_TYPEMOUSE.0 => {
                if n < header_size + std::mem::size_of::<RAWMOUSE>() as u32 {
                    return;
                }
                // Safety: 仅读取与 dwType 匹配的成员，且 n 已验证覆盖 header+RAWMOUSE
                let mouse = unsafe { &raw.data.mouse };
                self.handle_mouse(raw.header.hDevice, mouse);
            }
            _ => {} // 未注册的 RIM 类型（HID 等）：防御性忽略
        }
    }

    /// RAWKEYBOARD → `RawEvent::Keyboard`。down/up 均投递（Engine 依赖按下状态表去重
    /// 自动重复、判定组合键）；MakeCode==0 且 VKey!=0 先 MapVirtualKeyW 预处理；
    /// 0xFF 溢出码与垃圾码在 core 归一化中丢弃（§4.1）。事件携带来源注册器分配的
    /// 连接 ID（§4.3：有效句柄即使型号解析失败也独立来源）。
    fn handle_keyboard(&mut self, hdevice: HANDLE, kb: &RAWKEYBOARD) {
        // §4.1：make 取 RAWKEYBOARD.MakeCode 的低 8 位（windows 绑定中该字段为 u16）
        let sc =
            normalize_keyboard_event((kb.MakeCode & 0xFF) as u8, kb.Flags, kb.VKey, |vk| unsafe {
                MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX)
            });
        let Some(sc) = sc else {
            return; // 溢出码/垃圾码：丢弃当条，不影响其他事件（§1 防御性兜底）
        };
        let down = keyboard_is_down(kb.Flags);
        let source = self
            .sources
            .source_for(hdevice.0 as isize, DeviceKind::Keyboard);
        let device = self.devices.resolve(hdevice, DeviceKind::Keyboard);
        let _ = self.tx.send(AggEvent::Input(RawEvent::Keyboard {
            source,
            device,
            sc,
            down,
        }));
    }

    /// RAWMOUSE → 运动增量 counts + 按下边沿 `MouseClick` + 滚轮刻度（§4.6 累计器）。
    /// 滚轮**按来源**独立累计（§4.3/F5：两只鼠标的零头互不合并）；相对移动按
    /// motion-dpi §4.3 入移动桶（每包 hypot 一次，25ms 批发）。
    fn handle_mouse(&mut self, hdevice: HANDLE, mouse: &RAWMOUSE) {
        let source = self
            .sources
            .source_for(hdevice.0 as isize, DeviceKind::Mouse);
        let (flags, data, last_x, last_y, mouse_flags) = unsafe {
            let a = &mouse.Anonymous.Anonymous;
            (
                a.usButtonFlags,
                a.usButtonData,
                mouse.lLastX,
                mouse.lLastY,
                mouse.usFlags,
            )
        };
        // 运动增量（motion-dpi §4.3）：相对包每包只算一次 hypot(dx,dy)；
        // 绝对输入不混成 counts；不再发送旧 RawEvent::MouseMove（保留给旧合成 fixture）。
        const MOUSE_MOVE_ABSOLUTE: u16 = 0x01;
        if mouse_flags.0 & MOUSE_MOVE_ABSOLUTE == 0 && (last_x != 0 || last_y != 0) {
            let counts = ((last_x as f64).powi(2) + (last_y as f64).powi(2)).sqrt();
            self.accumulate_travel(hdevice.0 as isize, counts);
        }
        // 按下边沿（§5.2：仅物理按下计数；抬起不投递）
        if flags & MOUSE_BUTTON_DOWN_MASK != 0 {
            let device = self.devices.resolve(hdevice, DeviceKind::Mouse);
            for button in mouse_down_edges(flags) {
                let _ = self.tx.send(AggEvent::Input(RawEvent::MouseClick {
                    device: device.clone(),
                    button,
                }));
            }
        }
        // 垂直滚轮：正值=上滚（高分辨率滚轮单事件可 >120 → 累计器折算）；本来源累计（F5）
        if flags & RI_MOUSE_WHEEL as u16 != 0 {
            let ticks = self.mouse.entry(source).or_default().wheel.add(data as i16);
            if ticks != 0 {
                let device = self.devices.resolve(hdevice, DeviceKind::Mouse);
                let button = if ticks > 0 {
                    MouseButton::WheelUp
                } else {
                    MouseButton::WheelDown
                };
                self.send_mouse_clicks(&device, button, ticks.unsigned_abs());
            }
        }
        // 水平滚轮：正值=右倾；本来源累计（F5）
        if flags & RI_MOUSE_HWHEEL as u16 != 0 {
            let ticks = self
                .mouse
                .entry(source)
                .or_default()
                .hwheel
                .add(data as i16);
            if ticks != 0 {
                let device = self.devices.resolve(hdevice, DeviceKind::Mouse);
                let button = if ticks > 0 {
                    MouseButton::WheelRight
                } else {
                    MouseButton::WheelLeft
                };
                self.send_mouse_clicks(&device, button, ticks.unsigned_abs());
            }
        }
    }

    /// WM_INPUT_DEVICE_CHANGE 分支（§4.3）：wParam 携带 GIDC_ARRIVAL/GIDC_REMOVAL、
    /// lParam 携带设备句柄。只处理移除——到达无需处理，首条 WM_INPUT 懒注册来源。
    fn on_device_change(&mut self, wparam: WPARAM, lparam: LPARAM) {
        self.on_device_change_with(wparam, lparam, enumerate_current_mice);
    }

    fn on_device_change_with(
        &mut self,
        wparam: WPARAM,
        lparam: LPARAM,
        enumerate: impl FnOnce(&mut Self),
    ) {
        if wparam.0 == GIDC_ARRIVAL as usize {
            enumerate(self);
            return;
        }
        if wparam.0 != GIDC_REMOVAL as usize {
            return;
        }
        self.on_device_removed(HANDLE(lparam.0 as *mut core::ffi::c_void));
    }

    /// 设备移除（§4.3/§5.2-5）：删来源映射与该来源全部鼠标累计（不足阈值的余数随来源
    /// 丢弃——绝不转嫁给其他设备）、resolver 缓存失效（句柄复用防护）、逐个发
    /// `SourceRemoved`。未知句柄无来源可清、无副作用。
    fn on_device_removed(&mut self, hdevice: HANDLE) {
        // motion-dpi §4.3：运动来源先行收尾——排出尾桶（不双计：桶即清零/移除）、
        // 发布断连状态、移除映射。枚举预注册过但从未产生 InputSourceId 的鼠标
        // 也要覆盖（运动来源独立于按键来源注册）。
        if let Some(mut ms) = self.motion_sources.remove(&(hdevice.0 as isize)) {
            if let Some(b) = ms.bucket.take() {
                if b.counts > 0.0 {
                    let delta = MouseTravelDelta {
                        descriptor: ms.descriptor.clone(),
                        connection: ms.connection,
                        day: b.day,
                        counts: b.counts,
                        dpi: b.dpi,
                        control: b.control,
                    };
                    let _ = self.tx.send(AggEvent::MouseTravel(delta));
                }
            }
            self.motion
                .observe_mouse(ms.descriptor, ms.connection, false);
        }
        let ids = self.sources.remove_handle(hdevice.0 as isize);
        if ids.is_empty() {
            return;
        }
        for id in &ids {
            self.mouse.remove(id);
        }
        self.devices.forget_handle(hdevice);
        log::info!(
            "raw_input 设备移除：清理 {} 个来源（含鼠标累计与解析缓存）",
            ids.len()
        );
        for id in ids {
            let _ = self.tx.send(AggEvent::SourceRemoved { source: id });
        }
    }

    /// 投递 N 个同方向滚轮刻度事件。
    fn send_mouse_clicks(&self, device: &DeviceKey, button: MouseButton, count: u32) {
        for _ in 0..count {
            let _ = self.tx.send(AggEvent::Input(RawEvent::MouseClick {
                device: device.clone(),
                button,
            }));
        }
    }

    // ---------- 鼠标运动来源（motion-dpi §4.3） ----------

    /// 解析并注册鼠标运动来源（连接代际分配 + 运行时注册）。接口路径/ContainerID
    /// 只在注册或重连时查询（DeviceResolver 缓存，§4.3）；重复注册无副作用。
    fn register_motion_source_by_handle(&mut self, hdevice: HANDLE) {
        if self.motion_sources.contains_key(&(hdevice.0 as isize)) {
            return;
        }
        let descriptor = self.devices.mouse_source(hdevice);
        self.register_motion_source_with(hdevice.0 as isize, descriptor);
    }

    /// 注册核心（描述来源可注入——生产走 [`Self::register_motion_source_by_handle`]，
    /// 单测注入物理/虚拟描述）。
    fn register_motion_source_with(
        &mut self,
        raw_handle: isize,
        descriptor: MouseSourceDescriptor,
    ) {
        let connection = self.motion.allocate_connection();
        self.motion
            .observe_mouse(descriptor.clone(), connection, true);
        self.motion_sources.insert(
            raw_handle,
            MouseMotionSource {
                descriptor,
                connection,
                bucket: None,
            },
        );
    }

    /// 相对移动包 → counts 桶（motion-dpi §4.3）：
    /// - 捕获一份一致 control 快照与采样日；paused 包不累计；
    /// - DPI 快照/捕获日/暂停 epoch 任一变化：先发旧有效桶再开始新桶（不跨桶混计）；
    /// - 首条输入懒注册来源（启动枚举已覆盖既有设备）。
    fn accumulate_travel(&mut self, raw_handle: isize, counts: f64) {
        if !self.motion_sources.contains_key(&raw_handle) {
            self.register_motion_source_by_handle(HANDLE(raw_handle as *mut core::ffi::c_void));
        }
        let control = self.motion.control();
        let stamp = self.motion.stamp();
        let capture_day = local_day_from_unix_us(stamp.unix_us)
            .map(day::format_day)
            .unwrap_or_else(day::today_local);
        let Some(ms) = self.motion_sources.get_mut(&raw_handle) else {
            return;
        };
        let dpi = self.motion.dpi_for(&ms.descriptor.source_key);
        // 桶键（epoch×日×DPI）变化：先发旧有效桶再换新桶
        let mut flushed = None;
        if let Some(b) = ms.bucket.as_ref() {
            if b.control.epoch != control.epoch || b.day != capture_day || b.dpi != dpi {
                if b.counts > 0.0 {
                    flushed = Some(MouseTravelDelta {
                        descriptor: ms.descriptor.clone(),
                        connection: ms.connection,
                        day: b.day.clone(),
                        counts: b.counts,
                        dpi: b.dpi,
                        control: b.control,
                    });
                }
                ms.bucket = None;
            }
        }
        if !control.paused {
            let bucket = ms.bucket.get_or_insert_with(|| TravelBucket {
                day: capture_day.clone(),
                dpi,
                control: MotionControlSnapshot {
                    epoch: control.epoch,
                    paused: false,
                },
                counts: 0.0,
            });
            bucket.counts += counts;
        }
        if let Some(delta) = flushed {
            let _ = self.tx.send(AggEvent::MouseTravel(delta));
        }
    }

    /// WM_TIMER 25ms 批发（motion-dpi §4.3）：发出全部有内容的桶并清零计数
    /// （桶键保留，供 epoch/日/DPI 变化判定；零内容桶不投递）。
    fn on_flush_tick(&mut self) {
        for ms in self.motion_sources.values_mut() {
            let Some(b) = ms.bucket.as_mut() else {
                continue;
            };
            if b.counts > 0.0 {
                let delta = MouseTravelDelta {
                    descriptor: ms.descriptor.clone(),
                    connection: ms.connection,
                    day: b.day.clone(),
                    counts: b.counts,
                    dpi: b.dpi,
                    control: b.control,
                };
                let _ = self.tx.send(AggEvent::MouseTravel(delta));
                b.counts = 0.0;
            }
        }
    }

    /// 全部来源的尾桶排出（WM_QUIT 正常退出，motion-dpi §4.3"正常退出排出尾数"）。
    fn drain_all_travel(&mut self) {
        for ms in self.motion_sources.values_mut() {
            if let Some(b) = ms.bucket.take() {
                if b.counts > 0.0 {
                    let delta = MouseTravelDelta {
                        descriptor: ms.descriptor.clone(),
                        connection: ms.connection,
                        day: b.day,
                        counts: b.counts,
                        dpi: b.dpi,
                        control: b.control,
                    };
                    let _ = self.tx.send(AggEvent::MouseTravel(delta));
                }
            }
        }
    }
}

/// 单个鼠标的运动来源状态（motion-dpi §4.3）：描述 + 连接代际 + 当前移动桶。
struct MouseMotionSource {
    descriptor: MouseSourceDescriptor,
    connection: MotionConnectionId,
    /// 当前未批发/未换新的移动桶（键 = 连接×捕获本地日×EffectiveDpi×暂停 epoch）
    bucket: Option<TravelBucket>,
}

/// 当前移动桶（motion-dpi §4.3）：DPI 快照/捕获日/暂停 epoch 任一变化即整桶
/// 批发换新——桶内 control 的 paused 恒为 false（paused 包不累计）。
struct TravelBucket {
    day: String,
    dpi: EffectiveDpi,
    control: MotionControlSnapshot,
    counts: f64,
}

/// 来源注册器（§4.3）：`(原生句柄, kind)` → 进程内连接 ID 的映射。
///
/// - 同连接重复访问返回同 ID；不同 kind 是不同来源（注册键含 kind）；
/// - 原生句柄 0（系统未提供 hDevice，如 precision touchpad）只能降级为该 kind 的
///   **一个共享未知来源**——不声称能区分系统未提供句柄的设备；有效句柄即使型号
///   解析失败也独立来源；
/// - [`SourceRegistry::remove_handle`] 返回该句柄全部 kind 的 ID；移除后再次出现
///   必须分配新 ID；
/// - ID 来自进程级单调计数器，不随 message_loop 重建归零；0 保留给 Engine
///   单来源兼容入口（`clrecoder_core::event::InputSourceId` 契约）。
#[derive(Debug, Default)]
struct SourceRegistry {
    by_handle: HashMap<(isize, DeviceKind), InputSourceId>,
}

impl SourceRegistry {
    fn new() -> Self {
        Self::default()
    }

    /// 注册或查询来源：同 `(原生句柄, kind)` 连接复用同 ID，新连接分配新 ID。
    fn source_for(&mut self, raw_handle: isize, kind: DeviceKind) -> InputSourceId {
        if let Some(id) = self.by_handle.get(&(raw_handle, kind)) {
            return *id;
        }
        let id = InputSourceId(next_source_id());
        self.by_handle.insert((raw_handle, kind), id);
        id
    }

    /// 移除一个原生句柄的全部 kind 来源并返回其 ID（升序）；未知句柄返回空、无副作用。
    fn remove_handle(&mut self, raw_handle: isize) -> Vec<InputSourceId> {
        let mut ids = Vec::new();
        self.by_handle.retain(|&(h, _), id| {
            if h == raw_handle {
                ids.push(*id);
                false
            } else {
                true
            }
        });
        // 升序发送 SourceRemoved，行为确定（合同仅要求逐个发送）
        ids.sort_unstable_by_key(|id| id.0);
        ids
    }
}

/// 进程级来源序号（§4.3）：单调递增、不随 message_loop 重建归零；从 1 起——
/// 0 保留给 Engine 单来源兼容入口。
fn next_source_id() -> u64 {
    static NEXT_SOURCE_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_SOURCE_ID.fetch_add(1, Ordering::Relaxed)
}

/// 单个来源的鼠标累计状态（§4.3）：垂直/水平滚轮各自走 [`WheelAccumulator`]。
/// 来源移除时整项丢弃——不足一格的零头绝不转嫁给其他设备（§5.2）。
/// （motion-dpi §4.3：移动 counts 改由 [`MouseMotionSource`] 按桶批发，不经本累计器。）
#[derive(Debug, Default)]
struct MouseAccumulator {
    wheel: WheelAccumulator,
    hwheel: WheelAccumulator,
}

/// 滚轮 delta 累计器（§4.6）：120 delta = 1 刻度。高分辨率滚轮（单事件 ±240 或更大）与
/// 自由滚轮小步长均正确折算；acc 用 i32 防溢出（单事件 delta 可达 i16 极值，
/// 119 + 32767 会击穿 i16 累计器），折算语义与 §4.6 逐字一致。
#[derive(Debug, Default)]
struct WheelAccumulator {
    acc: i32,
}

impl WheelAccumulator {
    /// 累计一个 delta（`RAWMOUSE.usButtonData` 的有符号解释），
    /// 返回折算出的净刻度数（正=上/右，负=下/左）。
    fn add(&mut self, delta: i16) -> i32 {
        self.acc += delta as i32;
        let mut ticks = 0;
        while self.acc >= 120 {
            ticks += 1;
            self.acc -= 120;
        }
        while self.acc <= -120 {
            ticks -= 1;
            self.acc += 120;
        }
        ticks
    }
}

/// RAWMOUSE `usButtonFlags` → 本次 WM_INPUT 中"按下"的按键（按下边沿；up 标志忽略）。
/// 顺序固定为 Left/Right/Middle/X1/X2（与掩码位序一致）。
fn mouse_down_edges(flags: u16) -> Vec<MouseButton> {
    let mut down = Vec::with_capacity(2);
    if flags & RI_MOUSE_LEFT_BUTTON_DOWN as u16 != 0 {
        down.push(MouseButton::Left);
    }
    if flags & RI_MOUSE_RIGHT_BUTTON_DOWN as u16 != 0 {
        down.push(MouseButton::Right);
    }
    if flags & RI_MOUSE_MIDDLE_BUTTON_DOWN as u16 != 0 {
        down.push(MouseButton::Middle);
    }
    if flags & RI_MOUSE_BUTTON_4_DOWN as u16 != 0 {
        down.push(MouseButton::X1);
    }
    if flags & RI_MOUSE_BUTTON_5_DOWN as u16 != 0 {
        down.push(MouseButton::X2);
    }
    down
}

/// `RI_KEY_BREAK` 位决定按下/抬起边沿（0=make，1=break）；E0/E1 标志不参与判定。
fn keyboard_is_down(flags: u16) -> bool {
    flags & RI_KEY_BREAK as u16 == 0
}

/// 键盘事件归一化（§4.1/§4.6）：MakeCode==0 且 VKey!=0 时先经 `vsc_resolver`
/// （生产实现即 `MapVirtualKeyW(VK, MAPVK_VK_TO_VSC_EX)`，VSC_EX 高字节含 0xE0/0xE1 前缀）
/// 解析出 scancode 再调 `core::codes::normalize_scancode`（0xFF 丢弃、Pause 归一等在其中）。
/// `vsc_resolver` 注入以便单测。
fn normalize_keyboard_event(
    make: u8,
    flags: u16,
    vkey: u16,
    vsc_resolver: impl Fn(u16) -> u32,
) -> Option<u16> {
    let (make, e0, e1) = if make == 0 && vkey != 0 {
        let vsc = vsc_resolver(vkey);
        if vsc == 0 {
            (0u8, false, false) // 解析不出 scancode：交由 normalize_scancode 的 make==0 规则丢弃
        } else {
            let prefix = (vsc >> 8) as u8;
            ((vsc & 0xFF) as u8, prefix == 0xE0, prefix == 0xE1)
        }
    } else {
        (
            make,
            flags & RI_KEY_E0 as u16 != 0,
            flags & RI_KEY_E1 as u16 != 0,
        )
    };
    normalize_scancode(make, e0, e1, vkey)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_dpi_arrival_registers_mouse_before_any_movement() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let runtime = MotionRuntime::offline(Arc::new(crate::ipc_server::Flags::default()));
        let mut state = RawInputState::new(&tx, Arc::clone(&runtime));
        state.on_device_change_with(WPARAM(GIDC_ARRIVAL as usize), LPARAM(42), |s| {
            s.register_motion_source_with(
                42,
                MouseSourceDescriptor {
                    source_key: "test:hotplug".into(),
                    model: DeviceKey {
                        kind: DeviceKind::Mouse,
                        vid: 1,
                        pid: 2,
                        name: "测试鼠标".into(),
                    },
                    interface_path: None,
                    physical: true,
                },
            );
        });
        assert!(state.motion_sources.contains_key(&42));
        assert!(runtime
            .sources_snapshot()
            .iter()
            .any(|s| s.source_key == "test:hotplug" && s.connected));
    }
    use clrecoder_core::motion::{DpiOrigin, MotionStamp};
    use windows::Win32::UI::Input::{MOUSE_STATE, RAWMOUSE_0, RAWMOUSE_0_0};
    use windows::Win32::UI::WindowsAndMessaging::RI_MOUSE_LEFT_BUTTON_UP;

    use crate::ipc_server::Flags;

    // ---------- WheelAccumulator（§4.6：120 delta 一格） ----------

    #[test]
    fn wheel_accumulates_one_notch_at_threshold() {
        let mut acc = WheelAccumulator::default();
        assert_eq!(acc.add(120), 1);
        assert_eq!(acc.add(120), 1);
        assert_eq!(acc.add(-120), -1);
        // 不足一格不产出
        assert_eq!(acc.add(119), 0);
        assert_eq!(acc.add(1), 1); // 119+1=120 → 1 格，余 0
    }

    #[test]
    fn wheel_accumulator_carries_remainder_across_events() {
        let mut acc = WheelAccumulator::default();
        assert_eq!(acc.add(40), 0);
        assert_eq!(acc.add(40), 0);
        assert_eq!(acc.add(40), 1); // 自由滚轮小步长累计到一格
        assert_eq!(acc.add(40), 0); // 余 40 保留
    }

    #[test]
    fn wheel_multiple_notches_per_event_for_high_resolution_wheels() {
        let mut acc = WheelAccumulator::default();
        assert_eq!(acc.add(240), 2);
        assert_eq!(acc.add(-300), -2); // -240 → -2，余 -60
        assert_eq!(acc.add(-60), -1); // 余量补满一格
        assert_eq!(acc.add(0), 0);
    }

    #[test]
    fn wheel_extreme_deltas_do_not_overflow_and_keep_remainder() {
        let mut acc = WheelAccumulator::default();
        // i16::MAX = 32767 = 273*120 + 7
        assert_eq!(acc.add(i16::MAX), 273);
        assert_eq!(acc.add(113), 1); // 7 + 113 = 120
                                     // i16::MIN = -32768 = -(273*120 + 8)
        assert_eq!(acc.add(i16::MIN), -273);
        assert_eq!(acc.add(113), 0); // -8 + 113 = 105，余量保留
        assert_eq!(acc.add(15), 1); // 105 + 15 = 120
    }

    // ---------- mouse_down_edges（RAWMOUSE ButtonFlags 按下边沿提取） ----------

    #[test]
    fn mouse_down_edges_extract_press_edges_only() {
        assert_eq!(mouse_down_edges(0), Vec::<MouseButton>::new());
        assert_eq!(
            mouse_down_edges(RI_MOUSE_LEFT_BUTTON_DOWN as u16),
            vec![MouseButton::Left]
        );
        assert_eq!(
            mouse_down_edges(RI_MOUSE_RIGHT_BUTTON_DOWN as u16),
            vec![MouseButton::Right]
        );
        assert_eq!(
            mouse_down_edges(RI_MOUSE_MIDDLE_BUTTON_DOWN as u16),
            vec![MouseButton::Middle]
        );
        assert_eq!(
            mouse_down_edges(RI_MOUSE_BUTTON_4_DOWN as u16),
            vec![MouseButton::X1]
        );
        assert_eq!(
            mouse_down_edges(RI_MOUSE_BUTTON_5_DOWN as u16),
            vec![MouseButton::X2]
        );
        // 抬起标志被忽略（仅按下边沿计数，§5.2）
        assert_eq!(
            mouse_down_edges((RI_MOUSE_LEFT_BUTTON_DOWN | RI_MOUSE_LEFT_BUTTON_UP) as u16),
            vec![MouseButton::Left]
        );
        assert_eq!(
            mouse_down_edges(RI_MOUSE_LEFT_BUTTON_UP as u16),
            Vec::<MouseButton>::new()
        );
    }

    #[test]
    fn mouse_down_edges_handle_combined_flags_in_fixed_order() {
        // 左键 + X1 同时按下
        assert_eq!(
            mouse_down_edges((RI_MOUSE_LEFT_BUTTON_DOWN | RI_MOUSE_BUTTON_4_DOWN) as u16),
            vec![MouseButton::Left, MouseButton::X1]
        );
        // 全部按下
        assert_eq!(
            mouse_down_edges(
                (RI_MOUSE_LEFT_BUTTON_DOWN
                    | RI_MOUSE_RIGHT_BUTTON_DOWN
                    | RI_MOUSE_MIDDLE_BUTTON_DOWN
                    | RI_MOUSE_BUTTON_4_DOWN
                    | RI_MOUSE_BUTTON_5_DOWN) as u16
            ),
            vec![
                MouseButton::Left,
                MouseButton::Right,
                MouseButton::Middle,
                MouseButton::X1,
                MouseButton::X2
            ]
        );
        // 滚轮标志不属于按键
        assert_eq!(
            mouse_down_edges(RI_MOUSE_WHEEL as u16),
            Vec::<MouseButton>::new()
        );
    }

    // ---------- normalize_keyboard_event（E0/E1 归一化 + MapVirtualKeyW 预处理） ----------

    #[test]
    fn keyboard_nonzero_make_code_passes_flags_through() {
        let never = |_: u16| 0u32; // make!=0 时不得触发 MapVirtualKeyW 预处理
        assert_eq!(normalize_keyboard_event(0x1E, 0, 0x41, never), Some(0x1E)); // 'A'
        assert_eq!(normalize_keyboard_event(0x1D, 0, 0x11, never), Some(0x1D)); // 左 Ctrl
        assert_eq!(
            normalize_keyboard_event(0x1D, RI_KEY_E0 as u16, 0x11, never),
            Some(0xE01D) // 右 Ctrl（E0 前缀）
        );
        assert_eq!(
            normalize_keyboard_event(0x5B, RI_KEY_E0 as u16, 0x5B, never),
            Some(0xE05B) // 左 Win
        );
        // 0xFF 溢出码在 core 归一化丢弃（端到端验证采集层入口，§4.1）
        assert_eq!(normalize_keyboard_event(0xFF, 0, 0x00, never), None);
        // make==0 且 vkey==0 的垃圾事件 → 丢弃
        assert_eq!(normalize_keyboard_event(0, 0, 0, never), None);
    }

    #[test]
    fn keyboard_zero_make_code_resolved_via_mapvirtualkey_stub() {
        // 模拟 MAPVK_VK_TO_VSC_EX：'A'→0x1E；右 Ctrl→0xE01D；Pause→0xE11D；其余→0
        let resolver = |vk: u16| match vk {
            0x41 => 0x1E,
            0xA3 => 0xE01D,
            0x13 => 0xE11D,
            _ => 0,
        };
        assert_eq!(normalize_keyboard_event(0, 0, 0x41, resolver), Some(0x1E));
        // VSC_EX 高字节 0xE0 → e0 标志（与事件自带 E0 标志语义一致）
        assert_eq!(normalize_keyboard_event(0, 0, 0xA3, resolver), Some(0xE01D));
        // VSC_EX 高字节 0xE1 → e1 标志（Pause 首事件归一化契约）
        assert_eq!(normalize_keyboard_event(0, 0, 0x13, resolver), Some(0xE11D));
        // 解析失败（返回 0）→ 丢弃
        assert_eq!(normalize_keyboard_event(0, 0, 0x99, resolver), None);
        // 若 MapVirtualKeyW 对 VK_PAUSE 返回伴随码 0x45 → Pause 伴随事件特判归一化
        assert_eq!(
            normalize_keyboard_event(0, 0, 0x13, |vk| if vk == 0x13 { 0x45 } else { 0 }),
            Some(0xE11D)
        );
    }

    #[test]
    fn keyboard_break_bit_only_decides_edge_direction() {
        // RI_KEY_BREAK 只决定 down/up，不影响 sc 归一化
        assert!(keyboard_is_down(0));
        assert!(!keyboard_is_down(RI_KEY_BREAK as u16));
        // E0 标志不影响 make/break 判定
        assert!(keyboard_is_down(RI_KEY_E0 as u16));
        let never = |_: u16| 0u32;
        assert_eq!(
            normalize_keyboard_event(0x2A, 0, 0x2A, never),
            normalize_keyboard_event(0x2A, RI_KEY_BREAK as u16, 0x2A, never)
        );
    }

    // ==================================================================
    // correctness-v2 §4.3：来源注册器 / 鼠标按来源累计 / 设备移除生命周期（F5）
    // 仅构造 RawInputState 与 RAWKEYBOARD/RAWMOUSE 值直接驱动处理函数——不起窗口、
    // 不注册设备、不泵消息（无实时采集）。假句柄的设备路径查询必然失败，
    // DeviceKey 落"未知/虚拟设备"桶（§4.2），不影响来源隔离与累计语义。
    // ==================================================================

    /// 测试用伪句柄（仅用作映射键，不传给任何窗口/设备注册调用）。
    fn handle(v: isize) -> HANDLE {
        HANDLE(v as *mut core::ffi::c_void)
    }

    /// 测试用 offline 运行时（无 worker、无 DB；§8 fixture 禁真实 HID/生产 DB）。
    fn test_motion() -> Arc<MotionRuntime> {
        MotionRuntime::offline(Arc::new(Flags::default()))
    }

    /// 测试用物理鼠标来源描述（source_key 按句柄区分，供 DPI 分桶用例）。
    fn physical_desc(key: &str) -> MouseSourceDescriptor {
        MouseSourceDescriptor {
            source_key: key.to_string(),
            model: DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0x046D,
                pid: 0xC08B,
                name: "测试鼠标".to_string(),
            },
            interface_path: Some(r"\\?\HID#VID_046D&PID_C08B&MI_00#7&2f3a3d&0&0000".to_string()),
            physical: true,
        }
    }

    /// 排出通道里的 MouseTravel。
    fn drain_travel(rx: &crossbeam_channel::Receiver<AggEvent>) -> Vec<MouseTravelDelta> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let AggEvent::MouseTravel(d) = ev {
                out.push(d);
            }
        }
        out
    }

    fn keyboard(make: u16, flags: u16) -> RAWKEYBOARD {
        RAWKEYBOARD {
            MakeCode: make,
            Flags: flags,
            Reserved: 0,
            VKey: 0,
            Message: 0,
            ExtraInformation: 0,
        }
    }

    /// 按钮标志 + 滚轮 delta + 相对位移构造 RAWMOUSE（usButtonData 以 i16 有符号解释）。
    fn mouse(button_flags: u16, button_data: i16, last_x: i32, last_y: i32) -> RAWMOUSE {
        RAWMOUSE {
            usFlags: MOUSE_STATE(0),
            Anonymous: RAWMOUSE_0 {
                Anonymous: RAWMOUSE_0_0 {
                    usButtonFlags: button_flags,
                    usButtonData: button_data as u16,
                },
            },
            ulRawButtons: 0,
            lLastX: last_x,
            lLastY: last_y,
            ulExtraInformation: 0,
        }
    }

    fn wheel(delta: i16) -> RAWMOUSE {
        mouse(RI_MOUSE_WHEEL as u16, delta, 0, 0)
    }

    /// 注册器：同 (句柄, kind) 复用同 ID；不同 kind 与不同句柄都是不同来源；正 ID。
    #[test]
    fn correctness_v2_source_registry_reuses_id_per_handle_and_kind() {
        let mut r = SourceRegistry::new();
        let a1 = r.source_for(9, DeviceKind::Keyboard);
        let a2 = r.source_for(9, DeviceKind::Keyboard);
        assert_eq!(a1, a2, "同连接重复访问必须同 ID");
        assert!(a1.0 >= 1, "0 保留给 Engine 兼容入口，真实来源用正 ID");
        // 不同 kind：不同来源
        assert_ne!(r.source_for(9, DeviceKind::Mouse), a1);
        // 不同句柄：不同来源（有效句柄即使型号解析失败也独立来源）
        assert_ne!(r.source_for(11, DeviceKind::Keyboard), a1);
    }

    /// null 句柄（0）降级为该 kind 的一个共享未知来源；不声称区分无句柄设备。
    #[test]
    fn correctness_v2_source_registry_null_handle_shares_one_unknown_source_per_kind() {
        let mut r = SourceRegistry::new();
        let k1 = r.source_for(0, DeviceKind::Keyboard);
        let k2 = r.source_for(0, DeviceKind::Keyboard);
        assert_eq!(k1, k2, "null 句柄同 kind 共享一个未知来源");
        assert_ne!(r.source_for(0, DeviceKind::Mouse), k1, "共享按 kind 分立");
        assert_ne!(
            r.source_for(5, DeviceKind::Keyboard),
            k1,
            "有效句柄独立来源"
        );
    }

    /// remove_handle 返回该句柄全部 kind 的 ID；重复移除为空；再次出现必须新 ID；
    /// 无关句柄不受影响。
    #[test]
    fn correctness_v2_source_registry_remove_handle_returns_all_kinds_then_new_ids() {
        let mut r = SourceRegistry::new();
        let kb = r.source_for(9, DeviceKind::Keyboard);
        let mo = r.source_for(9, DeviceKind::Mouse);
        let other = r.source_for(10, DeviceKind::Keyboard);
        let mut removed = r.remove_handle(9);
        removed.sort_by_key(|id| id.0);
        let mut expected = vec![kb, mo];
        expected.sort_by_key(|id| id.0);
        assert_eq!(removed, expected, "移除句柄应返回其全部 kind 来源");
        assert_eq!(
            r.remove_handle(9),
            Vec::<InputSourceId>::new(),
            "重复移除无副作用"
        );
        let kb2 = r.source_for(9, DeviceKind::Keyboard);
        assert_ne!(kb2, kb, "移除后再次出现必须新 ID");
        assert_eq!(
            r.source_for(10, DeviceKind::Keyboard),
            other,
            "无关句柄不受影响"
        );
    }

    /// 进程级来源序号不随注册器（loop）重建归零复用（§4.3）。
    #[test]
    fn correctness_v2_source_ids_do_not_reuse_across_registry_recreation() {
        let id_before = {
            let mut r1 = SourceRegistry::new();
            r1.source_for(7, DeviceKind::Keyboard)
        };
        let id_after = {
            let mut r2 = SourceRegistry::new(); // 模拟 message_loop 重建后的全新注册器
            r2.source_for(7, DeviceKind::Keyboard)
        };
        assert!(
            id_after.0 > id_before.0,
            "重建后新 ID 必须严格递增（{id_before:?} → {id_after:?}）"
        );
    }

    /// 键盘事件携带来源注册器分配的连接 ID；假句柄型号解析失败仍独立来源并落未知桶。
    #[test]
    fn correctness_v2_keyboard_events_carry_registry_source() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = RawInputState::new(&tx, test_motion());
        let h = handle(0x77);
        let expected = state.sources.source_for(h.0 as isize, DeviceKind::Keyboard);
        state.handle_keyboard(h, &keyboard(0x1E, 0)); // 'A' down
        match rx.try_recv().unwrap() {
            AggEvent::Input(RawEvent::Keyboard {
                source,
                device,
                sc,
                down,
            }) => {
                assert_eq!(source, expected, "键盘事件必须携带注册器分配的来源");
                assert_eq!(sc, 0x1E);
                assert!(down);
                assert_eq!(
                    device,
                    DeviceKey {
                        kind: DeviceKind::Keyboard,
                        vid: 0,
                        pid: 0,
                        name: "未知/虚拟设备".to_string()
                    },
                    "假句柄解析失败落未知桶（§4.2），但来源仍独立注册"
                );
            }
            other => panic!("应为键盘事件: {other:?}"),
        }
    }

    /// 两只鼠标的滚轮零头互不合并（F5 反例锚点）：各 +60 都不出格；各自补到 120 才出格。
    #[test]
    fn correctness_v2_two_mice_wheel_thresholds_are_independent() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = RawInputState::new(&tx, test_motion());
        let a = handle(0xA1);
        let b = handle(0xB2);
        // 合计 120 但分属不同来源——不得合成一格
        state.handle_mouse(a, &wheel(60));
        state.handle_mouse(b, &wheel(60));
        assert!(rx.try_recv().is_err(), "不同来源的滚轮零头不得合并投递");
        // 来源 A 补 60：自身达 120 → 恰好 1 格 WheelUp
        state.handle_mouse(a, &wheel(60));
        assert!(matches!(
            rx.try_recv().unwrap(),
            AggEvent::Input(RawEvent::MouseClick {
                button: MouseButton::WheelUp,
                ..
            })
        ));
        // 来源 B 仍欠 60：补 60 才出格
        state.handle_mouse(b, &wheel(60));
        assert!(matches!(
            rx.try_recv().unwrap(),
            AggEvent::Input(RawEvent::MouseClick {
                button: MouseButton::WheelUp,
                ..
            })
        ));
        assert!(rx.try_recv().is_err());
    }

    /// 两只鼠标的移动门槛互不合并（F5）——motion-dpi §4.3 后的形态：相对 counts
    /// 按来源分桶、25ms 批发，无 0.25 英寸门槛；旧 `RawEvent::MouseMove` 不再由
    /// 真实生产分支发送（§4.1 保留给旧合成 fixture）。
    #[test]
    fn motion_dpi_two_mice_travel_buckets_are_independent_no_legacy_mouse_move() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let a = handle(0xC1);
        let b = handle(0xD2);
        // 各自注册物理来源（同一 DeviceKey 型号、不同 source_key）
        state.register_motion_source_with(a.0 as isize, physical_desc("k-a"));
        state.register_motion_source_with(b.0 as isize, physical_desc("k-b"));
        // 合计分属不同来源——不得合并成一条
        state.handle_mouse(a, &mouse(0, 0, 10, 0));
        state.handle_mouse(b, &mouse(0, 0, 10, 0));
        assert!(drain_travel(&rx).is_empty(), "25ms 批发前不投递");
        // 25ms 批发：两条独立增量（各 10 counts），无 MouseMove
        state.on_flush_tick();
        let mut travels = drain_travel(&rx);
        assert_eq!(travels.len(), 2, "两只鼠标各自一条增量");
        travels.sort_by(|x, y| x.descriptor.source_key.cmp(&y.descriptor.source_key));
        assert_eq!(travels[0].descriptor.source_key, "k-a");
        assert_eq!(travels[1].descriptor.source_key, "k-b");
        for t in &travels {
            assert!(
                (t.counts - 10.0).abs() < 1e-9,
                "hypot(10,0)=10，实际 {}",
                t.counts
            );
            assert_eq!(
                t.dpi,
                EffectiveDpi {
                    value: None,
                    origin: DpiOrigin::Unknown
                }
            );
        }
        // 再次批发：桶已清零，不双计
        state.on_flush_tick();
        assert!(drain_travel(&rx).is_empty(), "批发后不得重复投递");
        // 整条通道无任何 MouseMove（真实生产分支停止发送，§4.1 保留给旧合成 fixture）
        while let Ok(ev) = rx.try_recv() {
            assert!(
                !matches!(ev, AggEvent::Input(RawEvent::MouseMove { .. })),
                "真实生产分支不得发送旧 MouseMove: {ev:?}"
            );
        }
    }

    /// 设备移除（§4.3/§5.2-5）：逐个发 SourceRemoved（键盘+鼠标来源），鼠标不足阈值
    /// 零头随来源丢弃不转发；句柄复用后从零累计（不继承旧零头）且必须新 ID。
    #[test]
    fn correctness_v2_device_removal_drops_remainder_notifies_and_reallocates() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = RawInputState::new(&tx, test_motion());
        let h = handle(0xE1);
        let id_kb = state.sources.source_for(h.0 as isize, DeviceKind::Keyboard);
        let id_mouse = state.sources.source_for(h.0 as isize, DeviceKind::Mouse);
        // 鼠标滚 60（不足一格）后拔出
        state.handle_mouse(h, &wheel(60));
        state.on_device_removed(h);
        // 键盘与鼠标两个 kind 来源逐个发 SourceRemoved（升序）
        let mut removed = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AggEvent::SourceRemoved { source } => removed.push(source),
                other => panic!("移除时只应发 SourceRemoved: {other:?}"),
            }
        }
        removed.sort_by_key(|id| id.0);
        let mut expected = vec![id_kb, id_mouse];
        expected.sort_by_key(|id| id.0);
        assert_eq!(removed, expected, "每个 kind 来源各发一条 SourceRemoved");

        // 句柄复用：再次出现必须新 ID，且鼠标累计从零开始（60 不足一格，不继承旧零头）
        state.handle_mouse(h, &wheel(60));
        assert!(rx.try_recv().is_err(), "新来源必须从零累计");
        let id_new = state.sources.source_for(h.0 as isize, DeviceKind::Mouse);
        assert_ne!(id_new, id_mouse, "移除后再次出现必须新 ID");
        state.handle_mouse(h, &wheel(60));
        assert!(matches!(
            rx.try_recv().unwrap(),
            AggEvent::Input(RawEvent::MouseClick {
                button: MouseButton::WheelUp,
                ..
            })
        ));
    }

    /// 未知句柄移除无副作用：不发事件、不影响已注册来源。
    #[test]
    fn correctness_v2_unknown_handle_removal_is_noop() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = RawInputState::new(&tx, test_motion());
        let h = handle(0xF1);
        let id = state.sources.source_for(h.0 as isize, DeviceKind::Mouse);
        state.on_device_removed(handle(0x999)); // 从未注册的句柄
        assert!(rx.try_recv().is_err(), "未知句柄移除不得发 SourceRemoved");
        state.handle_mouse(h, &wheel(120));
        assert!(
            matches!(
                rx.try_recv().unwrap(),
                AggEvent::Input(RawEvent::MouseClick {
                    button: MouseButton::WheelUp,
                    ..
                })
            ),
            "已注册来源的累计不受无关移除影响"
        );
        state.on_device_removed(h);
        match rx.try_recv().unwrap() {
            AggEvent::SourceRemoved { source } => assert_eq!(source, id),
            other => panic!("应为 SourceRemoved: {other:?}"),
        }
    }

    // ==================================================================
    // motion-dpi §4.3：相对移动 counts（hypot / 绝对排除 / 分桶 / 批发 / 暂停）
    // ==================================================================

    /// 每个相对包只计算一次 hypot(dx,dy)：(3,4) → 恰 5 counts；25ms 批发一次。
    #[test]
    fn motion_dpi_relative_packet_counts_hypot_once() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let h = handle(0xA01);
        state.register_motion_source_with(h.0 as isize, physical_desc("k1"));
        state.handle_mouse(h, &mouse(0, 0, 3, 4));
        assert!(drain_travel(&rx).is_empty(), "批发前不投递");
        state.on_flush_tick();
        let travels = drain_travel(&rx);
        assert_eq!(travels.len(), 1);
        assert!(
            (travels[0].counts - 5.0).abs() < 1e-9,
            "hypot(3,4)=5，实际 {}",
            travels[0].counts
        );
        assert!(travels[0].counts.is_finite() && travels[0].counts > 0.0);
        // 一致快照：connection/control/dpi/day 齐备
        assert!(travels[0].connection.0 >= 1);
        assert!(!travels[0].control.paused, "桶内 control 恒为非暂停捕获");
    }

    /// 绝对输入不混成 counts（MOUSE_MOVE_ABSOLUTE）；零位移相对包也不入桶。
    #[test]
    fn motion_dpi_absolute_packets_are_excluded_from_counts() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let h = handle(0xA02);
        state.register_motion_source_with(h.0 as isize, physical_desc("k1"));
        // 绝对包带位移（模拟数位板/远程桌面绝对坐标）：usFlags 置 MOUSE_MOVE_ABSOLUTE
        const MOUSE_MOVE_ABSOLUTE: u16 = 0x01;
        let absolute = RAWMOUSE {
            usFlags: MOUSE_STATE(MOUSE_MOVE_ABSOLUTE),
            Anonymous: RAWMOUSE_0 {
                Anonymous: RAWMOUSE_0_0 {
                    usButtonFlags: 0,
                    usButtonData: 0,
                },
            },
            ulRawButtons: 0,
            lLastX: 500,
            lLastY: 300,
            ulExtraInformation: 0,
        };
        state.handle_mouse(h, &absolute);
        // 零位移相对包
        state.handle_mouse(h, &mouse(0, 0, 0, 0));
        state.on_flush_tick();
        assert!(
            drain_travel(&rx).is_empty(),
            "绝对输入与零位移不得产生 counts"
        );
    }

    /// 桶按 DPI 快照分隔（§4.3：新读数发布分桶；同型不同 DPI 不串设置）：
    /// manual 800 期间累计 → 改 1600 → 先发旧桶（800）再开新桶（1600）。
    #[test]
    fn motion_dpi_dpi_snapshot_change_splits_bucket_first_flush_old() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let h = handle(0xA03);
        state.register_motion_source_with(h.0 as isize, physical_desc("k1"));
        motion.apply_manual("k1", Some(800));
        state.handle_mouse(h, &mouse(0, 0, 10, 0)); // 800 桶 +10
                                                    // DPI 快照变化（GUI 配置刷新 ≤500ms 后批读生效）
        motion.apply_manual("k1", Some(1600));
        state.handle_mouse(h, &mouse(0, 0, 10, 0)); // 旧桶先发（+10 @800），新桶 +10 @1600
        state.on_flush_tick();
        let mut travels = drain_travel(&rx);
        assert_eq!(travels.len(), 2, "旧桶先发，新桶随批发发出");
        travels.sort_by_key(|t| t.dpi.value);
        assert_eq!(travels[0].dpi.value, Some(800));
        assert!((travels[0].counts - 10.0).abs() < 1e-9);
        assert_eq!(travels[1].dpi.value, Some(1600));
        assert!((travels[1].counts - 10.0).abs() < 1e-9);
    }

    /// 同型号两只鼠标、不同 DPI 配置互不串桶（§4.4 验收点）。
    #[test]
    fn motion_dpi_two_physical_sources_same_model_keep_separate_dpi_buckets() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let a = handle(0xA04);
        let b = handle(0xA05);
        state.register_motion_source_with(a.0 as isize, physical_desc("k-a"));
        state.register_motion_source_with(b.0 as isize, physical_desc("k-b"));
        motion.apply_manual("k-a", Some(800));
        motion.apply_manual("k-b", Some(1600));
        state.handle_mouse(a, &mouse(0, 0, 10, 0));
        state.handle_mouse(b, &mouse(0, 0, 10, 0));
        state.on_flush_tick();
        let mut travels = drain_travel(&rx);
        assert_eq!(travels.len(), 2);
        travels.sort_by(|x, y| x.descriptor.source_key.cmp(&y.descriptor.source_key));
        assert_eq!(
            (
                travels[0].descriptor.source_key.as_str(),
                travels[0].dpi.value
            ),
            ("k-a", Some(800))
        );
        assert_eq!(
            (
                travels[1].descriptor.source_key.as_str(),
                travels[1].dpi.value
            ),
            ("k-b", Some(1600))
        );
    }

    /// 暂停语义（§4.3/§4.3.1）：paused 包不累计；epoch 变化先发旧有效桶再换新桶；
    /// 短暂停再恢复（epoch 0→1→2）不跨 epoch 混桶；恢复后增量正常落桶。
    #[test]
    fn motion_dpi_pause_packets_not_accumulated_and_epoch_switch_flushes_old_bucket() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let flags = Arc::new(Flags::default());
        let motion = MotionRuntime::offline(Arc::clone(&flags));
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let h = handle(0xA06);
        state.register_motion_source_with(h.0 as isize, physical_desc("k1"));

        // epoch 0 活动期：+10
        state.handle_mouse(h, &mouse(0, 0, 10, 0));
        // 暂停（epoch 1）：包不累计，但 epoch 变化先发旧有效桶
        flags.set_paused(true);
        state.handle_mouse(h, &mouse(0, 0, 10, 0));
        state.on_flush_tick();
        let travels = drain_travel(&rx);
        assert_eq!(travels.len(), 1, "暂停包不累计，epoch 变化先发旧桶");
        assert_eq!(travels[0].control.epoch, 0, "旧桶携带其捕获 epoch");
        assert!((travels[0].counts - 10.0).abs() < 1e-9);

        // 暂停期再来的包：不累计（epoch 1）
        state.handle_mouse(h, &mouse(0, 0, 10, 0));
        state.on_flush_tick();
        assert!(drain_travel(&rx).is_empty(), "paused 包不得累计");

        // 恢复（epoch 2）：短暂停再恢复也换桶——新桶从零开始，携带新 epoch
        flags.set_paused(false);
        state.handle_mouse(h, &mouse(0, 0, 6, 8)); // hypot=10
        state.on_flush_tick();
        let travels = drain_travel(&rx);
        assert_eq!(travels.len(), 1);
        assert_eq!(travels[0].control.epoch, 2, "恢复后新桶携带新 epoch");
        assert!(!travels[0].control.paused);
        assert!(
            (travels[0].counts - 10.0).abs() < 1e-9,
            "恢复后独立增量落库，不跨 epoch 混桶"
        );
    }

    /// 捕获本地日变化（§4.3：原始桶按捕获本地日分隔）：捕获日跨天后先发旧日桶。
    #[test]
    fn motion_dpi_capture_day_change_splits_bucket() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let h = handle(0xA07);
        state.register_motion_source_with(h.0 as isize, physical_desc("k1"));
        // 今日（真实时钟）捕获
        state.handle_mouse(h, &mouse(0, 0, 10, 0));
        // 注入两天后的采样时刻（假 clock）→ 捕获日变化
        let now_unix = motion.stamp().unix_us;
        motion.set_stamp_override_for_tests(Some(MotionStamp {
            mono_us: 0,
            unix_us: now_unix + 48 * 3_600_000_000,
        }));
        state.handle_mouse(h, &mouse(0, 0, 10, 0));
        state.on_flush_tick();
        let travels = drain_travel(&rx);
        assert_eq!(travels.len(), 2, "日变化先发旧日桶，新日桶随批发");
        assert_ne!(travels[0].day, travels[1].day, "两桶归属不同捕获日");
        let old = if travels[0].day < travels[1].day {
            &travels[0]
        } else {
            &travels[1]
        };
        let new = if travels[0].day < travels[1].day {
            &travels[1]
        } else {
            &travels[0]
        };
        assert_eq!(old.counts, 10.0);
        assert_eq!(new.counts, 10.0);
        // 旧桶的 day 必须是真实时钟当日（第一包捕获日）
        assert_eq!(old.day, day::today_local());
    }

    /// 断连收尾（§4.3）：尾桶排出一次（不双计）、断连状态发布、句柄复用后
    /// 重新注册必得新连接代际且从零累计。
    #[test]
    fn motion_dpi_device_removal_drains_tail_once_and_reports_disconnect() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let h = handle(0xA08);
        state.register_motion_source_with(h.0 as isize, physical_desc("k1"));
        let conn = state
            .motion_sources
            .get(&(h.0 as isize))
            .unwrap()
            .connection;
        state.handle_mouse(h, &mouse(0, 0, 7, 0)); // 尾桶 +7

        state.on_device_removed(h);
        // 尾桶恰好一条（先于断连状态）
        let travels = drain_travel(&rx);
        assert_eq!(travels.len(), 1, "断连排出尾数");
        assert!((travels[0].counts - 7.0).abs() < 1e-9);
        assert_eq!(travels[0].connection, conn);
        // 断连后批发不得重复（不双计）
        state.on_flush_tick();
        assert!(drain_travel(&rx).is_empty(), "尾桶已排出，不得二次投递");
        // 来源映射已删：句柄复用后重新注册必得新连接
        state.register_motion_source_with(h.0 as isize, physical_desc("k1"));
        let conn2 = state
            .motion_sources
            .get(&(h.0 as isize))
            .unwrap()
            .connection;
        assert_ne!(conn2, conn, "断连重连必须新连接代际");
        state.handle_mouse(h, &mouse(0, 0, 5, 0));
        state.on_flush_tick();
        let travels = drain_travel(&rx);
        assert_eq!(travels.len(), 1);
        assert_eq!(travels[0].connection, conn2, "新连接的增量携带新代际");
        assert!((travels[0].counts - 5.0).abs() < 1e-9, "新连接从零累计");
    }

    /// 首条输入懒注册（motion-dpi §4.3）：未预注册的句柄首包即注册并累计。
    #[test]
    fn motion_dpi_lazy_registration_on_first_travel_packet() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        let h = handle(0xA09);
        assert!(!state.motion_sources.contains_key(&(h.0 as isize)));
        state.handle_mouse(h, &mouse(0, 0, 3, 4)); // 懒注册 + 累计
        assert!(
            state.motion_sources.contains_key(&(h.0 as isize)),
            "首条输入懒注册"
        );
        state.on_flush_tick();
        let travels = drain_travel(&rx);
        assert_eq!(travels.len(), 1);
        assert!((travels[0].counts - 5.0).abs() < 1e-9);
        // 重复注册无副作用（连接代际不换）
        let conn = state
            .motion_sources
            .get(&(h.0 as isize))
            .unwrap()
            .connection;
        state.register_motion_source_by_handle(h);
        assert_eq!(
            state
                .motion_sources
                .get(&(h.0 as isize))
                .unwrap()
                .connection,
            conn
        );
    }

    /// 启动枚举注册（motion-dpi §4.3：无需移动即出现 DPI 入口）：
    /// 枚举句柄全部注册（含不可解析句柄 → virtual 桶），且不产生任何 counts。
    #[test]
    fn motion_dpi_startup_enumeration_registers_mice_without_movement() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let motion = test_motion();
        let mut state = RawInputState::new(&tx, Arc::clone(&motion));
        // 注入枚举结果：两台可解析物理鼠标 + 一台假句柄（解析失败 → virtual:unknown）
        state.register_motion_source_with(0xA10, physical_desc("k1"));
        state.register_motion_source_with(0xA11, physical_desc("k2"));
        state.register_motion_source_by_handle(handle(0xA12));
        assert_eq!(state.motion_sources.len(), 3);
        let virtual_entry = state.motion_sources.get(&0xA12).unwrap();
        assert_eq!(virtual_entry.descriptor.source_key, "virtual:unknown");
        assert!(!virtual_entry.descriptor.physical);
        // 注册即发布观察：无移动不产生 counts；连接 ID 各自独立分配（互不相同且非零）
        state.on_flush_tick();
        assert!(drain_travel(&rx).is_empty(), "注册/枚举不得产生 counts");
        let mut conns: Vec<u64> = state
            .motion_sources
            .values()
            .map(|m| m.connection.0)
            .collect();
        conns.sort_unstable();
        assert!(
            conns.windows(2).all(|w| w[0] < w[1]),
            "连接代际必须互不相同且单调: {conns:?}"
        );
        assert!(conns[0] >= 1, "连接 ID 非零");
    }
}
