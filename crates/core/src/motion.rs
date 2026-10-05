//! 运动共享类型（motion-dpi §4.1）——鼠标来源/DPI/连接代际、手柄摇杆帧与 tracker 产出增量。
//!
//! 本模块只承载**跨进程共享的纯数据类型**与两个日历换算助手：不含 IO、不含采集逻辑。
//! [`crate::event::AggEvent`] 的运动变体接入（MouseSourceState/MouseTravel/GamepadMotion…）
//! 属 S4/S5，本 Stage 不改事件枚举。serde 线上字段为 Rust snake_case，仅供内部/自测；
//! GUI camelCase 合同由 §4.5 的 DTO adapter 负责，本模块不做转换。
//!
//! 类型语义要点（§4.1 锁定）：
//! - [`MotionConnectionId`]：一次运动连接在 collector 进程内单调分配的代际 ID，断连重连
//!   生成新值；与来源 key（设备身份）不可互换。
//! - [`MouseSourceDescriptor::source_key`]：真实鼠标为按 Windows 大小写不敏感语义规范化
//!   的小写完整接口路径；路径不可读/hDevice=0 归入固定桶 `virtual:unknown`
//!   （`physical=false`，保留 raw counts，禁自动/手动物理 DPI）。
//! - [`MotionStamp`]：采样时刻的单调 µs 与 UTC unix µs 成对取得（MotionRuntime 持有同一
//!   时钟），绝不以 aggregator 到达时间代替捕获时间。
//! - [`MotionControlSnapshot`]：暂停控制快照（epoch+paused 必须一致读取）；epoch 变化时
//!   tracker 必须 reset。
//!
//! 日历助手（§4.2 跨日归属用 chrono 本地日历换算；engine crate 不直接依赖 chrono，
//! 故助手随共享类型放在本模块，供 engine/collector 双侧复用）：
//! - [`local_day_from_unix_us`]：UTC µs → 本地日期；
//! - [`local_midnight_unix_us`]：本地某日 00:00 → UTC µs（DST 歧义处理见函数文档）。

use chrono::{DateTime, Local, LocalResult, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::event::DeviceKey;

/// engine 等不直接依赖 chrono 的 crate 经本模块引用日历类型的名字出口
/// （[`local_day_from_unix_us`] 的返回类型，避免各 crate 重复声明依赖）。
pub use chrono::NaiveDate;

/// 运动连接 ID：一次物理连接在 collector 进程内的代际身份（§4.1）。
///
/// 进程内单调分配且断连重连生成新值；只表达连接，不编码 vid/pid/kind——
/// 与 [`MouseSourceDescriptor::source_key`]（设备身份）不可互换。
/// 线上形状为裸 u64（newtype 直传）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MotionConnectionId(pub u64);

/// 运动采样时间戳（§4.1）：捕获时刻的单调 µs 与 UTC unix µs 成对取得。
///
/// `mono_us` 用于时长积分（不受系统改钟影响）；`unix_us` 用于本地日归属与
/// 时钟跳变检测——两者必须来自同一采样时刻，不得分开取或用到达时间补齐。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MotionStamp {
    /// 单调时钟微秒（MotionRuntime 启动 Instant 起算）
    pub mono_us: u64,
    /// UTC unix 微秒
    pub unix_us: i64,
}

/// 暂停控制快照（§4.1）：epoch 与 paused 必须作为一致快照读取，
/// 不得把 AtomicBool 与 epoch 分开读。
///
/// `Flags::set_paused` 仅在暂停状态实际改变时递增 `epoch`——因此即使真实暂停
/// 发生在两个采样之间又恢复，epoch 仍变化，tracker 必须 reset（不跨暂停积分）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MotionControlSnapshot {
    /// 暂停状态代际：每次暂停/恢复切换 +1（进程内从 0 起）
    pub epoch: u64,
    /// 当前是否暂停
    pub paused: bool,
}

/// 摇杆采样点（§4.2/§4.6）：x 向右、y 向上，范围 [-1, 1]。
///
/// 半径 >1 的原始读数由消费方做径向投影到单位圆（不逐轴硬压对角）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StickPoint {
    /// 横向偏移（右为正）
    pub x: f64,
    /// 纵向偏移（上为正）
    pub y: f64,
}

