//! 实时输入 v1 跨进程协议（DEVPLAN-input-live-trigger §4.1）。
//!
//! - 传输：独立命名管道 [`LIVE_PIPE_NAME`]（与控制管 `clrecoder-control` 分离）。
//! - 编码：UTF-8 NDJSON，一行请求 / 一行响应；长度上限含换行。
//! - 字段：Rust snake_case → 线上 camelCase；枚举值为小写 snake_case。
//! - [`LiveSource`]：内部标签 `kind` + `rename_all_fields = "camelCase"`。
//!
//! 本模块只承载纯 DTO/常量与序列化测试；不含 IO。序号/epoch/连接 ID 以十进制
//! 字符串跨进程传递，避免 JS u64 精度丢失。

use serde::{Deserialize, Serialize};

use crate::event::DeviceKey;

/// 实时管道名（与控制管分离）。
pub const LIVE_PIPE_NAME: &str = r"\\.\pipe\clrecoder-live-v1";

/// 协议主版本（请求/响应均须等于此值）。
pub const LIVE_PROTOCOL_VERSION: u8 = 1;

/// 单行请求最大字节数（含换行）。
pub const LIVE_MAX_REQUEST_BYTES: usize = 512;

/// 单行响应最大字节数（含换行）。
pub const LIVE_MAX_RESPONSE_BYTES: usize = 131_072;

/// 客户端 I/O 超时（毫秒）；可取消。
pub const LIVE_IO_TIMEOUT_MS: u64 = 250;

/// 来源过期阈值（毫秒）：超过则视为 stale。
pub const LIVE_SOURCE_STALE_MS: u32 = 500;

/// GUI → collector 快照请求（NDJSON 单行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSnapshotRequest {
    /// 协议版本；必须等于 [`LIVE_PROTOCOL_VERSION`]
    pub protocol_version: u8,
}

/// 采集器捕获态（线上 snake_case）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveCaptureState {
    /// 正在捕获
    Active,
    /// 已挂起（如会话锁定）
    Suspended,
    /// 未知
    Unknown,
}

/// 手柄来源可用性（线上 snake_case）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveSourceAvailability {
    /// 可读
    Ready,
    /// 不可用（断连/槽位空）
    Unavailable,
}

/// 实时协议错误码（线上 snake_case）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveErrorCode {
    /// 无可用实时服务（含旧版采集器）
    NotRunning,
    /// 管道访问被拒
    AccessDenied,
    /// 管道忙
    Busy,
    /// I/O 超时
    Timeout,
    /// 其它 I/O 失败
    Io,
    /// 协议不匹配 / 非法载荷
    Protocol,
    /// 响应过大
    TooLarge,
}

/// 结构化错误载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveError {
    /// 稳定短码
    pub code: LiveErrorCode,
    /// 展示文案
    pub message: String,
}

/// 单键按下序号与年龄（每 code 至多一项，非事件日志）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LivePress {
    /// 设备种类内 code
    pub code: u16,
    /// 该来源该 code 已观察有效按下边沿数（十进制字符串）
    pub press_seq: String,
    /// 距最近一次有效按下的毫秒数
    pub age_ms: u32,
}

/// 数字按键实时态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveDigitalState {
    /// 当前仍按住的 code（去重、稳定排序）
    pub held_codes: Vec<u16>,
    /// 各 code 最近一次有效按下的序号与年龄
    pub presses: Vec<LivePress>,
}

/// 二维模拟点（摇杆等）；Y 正向上。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LivePoint {
    /// 横向（右为正）
    pub x: f64,
    /// 纵向（上为正）
    pub y: f64,
}

/// 手柄连续模拟量（行程，非压力）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveGamepadAnalog {
    /// 左摇杆
    pub left_stick: LivePoint,
    /// 右摇杆
    pub right_stick: LivePoint,
    /// LT = raw/255
    pub left_trigger: f64,
    /// RT = raw/255
    pub right_trigger: f64,
}

/// 单条采集来源（内部标签 `kind`；字段 camelCase）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum LiveSource {
    /// 键盘来源
    Keyboard {
        /// 稳定连接标识（不可由前端推算 DB id）
        source_id: String,
        /// 型号身份（原名，非 nickname）
        device: DeviceKey,
        /// 数字按键态
        digital: LiveDigitalState,
    },
    /// 鼠标来源
    Mouse {
        /// 稳定连接标识
        source_id: String,
        /// 型号身份
        device: DeviceKey,
        /// 与持久库 `source_key` 完全一致
        source_key: String,
        /// 数字按键态（1..5 held；6..9 仅 pulse）
        digital: LiveDigitalState,
    },
    /// 手柄来源（slot 0..3）
    Gamepad {
        /// 稳定连接标识
        source_id: String,
        /// 型号身份
        device: DeviceKey,
        /// XInput 槽位 0..3
        slot: u8,
        /// 连接代际（十进制字符串，源自 MotionConnectionId）
        connection_id: String,
        /// 槽位可读性
        availability: LiveSourceAvailability,
        /// 距最近一次成功读的毫秒数
        age_ms: u32,
        /// 数字按键态（1..17；Guide=11 仅 unsupported）
        digital: LiveDigitalState,
        /// 连续模拟量；不可用时为 None
        analog: Option<LiveGamepadAnalog>,
        /// 不支持的 code（必须含 11/Guide）
        unsupported_codes: Vec<u16>,
    },
}

