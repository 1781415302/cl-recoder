//! 手柄采集线程（PLAN §4.6 gamepad.rs 契约，gilrs xinput 后端）。
//!
//! - `spawn(tx)`：独立线程轮询 [`Gilrs::next_event`]（**非阻塞**）+ `sleep(8ms)`（≈125Hz）。
//! - `ButtonPressed` → [`RawEvent::GamepadPress`]（只投递按下边沿）；
//!   **LeftTrigger2/RightTrigger2（物理 LT/RT 模拟扳机）例外**——它们由 [`EventType::ButtonChanged`] 的模拟量
//!   以上穿 0.33 计 1 次、下穿复位（见 [`TriggerGate`]）。gilrs 0.11 的轴→按键合成逻辑
//!   会在自己的 0.75/0.65 阈值处额外发出这两个键的 `ButtonPressed/ButtonReleased`
//!   （gilrs-0.11.2/src/gamepad.rs:297-330，`axis_to_btn_pressed=0.75`），若并入 `ButtonPressed`
//!   路径会与 0.33 迟滞对同一次物理行程**双重计数**，违背 §5.3"回落后才可再计"。
//! - 设备身份：`DeviceKey{kind:Gamepad, vid:0, pid:0, name}`——gilrs xinput 后端
//!   `vendor_id()/product_id()` 恒为 `None`（gilrs-core-0.6.8 windows_xinput/gamepad.rs:413-419），
//!   故 vid/pid 固定为 0；`name` 取 `gamepad.name()`，非空否则兜底 `"未知手柄"`。
//!   xinput 后端 name 恒定（实测源码常量 `"Xbox Controller"`），因此所有 XInput 手柄
//!   仍按 `UNIQUE(kind, vid, pid, name)` 合并为一行——对 R3 的已声明降级（PLAN §9.3）。
//! - DeviceKey 与触发器迟滞状态**按连接（GamepadId）缓存**：`Connected` 时建立，
//!   `Disconnected` 时移除；启动时已插着的 pads 不补发 `Connected` 事件
//!   （gilrs `finish_gamepads_creation` 静默注册），故任何事件遇到未知 id 时懒建兜底。
//!
//! 本模块不做键名翻译、不知道 WhatPulse 存在（PLAN §2.5）；失败一律降级继续，绝不 panic。

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::Sender;
use gilrs::{Button as GilrsButton, Event, EventType, GamepadId, Gilrs};

use clrecoder_core::codes::{DeviceKind, GamepadButton};
use clrecoder_core::event::{AggEvent, DeviceKey, RawEvent};

/// 触发器（LeftTrigger2/RightTrigger2）模拟量计数阈值：事件值上穿它计 1 次（PLAN §4.6/§5.3）。
const TRIGGER_THRESHOLD: f32 = 0.33;

/// `next_event()` 非阻塞轮询间隔（PLAN §4.6：sleep 8ms ≈ 125Hz，与 XInput 轮询成本匹配）。
const POLL_INTERVAL: Duration = Duration::from_millis(8);

/// gilrs 上下文初始化失败后的重试间隔（如 xinput 暂不可用，静默自愈重试）。
const INIT_RETRY: Duration = Duration::from_millis(5000);

/// panic 兜底重启前的等待（避免持续 panic 时热循环烧 CPU，PLAN §9.4）。
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// 启动手柄采集线程。事件以 [`AggEvent::Input`] 投入 `tx`；接收端（aggregator）关闭后线程自行退出。
pub fn spawn(tx: Sender<AggEvent>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("gamepad".to_string())
        .spawn(move || run(tx))
        .expect("创建 gamepad 采集线程失败")
}

/// 活跃根：S9 组装把 `spawn` 接入 main.rs 之前，以此引用维持整模块不被 dead_code 判死；
/// 接线后本常量冗余但无害。
const _: fn(Sender<AggEvent>) -> JoinHandle<()> = spawn;

/// 线程主体：轮询循环 panic 时记录并重建 gilrs 上下文继续（PLAN §9.4 catch_unwind 兜底）；
/// 通道关闭（aggregator 已停止）则正常退出。
fn run(tx: Sender<AggEvent>) {
    loop {
        // Gilrs 持有平台句柄（非 UnwindSafe），跨 catch_unwind 传递需断言；重建即全新状态，安全。
        match catch_unwind(AssertUnwindSafe(|| poll_loop(tx.clone()))) {
            Ok(()) => return,
            Err(_) => {
                log::error!("gamepad 轮询线程 panic，{}ms 后重建上下文继续", RESTART_DELAY.as_millis());
                thread::sleep(RESTART_DELAY);
            }
        }
    }
}

