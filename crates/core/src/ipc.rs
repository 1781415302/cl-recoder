//! GUI ↔ collector 控制协议（PLAN §4.4 逐字对齐）。
//!
//! - 传输：命名管道 [`PIPE_NAME`]，字节模式、PIPE_WAIT、`PIPE_REJECT_REMOTE_CLIENTS`；
//!   显式 DACL `D:P(A;;GA;;;<当前用户SID>)`（SID 运行时解析，collector 侧实现）。
//! - 编码：NDJSON——一行请求（UTF-8，**≤ [`MAX_REQUEST_BYTES`] 字节，超长即断开**）→ 一行响应。
//! - 两侧类型共享本模块（编译期保证一致）；**协议结构变更必须同版升级两侧**（PLAN §2.5）。
//!
//! 线格式示例（GUI 客户端按此实现）：
//!
//! ```text
//! → {"cmd":"status"}
//! ← {"ok":true,"data":{"paused":false,"version":"0.1.0","started_at":"2026-09-27T22:00:00+08:00","last_event_at":"2026-09-27T22:14:31+08:00","events_seen":48219}}
//! → {"cmd":"set_paused","paused":true}
//! ← {"ok":true,"data":null}
//! → {"cmd":"bogus"}
//! ← {"ok":false,"error":"unknown command"}
//! ```
//!
//! 响应形状契约：成功带数据 = `{"ok":true,"data":{...}}`；成功无数据 = `{"ok":true,"data":null}`；
//! 失败 = `{"ok":false,"error":"..."}`（不出现在成功响应中）。 [`CtlRequest`] 反序列化失败的
//! 未知命令（如 `"bogus"`）由服务端捕获并回 `{"ok":false,"error":"unknown command"}`。

use serde::{Deserialize, Serialize};

/// 控制管道名（字节模式）。
pub const PIPE_NAME: &str = r"\\.\pipe\clrecoder-control";

/// 单行请求最大字节数（8KB，含换行）；超长请求服务端直接断开连接（§4.4）。
pub const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// GUI → collector 控制请求（NDJSON 单行）。
///
/// serde 内部标签 `cmd`，变体名 snake_case：`{"cmd":"status"}` /
/// `{"cmd":"set_paused","paused":true}` / `{"cmd":"shutdown"}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum CtlRequest {
    /// 查询运行状态（返回 [`StatusData`]）
    Status,
    /// 暂停/恢复统计：暂停只丢弃 Input 事件，Foreground 照常处理（§5.3）
    SetPaused {
        /// true=暂停，false=恢复
        paused: bool,
    },
    /// 请求采集进程优雅退出
    Shutdown,
}

/// 采集进程运行状态（`Status` 请求的 data 载荷）。
///
/// `started_at` / `last_event_at` 为 RFC 3339 本地时区字符串
/// （例：`"2026-09-27T22:00:00+08:00"`，见 [`crate::day::now_local_rfc3339`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusData {
    /// 是否处于暂停统计状态（不持久化，重启即恢复统计）
    pub paused: bool,
    /// collector 版本（如 "0.1.0"）
    pub version: String,
    /// 进程启动时刻（RFC 3339 带本地时区偏移）
    pub started_at: String,
    /// 最近一次输入事件时刻；尚无事件时为 `null`
    pub last_event_at: Option<String>,
    /// 进程启动以来累计收到的事件数
    pub events_seen: u64,
}

/// collector → GUI 的统一响应。
///
/// **形状契约（与 §4.4 三个线格式示例逐字对齐，S8 往返/S10 客户端均以此为准）**：
/// - 成功带数据：`{"ok":true,"data":{...}}`
/// - 成功无数据：`{"ok":true,"data":null}`（`data` 字段**必须存在**且为 `null`）
/// - 失败：`{"ok":false,"error":"..."}`（无 `data` 字段）
///
/// 因此 `Serialize` 为手写实现（`skip_serializing_if` 无法表达"data 字段的有无取决于
/// `ok`"这一跨字段规则）：成功时 `data` 恒出现（`None` → `null`）；失败时 `data` 恒省略、
/// `error` 在场。`Deserialize` 保持 derive：`data` 缺失/为 `null` 均反序列化为 `None`
/// （对旧版/宽松客户端向后兼容）。
///
/// 普通结构体而非 enum：serde 默认枚举表示产不出 `{"ok":true,"data":null}` 形状（PLAN §4.4 原注）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CtlResponse {
    /// 请求是否成功
    pub ok: bool,
    /// 成功时的数据载荷；无数据时序列化为 `null`
    pub data: Option<StatusData>,
    /// 失败原因；成功响应中省略
    pub error: Option<String>,
}

impl Serialize for CtlResponse {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if self.ok {
            // 成功：{"ok":true,"data":<obj|null>[,"error":...]}
            let mut st = serializer.serialize_struct("CtlResponse", 2 + usize::from(self.error.is_some()))?;
            st.serialize_field("ok", &self.ok)?;
            st.serialize_field("data", &self.data)?;
            if let Some(err) = &self.error {
                st.serialize_field("error", err)?;
            }
            st.end()
        } else {
            // 失败：{"ok":false[,"error":...]}——data 字段不出现
            let mut st = serializer.serialize_struct("CtlResponse", 1 + usize::from(self.error.is_some()))?;
            st.serialize_field("ok", &self.ok)?;
            if let Some(err) = &self.error {
                st.serialize_field("error", err)?;
            }
            st.end()
        }
    }
}