/// 一次成功快照载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSnapshot {
    /// 采集器进程会话（如 PID:启动时间）
    pub session_id: String,
    /// 快照序号（十进制字符串；每次生成递增）
    pub sample_seq: String,
    /// 复位代际（十进制字符串；会话/破坏性断段时递增）
    pub reset_epoch: String,
    /// 统计是否暂停（不等价于实时挂起）
    pub statistics_paused: bool,
    /// 捕获态
    pub capture_state: LiveCaptureState,
    /// 距最近 RAW 输入的毫秒；未知为 None
    pub raw_input_age_ms: Option<u32>,
    /// 来源列表（稳定排序）
    pub sources: Vec<LiveSource>,
}

/// 快照响应：成功 `snapshot!=null && error==null`；失败相反。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSnapshotReply {
    /// 协议版本
    pub protocol_version: u8,
    /// 成功时的快照
    pub snapshot: Option<LiveSnapshot>,
    /// 失败时的错误
    pub error: Option<LiveError>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes::DeviceKind;

    fn kb_device() -> DeviceKey {
        DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 0x04D9,
            pid: 0x0169,
            name: "HID Keyboard Device".to_string(),
        }
    }

    fn mouse_device() -> DeviceKey {
        DeviceKey {
            kind: DeviceKind::Mouse,
            vid: 0x046D,
            pid: 0xC077,
            name: "USB Optical Mouse".to_string(),
        }
    }

    fn pad_device() -> DeviceKey {
        DeviceKey {
            kind: DeviceKind::Gamepad,
            vid: 0x045E,
            pid: 0x028E,
            name: "Xbox 360 Controller".to_string(),
        }
    }

    #[test]
    fn constants_match_contract() {
        assert_eq!(LIVE_PIPE_NAME, r"\\.\pipe\clrecoder-live-v1");
        assert_eq!(LIVE_PROTOCOL_VERSION, 1);
        assert_eq!(LIVE_MAX_REQUEST_BYTES, 512);
        assert_eq!(LIVE_MAX_RESPONSE_BYTES, 131_072);
        assert_eq!(LIVE_IO_TIMEOUT_MS, 250);
        assert_eq!(LIVE_SOURCE_STALE_MS, 500);
    }

    #[test]
    fn request_wire_format() {
        let req = LiveSnapshotRequest {
            protocol_version: LIVE_PROTOCOL_VERSION,
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"protocolVersion":1}"#
        );
        let back: LiveSnapshotRequest = serde_json::from_str(r#"{"protocolVersion":1}"#).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn success_empty_sources_fixture() {
        let wire = r#"{"protocolVersion":1,"snapshot":{"sessionId":"42:1791970000000000","sampleSeq":"8","resetEpoch":"0","statisticsPaused":false,"captureState":"active","rawInputAgeMs":10,"sources":[]},"error":null}"#;
        let reply: LiveSnapshotReply = serde_json::from_str(wire).unwrap();
        assert_eq!(reply.protocol_version, 1);
        assert!(reply.error.is_none());
        let snap = reply.snapshot.as_ref().unwrap();
        assert_eq!(snap.session_id, "42:1791970000000000");
        assert_eq!(snap.sample_seq, "8");
        assert_eq!(snap.reset_epoch, "0");
        assert!(!snap.statistics_paused);
        assert_eq!(snap.capture_state, LiveCaptureState::Active);
        assert_eq!(snap.raw_input_age_ms, Some(10));
        assert!(snap.sources.is_empty());
        assert_eq!(serde_json::to_string(&reply).unwrap(), wire);
    }

    #[test]
    fn not_running_error_fixture() {
        let wire = r#"{"protocolVersion":1,"snapshot":null,"error":{"code":"not_running","message":"实时输入不可用，请更新并启动采集器"}}"#;
        let reply: LiveSnapshotReply = serde_json::from_str(wire).unwrap();
        assert!(reply.snapshot.is_none());
        let err = reply.error.as_ref().unwrap();
        assert_eq!(err.code, LiveErrorCode::NotRunning);
        assert_eq!(err.message, "实时输入不可用，请更新并启动采集器");
        assert_eq!(serde_json::to_string(&reply).unwrap(), wire);
    }

    #[test]
    fn error_codes_snake_case() {
        let cases = [
            (LiveErrorCode::NotRunning, "not_running"),
            (LiveErrorCode::AccessDenied, "access_denied"),
            (LiveErrorCode::Busy, "busy"),
            (LiveErrorCode::Timeout, "timeout"),
            (LiveErrorCode::Io, "io"),
            (LiveErrorCode::Protocol, "protocol"),
            (LiveErrorCode::TooLarge, "too_large"),
        ];
        for (code, expected) in cases {
            let s = serde_json::to_string(&code).unwrap();
            assert_eq!(s, format!("\"{expected}\""));
            let back: LiveErrorCode = serde_json::from_str(&s).unwrap();
            assert_eq!(back, code);
        }
    }

    #[test]
    fn capture_state_and_availability_snake_case() {
        assert_eq!(
            serde_json::to_string(&LiveCaptureState::Suspended).unwrap(),
            r#""suspended""#
        );
        assert_eq!(
            serde_json::to_string(&LiveCaptureState::Unknown).unwrap(),
            r#""unknown""#
        );
        assert_eq!(
            serde_json::to_string(&LiveSourceAvailability::Ready).unwrap(),
            r#""ready""#
        );
        assert_eq!(
            serde_json::to_string(&LiveSourceAvailability::Unavailable).unwrap(),
            r#""unavailable""#
        );
    }

    #[test]
    fn live_source_keyboard_mouse_gamepad_round_trip() {
        let sources = vec![
            LiveSource::Keyboard {
                source_id: "kb-1".into(),
                device: kb_device(),
                digital: LiveDigitalState {
                    held_codes: vec![0x1E],
                    presses: vec![LivePress {
                        code: 0x1E,
                        press_seq: "3".into(),
                        age_ms: 12,
                    }],
                },
            },
            LiveSource::Mouse {
                source_id: "ms-1".into(),
                device: mouse_device(),
                source_key: r"\\?\hid#vid_046d&pid_c077#6&abc".into(),
                digital: LiveDigitalState {
                    held_codes: vec![1],
                    presses: vec![],
                },
            },
            LiveSource::Gamepad {
                source_id: "gp-0".into(),
                device: pad_device(),
                slot: 0,
                connection_id: "7".into(),
                availability: LiveSourceAvailability::Ready,
                age_ms: 5,
                digital: LiveDigitalState {
                    held_codes: vec![1, 7],
                    presses: vec![LivePress {
                        code: 1,
                        press_seq: "1".into(),
                        age_ms: 2,
                    }],
                },
                analog: Some(LiveGamepadAnalog {
                    left_stick: LivePoint { x: 0.5, y: -0.25 },
                    right_stick: LivePoint { x: 0.0, y: 1.0 },
                    left_trigger: 0.0,
                    right_trigger: 128.0 / 255.0,
                }),
                unsupported_codes: vec![11],
            },
        ];
        let reply = LiveSnapshotReply {
            protocol_version: LIVE_PROTOCOL_VERSION,
            snapshot: Some(LiveSnapshot {
                session_id: "99:1".into(),
                sample_seq: "1".into(),
                reset_epoch: "0".into(),
                statistics_paused: true,
                capture_state: LiveCaptureState::Active,
                raw_input_age_ms: None,
                sources,
            }),
            error: None,
        };
        let s = serde_json::to_string(&reply).unwrap();
        assert!(s.contains(r#""kind":"keyboard""#));
        assert!(s.contains(r#""kind":"mouse""#));
        assert!(s.contains(r#""kind":"gamepad""#));
        assert!(s.contains(r#""sourceId":"kb-1""#));
        assert!(s.contains(r#""sourceKey":"#));
        assert!(s.contains(r#""connectionId":"7""#));
        assert!(s.contains(r#""leftStick":"#));
        assert!(s.contains(r#""unsupportedCodes":[11]"#));
        assert!(s.contains(r#""heldCodes":[30]"#));
        let back: LiveSnapshotReply = serde_json::from_str(&s).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn reject_bad_protocol_version_field_type() {
        // protocolVersion 非数字 → 结构错误
        let err = serde_json::from_str::<LiveSnapshotRequest>(r#"{"protocolVersion":"1"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn reject_bad_structure_missing_fields() {
        // 请求缺 protocolVersion
        assert!(serde_json::from_str::<LiveSnapshotRequest>(r#"{}"#).is_err());
        // 响应 snapshot 类型错误（应为 object|null）
        assert!(serde_json::from_str::<LiveSnapshotReply>(
            r#"{"protocolVersion":1,"snapshot":"oops","error":null}"#
        )
        .is_err());
        // LiveError 缺 message
        assert!(serde_json::from_str::<LiveError>(r#"{"code":"not_running"}"#).is_err());
        // LiveSnapshot 缺必填 sessionId
        assert!(serde_json::from_str::<LiveSnapshot>(
            r#"{"sampleSeq":"1","resetEpoch":"0","statisticsPaused":false,"captureState":"active","rawInputAgeMs":null,"sources":[]}"#
        )
        .is_err());
    }

    #[test]
    fn reject_unknown_error_code() {
        let err = serde_json::from_str::<LiveErrorCode>(r#""bogus""#);
        assert!(err.is_err());
    }

    #[test]
    fn reject_unknown_live_source_kind() {
        let wire = r#"{"kind":"joystick","sourceId":"x","device":{"kind":"gamepad","vid":0,"pid":0,"name":"x"},"digital":{"heldCodes":[],"presses":[]}}"#;
        assert!(serde_json::from_str::<LiveSource>(wire).is_err());
    }
}
