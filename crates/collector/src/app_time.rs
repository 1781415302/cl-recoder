//! 应用时长亚秒余数账本（correctness-v2 §4.4 合同）：按 `(本地日期, exe)` 累计前台
//! 时长的不足整秒零头，凑满整秒才产出 [`AppSecondsDelta`]——S4 接线后替代
//! aggregator 私有标量 `fg_residual_ms`。
//!
//! 相对旧毫秒残差法的两个修复（§4.4 逐字语义）：
//!
//! - **先拆日再计秒**：旧法把本段时长与残差先合成整秒、再用 `end-整秒` 倒推墙钟
//!   拆日，跨午夜时整段整秒会记到错误的日期；本模块先把真实归账区间按本地午夜
//!   切成精确 [`Duration`] 段，再分别加入对应 `(day, exe)` 余数；
//! - **亚毫秒不丢**：旧法以毫秒为最小单位、逐段丢亚毫秒零头；本模块全程纳秒
//!   精度（chrono `TimeDelta` 与 `std::time::Duration` 同为纳秒表示，无舍入）。
//!
//! 纯计算模块（PLAN §4.4 约束行）：只用 std/chrono/clrecoder_core::day，无 IO、
//! 不读时钟——归账区间由调用方以 naive 本地时间端点传入，可完全确定性单测
//! （禁止 sleep 逼近边界）。暂停/恢复接续、flush 失败回并、`discard_before`
//! 调用时机等生命周期接线属 S4 的 engine_loop 职责，见各方法文档。

// S3 只交付能力本身：aggregator 到 S4 才调用，暂以模块级 allow 换取独立可编译；
// S4 接线时应移除本行。
#![allow(dead_code)]

use std::collections::HashMap;
use std::time::Duration;

use chrono::{NaiveDateTime, NaiveTime};
use clrecoder_core::day;

/// 一次归账产出的整秒增量：调用方原样并入 `app_daily (day, exe)` 的 secs 桶
/// （§4.6 持久整数秒含义不变）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppSecondsDelta {
    /// 本地日期 `YYYY-MM-DD`（core::day 规范形，字典序即日历序）。
    pub day: String,
    /// 前台进程 exe（`EXE_UNKNOWN` 兜底值同样合法）。
    pub exe: String,
    /// 本调用从该 (day, exe) 提取的整秒数（恒 > 0）。
    pub seconds: u64,
}

/// `(本地日期, exe)` → 亚秒余数账本。
///
/// 每项只保存 `<1s` 的余数，产出的整秒立即随 [`AppTimeLedger::account_interval`]
/// 返回、绝不滞留账内；与统计桶生命周期分离——flush 成功不清账本、flush 失败
/// 回并整秒桶时也不回滚/重复提取账内已提取的整数秒（§4.4）。
#[derive(Debug, Default)]
pub(crate) struct AppTimeLedger {
    /// key 为 `(day, exe)`；value 恒 `<1s`。
    remainders: HashMap<(String, String), Duration>,
}

impl AppTimeLedger {
    /// 新建空账本。
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 归账一段前台区间 `[start, end)`（naive 本地时间端点，右开）到 `exe` 名下，
    /// 返回本次从账内提取出的整秒增量。
    ///
    /// - `end <= start`（时钟回拨/零长区间）：返回空且不改任何余数——宁可不记，
    ///   绝不负值（§4.6 既有语义）；
    /// - 跨多天按日历日切分（休眠跨天自然切出多段）；返回值按日期升序、只含
    ///   `seconds > 0` 的项、同一调用同 `(day, exe)` 至多一条；
    /// - 暂停语义（§4.4）：暂停区间不调用本方法，暂停前已赚取的余数原样保留；
    ///   恢复后同日同 exe 的下一次归账自然接上旧余数，暂停秒数不会混入。
    #[must_use]
    pub(crate) fn account_interval(
        &mut self,
        exe: &str,
        start: NaiveDateTime,
        end: NaiveDateTime,
    ) -> Vec<AppSecondsDelta> {
        let mut out: Vec<AppSecondsDelta> = Vec::new();
        if end <= start {
            return out;
        }
        let mut seg_start = start;
        while seg_start < end {
            let seg_day = seg_start.date();
            // 次日零点为段界；无次日（理论不可达：end > start 保证日期可推进）即止
            let Some(next_day) = seg_day.succ_opt() else { break };
            let seg_end = next_day.and_time(NaiveTime::MIN).min(end);
            let seg_delta = seg_end - seg_start; // 恒 >0（循环不变量），纳秒精确无舍入
            let day_str = day::format_day(seg_day);
            // 段长受 chrono 日期范围约束，加到 <1s 余数上不可能溢出
            let acc = self
                .remainders
                .entry((day_str.clone(), exe.to_string()))
                .or_insert(Duration::ZERO);
            *acc += Duration::new(
                u64::try_from(seg_delta.num_seconds()).unwrap_or(0),
                u32::try_from(seg_delta.subsec_nanos()).unwrap_or(0),
            );
            // 整秒即时提取，账内只留 <1s 余数（§4.4"不能保留已产出的整秒"）
            let whole = acc.as_secs();
            *acc -= Duration::from_secs(whole);
            if whole > 0 {
                // 单 exe 区间按日切分后日期严格递增，正常不会撞上同日两条；
                // 防御式合并保证"同一调用同 day/exe 至多一条"（§4.4）
                match out.last_mut() {
                    Some(last) if last.day == day_str && last.exe == exe => last.seconds += whole,
                    _ => out.push(AppSecondsDelta {
                        day: day_str,
                        exe: exe.to_string(),
                        seconds: whole,
                    }),
                }
            }
            if seg_end >= end {
                break;
            }
            seg_start = seg_end;
        }
        out
    }