/// 摇杆侧（§4.1）：线上 snake_case（`"left"` / `"right"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StickSide {
    /// 左摇杆
    Left,
    /// 右摇杆
    Right,
}

/// 有效 DPI 的取值来源（§4.1）：线上 snake_case。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpiOrigin {
    /// 自动读取（HID++ 探测，带有效期）
    Auto,
    /// 用户手动配置
    Manual,
    /// 未知（未配置/不支持/不可读——保留 raw counts，不换算米）
    Unknown,
}

/// 换算里程所用的有效 DPI（§4.1）：`value=None` 表示未知，只记 counts 不算米。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectiveDpi {
    /// 有效 DPI 值（未知为 None）
    pub value: Option<u32>,
    /// 取值来源
    pub origin: DpiOrigin,
}

/// 鼠标来源描述（§4.1）：来源 key（接口身份）+ 型号（DeviceKey）+ 物理性。
///
/// - `source_key`：真实鼠标为规范化小写完整接口路径；不可读/虚拟设备归固定桶
///   `virtual:unknown`（`physical=false`，保留 raw counts，禁自动/手动物理 DPI）。
/// - `interface_path`：heap-safe 原生读取到的原始路径，保留原样供 HID 定位。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseSourceDescriptor {
    /// 来源 key（小写规范化接口路径或 `virtual:unknown` 固定桶）
    pub source_key: String,
    /// 型号身份（跨进程共享的设备标识）
    pub model: DeviceKey,
    /// 原始接口路径（保留原大小写供 HID 定位；未知为 None）
    pub interface_path: Option<String>,
    /// 是否识别为物理设备（虚拟/未知来源为 false）
    pub physical: bool,
}

/// DPI 探测状态（§4.1）：线上 snake_case。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpiProbeStatus {
    /// 探测尚未完成
    Pending,
    /// 自动读取成功（auto_dpi 带有效期）
    Available,
    /// 设备不支持自动读取（用 manual，无则 unknown）
    Unsupported,
    /// 多传感器/歧义读数，拒绝采信
    Ambiguous,
    /// 探测失败（HID 权限/设备忙/协议偏差——业务降级，不影响原输入）
    Unavailable,
    /// 来源已断连
    Disconnected,
}

/// 鼠标来源的当前状态快照（§4.1）：由采集线程发布、aggregator 落库、GUI 只读展示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseSourceState {
    /// 来源描述
    pub descriptor: MouseSourceDescriptor,
    /// 当前连接代际（断连重连换新值）
    pub connection: MotionConnectionId,
    /// 当前是否在线
    pub connected: bool,
    /// 状态取得时刻
    pub stamp: MotionStamp,
    /// DPI 探测状态
    pub probe_status: DpiProbeStatus,
    /// 自动读取到的 DPI（有效期见 `auto_valid_until_unix_us`）
    pub auto_dpi: Option<u32>,
    /// 自动 DPI 失效时刻（UTC unix µs；过期不继续沿用）
    pub auto_valid_until_unix_us: Option<i64>,
}

/// 鼠标位移增量（§4.1）：按（来源, 日, DPI 桶）累计的 raw counts。
///
/// 生命周期/运动不是按钮事件——不增加 events_seen/按钮总量，不计入应用键鼠次数。
/// `counts` 为原始计数（未除 DPI）；换算米由 store 按 `Σ(counts/dpi×0.0254)`（仅 dpi>0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseTravelDelta {
    /// 来源描述
    pub descriptor: MouseSourceDescriptor,
    /// 产生位移的连接代际
    pub connection: MotionConnectionId,
    /// 归属日（`YYYY-MM-DD` 本地时区）
    pub day: String,
    /// 本桶 raw counts 增量
    pub counts: f64,
    /// 打桶时所用的有效 DPI
    pub dpi: EffectiveDpi,
    /// 打桶时的暂停控制快照
    pub control: MotionControlSnapshot,
}

