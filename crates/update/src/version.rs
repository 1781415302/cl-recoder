//! 语义化三段版本（PLAN §4.2）。
//!
//! 只接受恰好三段十进制数字，可选前导 `v`/`V`；比较按数值（`0.10.0 > 0.9.9`）。

use std::fmt;

/// 三段数值版本（major.minor.patch）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// 无 `v` 前缀的展示串，如 `"0.4.0"`（DTO / 资产名用）。
    #[must_use]
    pub fn to_string_no_v(self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 解析 `"0.4.0"` 或 `"v0.4.0"`；后缀 / 缺段 / 多段 / 空格 → [`None`]。
#[must_use]
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    // 中间空白仍拒绝（trim 只去掉两端）
    if s.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let parse_u64 = |p: &str| -> Option<u64> {
        if p.is_empty() {
            return None;
        }
        // 必须全是十进制数字（拒绝 "+build"、"-rc1"、前导加号等）
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    Some(Version {
        major: parse_u64(parts[0])?,
        minor: parse_u64(parts[1])?,
        patch: parse_u64(parts[2])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_plain_and_v_prefix() {
        assert_eq!(
            parse_version("0.4.0"),
            Some(Version {
                major: 0,
                minor: 4,
                patch: 0
            })
        );
        assert_eq!(
            parse_version("v0.4.0"),
            Some(Version {
                major: 0,
                minor: 4,
                patch: 0
            })
        );
        assert_eq!(
            parse_version("V1.2.3"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
    }

    #[test]
    fn parse_rejects_suffix_missing_extra_whitespace() {
        assert_eq!(parse_version("0.4.0-rc1"), None);
        assert_eq!(parse_version("0.4.0+build"), None);
        assert_eq!(parse_version("0.4"), None);
        assert_eq!(parse_version("0.4.0.1"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("  "), None);
        assert_eq!(parse_version("0. 4.0"), None);
        assert_eq!(parse_version("0..0"), None);
        assert_eq!(parse_version(".0.0"), None);
        assert_eq!(parse_version("0.0."), None);
        assert_eq!(parse_version("v"), None);
        assert_eq!(parse_version("latest"), None);
    }

    #[test]
    fn numeric_compare_and_equality() {
        let a = parse_version("0.10.0").unwrap();
        let b = parse_version("0.9.9").unwrap();
        assert!(a > b);
        assert!(b < a);
        assert_eq!(parse_version("0.4.0"), parse_version("v0.4.0"));
        assert_eq!(a, a);
        assert_ne!(a, b);
        assert!(parse_version("1.0.0").unwrap() > parse_version("0.99.99").unwrap());
    }

    #[test]
    fn display_has_no_v_prefix() {
        let v = parse_version("v0.5.0").unwrap();
        assert_eq!(v.to_string(), "0.5.0");
        assert_eq!(v.to_string_no_v(), "0.5.0");
    }
}
