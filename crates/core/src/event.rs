//! 采集层 → aggregator 的事件语言（PLAN §4.2 + correctness-v2 §4.1）。
//!
//! 三个采集线程（raw_input / gamepad / apps）各自翻译为 [`RawEvent`] / [`AggEvent::Foreground`]，
//! 经 crossbeam channel 送入 aggregator（engine_loop）。按键边沿提取、滚轮 delta→刻度折算
//! 都在采集层完成——本层事件已经是"一次物理按下"语义。
//!
//! [`InputSourceId`] 表达"一次物理连接"：collector 进程内单调分配且不重用，不编码
//! vid/pid/kind（设备身份由 [`DeviceKey`] 承载）；`0` 保留给 Engine 的单来源兼容入口，
//! 真实 Raw Input 与 selftest 显式注入使用正 ID。[`AggEvent::SourceRemoved`] /
//! [`AggEvent::KeyboardSourcesReset`] 是生命周期控制事件——不写统计、不增加 events_seen、
//! 不改变 paused/shutdown。

use serde::{Deserialize, Serialize};

use crate::codes::{DeviceKind, GamepadButton, MouseButton};
use crate::motion::{GamepadMotionFrame, MotionConnectionId, MouseSourceState, MouseTravelDelta};

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

/// 输入来源 ID（correctness-v2 §4.1）：一次物理连接在 collector 进程内的身份。
///
/// - 真实 ID 由 collector 单调分配且**不重用**；原生句柄（hDevice 等）的 bit pattern
///   不能直接充当 ID；ID 仅表达连接，不编码 vid/pid/kind。
/// - `0` 专用于 Engine 的单来源兼容入口（[`clrecoder_engine::Engine::on_key`]）；
///   真实 Raw Input 以及 selftest 显式注入使用正 ID。
/// - `#[serde(transparent)]`：线上形状是裸 u64。`Keyboard.source` 不设 serde 缺省——
///   内部构造者必须显式携带来源（项目无原始事件持久化，不建立回放兼容合同）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputSourceId(pub u64);

