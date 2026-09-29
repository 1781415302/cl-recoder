//! 键鼠 Raw Input 采集（PLAN §4.6 raw_input.rs 契约）。
//!
//! 流程：message-only 窗口 + 三组 `RIDEV_INPUTSINK` 注册（keyboard 1/6、mouse 1/2、
//! Consumer Control 0x0C/1——音量/播放等媒体键按键盘进统计，§5.3）→ `WM_INPUT` 消息循环
//! → RAWKEYBOARD/RAWMOUSE 解析 → `AggEvent::Input(RawEvent::…)` 经 crossbeam channel 送
//! aggregator。
//!
//! 解析规则（§4.1/§4.2/§4.6）：
//! - 键盘：MakeCode==0 且 VKey!=0 先 `MapVirtualKeyW(VK, MAPVK_VK_TO_VSC_EX)` 预处理
//!   （扩展键前缀在 VSC_EX 高字节），再交 `core::codes::normalize_scancode`
//!   （0xFF 溢出码丢弃、Pause E1 归一等）；down/up 边沿均投递（Engine 需按下状态表
//!   去重自动重复、判定组合键）；
//! - 鼠标：`ButtonFlags` 提取按下边沿（抬起/移动不投递，§5.2 仅物理按下计数）；
//!   `RI_MOUSE_WHEEL/HWHEEL` 的 i16 delta 进累计器（120=1 刻度），刻度折算为
//!   WheelUp/Down/Left/Right 鼠标按键事件投递；
//! - 设备：hDevice → `crate::device::DeviceResolver`（句柄缓存；hDevice==0/非 HID 归
//!   "未知/虚拟设备"桶，照常计数）。
//!
//! 窗口实现说明：windows 0.62 将 `WNDCLASSW`/`WNDCLASSEXW` gate 在 `Win32_Graphics_Gdi`
//! feature 之后（§9.1 依赖白名单未含该 feature），故复用 user32 系统全局类 `STATIC` 创建
//! message-only 窗口（父窗口 `HWND_MESSAGE`），再以 `GWLP_WNDPROC` 子类化挂接窗口过程——
//! 消息循环与 WM_INPUT 投递行为与自注册类完全一致。
//!
//! 健壮性（§1/§9.4）：窗口过程跨 FFI 边界（panic 即 abort），内部 catch_unwind 只丢当条
//! 事件；线程体 catch_unwind + 1s 重试（限频日志）；tx 断开静默丢弃；状态盒有意泄漏
//! （窗口随线程销毁时仍可能触发窗口过程，回收即悬垂）。

use clrecoder_core::codes::{normalize_scancode, DeviceKind, MouseButton};
use clrecoder_core::event::{AggEvent, DeviceKey, RawEvent};
use crossbeam_channel::Sender;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC_EX};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RAWKEYBOARD, RAWMOUSE, RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, GWLP_WNDPROC, HWND_MESSAGE, MSG,
    RI_KEY_BREAK, RI_KEY_E0, RI_KEY_E1, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_5_DOWN,
    RI_MOUSE_HWHEEL, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_DOWN,
    RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_WHEEL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT,
};

/// RAWMOUSE 按下边沿位掩码（左/右/中/X1/X2 的 down 标志；up 标志与滚轮标志不在此列）。
const MOUSE_BUTTON_DOWN_MASK: u16 = (RI_MOUSE_LEFT_BUTTON_DOWN
    | RI_MOUSE_RIGHT_BUTTON_DOWN
    | RI_MOUSE_MIDDLE_BUTTON_DOWN
    | RI_MOUSE_BUTTON_4_DOWN
    | RI_MOUSE_BUTTON_5_DOWN) as u16;

/// 复用输入缓冲字数（64 字节 ≥ 键盘/鼠标事件 RAWINPUT 上限 48 字节；HID 未注册不到达）。
const BUF_WORDS: usize = 8;

/// 启动 raw_input 采集线程（PLAN §4.6）：`spawn(tx) -> JoinHandle`。
/// 事件经 crossbeam channel 送 aggregator；线程体 panic / 初始化失败自动重启
/// （1s 间隔、限频日志，§9.4 catch_unwind 兜底记录后继续）。
/// 【S9 接线】由 main 调用；组装前 crate 内暂无引用，临时豁免 dead_code。
#[allow(dead_code)]
pub fn spawn(tx: Sender<AggEvent>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("raw-input".to_string())
        .spawn(move || run(tx))
        .expect("raw_input 线程创建失败（内存耗尽等进程级错误）")
}

