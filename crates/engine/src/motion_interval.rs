//! 共享运动时间校验与本地午夜拆分（DEVPLAN-input-live-trigger §4.2 / E1）。
//!
//! 从摇杆 tracker 抽出的纯时间/日历逻辑：单调与 UTC 合法性、断段阈值、跨本地午夜
//! 按 UTC 端点比例分配单调 dt。[`MotionInterval`] 仅 crate 内部使用，不跨 IPC。
//! 摇杆半径/活动迟滞/路径去噪与扳机行程积分各自保留在 stick/trigger 模块。

use clrecoder_core::day::format_day;
use clrecoder_core::motion::{local_day_from_unix_us, local_midnight_unix_us, MotionStamp, NaiveDate};

/// 断段阈值：单调 dt 或 UTC/单调偏差超过 250 ms 视为断段（休眠/时钟跳变），不填补。
const MAX_INTERVAL_US: i128 = 250_000;

/// 已通过时间合法性检查的积分区间（engine 内部；不跨 crate/IPC）。
///
/// `mono_us` 为 `[prev, current)` 的单调微秒；`prev_day` / `current_day` 为两端本地日。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MotionInterval {
    /// 单调间隔微秒（已保证 >0 且 ≤250000）
    pub mono_us: u64,
    /// 区间左端本地日
    pub prev_day: NaiveDate,
    /// 区间右端本地日
    pub current_day: NaiveDate,
}

/// 校验 `[prev, current)` 时间合法性并换算两端本地日（§4.2）。
///
/// 断段（返回 `None`）：mono dt≤0、dt>250000µs、UTC 不递增、UTC/mono 差值偏差>250000µs、
/// 任一日不可换算。调用方将当前合法采样设为新锚，下一帧再积分。
#[must_use]
pub(crate) fn accepted_interval(prev: MotionStamp, current: MotionStamp) -> Option<MotionInterval> {
    let dt = i128::from(current.mono_us) - i128::from(prev.mono_us);
    let d_utc = i128::from(current.unix_us) - i128::from(prev.unix_us);
    if current.unix_us <= prev.unix_us
        || dt <= 0
        || dt > MAX_INTERVAL_US
        || (d_utc - dt).abs() > MAX_INTERVAL_US
    {
        return None;
    }
    let prev_day = local_day_from_unix_us(prev.unix_us)?;
    let current_day = local_day_from_unix_us(current.unix_us)?;
    Some(MotionInterval {
        mono_us: dt as u64,
        prev_day,
        current_day,
    })
}

/// 将已接受区间按本地午夜拆成正微秒日片段（§4.2）。
///
/// 同一日一段；跨本地午夜以 UTC 两端确定切点比例分配 `interval.mono_us`，后一段取余
/// 保证总量守恒；零微秒片段不输出。不硬编码一天 24 小时。
#[must_use]
pub(crate) fn split_interval_by_local_day(
    prev: MotionStamp,
    current: MotionStamp,
    interval: MotionInterval,
) -> Vec<(String, u64)> {
    let dwell_us = interval.mono_us;
    if interval.prev_day == interval.current_day {
        return vec![(format_day(interval.prev_day), dwell_us)];
    }
    let midnight = local_midnight_unix_us(interval.current_day, prev.unix_us)
        .clamp(prev.unix_us, current.unix_us);
    let span = (i128::from(current.unix_us) - i128::from(prev.unix_us)).max(1);
    let first =
        ((i128::from(midnight) - i128::from(prev.unix_us)) * i128::from(dwell_us) / span) as u64;
    let second = dwell_us - first;
    let mut out = Vec::with_capacity(2);
    if first > 0 {
        out.push((format_day(interval.prev_day), first));
    }
    if second > 0 {
        out.push((format_day(interval.current_day), second));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::motion::local_day_from_unix_us;

    /// 基准 UTC µs：2026-06-15T00:00:00Z。
    const BASE_UNIX_US: i64 = 1_780_272_000_000_000;

    fn stamp(mono_us: u64, unix_us: i64) -> MotionStamp {
        MotionStamp { mono_us, unix_us }
    }

    #[test]
    fn accepted_interval_rejects_non_positive_and_oversized_dt() {
        let p = stamp(0, BASE_UNIX_US);
        assert!(accepted_interval(p, stamp(0, BASE_UNIX_US + 1)).is_none());
        assert!(accepted_interval(p, stamp(250_001, BASE_UNIX_US + 250_001)).is_none());
        let ok = accepted_interval(p, stamp(250_000, BASE_UNIX_US + 250_000)).unwrap();
        assert_eq!(ok.mono_us, 250_000);
    }

    #[test]
    fn accepted_interval_rejects_utc_skew_and_non_increasing_utc() {
        let p = stamp(0, BASE_UNIX_US);
        assert!(accepted_interval(p, stamp(20_000, BASE_UNIX_US + 320_000)).is_none());
        assert!(accepted_interval(p, stamp(20_000, BASE_UNIX_US - 1)).is_none());
        assert!(accepted_interval(p, stamp(20_000, BASE_UNIX_US)).is_none());
    }

    #[test]
    fn split_same_day_is_single_piece() {
        let p = stamp(0, BASE_UNIX_US);
        let c = stamp(20_000, BASE_UNIX_US + 20_000);
        let iv = accepted_interval(p, c).unwrap();
        let pieces = split_interval_by_local_day(p, c, iv);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].1, 20_000);
        assert_eq!(pieces[0].0, format_day(iv.prev_day));
    }

    #[test]
    fn split_midnight_conserves_total() {
        let mut day = local_day_from_unix_us(BASE_UNIX_US).unwrap();
        let mid = (0..400)
            .find_map(|_| {
                let m = local_midnight_unix_us(day, 0);
                if local_day_from_unix_us(m) == Some(day)
                    && local_day_from_unix_us(m - 1) == day.pred_opt()
                {
                    Some(m)
                } else {
                    day = day.succ_opt()?;
                    None
                }
            })
            .expect("干净午夜");
        let p = stamp(0, mid - 5_000);
        let c = stamp(20_000, mid + 15_000);
        let iv = accepted_interval(p, c).unwrap();
        let pieces = split_interval_by_local_day(p, c, iv);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].1, 5_000);
        assert_eq!(pieces[1].1, 15_000);
        assert_eq!(pieces.iter().map(|(_, u)| u).sum::<u64>(), 20_000);
    }
}
