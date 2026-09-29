//! 日期工具（PLAN §4）：所有 `day` 一律 `YYYY-MM-DD`（**本地时区**）字符串。
//!
//! 供采集聚合（跨天开桶）、查询范围校验（from/to）与导出复用；基于 chrono，无 IO。
//! 时间戳类字段（`started_at` / `last_event_at`）用 [`now_local_rfc3339`]——
//! RFC 3339 带本地时区偏移，与 §4.4 示例 `"2026-09-27T22:00:00+08:00"` 同形。

use chrono::{Datelike, Local, NaiveDate, SecondsFormat};

/// 当前本地日期，格式 `"YYYY-MM-DD"`（本地时区，PLAN §4 锁定）。
#[must_use]
pub fn today_local() -> String {
    format_day(Local::now().date_naive())
}

/// `NaiveDate` → `"YYYY-MM-DD"`（零填充）。
#[must_use]
pub fn format_day(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

/// 解析 `"YYYY-MM-DD"` 为 `NaiveDate`；**只接受规范零填充形式**（回写比对校验，
/// `"2026-9-7"`、带时间的字符串、非法日期一律返回 `None`）。
#[must_use]
pub fn parse_day(s: &str) -> Option<NaiveDate> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    // 严格性：重新格式化后必须与输入逐字一致，杜绝 "2026-9-7" 之类的宽松解析混入
    if format_day(d) == s {
        Some(d)
    } else {
        None
    }
}

/// 当前本地时刻，RFC 3339 带本地时区偏移、秒级精度
/// （例：`"2026-09-27T22:00:00+08:00"`）。
/// 用于 IPC [`crate::ipc::StatusData`] 的 `started_at` / `last_event_at`。
#[must_use]
pub fn now_local_rfc3339() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// 校验 from/to 范围：两者都必须是规范 `YYYY-MM-DD` 且 `from <= to`。
#[must_use]
pub fn valid_range(from: &str, to: &str) -> bool {
    match (parse_day(from), parse_day(to)) {
        (Some(f), Some(t)) => f <= t,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    #[test]
    fn today_local_is_canonical() {
        let s = today_local();
        assert_eq!(s.len(), 10, "len = {s}");
        assert!(parse_day(&s).is_some(), "today_local 必须可被 parse_day 解析: {s}");
    }

    #[test]
    fn format_day_zero_padding() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        assert_eq!(format_day(d), "2026-09-07");
        let d = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        assert_eq!(format_day(d), "2026-12-31");
    }

    #[test]
    fn parse_day_accepts_canonical_only() {
        assert_eq!(parse_day("2026-09-07"), NaiveDate::from_ymd_opt(2026, 9, 7));
        // 宽松形式拒绝（回写比对）
        assert_eq!(parse_day("2026-9-7"), None);
        assert_eq!(parse_day("2026-09-7"), None);
        // 带时间/垃圾/空串拒绝
        assert_eq!(parse_day("2026-09-07T22:00:00"), None);
        assert_eq!(parse_day("2026-13-01"), None); // 越界月份
        assert_eq!(parse_day("2026-02-30"), None); // 不存在的日期
        assert_eq!(parse_day("not-a-day"), None);
        assert_eq!(parse_day(""), None);
    }

    #[test]
    fn valid_range_checks_order_and_format() {
        assert!(valid_range("2026-01-01", "2026-12-31"));
        assert!(valid_range("2026-09-07", "2026-09-07")); // 单日相等合法
        assert!(!valid_range("2026-12-31", "2026-01-01")); // 逆序
        assert!(!valid_range("2026-1-1", "2026-12-31")); // 非规范
        assert!(!valid_range("2026-01-01", "垃圾"));
    }

    #[test]
    fn now_local_rfc3339_shape() {
        let s = now_local_rfc3339();
        // 秒级精度 + 本地偏移：形如 2026-09-27T22:00:00+08:00
        assert_eq!(s.len(), 25, "shape = {s}");
        let parsed = DateTime::parse_from_rfc3339(&s);
        assert!(parsed.is_ok(), "必须可被 RFC3339 解析: {s}");
        // 日期部分与 today_local 一致（跨天瞬间以外恒成立）
        assert_eq!(&s[..10], today_local());
    }
}