/// 手柄摇杆完整帧（§4.1）：直接 XInput 状态另取两根摇杆，与 gilrs 按钮事件隔离。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GamepadMotionFrame {
    /// 设备身份（结果归既有 deviceId）
    pub device: DeviceKey,
    /// 连接代际（断连重连换新值）
    pub connection: MotionConnectionId,
    /// 捕获时刻
    pub stamp: MotionStamp,
    /// 左摇杆点
    pub left: StickPoint,
    /// 右摇杆点
    pub right: StickPoint,
    /// 捕获时的暂停控制快照
    pub control: MotionControlSnapshot,
}

/// 摇杆热力单格停留增量（§4.1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StickBinDelta {
    /// 热力格号（`stick_bin`：row×25+column，0 在左上）
    pub bin: u16,
    /// 本格停留微秒
    pub dwell_us: u64,
}

/// 摇杆按日增量（§4.2）：tracker 每帧回吐的时间/路程增量，按本地日拆分。
///
/// 不变量：`active_us == Σ bins.dwell_us`（跨日拆分时微秒尾差修到原位置格）；
/// `travel_r` 为本次接受的欧氏路程（单位 R），增量归当前采样日。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StickDayDelta {
    /// 归属日（`YYYY-MM-DD` 本地时区）
    pub day: String,
    /// 摇杆侧
    pub side: StickSide,
    /// 本日活动微秒（恒等于 `Σbins.dwell_us`）
    pub active_us: u64,
    /// 本次接受的路程增量（R）
    pub travel_r: f64,
    /// 本日各格停留增量
    pub bins: Vec<StickBinDelta>,
}

/// UTC µs → 本地日期（§4.2：chrono 本地日历换算，不固定加 24 小时）。
///
/// 超出 chrono 可表示范围（不可能来自真实时钟）返回 `None`，调用方按时钟异常处理。
#[must_use]
pub fn local_day_from_unix_us(unix_us: i64) -> Option<NaiveDate> {
    let utc = DateTime::<Utc>::from_timestamp_micros(unix_us)?;
    Some(utc.with_timezone(&Local).date_naive())
}