impl CtlResponse {
    /// 成功 + 数据载荷（`{"ok":true,"data":{...}}`）。
    #[must_use]
    pub fn ok_with(data: StatusData) -> Self {
        Self { ok: true, data: Some(data), error: None }
    }

    /// 成功无数据（`{"ok":true,"data":null}`）。
    #[must_use]
    pub fn ok_no_data() -> Self {
        Self { ok: true, data: None, error: None }
    }

    /// 失败（`{"ok":false,"error":"..."}`）。
    #[must_use]
    pub fn err<E: Into<String>>(msg: E) -> Self {
        Self { ok: false, data: None, error: Some(msg.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 请求线格式 ----------

    #[test]
    fn request_status_wire_format() {
        assert_eq!(serde_json::to_string(&CtlRequest::Status).unwrap(), r#"{"cmd":"status"}"#);
        let back: CtlRequest = serde_json::from_str(r#"{"cmd":"status"}"#).unwrap();
        assert_eq!(back, CtlRequest::Status);
    }

    #[test]
    fn request_set_paused_wire_format() {
        let s = serde_json::to_string(&CtlRequest::SetPaused { paused: true }).unwrap();
        assert_eq!(s, r#"{"cmd":"set_paused","paused":true}"#);
        let back: CtlRequest = serde_json::from_str(r#"{"cmd":"set_paused","paused":true}"#).unwrap();
        assert_eq!(back, CtlRequest::SetPaused { paused: true });
    }

    #[test]
    fn request_shutdown_wire_format() {
        assert_eq!(serde_json::to_string(&CtlRequest::Shutdown).unwrap(), r#"{"cmd":"shutdown"}"#);
        let back: CtlRequest = serde_json::from_str(r#"{"cmd":"shutdown"}"#).unwrap();
        assert_eq!(back, CtlRequest::Shutdown);
    }

    #[test]
    fn request_unknown_command_fails_deserialize() {
        // §4.4 第三组：{"cmd":"bogus"} 无法映射到任何变体 → 服务端回 unknown command
        assert!(serde_json::from_str::<CtlRequest>(r#"{"cmd":"bogus"}"#).is_err());
        assert!(serde_json::from_str::<CtlRequest>(r#"{}"#).is_err());
        // 缺字段同样失败
        assert!(serde_json::from_str::<CtlRequest>(r#"{"cmd":"set_paused"}"#).is_err());
    }

    // ---------- 响应线格式：§4.4 三个示例逐字对齐 ----------

    #[test]
    fn response_status_example_verbatim() {
        let resp = CtlResponse::ok_with(StatusData {
            paused: false,
            version: "0.1.0".to_string(),
            started_at: "2026-09-27T22:00:00+08:00".to_string(),
            last_event_at: Some("2026-09-27T22:14:31+08:00".to_string()),
            events_seen: 48_219,
        });
        let line = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            line,
            r#"{"ok":true,"data":{"paused":false,"version":"0.1.0","started_at":"2026-09-27T22:00:00+08:00","last_event_at":"2026-09-27T22:14:31+08:00","events_seen":48219}}"#
        );
        // 往返
        let back: CtlResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn response_set_paused_example_verbatim() {
        let resp = CtlResponse::ok_no_data();
        let line = serde_json::to_string(&resp).unwrap();
        assert_eq!(line, r#"{"ok":true,"data":null}"#);
        // 往返
        let back: CtlResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(back, resp);
        assert_eq!(back.data, None);
        assert_eq!(back.error, None);
    }

    #[test]
    fn response_bogus_example_verbatim() {
        let resp = CtlResponse::err("unknown command");
        let line = serde_json::to_string(&resp).unwrap();
        assert_eq!(line, r#"{"ok":false,"error":"unknown command"}"#);
        // 往返
        let back: CtlResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn status_data_last_event_at_null() {
        // last_event_at 为 Option：尚无事件时序列化为 null（GUI TS: string | null）
        let resp = CtlResponse::ok_with(StatusData {
            paused: true,
            version: "0.1.0".to_string(),
            started_at: "2026-09-27T22:00:00+08:00".to_string(),
            last_event_at: None,
            events_seen: 0,
        });
        let line = serde_json::to_string(&resp).unwrap();
        assert!(line.contains(r#""last_event_at":null"#), "line = {line}");
        let back: CtlResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(back.data.unwrap().last_event_at, None);
    }

    #[test]
    fn response_parses_with_optional_fields_missing() {
        // 容错：data/error 字段缺失时反序列化为 None（对旧版响应保持向后兼容）
        let back: CtlResponse = serde_json::from_str(r#"{"ok":true}"#).unwrap();
        assert!(back.ok);
        assert_eq!(back.data, None);
        assert_eq!(back.error, None);
    }

    #[test]
    fn constants_match_plan() {
        assert_eq!(PIPE_NAME, r"\\.\pipe\clrecoder-control");
        assert_eq!(MAX_REQUEST_BYTES, 8 * 1024);
    }
}