/// 线程主体：消息循环 panic / 初始化失败 → 限频日志 + 1s 后重建窗口重试；
/// 收到 WM_QUIT（进程关停）才正常退出。
fn run(tx: Sender<AggEvent>) {
    let mut failures: u64 = 0;
    loop {
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| message_loop(&tx)));
        match outcome {
            Ok(Ok(())) => return,
            Ok(Err(e)) => log_retry(&mut failures, &format!("raw_input 初始化/消息循环失败：{e}")),
            Err(_) => log_retry(&mut failures, "raw_input 线程 panic（已捕获，不影响其他采集线程）"),
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// 重试日志限频：首次必记，此后每 60 次（≈1 分钟）记一次，避免热循环刷日志（§9.2）。
fn log_retry(failures: &mut u64, message: &str) {
    if *failures == 0 || (*failures).is_multiple_of(60) {
        log::error!("{message}（第 {} 次，1s 后重试；每 60 次记录一次）", *failures + 1);
    }
    *failures += 1;
}

/// 建 message-only 窗口 → 注册三组 RIDEV_INPUTSINK → GetMessageW 消息循环。
fn message_loop(tx: &Sender<AggEvent>) -> Result<(), String> {
    // 状态盒有意泄漏：窗口随线程退出被系统销毁时仍可能触发窗口过程，回收即悬垂；
    // 状态体量仅设备缓存 + 累计器（KB 级），进程驻留全程成本可忽略。
    let state = Box::into_raw(Box::new(RawInputState::new(tx)));
    let hwnd = unsafe { create_message_only_window(state) }
        .map_err(|e| format!("创建 message-only 窗口失败：{e}"))?;
    unsafe { register_devices(hwnd) }.map_err(|e| format!("注册 Raw Input 设备失败：{e}"))?;
    log::info!("raw_input 采集线程就绪（keyboard+mouse+consumer，RIDEV_INPUTSINK）");
    let mut msg = MSG::default();
    loop {
        // GetMessageW 返回 0=WM_QUIT（进程关停）；-1=错误（如句柄失效）
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 == 0 {
            log::info!("raw_input 收到 WM_QUIT，消息循环退出");
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

/// WM_INPUT 窗口过程（`GWLP_WNDPROC` 子类化挂接）。
/// 其余消息一律交 `DefWindowProcW`——WM_INPUT 契约要求调用 DefWindowProc 以便系统清理。
/// 本过程跨 FFI 边界，panic 即 abort 进程（§9.4）：内部 catch_unwind 只丢当条事件。
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
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// §4.6：注册 keyboard(UsagePage 1, Usage 6) + mouse(1, 2) + Consumer Control(0x0C, 1)
/// 三组目标设备，全部 `RIDEV_INPUTSINK`（后台接收、无需窗口焦点）。
unsafe fn register_devices(hwnd: HWND) -> windows::core::Result<()> {
    let devices = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x0C,
            usUsage: 0x01,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
    ];
    RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
}

/// raw_input 线程状态：channel、设备解析缓存、两个滚轮累计器、移动距离累计、复用输入缓冲。
struct RawInputState {
    tx: Sender<AggEvent>,
    devices: crate::device::DeviceResolver,
    wheel: WheelAccumulator,
    hwheel: WheelAccumulator,
    /// 鼠标移动距离累计（英寸），满 0.25 再投递
    move_acc: f64,
    buf: Vec<u64>,
}

impl RawInputState {
    fn new(tx: &Sender<AggEvent>) -> Self {
        Self {
            tx: tx.clone(),
            devices: crate::device::DeviceResolver::new(),
            wheel: WheelAccumulator::new(),
            hwheel: WheelAccumulator::new(),
            move_acc: 0.0,
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
    /// 0xFF 溢出码与垃圾码在 core 归一化中丢弃（§4.1）。
    fn handle_keyboard(&mut self, hdevice: HANDLE, kb: &RAWKEYBOARD) {
        // §4.1：make 取 RAWKEYBOARD.MakeCode 的低 8 位（windows 绑定中该字段为 u16）
        let sc = normalize_keyboard_event((kb.MakeCode & 0xFF) as u8, kb.Flags, kb.VKey, |vk| unsafe {
            MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX)
        });
        let Some(sc) = sc else {
            return; // 溢出码/垃圾码：丢弃当条，不影响其他事件（§1 防御性兜底）
        };
        let down = keyboard_is_down(kb.Flags);
        let device = self.devices.resolve(hdevice, DeviceKind::Keyboard);
        let _ = self.tx.send(AggEvent::Input(RawEvent::Keyboard { device, sc, down }));
    }

    /// RAWMOUSE → 按下边沿 `MouseClick` + 滚轮刻度（§4.6 累计器）+ 移动距离。
    fn handle_mouse(&mut self, hdevice: HANDLE, mouse: &RAWMOUSE) {
        let (flags, data, last_x, last_y, mouse_flags) = unsafe {
            let a = &mouse.Anonymous.Anonymous;
            (a.usButtonFlags, a.usButtonData, mouse.lLastX, mouse.lLastY, mouse.usFlags)
        };
        // 移动距离：相对位移取欧氏距离，折算为英寸（约 80 counts/inch，与 WhatPulse 近似同口径）。
        // 批量到 0.25 英寸再投递，避免每像素一事件打爆 channel。
        const MOUSE_MOVE_ABSOLUTE: u16 = 0x01;
        const COUNTS_PER_INCH: f64 = 80.0;
        if mouse_flags.0 & MOUSE_MOVE_ABSOLUTE == 0 && (last_x != 0 || last_y != 0) {
            let counts = ((last_x as f64).powi(2) + (last_y as f64).powi(2)).sqrt();
            self.move_acc += counts / COUNTS_PER_INCH;
            if self.move_acc >= 0.25 {
                let device = self.devices.resolve(hdevice, DeviceKind::Mouse);
                let distance_inches = std::mem::take(&mut self.move_acc);
                let _ = self.tx.send(AggEvent::Input(RawEvent::MouseMove {
                    device,
                    distance_inches,
                }));
            }
        }
        // 按下边沿（§5.2：仅物理按下计数；抬起不投递）
        if flags & MOUSE_BUTTON_DOWN_MASK != 0 {
            let device = self.devices.resolve(hdevice, DeviceKind::Mouse);
            for button in mouse_down_edges(flags) {
                let _ =
                    self.tx.send(AggEvent::Input(RawEvent::MouseClick { device: device.clone(), button }));
            }
        }
        // 垂直滚轮：正值=上滚（高分辨率滚轮单事件可 >120 → 累计器折算）
        if flags & RI_MOUSE_WHEEL as u16 != 0 {
            let ticks = self.wheel.add(data as i16);
            if ticks != 0 {
                let device = self.devices.resolve(hdevice, DeviceKind::Mouse);
                let button = if ticks > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
                self.send_mouse_clicks(&device, button, ticks.unsigned_abs());
            }
        }
        // 水平滚轮：正值=右倾
        if flags & RI_MOUSE_HWHEEL as u16 != 0 {
            let ticks = self.hwheel.add(data as i16);
            if ticks != 0 {
                let device = self.devices.resolve(hdevice, DeviceKind::Mouse);
                let button =
                    if ticks > 0 { MouseButton::WheelRight } else { MouseButton::WheelLeft };
                self.send_mouse_clicks(&device, button, ticks.unsigned_abs());
            }
        }
    }

    /// 投递 N 个同方向滚轮刻度事件。
    fn send_mouse_clicks(&self, device: &DeviceKey, button: MouseButton, count: u32) {
        for _ in 0..count {
            let _ =
                self.tx.send(AggEvent::Input(RawEvent::MouseClick { device: device.clone(), button }));
        }
    }
}

/// 滚轮 delta 累计器（§4.6）：120 delta = 1 刻度。高分辨率滚轮（单事件 ±240 或更大）与
/// 自由滚轮小步长均正确折算；acc 用 i32 防溢出（单事件 delta 可达 i16 极值，
/// 119 + 32767 会击穿 i16 累计器），折算语义与 §4.6 逐字一致。
#[derive(Debug, Default)]
struct WheelAccumulator {
    acc: i32,
}

impl WheelAccumulator {
    fn new() -> Self {
        Self::default()
    }

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
        (make, flags & RI_KEY_E0 as u16 != 0, flags & RI_KEY_E1 as u16 != 0)
    };
    normalize_scancode(make, e0, e1, vkey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::RI_MOUSE_LEFT_BUTTON_UP;

    // ---------- WheelAccumulator（§4.6：120 delta 一格） ----------

    #[test]
    fn wheel_accumulates_one_notch_at_threshold() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.add(120), 1);
        assert_eq!(acc.add(120), 1);
        assert_eq!(acc.add(-120), -1);
        // 不足一格不产出
        assert_eq!(acc.add(119), 0);
        assert_eq!(acc.add(1), 1); // 119+1=120 → 1 格，余 0
    }

    #[test]
    fn wheel_accumulator_carries_remainder_across_events() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.add(40), 0);
        assert_eq!(acc.add(40), 0);
        assert_eq!(acc.add(40), 1); // 自由滚轮小步长累计到一格
        assert_eq!(acc.add(40), 0); // 余 40 保留
    }

    #[test]
    fn wheel_multiple_notches_per_event_for_high_resolution_wheels() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.add(240), 2);
        assert_eq!(acc.add(-300), -2); // -240 → -2，余 -60
        assert_eq!(acc.add(-60), -1); // 余量补满一格
        assert_eq!(acc.add(0), 0);
    }

    #[test]
    fn wheel_extreme_deltas_do_not_overflow_and_keep_remainder() {
        let mut acc = WheelAccumulator::new();
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
        assert_eq!(mouse_down_edges(RI_MOUSE_LEFT_BUTTON_DOWN as u16), vec![MouseButton::Left]);
        assert_eq!(mouse_down_edges(RI_MOUSE_RIGHT_BUTTON_DOWN as u16), vec![MouseButton::Right]);
        assert_eq!(
            mouse_down_edges(RI_MOUSE_MIDDLE_BUTTON_DOWN as u16),
            vec![MouseButton::Middle]
        );
        assert_eq!(mouse_down_edges(RI_MOUSE_BUTTON_4_DOWN as u16), vec![MouseButton::X1]);
        assert_eq!(mouse_down_edges(RI_MOUSE_BUTTON_5_DOWN as u16), vec![MouseButton::X2]);
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
        assert_eq!(mouse_down_edges(RI_MOUSE_WHEEL as u16), Vec::<MouseButton>::new());
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
}