/// 本地某日 00:00 对应的 UTC µs（§4.2 跨日切点）。
///
/// DST 处理：本地午夜因夏令时回拨出现两次（Ambiguous）取**最早**一次——区间 <500ms
/// 时日界只可能在该处首次跨越；春令时跳过本地午夜（None）用 `near_unix_us` 处的本地
/// UTC 偏移近似切点。近似误差不影响微秒守恒：调用方把切点 clamp 进积分区间后按比例
/// 分配，最后一段取剩余微秒。
#[must_use]
pub fn local_midnight_unix_us(day: NaiveDate, near_unix_us: i64) -> i64 {
    let Some(naive) = day.and_hms_opt(0, 0, 0) else {
        return near_unix_us;
    };
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(t) => t.timestamp_micros(),
        LocalResult::Ambiguous(earliest, _latest) => earliest.timestamp_micros(),
        LocalResult::None => {
            // 春令时跳过午夜：切点 = naive 午夜 − 参考时刻的本地 UTC 偏移
            let off_us = match DateTime::<Utc>::from_timestamp_micros(near_unix_us) {
                Some(t) => i64::from(t.with_timezone(&Local).offset().local_minus_utc()) * 1_000_000,
                None => 0,
            };
            Utc.from_utc_datetime(&naive).timestamp_micros() - off_us
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes::DeviceKind;
    use crate::day::format_day;

    #[test]
    fn motion_dpi_stick_side_serde_snake_case() {
        assert_eq!(serde_json::to_string(&StickSide::Left).unwrap(), r#""left""#);
        assert_eq!(serde_json::to_string(&StickSide::Right).unwrap(), r#""right""#);
        assert_eq!(serde_json::from_str::<StickSide>(r#""left""#).unwrap(), StickSide::Left);
        assert_eq!(serde_json::from_str::<StickSide>(r#""right""#).unwrap(), StickSide::Right);
        // 非法值拒绝
        assert!(serde_json::from_str::<StickSide>(r#""center""#).is_err());
    }

    #[test]
    fn motion_dpi_dpi_origin_serde_snake_case() {
        assert_eq!(serde_json::to_string(&DpiOrigin::Auto).unwrap(), r#""auto""#);
        assert_eq!(serde_json::to_string(&DpiOrigin::Manual).unwrap(), r#""manual""#);
        assert_eq!(serde_json::to_string(&DpiOrigin::Unknown).unwrap(), r#""unknown""#);
        assert_eq!(serde_json::from_str::<DpiOrigin>(r#""manual""#).unwrap(), DpiOrigin::Manual);
    }

    #[test]
    fn motion_dpi_dpi_probe_status_serde_snake_case() {
        for (status, wire) in [
            (DpiProbeStatus::Pending, "pending"),
            (DpiProbeStatus::Available, "available"),
            (DpiProbeStatus::Unsupported, "unsupported"),
            (DpiProbeStatus::Ambiguous, "ambiguous"),
            (DpiProbeStatus::Unavailable, "unavailable"),
            (DpiProbeStatus::Disconnected, "disconnected"),
        ] {
            assert_eq!(serde_json::to_string(&status).unwrap(), format!(r#""{wire}""#));
            assert_eq!(
                serde_json::from_str::<DpiProbeStatus>(&format!(r#""{wire}""#)).unwrap(),
                status
            );
        }
    }

    #[test]
    fn motion_dpi_connection_id_and_stamp_serde_shape() {
        // newtype 直传：线上形状是裸 u64
        assert_eq!(serde_json::to_string(&MotionConnectionId(42)).unwrap(), "42");
        assert_eq!(serde_json::from_str::<MotionConnectionId>("42").unwrap(), MotionConnectionId(42));
        // Hash/Copy：可直接做连接代际表的键
        let mut m = std::collections::HashMap::new();
        m.insert(MotionConnectionId(1), "a");
        assert!(m.contains_key(&MotionConnectionId(1)));
        // stamp 成对字段：snake_case，mono 无符号 / unix 有符号
        let st = MotionStamp { mono_us: 5, unix_us: -1 };
        assert_eq!(serde_json::to_string(&st).unwrap(), r#"{"mono_us":5,"unix_us":-1}"#);
        assert_eq!(serde_json::from_str::<MotionStamp>(&serde_json::to_string(&st).unwrap()).unwrap(), st);
    }

    #[test]
    fn motion_dpi_motion_control_snapshot_default() {
        // Default：未暂停、epoch 0（IPC SetPaused 线格式不变，仅内部快照）
        assert_eq!(MotionControlSnapshot::default(), MotionControlSnapshot { epoch: 0, paused: false });
        // epoch 变化（哪怕 paused 又回到 false）即可区分快照——tracker reset 判定依据
        assert_ne!(
            MotionControlSnapshot { epoch: 1, paused: false },
            MotionControlSnapshot::default()
        );
    }

    #[test]
    fn motion_dpi_stick_day_delta_serde_shape_and_round_trip() {
        let delta = StickDayDelta {
            day: "2026-06-15".to_string(),
            side: StickSide::Left,
            active_us: 1_020_000,
            travel_r: 2.0,
            bins: vec![StickBinDelta { bin: 324, dwell_us: 1_020_000 }],
        };
        let s = serde_json::to_string(&delta).unwrap();
        // 线上 snake_case 字段逐字锚定（内部/自测合同）
        assert_eq!(
            s,
            r#"{"day":"2026-06-15","side":"left","active_us":1020000,"travel_r":2.0,"bins":[{"bin":324,"dwell_us":1020000}]}"#
        );
        let back: StickDayDelta = serde_json::from_str(&s).unwrap();
        assert_eq!(back, delta);
    }

    #[test]
    fn motion_dpi_mouse_source_state_serde_round_trip() {
        let state = MouseSourceState {
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
            connection: MotionConnectionId(7),
            connected: true,
            stamp: MotionStamp { mono_us: 123_456, unix_us: 1_780_272_000_000_000 },
            probe_status: DpiProbeStatus::Available,
            auto_dpi: Some(800),
            auto_valid_until_unix_us: Some(1_780_272_004_000_000),
        };
        let s = serde_json::to_string(&state).unwrap();
        let back: MouseSourceState = serde_json::from_str(&s).unwrap();
        assert_eq!(back, state);
        // 关键线形状：来源 key/物理性、连接 ID 裸 u64、probe_status snake、stamp 成对
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["descriptor"]["source_key"], "virtual:unknown");
        assert_eq!(v["descriptor"]["physical"], serde_json::Value::Bool(false));
        assert_eq!(v["descriptor"]["model"]["kind"], "mouse");
        assert_eq!(v["connection"], serde_json::json!(7));
        assert_eq!(v["probe_status"], "available");
        assert_eq!(v["stamp"]["mono_us"], serde_json::json!(123456));
        assert_eq!(v["auto_valid_until_unix_us"], serde_json::json!(1780272004000000i64));
    }

    #[test]
    fn motion_dpi_gamepad_frame_and_travel_delta_serde_round_trip() {
        let frame = GamepadMotionFrame {
            device: DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 0x045E,
                pid: 0x028E,
                name: "XInput 手柄".to_string(),
            },
            connection: MotionConnectionId(3),
            stamp: MotionStamp { mono_us: 999, unix_us: 1_780_272_000_000_000 },
            left: StickPoint { x: 0.5, y: -0.5 },
            right: StickPoint { x: 0.0, y: 1.0 },
            control: MotionControlSnapshot { epoch: 2, paused: false },
        };
        let s = serde_json::to_string(&frame).unwrap();
        let back: GamepadMotionFrame = serde_json::from_str(&s).unwrap();
        assert_eq!(back, frame);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["left"]["x"], serde_json::json!(0.5));
        assert_eq!(v["right"]["y"], serde_json::json!(1.0));
        assert_eq!(v["control"]["epoch"], serde_json::json!(2));

        let travel = MouseTravelDelta {
            descriptor: MouseSourceDescriptor {
                source_key: r"\\?\hid#vid_1532&pid_0045&mi_00".to_string(),
                model: DeviceKey {
                    kind: DeviceKind::Mouse,
                    vid: 0x1532,
                    pid: 0x0045,
                    name: "Razer Mouse".to_string(),
                },
                interface_path: None,
                physical: true,
            },
            connection: MotionConnectionId(9),
            day: "2026-06-15".to_string(),
            counts: 1234.5,
            dpi: EffectiveDpi { value: Some(800), origin: DpiOrigin::Manual },
            control: MotionControlSnapshot { epoch: 0, paused: true },
        };
        let s = serde_json::to_string(&travel).unwrap();
        let back: MouseTravelDelta = serde_json::from_str(&s).unwrap();
        assert_eq!(back, travel);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["dpi"]["origin"], "manual");
        assert_eq!(v["dpi"]["value"], serde_json::json!(800));
        assert_eq!(v["control"]["paused"], serde_json::Value::Bool(true));
    }

    #[test]
    fn motion_dpi_local_day_and_midnight_helpers() {
        // 直接锚定：2026-06-15 本地午夜（全球主流时区该日无午夜 DST 切换）。
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let mid = local_midnight_unix_us(day, 0);
        if let LocalResult::Single(t) = Local.with_ymd_and_hms(2026, 6, 15, 0, 0, 0) {
            assert_eq!(mid, t.timestamp_micros(), "唯一转换时必须与 chrono 直接构造一致");
        }
        // 相对锚定：00:00 起属当日，前一微秒仍属前一日（本地日历，非固定 +24h 假设）
        assert_eq!(local_day_from_unix_us(mid), Some(day));
        assert_eq!(local_day_from_unix_us(mid - 1), day.pred_opt());
        // 次日午夜同样边界干净：两日交界两侧日期相邻
        if let Some(next) = day.succ_opt() {
            let mid2 = local_midnight_unix_us(next, mid);
            assert_eq!(local_day_from_unix_us(mid2), Some(next));
            assert_eq!(local_day_from_unix_us(mid2 - 1), Some(day));
        }
        // 日标签可回写解析（day.rs 同形规范）
        assert_eq!(crate::day::parse_day(&format_day(day)), Some(day));
    }
}