    /// 丢弃 `day` 之前（更早）日期的亚秒余数。只能在完整归账区间处理完后调用
    /// （S4 接线：日结束/跨天 tick 时）；余数恒 `<1s`，丢弃即该零头不入库
    /// （§4.4"日结束每 exe 不足 1 秒的尾差允许丢弃，不写 SQLite 新列"）。
    pub(crate) fn discard_before(&mut self, day: &str) {
        // day 为规范 YYYY-MM-DD，字符串序即日历序；>= 保留当日及以后
        self.remainders.retain(|(d, _), _| d.as_str() >= day);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeDelta};

    const EXE_A: &str = "a.exe";
    const EXE_B: &str = "b.exe";
    const D1: &str = "2026-09-27";
    const D2: &str = "2026-09-28";
    const D3: &str = "2026-09-29";

    /// 便捷构造 naive 本地时刻（秒级精度，与 engine_loop 测试同形）。
    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, s).unwrap()
    }

    /// 便捷构造纳秒级 naive 时刻（亚毫秒测试用）。
    fn at_nanos(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32, nano: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_nano_opt(h, mi, s, nano)
            .unwrap()
    }

    /// 按 exe 汇总一次调用返回的全部整秒。
    fn sum_exe(deltas: &[AppSecondsDelta], exe: &str) -> u64 {
        deltas.iter().filter(|d| d.exe == exe).map(|d| d.seconds).sum()
    }

    /// 读取某 (day, exe) 的当前余数（同模块测试直达私有字段）。
    fn remainder_of(ledger: &AppTimeLedger, day: &str, exe: &str) -> Duration {
        ledger
            .remainders
            .get(&(day.to_string(), exe.to_string()))
            .copied()
            .unwrap_or(Duration::ZERO)
    }

    /// §4.4 精确示例：A 400ms/B 600ms 交替 20 轮 → A=8 秒、B=12 秒，余数归零。
    #[test]
    fn correctness_v2_ab_alternating_20_rounds_settle_8_and_12_seconds() {
        let mut ledger = AppTimeLedger::new();
        let t0 = at(2026, 9, 28, 10, 0, 0);
        let (mut secs_a, mut secs_b) = (0u64, 0u64);
        for round in 0..20u32 {
            let t = t0 + TimeDelta::milliseconds(i64::from(round) * 1000);
            let deltas_a =
                ledger.account_interval(EXE_A, t, t + TimeDelta::milliseconds(400));
            let deltas_b = ledger.account_interval(
                EXE_B,
                t + TimeDelta::milliseconds(400),
                t + TimeDelta::seconds(1),
            );
            secs_a += sum_exe(&deltas_a, EXE_A);
            secs_b += sum_exe(&deltas_b, EXE_B);
            assert!(deltas_a.iter().chain(deltas_b.iter()).all(|d| d.day == D2));
        }
        assert_eq!(secs_a, 8, "20×400ms 必须凑出整 8 秒");
        assert_eq!(secs_b, 12, "20×600ms 必须凑出整 12 秒");
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::ZERO, "A 余数应精确耗尽");
        assert_eq!(remainder_of(&ledger, D2, EXE_B), Duration::ZERO, "B 余数应精确耗尽");
    }

    /// §4.4 精确示例：同 exe 两段 500ms → 第二段凑出 1 秒（残差跨调用接续）。
    #[test]
    fn correctness_v2_two_500ms_segments_of_same_exe_yield_one_second() {
        let mut ledger = AppTimeLedger::new();
        let t0 = at(2026, 9, 28, 9, 0, 0);
        let first = ledger.account_interval(EXE_A, t0, t0 + TimeDelta::milliseconds(500));
        assert!(first.is_empty(), "首段 0.5s 不足整秒，不得产出: {first:?}");
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::from_millis(500));
        let second = ledger.account_interval(
            EXE_A,
            t0 + TimeDelta::milliseconds(500),
            t0 + TimeDelta::seconds(1),
        );
        assert_eq!(
            second,
            vec![AppSecondsDelta {
                day: D2.to_string(),
                exe: EXE_A.to_string(),
                seconds: 1,
            }]
        );
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::ZERO);
    }

    /// §4.4 精确示例：前一天 23:59:59.600 → 次日 00:00:00.400，两日各留 400ms
    /// 余数——总零头 800ms 不得合成任一日的 1 秒（先按午夜拆余数）。
    #[test]
    fn correctness_v2_midnight_straddle_leaves_400ms_remainder_on_each_day() {
        let mut ledger = AppTimeLedger::new();
        let start = at_nanos(2026, 9, 27, 23, 59, 59, 600_000_000);
        let end = at_nanos(2026, 9, 28, 0, 0, 0, 400_000_000);
        let deltas = ledger.account_interval(EXE_A, start, end);
        assert!(deltas.is_empty(), "两日余数均不足 1 秒，不得产出: {deltas:?}");
        assert_eq!(remainder_of(&ledger, D1, EXE_A), Duration::from_millis(400));
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::from_millis(400));
        // 余数不跨日搬运：各补 600ms 各自凑出 1 秒，两日两条独立增量
        let t1 = at(2026, 9, 27, 23, 0, 0);
        let more_d1 = ledger.account_interval(EXE_A, t1, t1 + TimeDelta::milliseconds(600));
        let more_d2 = ledger.account_interval(EXE_A, end, end + TimeDelta::milliseconds(600));
        assert_eq!(
            more_d1,
            vec![AppSecondsDelta { day: D1.to_string(), exe: EXE_A.to_string(), seconds: 1 }]
        );
        assert_eq!(
            more_d2,
            vec![AppSecondsDelta { day: D2.to_string(), exe: EXE_A.to_string(), seconds: 1 }]
        );
    }

    /// §4.4 精确示例（暂停接续）：同日暂停前 A 400ms、暂停 100 秒、恢复 A 600ms
    /// → A 1 秒——暂停区间不归账，旧余数原样接上，暂停秒数不混入。
    #[test]
    fn correctness_v2_pause_gap_skipped_and_remainder_carries_over_same_day() {
        let mut ledger = AppTimeLedger::new();
        let before = at(2026, 9, 28, 10, 0, 0);
        let pre = ledger.account_interval(EXE_A, before, before + TimeDelta::milliseconds(400));
        assert!(pre.is_empty() && remainder_of(&ledger, D2, EXE_A) == Duration::from_millis(400));
        // 暂停 100 秒：期间不调用归账；恢复后游标重置，区间从恢复时刻起算
        let resume = before + TimeDelta::milliseconds(400) + TimeDelta::seconds(100);
        let post =
            ledger.account_interval(EXE_A, resume, resume + TimeDelta::milliseconds(600));
        assert_eq!(sum_exe(&post, EXE_A), 1, "400ms+600ms 接续凑出 1 秒，暂停 100s 不计入");
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::ZERO);
    }

    /// 同日累加：多次不足整秒的小段逐次入账，4×300ms → 1 秒，余 200ms 继续保留。
    #[test]
    fn correctness_v2_same_day_subsecond_segments_accumulate_fractionally() {
        let mut ledger = AppTimeLedger::new();
        let t0 = at(2026, 9, 28, 8, 0, 0);
        let mut produced = 0u64;
        for i in 0..4u32 {
            let t = t0 + TimeDelta::milliseconds(i64::from(i) * 300);
            let deltas =
                ledger.account_interval(EXE_A, t, t + TimeDelta::milliseconds(300));
            produced += sum_exe(&deltas, EXE_A);
        }
        assert_eq!(produced, 1, "4×300ms=1200ms 应凑出 1 秒");
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::from_millis(200), "余 200ms 留账");
    }

    /// 反向（时钟回拨）与零长区间：返回空且余数原样保留、不新增账目，绝不负值。
    #[test]
    fn correctness_v2_inverted_or_zero_interval_returns_empty_and_keeps_remainder() {
        let mut ledger = AppTimeLedger::new();
        let seed_t = at(2026, 9, 28, 8, 0, 0);
        let seed =
            ledger.account_interval(EXE_A, seed_t, seed_t + TimeDelta::milliseconds(400));
        assert!(seed.is_empty());
        let before = remainder_of(&ledger, D2, EXE_A);
        // 零长区间
        let t = at(2026, 9, 28, 9, 0, 0);
        assert!(ledger.account_interval(EXE_A, t, t).is_empty());
        // 反向区间
        assert!(ledger.account_interval(EXE_A, t + TimeDelta::seconds(1), t).is_empty());
        assert_eq!(remainder_of(&ledger, D2, EXE_A), before, "无效区间不得改动余数");
        assert_eq!(ledger.remainders.len(), 1, "无效区间不得新增账目");
    }

    /// 跨多天按日历日切分：休眠跨 2 天（2h+24h+6h）各日整秒独立产出，日期升序、
    /// 每 (day,exe) 一条；亚秒端点各归各日余数（先拆余数再产整秒）。
    #[test]
    fn correctness_v2_multi_day_span_splits_per_day_in_ascending_order() {
        let mut ledger = AppTimeLedger::new();
        let deltas = ledger.account_interval(
            EXE_A,
            at(2026, 9, 28, 22, 0, 0),
            at(2026, 9, 30, 6, 0, 0),
        );
        assert_eq!(
            deltas,
            vec![
                AppSecondsDelta {
                    day: D2.to_string(),
                    exe: EXE_A.to_string(),
                    seconds: 2 * 3600,
                },
                AppSecondsDelta {
                    day: D3.to_string(),
                    exe: EXE_A.to_string(),
                    seconds: 86_400,
                },
                AppSecondsDelta {
                    day: "2026-09-30".to_string(),
                    exe: EXE_A.to_string(),
                    seconds: 6 * 3600,
                },
            ],
            "按日切分、日期升序、每 (day,exe) 至多一条"
        );
        // 亚秒端点：两头的零头各留各日，不并入任何一日的整秒
        let mut ledger = AppTimeLedger::new();
        let start = at_nanos(2026, 9, 28, 23, 59, 59, 500_000_000);
        let end = at_nanos(2026, 9, 30, 0, 0, 0, 700_000_000);
        let deltas = ledger.account_interval(EXE_A, start, end);
        assert_eq!(
            deltas,
            vec![AppSecondsDelta {
                day: D3.to_string(),
                exe: EXE_A.to_string(),
                seconds: 86_400,
            }]
        );
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::from_millis(500));
        assert_eq!(remainder_of(&ledger, "2026-09-30", EXE_A), Duration::from_millis(700));
    }

    /// 亚毫秒精度不丢失：500ns 零头跨调用保留并精确凑整（§4.4"Duration 保留
    /// 亚毫秒精度"）。
    #[test]
    fn correctness_v2_nanosecond_remainder_survives_across_calls() {
        let mut ledger = AppTimeLedger::new();
        let t0 = at_nanos(2026, 9, 28, 0, 0, 0, 500);
        // 1s+500ns → 产 1 秒，余 500ns
        let first =
            ledger.account_interval(EXE_A, t0, t0 + TimeDelta::nanoseconds(1_000_000_500));
        assert_eq!(sum_exe(&first, EXE_A), 1);
        assert_eq!(
            remainder_of(&ledger, D2, EXE_A),
            Duration::from_nanos(500),
            "500ns 零头必须原样留账"
        );
        // 再 1s-500ns → 恰好凑整 1 秒，余数归零
        let t1 = t0 + TimeDelta::nanoseconds(1_000_000_500);
        let second =
            ledger.account_interval(EXE_A, t1, t1 + TimeDelta::nanoseconds(999_999_500));
        assert_eq!(sum_exe(&second, EXE_A), 1);
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::ZERO, "两次零头应精确抵消");
    }

    /// 禁止"每小段先丢毫秒"：2500 段 400µs 逐次归账必须凑出整 1 秒（逐段毫秒
    /// 截断的实现会全数丢成 0）。
    #[test]
    fn correctness_v2_submillisecond_segments_accumulate_to_one_second() {
        let mut ledger = AppTimeLedger::new();
        let t0 = at_nanos(2026, 9, 28, 12, 0, 0, 0);
        let step = TimeDelta::microseconds(400);
        let mut produced = 0u64;
        for i in 0..2500i32 {
            let t = t0 + step * i;
            let deltas = ledger.account_interval(EXE_A, t, t + step);
            produced += sum_exe(&deltas, EXE_A);
        }
        assert_eq!(produced, 1, "2500×400µs=1s 必须凑出整 1 秒");
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::ZERO);
    }

    /// 区间右开：恰好结束于午夜零点的时长全归前一日，次日不产生余数账目。
    #[test]
    fn correctness_v2_end_exactly_at_midnight_belongs_to_previous_day() {
        let mut ledger = AppTimeLedger::new();
        let deltas = ledger.account_interval(
            EXE_A,
            at(2026, 9, 27, 23, 59, 59),
            at(2026, 9, 28, 0, 0, 0),
        );
        assert_eq!(
            deltas,
            vec![AppSecondsDelta {
                day: D1.to_string(),
                exe: EXE_A.to_string(),
                seconds: 1,
            }]
        );
        assert!(
            !ledger.remainders.contains_key(&(D2.to_string(), EXE_A.to_string())),
            "次日零点时刻不属于次日"
        );
    }

    /// 过期日清理：discard_before 只丢更早日余数，当日及以后的保留、可继续凑整；
    /// 被丢的零头不搬移、不转嫁（§4.4 日结束尾差允许丢弃，不入 SQLite 新列）。
    #[test]
    fn correctness_v2_discard_before_drops_only_earlier_days_remainder() {
        let mut ledger = AppTimeLedger::new();
        let t1 = at(2026, 9, 27, 23, 0, 0);
        let seed1 = ledger.account_interval(EXE_A, t1, t1 + TimeDelta::milliseconds(300));
        let t2 = at(2026, 9, 28, 9, 0, 0);
        let seed2 = ledger.account_interval(EXE_A, t2, t2 + TimeDelta::milliseconds(700));
        let t3 = at(2026, 9, 29, 9, 0, 0);
        let seed3 = ledger.account_interval(EXE_B, t3, t3 + TimeDelta::milliseconds(900));
        assert!(seed1.is_empty() && seed2.is_empty() && seed3.is_empty(), "种子段均不足整秒: {seed1:?} {seed2:?} {seed3:?}");
        assert_eq!(remainder_of(&ledger, D1, EXE_A), Duration::from_millis(300));
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::from_millis(700));
        assert_eq!(remainder_of(&ledger, D3, EXE_B), Duration::from_millis(900));

        ledger.discard_before(D2);
        assert_eq!(remainder_of(&ledger, D1, EXE_A), Duration::ZERO, "更早日的余数应被清掉");
        assert!(
            !ledger.remainders.contains_key(&(D1.to_string(), EXE_A.to_string())),
            "清掉后不得残留空账目"
        );
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::from_millis(700), "当日余数保留");
        assert_eq!(remainder_of(&ledger, D3, EXE_B), Duration::from_millis(900), "以后日的余数保留");

        // 保留的余数仍可凑整（700+600=1300ms → 1 秒）；被丢的 300ms 不得再出现在任何账目里
        let more = ledger.account_interval(
            EXE_A,
            t2 + TimeDelta::milliseconds(700),
            t2 + TimeDelta::milliseconds(1300),
        );
        assert_eq!(
            more,
            vec![AppSecondsDelta { day: D2.to_string(), exe: EXE_A.to_string(), seconds: 1 }]
        );
        assert_eq!(
            remainder_of(&ledger, D2, EXE_A),
            Duration::from_millis(300),
            "凑整后余 300ms 仍在当日账上"
        );
        // 边界当日再次调用：幂等（只清更早日，当日余数不被自己清掉）
        ledger.discard_before(D2);
        assert_eq!(remainder_of(&ledger, D2, EXE_A), Duration::from_millis(300));
    }
}