/// 采集层产出的原始输入事件。**只投递按下边沿**（up 不计数）；
/// 键盘的 up 事件仍需投递（Engine 维护按下状态表去重自动重复/判定组合键）。
/// （无 `Eq`：MouseMove 携带 f64 距离。）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RawEvent {
    /// 键盘事件：`sc` 为 normalize_scancode 归一化后的 scancode，`down` 为按下/抬起边沿
    Keyboard {
        /// 输入来源（连接 ID；0 为 Engine 单来源兼容入口，真实输入用正 ID）
        source: InputSourceId,
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

/// 送入 aggregator 的聚合事件：`Input` 走统计，`Foreground` 更新当前归属 exe，
/// `SourceRemoved` / `KeyboardSourcesReset` 为生命周期控制事件（不写统计、不增加
/// events_seen、不改变 paused/shutdown），`MouseSourceState` / `MouseTravel` /
/// `GamepadMotion` / `GamepadMotionDisconnected` 为运动事件（motion-dpi §4.1/§4.3
/// ——不增加 events_seen/按钮总量、不计入应用键鼠次数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AggEvent {
    /// 原始输入事件（键盘/鼠标/手柄）
    Input(RawEvent),
    /// 前台应用切换，由 apps 线程发出；exe 为小写 basename（解析失败为 "unknown"）
    Foreground {
        /// 前台进程 exe 的小写 basename
        exe: String,
    },
    /// 输入来源移除（设备拔出/失联）：aggregator 据此清 Engine 中该来源的按下状态
    SourceRemoved {
        /// 被移除的连接 ID
        source: InputSourceId,
    },
    /// 全部键盘来源重置（待机/系统级重置）：aggregator 据此清空 Engine 按下状态
    KeyboardSourcesReset,
    /// 鼠标来源状态心跳（motion-dpi §4.3，约每 2s/状态变化一次）：采集侧发布，
    /// aggregator 注册来源并落库 connected/probe_status/auto_dpi（元数据非增量，
    /// 失败可重试、不涉及 counts）。连接代际由 aggregator 门控（旧代结果拒绝）。
    MouseSourceState(MouseSourceState),
    /// 鼠标位移增量（motion-dpi §4.3）：按（来源, 捕获本地日, EffectiveDpi 桶）批发的
    /// raw counts。paused 包不累计、DPI 快照/日期/暂停代际变化整桶批发——计数与
    /// 暂停语义由采集侧按捕获 control 快照保证，aggregator 无条件落库。
    MouseTravel(MouseTravelDelta),
    /// 手柄摇杆完整帧（motion-dpi §4.3）：直接 XInput 状态另取两根摇杆，与 gilrs 按钮
    /// 事件隔离（四轴一次快照同帧读取）。tracker（aggregator 侧按连接×side 维护）由
    /// 捕获 control 与当前快照比对决定喂入或 reset/跳过——不跨短暂停连接坐标。
    GamepadMotion(GamepadMotionFrame),
    /// 手柄摇杆连接结束（motion-dpi §4.3）：`ERROR_DEVICE_NOT_CONNECTED` 断连、其他
    /// 读取失败或轮询上下文重建——aggregator 据此 reset 该连接全部摇杆 tracker，
    /// 不跨断点连线；断连绝不携带合成的回中立路程帧。
    GamepadMotionDisconnected {
        /// 结束的运动连接代际
        connection: MotionConnectionId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::{
        DpiOrigin, DpiProbeStatus, EffectiveDpi, GamepadMotionFrame, MotionConnectionId,
        MotionControlSnapshot, MotionStamp, MouseSourceDescriptor, StickPoint,
    };

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
            RawEvent::Keyboard { source: InputSourceId(101), device: kb.clone(), sc: 0xE01D, down: true },
            RawEvent::Keyboard { source: InputSourceId(102), device: kb, sc: 0x2A, down: false },
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
            AggEvent::Input(RawEvent::Keyboard {
                source: InputSourceId(101),
                device: d,
                sc: 0x1E,
                down: true,
            }),
            AggEvent::Foreground { exe: "explorer.exe".to_string() },
            AggEvent::SourceRemoved { source: InputSourceId(102) },
            AggEvent::KeyboardSourcesReset,
        ];
        for e in events {
            let s = serde_json::to_string(&e).unwrap();
            let back: AggEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(back, e);
        }
    }

    // ------------------------------------------------------------------
    // correctness-v2 §4.1：InputSourceId 线上形状
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // motion-dpi §4.3：鼠标运动事件变体（AggEvent serde 往返）
    // ------------------------------------------------------------------

    #[test]
    fn motion_dpi_agg_event_mouse_variants_serde_round_trip() {
        let state = AggEvent::MouseSourceState(MouseSourceState {
            descriptor: MouseSourceDescriptor {
                source_key: r"\\?\hid#vid_046d&pid_c08b&mi_00#7&2f3a3d&0&0000".to_string(),
                model: DeviceKey {
                    kind: DeviceKind::Mouse,
                    vid: 0x046D,
                    pid: 0xC08B,
                    name: "测试鼠标".to_string(),
                },
                interface_path: None,
                physical: true,
            },
            connection: MotionConnectionId(5),
            connected: true,
            stamp: MotionStamp { mono_us: 1_000, unix_us: 1_780_272_000_000_000 },
            probe_status: DpiProbeStatus::Available,
            auto_dpi: Some(800),
            auto_valid_until_unix_us: Some(1_780_272_004_000_000),
        });
        let travel = AggEvent::MouseTravel(MouseTravelDelta {
            descriptor: MouseSourceDescriptor {
                source_key: "virtual:unknown".to_string(),
                model: DeviceKey {
                    kind: DeviceKind::Mouse,
                    vid: 0,
                    pid: 0,
                    name: "未知/虚拟设备".to_string(),
                },
                interface_path: None,
                physical: false,
            },
            connection: MotionConnectionId(6),
            day: "2026-06-15".to_string(),
            counts: 42.5,
            dpi: EffectiveDpi { value: None, origin: DpiOrigin::Unknown },
            control: MotionControlSnapshot { epoch: 1, paused: false },
        });
        for e in [state, travel] {
            let s = serde_json::to_string(&e).unwrap();
            let back: AggEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(back, e);
        }
    }

    // ------------------------------------------------------------------
    // motion-dpi §4.3：手柄运动事件变体（AggEvent serde 往返，S5）
    // ------------------------------------------------------------------

    #[test]
    fn motion_dpi_agg_event_gamepad_variants_serde_round_trip() {
        let frame = AggEvent::GamepadMotion(GamepadMotionFrame {
            device: DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 0x045E,
                pid: 0x028E,
                name: "Xbox Controller".to_string(),
            },
            connection: MotionConnectionId(11),
            stamp: MotionStamp { mono_us: 77_000, unix_us: 1_780_272_000_077_000 },
            left: StickPoint { x: -0.5, y: 1.0 },
            right: StickPoint { x: 0.0, y: -1.0 },
            control: MotionControlSnapshot { epoch: 3, paused: false },
        });
        let disconnected =
            AggEvent::GamepadMotionDisconnected { connection: MotionConnectionId(11) };
        for e in [frame, disconnected] {
            let s = serde_json::to_string(&e).unwrap();
            let back: AggEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(back, e);
        }
        // 线形状锚点：变体名不漂移、连接代际为裸 u64 newtype 直传
        assert_eq!(
            serde_json::to_string(&AggEvent::GamepadMotionDisconnected {
                connection: MotionConnectionId(11)
            })
            .unwrap(),
            r#"{"GamepadMotionDisconnected":{"connection":11}}"#
        );
    }


    #[test]
    fn correctness_v2_input_source_id_serde_transparent() {
        // #[serde(transparent)]：线上形状就是裸 u64（内部事件 serde 契约）
        assert_eq!(serde_json::to_string(&InputSourceId(101)).unwrap(), "101");
        assert_eq!(serde_json::from_str::<InputSourceId>("101").unwrap(), InputSourceId(101));
        // Default = 0（Engine 单来源兼容入口专用值）
        assert_eq!(InputSourceId::default(), InputSourceId(0));
        // Copy/Hash/Eq：可直接做 HashSet<(InputSourceId, u16)> 的键（Engine held 维度）
        use std::collections::HashSet;
        let mut set: HashSet<(InputSourceId, u16)> = HashSet::new();
        set.insert((InputSourceId(101), 0x1E));
        assert!(set.contains(&(InputSourceId(101), 0x1E)));
        assert!(!set.contains(&(InputSourceId(102), 0x1E)));
    }

    #[test]
    fn correctness_v2_keyboard_source_field_is_required() {
        // Keyboard.source 不设 serde 缺省：内部构造者必须显式携带来源——
        // 缺 source 的旧形状 JSON 反序列化必须失败（不建立虚假回放兼容合同）。
        let missing = r#"{"Keyboard":{"device":{"kind":"keyboard","vid":0,"pid":0,"name":"x"},"sc":30,"down":true}}"#;
        assert!(serde_json::from_str::<RawEvent>(missing).is_err());
        // 完整形状：source 是裸 u64 字段（transparent 穿透枚举内部）
        let full = r#"{"Keyboard":{"source":101,"device":{"kind":"keyboard","vid":0,"pid":0,"name":"x"},"sc":30,"down":true}}"#;
        let back: RawEvent = serde_json::from_str(full).unwrap();
        assert_eq!(
            back,
            RawEvent::Keyboard {
                source: InputSourceId(101),
                device: DeviceKey {
                    kind: DeviceKind::Keyboard,
                    vid: 0,
                    pid: 0,
                    name: "x".to_string(),
                },
                sc: 0x1E,
                down: true,
            }
        );
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
