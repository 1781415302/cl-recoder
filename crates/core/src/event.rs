//! 采集层 → aggregator 的事件语言（PLAN §4.2 逐字对齐）。
//!
//! 三个采集线程（raw_input / gamepad / apps）各自翻译为 [`RawEvent`] / [`AggEvent::Foreground`]，
//! 经 crossbeam channel 送入 aggregator（engine_loop）。按键边沿提取、滚轮 delta→刻度折算
//! 都在采集层完成——本层事件已经是"一次物理按下"语义。

use serde::{Deserialize, Serialize};

use crate::codes::{DeviceKind, GamepadButton, MouseButton};

/// 设备标识（跨进程共享的设备身份）。
///
/// - `vid` / `pid`：从设备路径 `\\?\HID#VID_....&PID_....#...` 提取；未知时为 **0**。
/// - `name`：注册表 FriendlyName → DeviceDesc → `"HID 设备 {VID:04X}:{PID:04X}"` 兜底（§4.2/§5.3）；
///   hDevice==0、RDP/虚拟设备等解析失败时归入固定桶 `{"未知/虚拟设备", vid:0, pid:0}`。
/// - 值域重叠警告：code 的唯一性只在设备种类内成立，消歧靠 [`DeviceKind`]（禁止按 code 判断种类）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceKey {
    /// 设备种类
    pub kind: DeviceKind,
    /// Vendor ID（未知为 0）
    pub vid: u16,
    /// Product ID（未知为 0）
    pub pid: u16,
    /// 显示名（兜底规则见模块文档）
    pub name: String,
}

/// 采集层产出的原始输入事件。**只投递按下边沿**（up 不计数）；
/// 键盘的 up 事件仍需投递（Engine 维护按下状态表去重自动重复/判定组合键）。
/// （无 `Eq`：MouseMove 携带 f64 距离。）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RawEvent {
    /// 键盘事件：`sc` 为 normalize_scancode 归一化后的 scancode，`down` 为按下/抬起边沿
    Keyboard {
        /// 设备
        device: DeviceKey,
        /// 归一化 scancode（0xE000/0xE100 前缀位含在内）
        sc: u16,
        /// true=按下边沿，false=抬起边沿
        down: bool,
    },
    /// 鼠标按键：只投递按下边沿；滚轮刻度由采集层按 §4.6 累计器折算为 WheelUp/Down/Left/Right
    MouseClick {
        /// 设备
        device: DeviceKey,
        /// 哪个按键/滚轮方向
        button: MouseButton,
    },
    /// 手柄按键：只投递按下边沿；触发器（LT/RT 深行程）以模拟量上穿 0.33 计 1 次（采集层实现）
    GamepadPress {
        /// 设备
        device: DeviceKey,
        /// 哪个按键
        button: GamepadButton,
    },
    /// 鼠标移动距离增量（英寸，WhatPulse 同口径；采集层按欧氏距离折算后批量投递）
    MouseMove {
        /// 设备
        device: DeviceKey,
        /// 本批移动距离（英寸）
        distance_inches: f64,
    },
}

/// 送入 aggregator 的聚合事件：`Input` 走统计，`Foreground` 更新当前归属 exe。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AggEvent {
    /// 原始输入事件（键盘/鼠标/手柄）
    Input(RawEvent),
    /// 前台应用切换，由 apps 线程发出；exe 为小写 basename（解析失败为 "unknown"）
    Foreground {
        /// 前台进程 exe 的小写 basename
        exe: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_key_serde_round_trip() {
        let d = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 0x04D9,
            pid: 0x0169,
            name: "HID Keyboard Device".to_string(),
        };
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(
            s,
            r#"{"kind":"keyboard","vid":1241,"pid":361,"name":"HID Keyboard Device"}"#
        );
        let back: DeviceKey = serde_json::from_str(&s).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn device_key_unknown_bucket_shape() {
        // §4.2 固定桶：hDevice==0 / RDP / 虚拟设备 → vid:pid 全 0 + "未知/虚拟设备"
        let d = DeviceKey {
            kind: DeviceKind::Mouse,
            vid: 0,
            pid: 0,
            name: "未知/虚拟设备".to_string(),
        };
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(s, r#"{"kind":"mouse","vid":0,"pid":0,"name":"未知/虚拟设备"}"#);
    }

    #[test]
    fn raw_event_serde_round_trip() {
        let kb = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 0,
            pid: 0,
            name: "未知/虚拟设备".to_string(),
        };
        let mouse = DeviceKey {
            kind: DeviceKind::Mouse,
            vid: 0x1532,
            pid: 0x0045,
            name: "Razer Mouse".to_string(),
        };
        let pad = DeviceKey {
            kind: DeviceKind::Gamepad,
            vid: 0,
            pid: 0,
            name: "XInput 手柄".to_string(),
        };
        let events = [
            RawEvent::Keyboard { device: kb.clone(), sc: 0xE01D, down: true },
            RawEvent::Keyboard { device: kb, sc: 0x2A, down: false },
            RawEvent::MouseClick { device: mouse, button: MouseButton::WheelUp },
            RawEvent::GamepadPress { device: pad, button: GamepadButton::DPadLeft },
        ];
        for e in events {
            let s = serde_json::to_string(&e).unwrap();
            let back: RawEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(back, e);
        }
    }

    #[test]
    fn agg_event_serde_round_trip() {
        let d = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 0x04D9,
            pid: 0x0169,
            name: "Keyboard".to_string(),
        };
        let events = [
            AggEvent::Input(RawEvent::Keyboard { device: d, sc: 0x1E, down: true }),
            AggEvent::Foreground { exe: "explorer.exe".to_string() },
        ];
        for e in events {
            let s = serde_json::to_string(&e).unwrap();
            let back: AggEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(back, e);
        }
    }

    #[test]
    fn device_key_hash_eq_for_cache() {
        // aggregator 用 DeviceKey→device_id 的 HashMap 缓存（§4.6）
        use std::collections::HashMap;
        let a = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 0x04D9,
            pid: 0x0169,
            name: "Keyboard".to_string(),
        };
        let b = a.clone();
        let mut m: HashMap<DeviceKey, i64> = HashMap::new();
        m.insert(a.clone(), 7);
        assert_eq!(m.get(&b), Some(&7));
        // kind 参与身份：同 vid/pid/name 不同 kind 视为不同设备
        let mut c = b.clone();
        c.kind = DeviceKind::Mouse;
        assert!(!m.contains_key(&c));
    }
}