/// 轮询循环：drain 所有待处理事件 → sleep 8ms → 重复。
/// 返回即表示通道关闭（接收端消失），线程应退出；其余错误一律降级继续。
fn poll_loop(tx: Sender<AggEvent>) {
    let mut gilrs = init_gilrs();
    // 启动时枚举一次已连接手柄（gilrs 对启动即插入的 pads 不补发 Connected）。
    {
        let mut n = 0;
        for (id, gamepad) in gilrs.gamepads() {
            log::info!("gamepad 已连接：id={id:?} name={:?}", gamepad.name());
            n += 1;
        }
        if n == 0 {
            log::info!("gamepad：当前无已连接的 XInput 手柄（本软件仅支持 XInput/Xbox 系）");
        }
    }
    // DeviceKey 按连接（GamepadId）缓存（PLAN §4.6"同一连接内缓存"）。
    let mut devices: HashMap<GamepadId, DeviceKey> = HashMap::new();
    // LT2/RT2 迟滞门限按连接缓存；断开即丢弃，重连时触发器处于静止态、重新武装。
    let mut gates: HashMap<GamepadId, [TriggerGate; 2]> = HashMap::new();
    // 数字键按下状态（ButtonPressed/ButtonChanged 去重）。
    let mut digital: HashMap<(GamepadId, GilrsButton), bool> = HashMap::new();

    loop {
        while let Some(event) = gilrs.next_event() {
            if !handle_event(&mut gilrs, event, &mut devices, &mut gates, &mut digital, &tx) {
                return; // 通道关闭：aggregator 已停止，线程正常收尾
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// 创建 gilrs 上下文；失败记录日志并周期重试（手柄缺失不是错误，xinput 初始化失败才走此处）。
fn init_gilrs() -> Gilrs {
    loop {
        match Gilrs::new() {
            Ok(gilrs) => return gilrs,
            Err(e) => {
                log::error!("gilrs 初始化失败，{}ms 后重试: {e}", INIT_RETRY.as_millis());
                thread::sleep(INIT_RETRY);
            }
        }
    }
}

/// 处理单个 gilrs 事件。返回 `false` 表示通道已关闭（接收端丢弃），调用方应停止轮询。
fn handle_event(
    gilrs: &mut Gilrs,
    event: Event,
    devices: &mut HashMap<GamepadId, DeviceKey>,
    gates: &mut HashMap<GamepadId, [TriggerGate; 2]>,
    digital: &mut HashMap<(GamepadId, GilrsButton), bool>,
    tx: &Sender<AggEvent>,
) -> bool {
    let Event { id, event, .. } = event;
    match event {
        // 连接建立：按当前连接解析设备身份并缓存。
        EventType::Connected => {
            let device = device_key(gilrs.gamepad(id).name());
            log::info!("gamepad Connected：{device:?}");
            devices.insert(id, device);
            gates.insert(id, [TriggerGate::armed(), TriggerGate::armed()]);
            digital.retain(|(gid, _), _| *gid != id);
            true
        }
        // 断开：缓存失效（gilrs 重连可能复用同一 id，届时按新连接重建）。
        EventType::Disconnected => {
            devices.remove(&id);
            gates.remove(&id);
            digital.retain(|(gid, _), _| *gid != id);
            true
        }
        // 数字按键按下边沿 → 计数。LT2/RT2 例外：由下方 ButtonChanged 迟滞路径独占计数
        // （gilrs 会在自己的 0.75 阈值处为这两个键合成 ButtonPressed，并入会双重计数，见模块文档）。
        EventType::ButtonPressed(button, _) => match map_button(button) {
            Some(b) if !is_threshold_button(b) => {
                digital.insert((id, button), true);
                let device = cached_device(gilrs, devices, id);
                send_press(tx, device, b)
            }
            _ => true,
        },
        EventType::ButtonReleased(button, _) => {
            digital.insert((id, button), false);
            true
        }
        // 模拟触发器：上穿 0.33 计 1 次（PLAN §5.3）。
        // 数字键：部分驱动只发 ButtonChanged——用 0.5 迟滞补按下边沿（与 ButtonPressed 去重）。
        EventType::ButtonChanged(button, value, _) => match map_button(button) {
            Some(b) if is_threshold_button(b) => {
                let idx = trigger_index(b);
                let gate = &mut gates
                    .entry(id)
                    .or_insert_with(|| [TriggerGate::armed(), TriggerGate::armed()])[idx];
                if gate.feed(value) {
                    let device = cached_device(gilrs, devices, id);
                    send_press(tx, device, b)
                } else {
                    true
                }
            }
            Some(b) => {
                let was = digital.get(&(id, button)).copied().unwrap_or(false);
                let now_down = value >= 0.5;
                if now_down && !was {
                    digital.insert((id, button), true);
                    let device = cached_device(gilrs, devices, id);
                    send_press(tx, device, b)
                } else {
                    if !now_down {
                        digital.insert((id, button), false);
                    }
                    true
                }
            }
            None => true,
        },
        // ButtonRepeated/AxisChanged/Dropped/ForceFeedbackEffectCompleted：
        // 均非"一次物理按下"边沿，不计数（抬键不计数是全系统语义，PLAN §4.2）。
        _ => true,
    }
}

/// 投递一次手柄按下计数；返回 `false` 表示通道已关闭。
fn send_press(tx: &Sender<AggEvent>, device: DeviceKey, button: GamepadButton) -> bool {
    tx.send(AggEvent::Input(RawEvent::GamepadPress { device, button }))
        .is_ok()
}

/// 取该连接的设备身份；未知 id（启动时已插入的 pads 不补发 Connected 事件）懒建兜底。
fn cached_device(
    gilrs: &Gilrs,
    devices: &mut HashMap<GamepadId, DeviceKey>,
    id: GamepadId,
) -> DeviceKey {
    devices
        .entry(id)
        .or_insert_with(|| device_key(gilrs.gamepad(id).name()))
        .clone()
}

/// 由 gilrs 的连接名构造 DeviceKey：`name` 非空（含全空白视为空）否则兜底 `"未知手柄"`；
/// gilrs xinput 后端无 VID/PID，恒为 0（见模块文档）。
fn device_key(name: &str) -> DeviceKey {
    DeviceKey {
        kind: DeviceKind::Gamepad,
        vid: 0,
        pid: 0,
        name: if name.trim().is_empty() {
            "未知手柄".to_string()
        } else {
            name.to_string()
        },
    }
}

/// gilrs 按键 → 本系统 code 空间（[`GamepadButton`]，PLAN §4.1）。
/// 物理键位 ↔ gilrs 枚举对照（主证据：本机 gilrs-core 0.6.8
/// `windows_xinput/gamepad.rs:310-380`——物理 X→West、物理 Y→North、肩键→LeftTrigger/RightTrigger、
/// 模拟扳机轴→LeftTrigger2/RightTrigger2）：code 1/2/3/4 = 物理 A/B/Y/X（南/东/北/西），
/// 5/6/7/8 = 物理 LB/LT/RB/RT。枚举名沿用 gilrs 原名不改，物理键名只体现在 GUI 显示标签。
/// gilrs 的 `C`/`Z`/`Unknown` 在 XInput 后端不会出现，防御性丢弃。
fn map_button(button: GilrsButton) -> Option<GamepadButton> {
    Some(match button {
        GilrsButton::South => GamepadButton::South,
        GilrsButton::East => GamepadButton::East,
        GilrsButton::North => GamepadButton::North,
        GilrsButton::West => GamepadButton::West,
        GilrsButton::LeftTrigger => GamepadButton::LeftTrigger,
        GilrsButton::LeftTrigger2 => GamepadButton::LeftTrigger2,
        GilrsButton::RightTrigger => GamepadButton::RightTrigger,
        GilrsButton::RightTrigger2 => GamepadButton::RightTrigger2,
        GilrsButton::Select => GamepadButton::Select,
        GilrsButton::Start => GamepadButton::Start,
        GilrsButton::Mode => GamepadButton::Mode,
        GilrsButton::LeftThumb => GamepadButton::LeftThumb,
        GilrsButton::RightThumb => GamepadButton::RightThumb,
        GilrsButton::DPadUp => GamepadButton::DPadUp,
        GilrsButton::DPadDown => GamepadButton::DPadDown,
        GilrsButton::DPadLeft => GamepadButton::DPadLeft,
        GilrsButton::DPadRight => GamepadButton::DPadRight,
        GilrsButton::C | GilrsButton::Z | GilrsButton::Unknown => return None,
    })
}

/// 是否为模拟触发器（走 ButtonChanged 0.33 迟滞计数、排除出 ButtonPressed 路径的键）。
fn is_threshold_button(button: GamepadButton) -> bool {
    matches!(button, GamepadButton::LeftTrigger2 | GamepadButton::RightTrigger2)
}

/// 迟滞门限在按连接状态数组中的下标。
fn trigger_index(button: GamepadButton) -> usize {
    match button {
        GamepadButton::LeftTrigger2 => 0,
        _ => 1,
    }
}

/// 模拟触发器的上穿/下穿迟滞状态机（纯逻辑，可离线单测）。
///
/// - `armed == true`：触发器已回落（或初始静止），下次上穿阈值应计 1 次；
/// - `armed == false`：本次行程已计数，需等值下穿阈值才复位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TriggerGate {
    armed: bool,
}

impl TriggerGate {
    /// 初始门限：触发器静止值为 0（低于阈值），处于可计数状态。
    const fn armed() -> Self {
        Self { armed: true }
    }

    /// 喂入一次 `ButtonChanged` 的事件值，返回是否应计 1 次。
    /// - `armed` 且 `v >= 0.33` → 计 1 次（返回 true）并进入已计状态；
    /// - `v < 0.33` → 下穿复位（返回 false）；
    /// - 其余（未武装的上穿段）→ 忽略；NaN 一切比较为 false，落入复位分支，绝不计数。
    fn feed(&mut self, value: f32) -> bool {
        // 必须以"上穿"为主判断：若以"下穿"为主判断，NaN 会落入计数分支。
        if value >= TRIGGER_THRESHOLD {
            if self.armed {
                self.armed = false;
                return true;
            }
        } else {
            self.armed = true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- TriggerGate：上穿 0.33 计 1 次、下穿复位（PLAN §4.6/§5.3） ----------

    #[test]
    fn trigger_gate_counts_once_per_pull_and_rearms_below_threshold() {
        let mut gate = TriggerGate::armed();
        // 缓慢按压：低于阈值不上报
        assert!(!gate.feed(0.0));
        assert!(!gate.feed(0.12));
        assert!(!gate.feed(0.32));
        // 上穿 0.33：恰好 1 次
        assert!(gate.feed(0.33));
        // 行程保持/继续加深/回落但未下穿阈值：不再计数
        assert!(!gate.feed(0.5));
        assert!(!gate.feed(0.9));
        assert!(!gate.feed(0.65));
        // 下穿复位后才能再计
        assert!(!gate.feed(0.32));
        assert!(gate.feed(0.9));
    }

    #[test]
    fn trigger_gate_threshold_is_inclusive() {
        let mut gate = TriggerGate::armed();
        assert!(!gate.feed(0.329));
        assert!(gate.feed(0.33));
    }

    #[test]
    fn trigger_gate_initial_state_counts_first_upcross() {
        // 启动时触发器已被按住（首个事件即高于阈值）：按静止初值语义计 1 次
        let mut gate = TriggerGate::armed();
        assert!(gate.feed(0.9));
    }

    #[test]
    fn trigger_gate_nan_is_ignored() {
        let mut gate = TriggerGate::armed();
        assert!(!gate.feed(f32::NAN));
        // NaN 不改变门限状态
        assert!(gate.feed(0.9));
    }

    // ---------- DeviceKey：vid/pid=0、name 兜底（本 stage 任务书） ----------

    #[test]
    fn device_key_uses_gilrs_name_with_vid_pid_zero() {
        let d = device_key("Xbox Controller");
        assert_eq!(d.kind, DeviceKind::Gamepad);
        assert_eq!(d.vid, 0);
        assert_eq!(d.pid, 0);
        assert_eq!(d.name, "Xbox Controller");
    }

    #[test]
    fn device_key_falls_back_when_name_blank() {
        assert_eq!(device_key("").name, "未知手柄");
        assert_eq!(device_key("   ").name, "未知手柄");
    }

    // ---------- 按键映射与触发器路径划分 ----------

    #[test]
    fn map_button_covers_all_17_code_space_buttons() {
        // 17 个 code 空间按键逐一映射且 code 值与 §4.1 一致（写入 input_daily 的锚点）；
        // 行尾注释为物理键位（usability-runtime-v3 §4.6 U2，证据见 map_button 文档）
        let pairs = [
            (GilrsButton::South, GamepadButton::South, 1),               // 物理 A（南）
            (GilrsButton::East, GamepadButton::East, 2),                 // 物理 B（东）
            (GilrsButton::North, GamepadButton::North, 3),               // 物理 Y（北）
            (GilrsButton::West, GamepadButton::West, 4),                 // 物理 X（西）
            (GilrsButton::LeftTrigger, GamepadButton::LeftTrigger, 5),   // 物理 LB（左肩）
            (GilrsButton::LeftTrigger2, GamepadButton::LeftTrigger2, 6), // 物理 LT（左扳机）
            (GilrsButton::RightTrigger, GamepadButton::RightTrigger, 7),  // 物理 RB（右肩）
            (GilrsButton::RightTrigger2, GamepadButton::RightTrigger2, 8), // 物理 RT（右扳机）
            (GilrsButton::Select, GamepadButton::Select, 9),             // 物理 View（选择）
            (GilrsButton::Start, GamepadButton::Start, 10),              // 物理 Menu（开始）
            (GilrsButton::Mode, GamepadButton::Mode, 11),                // 物理 Guide
            (GilrsButton::LeftThumb, GamepadButton::LeftThumb, 12),      // 物理 LS 按下
            (GilrsButton::RightThumb, GamepadButton::RightThumb, 13),    // 物理 RS 按下
            (GilrsButton::DPadUp, GamepadButton::DPadUp, 14),            // 十字上
            (GilrsButton::DPadDown, GamepadButton::DPadDown, 15),        // 十字下
            (GilrsButton::DPadLeft, GamepadButton::DPadLeft, 16),        // 十字左
            (GilrsButton::DPadRight, GamepadButton::DPadRight, 17),      // 十字右
        ];
        for (g, ours, code) in pairs {
            assert_eq!(map_button(g), Some(ours));
            assert_eq!(ours as u16, code);
        }
        // XInput 后端不产出、防御性丢弃的键
        assert_eq!(map_button(GilrsButton::C), None);
        assert_eq!(map_button(GilrsButton::Z), None);
        assert_eq!(map_button(GilrsButton::Unknown), None);
    }

    #[test]
    fn only_deep_triggers_are_threshold_buttons() {
        // 仅 LT2/RT2（物理 LT/RT 模拟扳机）走 ButtonChanged 迟滞；
        // LeftTrigger/RightTrigger（物理 LB/RB 肩键，见 map_button 注释）等数字键走 ButtonPressed 边沿
        assert!(is_threshold_button(GamepadButton::LeftTrigger2));
        assert!(is_threshold_button(GamepadButton::RightTrigger2));
        assert!(!is_threshold_button(GamepadButton::LeftTrigger));
        assert!(!is_threshold_button(GamepadButton::RightTrigger));
        assert!(!is_threshold_button(GamepadButton::South));
        assert_eq!(trigger_index(GamepadButton::LeftTrigger2), 0);
        assert_eq!(trigger_index(GamepadButton::RightTrigger2), 1);
    }

    /// U2（usability-runtime-v3 §4.6）：物理 Xbox 键位 ↔ code 锚定——17 码显示标签统一后，
    /// 采集侧必须保证"哪个物理键落在哪个 code"不变（枚举名/数值均不改，仅锚定映射）：
    /// 3=物理 Y（北）、4=物理 X（西）、5=LB（左肩）、6=LT（左扳机）、7=RB（右肩）、8=RT（右扳机）。
    /// 主证据：本机 gilrs-core 0.6.8 windows_xinput/gamepad.rs:310-380（肩/面键）与 628-641（模拟扳机轴）。
    #[test]
    fn usability_v3_physical_xbox_layout_lands_on_u2_codes() {
        let physical: [(GilrsButton, GamepadButton, u16, &str); 8] = [
            (GilrsButton::South, GamepadButton::South, 1, "A（南）"),
            (GilrsButton::East, GamepadButton::East, 2, "B（东）"),
            (GilrsButton::North, GamepadButton::North, 3, "Y（北）"),
            (GilrsButton::West, GamepadButton::West, 4, "X（西）"),
            (GilrsButton::LeftTrigger, GamepadButton::LeftTrigger, 5, "LB（左肩）"),
            (GilrsButton::LeftTrigger2, GamepadButton::LeftTrigger2, 6, "LT（左扳机）"),
            (GilrsButton::RightTrigger, GamepadButton::RightTrigger, 7, "RB（右肩）"),
            (GilrsButton::RightTrigger2, GamepadButton::RightTrigger2, 8, "RT（右扳机）"),
        ];
        for (g, ours, code, label) in physical {
            assert_eq!(map_button(g), Some(ours));
            assert_eq!(ours as u16, code, "物理标签 {label} 的 code 漂移");
        }
    }
}
