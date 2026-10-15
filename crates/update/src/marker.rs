//! 待完成更新标记（PLAN §4.7）：`update-pending.json` 的序列化。

use serde::{Deserialize, Serialize};

/// `%LOCALAPPDATA%\ClRecoder\update-pending.json` 内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMarker {
    pub from_version: String,
    pub to_version: String,
    pub collector_was_running: bool,
    /// RFC3339 时间戳（由胶水层填入）。
    pub created_at: String,
}

/// 序列化为紧凑 JSON（camelCase 键）。
pub fn serialize_marker(marker: &PendingMarker) -> Result<String, serde_json::Error> {
    serde_json::to_string(marker)
}

/// 解析标记；损坏 / 缺字段 → `Err`（调用方应忽略并删除文件）。
pub fn parse_marker(text: &str) -> Result<PendingMarker, serde_json::Error> {
    serde_json::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_roundtrip_camel_case() {
        let m = PendingMarker {
            from_version: "0.4.0".into(),
            to_version: "0.5.0".into(),
            collector_was_running: true,
            created_at: "2026-10-15T12:00:00Z".into(),
        };
        let s = serialize_marker(&m).unwrap();
        assert!(s.contains("fromVersion"));
        assert!(s.contains("toVersion"));
        assert!(s.contains("collectorWasRunning"));
        assert!(s.contains("createdAt"));
        assert!(!s.contains("from_version"));
        let back = parse_marker(&s).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn marker_rejects_corrupt_input() {
        assert!(parse_marker("").is_err());
        assert!(parse_marker("{").is_err());
        assert!(parse_marker(r#"{"fromVersion":"0.4.0"}"#).is_err());
        assert!(parse_marker("not json").is_err());
        assert!(parse_marker("null").is_err());
    }
}
