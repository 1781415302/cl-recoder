//! 扳机行程停留 tracker（DEVPLAN-input-live-trigger §4.2）——每个（连接, side）一个实例。
//!
//! 职责边界与 [`crate::stick_motion::StickMotionTracker`] 同层：不依赖 windows、不做 IO；
//! 输入为归一化行程 `position = raw/255`（有限值越界 clamp），输出按本地日拆分的
//! [`TriggerDayDelta`] 稀疏增量。时间合法性与午夜拆分复用 [`crate::motion_interval`]。
//!
//! 算法合同（§4.2 非显然规则）：
//! 1) NaN/Inf reset 且无输出；有限越界 clamp。静止下 position≥2/255 进入活动，活动中
//!    position≤1/255 退出；原始 0/1 不积热，2 及以上轻按可记录。101 个百分比位置格。
//! 2) 首帧只建锚点；`[prev,current)` 用上一帧活动态及上一帧 bin 积分。释放帧结算前一
//!    间隔，随后松开不累计。不得只在值变化时采样。
//! 3) 时间断段规则见 motion_interval；当前合法值作新锚，下一帧再积分。
//! 4) 跨本地午夜按 UTC 端点比例分配 mono dt，总量守恒。

use clrecoder_core::motion::{
    MotionStamp, TriggerBinDelta, TriggerDayDelta, TriggerSide, TRIGGER_ENTER, TRIGGER_EXIT,
};

use crate::motion_interval::{accepted_interval, split_interval_by_local_day};

/// 扳机连接内纯状态（§4.2）：一个实例只服务一个（连接, side）组合。
#[derive(Debug, Clone)]
pub struct TriggerMotionTracker {
    /// 已收到首帧（首帧仅建立锚点，不积分）
    has_frame: bool,
    /// 上一完整帧时间戳（积分区间 [prev, current) 的左端点）
    prev_stamp: MotionStamp,
    /// 上一完整帧行程（已 clamp 到 [0,1]）
    prev_position: f64,
    /// 上一完整帧活动态（迟滞状态机，跨帧保持）
    prev_active: bool,
    /// 最近喂入的 side（混用两侧属调用方错误，按断段处理）
    last_side: Option<TriggerSide>,
}

impl Default for TriggerMotionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TriggerMotionTracker {
    /// 创建空 tracker：无锚点、静止态。
    #[must_use]
    pub fn new() -> Self {
        Self {
            has_frame: false,
            prev_stamp: MotionStamp {
                mono_us: 0,
                unix_us: 0,
            },
            prev_position: 0.0,
            prev_active: false,
            last_side: None,
        }
    }

    /// 喂入一次扳机采样，返回本次产生的按日稀疏增量（0..=2 条）。
    ///
    /// - `stamp`：捕获时刻（mono/unix 成对）；
    /// - `side`：左/右扳机；
    /// - `position`：行程 0..1（raw/255）；NaN/Inf 拒绝且 reset；有限越界 clamp。
    ///
    /// 断段不填补该间隔：reset 并将当前点设为新锚点。暂停/断连由调用方先 [`Self::reset`]。
    #[must_use]
    pub fn feed(
        &mut self,
        stamp: MotionStamp,
        side: TriggerSide,
        position: f64,
    ) -> Vec<TriggerDayDelta> {
        // NaN/Inf 拒绝且 reset（§4.2）：脏点不得进入积分或锚点。
        if !position.is_finite() {
            self.reset();
            return Vec::new();
        }
        // 合同用法是每连接×side 独立 tracker；混喂两侧按断段处理。
        if self.last_side.is_some_and(|s| s != side) {
            self.reset();
        }
        let position = position.clamp(0.0, 1.0);
        // 活动迟滞（§4.2）：进入 ≥2/255、退出 ≤1/255；(EXIT, ENTER) 保持原状态。
        let active = if self.prev_active {
            position > TRIGGER_EXIT
        } else {
            position >= TRIGGER_ENTER
        };

        // 首帧仅建立锚点：不积分。
        if !self.has_frame {
            self.establish(stamp, side, position, active);
            return Vec::new();
        }

        let Some(interval) = accepted_interval(self.prev_stamp, stamp) else {
            self.reset();
            self.establish(stamp, side, position, active);
            return Vec::new();
        };

        // 时间积分：[prev, current) 用上一完整帧的活动态与 bin 累计实际 dt（µs）。
        let mut deltas = Vec::new();
        if self.prev_active {
            let bin = trigger_bin(self.prev_position);
            deltas = split_interval_by_local_day(self.prev_stamp, stamp, interval)
                .into_iter()
                .map(|(day, us)| TriggerDayDelta {
                    day,
                    side,
                    bins: vec![TriggerBinDelta {
                        bin,
                        dwell_us: us,
                    }],
                })
                .collect();
        }

        self.prev_stamp = stamp;
        self.prev_position = position;
        self.prev_active = active;
        self.last_side = Some(side);
        deltas
    }

    /// 清空全部连接内状态（暂停 epoch 变化/断连重建时由调用方触发）：
    /// 下一帧按首帧规则重建锚点，绝不跨 reset 补积分。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 首帧/断段后重建：只确立时间戳、行程与活动态，不积分。
    fn establish(&mut self, stamp: MotionStamp, side: TriggerSide, position: f64, active: bool) {
        self.has_frame = true;
        self.last_side = Some(side);
        self.prev_stamp = stamp;
        self.prev_position = position;
        self.prev_active = active;
    }
}

/// 扳机行程 → 热力格号（§4.2）：有限值 clamp 后 `round(position×100)`，范围 0..=100。
///
/// 非有限输入不在本函数调用合同内（由 [`TriggerMotionTracker::feed`] 先行拒绝）。
#[must_use]
pub fn trigger_bin(position: f64) -> u8 {
    let p = position.clamp(0.0, 1.0);
    (p * 100.0).round().clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use clrecoder_core::day::format_day;
    use clrecoder_core::motion::{local_day_from_unix_us, local_midnight_unix_us, TRIGGER_BIN_COUNT};

    use super::*;

    /// 基准 UTC µs：2026-06-15T00:00:00Z。
    const BASE_UNIX_US: i64 = 1_780_272_000_000_000;

    fn stamp_at(mono_us: u64, unix_us: i64) -> MotionStamp {
        MotionStamp { mono_us, unix_us }
    }

    /// raw byte → 行程（与 XInput 合同一致）。
    fn raw(v: u8) -> f64 {
        f64::from(v) / 255.0
    }

    fn sum_dwell(deltas: &[TriggerDayDelta]) -> u64 {
        deltas
            .iter()
            .flat_map(|d| d.bins.iter())
            .map(|b| b.dwell_us)
            .sum()
    }

    fn dwell_by_bin(deltas: &[TriggerDayDelta]) -> BTreeMap<u8, u64> {
        let mut m: BTreeMap<u8, u64> = BTreeMap::new();
        for d in deltas {
            for b in &d.bins {
                *m.entry(b.bin).or_default() += b.dwell_us;
            }
        }
        m
    }

    fn feed_hold(
        tracker: &mut TriggerMotionTracker,
        side: TriggerSide,
        position: f64,
        mono0: u64,
        unix0: i64,
        dt_us: u64,
        n: usize,
    ) -> Vec<TriggerDayDelta> {
        let mut out = Vec::new();
        for i in 0..n {
            let stamp = MotionStamp {
                mono_us: mono0 + (i as u64) * dt_us,
                unix_us: unix0 + (i as u64 * dt_us) as i64,
            };
            out.extend(tracker.feed(stamp, side, position));
        }
        out
    }

    fn clean_midnight_unix_us() -> i64 {
        let mut day = local_day_from_unix_us(BASE_UNIX_US).expect("基准时刻应可换算本地日期");
        for _ in 0..400 {
            let mid = local_midnight_unix_us(day, 0);
            if local_day_from_unix_us(mid) == Some(day)
                && local_day_from_unix_us(mid - 1) == day.pred_opt()
            {
                return mid;
            }
            day = day.succ_opt().expect("日历扫描不应越界");
        }
        panic!("400 天内应能找到干净的本地午夜");
    }

    // ------------------------------------------------------------------
    // §4.2 示例（原样）：raw64 保持→释放 → bin25 = 40000µs
    // ------------------------------------------------------------------

    /// raw64 在 t=0、20ms 保持，40ms 释放到 0，60ms 仍 0；仅 bin25 累计 40000µs。
    #[test]
    fn trigger_example_raw64_hold_release_bin25_40000us() {
        let mut t = TriggerMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(64));
        all.extend(t.feed(
            stamp_at(20_000, BASE_UNIX_US + 20_000),
            TriggerSide::Left,
            raw(64),
        ));
        all.extend(t.feed(
            stamp_at(40_000, BASE_UNIX_US + 40_000),
            TriggerSide::Left,
            raw(0),
        ));
        all.extend(t.feed(
            stamp_at(60_000, BASE_UNIX_US + 60_000),
            TriggerSide::Left,
            raw(0),
        ));
        assert_eq!(trigger_bin(raw(64)), 25);
        let dwell = dwell_by_bin(&all);
        assert_eq!(dwell.len(), 1);
        assert_eq!(dwell.get(&25), Some(&40_000), "仅 bin25 累计 40000µs：{dwell:?}");
        assert_eq!(sum_dwell(&all), 40_000);
    }

    /// raw255 保持 1 秒只增加 bin100。
    #[test]
    fn trigger_full_pull_one_second_only_bin100() {
        let mut t = TriggerMotionTracker::new();
        assert!(t
            .feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Right, raw(255))
            .is_empty());
        let all = feed_hold(
            &mut t,
            TriggerSide::Right,
            raw(255),
            20_000,
            BASE_UNIX_US + 20_000,
            20_000,
            50,
        );
        assert_eq!(trigger_bin(raw(255)), 100);
        let dwell = dwell_by_bin(&all);
        assert_eq!(dwell.len(), 1);
        assert_eq!(dwell.get(&100), Some(&1_000_000));
        assert_eq!(sum_dwell(&all), 1_000_000);
    }

    // ------------------------------------------------------------------
    // 轻按 / 原始 0/1 / 迟滞 / 分箱
    // ------------------------------------------------------------------

    /// 原始 0/1 持续多帧无热度；raw2 轻按可记录。
    #[test]
    fn trigger_raw0_raw1_no_heat_raw2_records() {
        let mut t = TriggerMotionTracker::new();
        let zero = feed_hold(
            &mut t,
            TriggerSide::Left,
            raw(0),
            0,
            BASE_UNIX_US,
            20_000,
            100,
        );
        assert!(zero.is_empty(), "raw0 不得积热");
        t.reset();
        let one = feed_hold(
            &mut t,
            TriggerSide::Left,
            raw(1),
            0,
            BASE_UNIX_US,
            20_000,
            100,
        );
        assert!(one.is_empty(), "raw1 不得积热");
        t.reset();
        assert!(t
            .feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(2))
            .is_empty());
        let light = feed_hold(
            &mut t,
            TriggerSide::Left,
            raw(2),
            20_000,
            BASE_UNIX_US + 20_000,
            20_000,
            50,
        );
        assert_eq!(sum_dwell(&light), 1_000_000);
        assert_eq!(dwell_by_bin(&light).get(&trigger_bin(raw(2))), Some(&1_000_000));
    }

    /// 迟滞：≥ENTER 进入，≤EXIT 退出；(EXIT, ENTER) 保持。
    #[test]
    fn trigger_hysteresis_enter_exit_band() {
        let mut t = TriggerMotionTracker::new();
        // raw1 不进入
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(1));
        all.extend(t.feed(
            stamp_at(20_000, BASE_UNIX_US + 20_000),
            TriggerSide::Left,
            raw(1),
        ));
        assert!(all.is_empty());
        // raw2 进入 → 保持 raw2 一帧 → 回到 raw1（仍 >EXIT? 1/255 == EXIT，退出条件是 <=EXIT）
        // EXIT = 1/255，活动时 position <= EXIT 退出，所以 raw1 退出。
        // 带内：raw 值在 (1, 2) 之间——浮点只有 raw 整数，所以用略高于 EXIT 的值。
        let band = TRIGGER_EXIT + (TRIGGER_ENTER - TRIGGER_EXIT) / 2.0;
        all.extend(t.feed(
            stamp_at(40_000, BASE_UNIX_US + 40_000),
            TriggerSide::Left,
            raw(2),
        ));
        all.extend(t.feed(
            stamp_at(60_000, BASE_UNIX_US + 60_000),
            TriggerSide::Left,
            band,
        ));
        all.extend(t.feed(
            stamp_at(80_000, BASE_UNIX_US + 80_000),
            TriggerSide::Left,
            band,
        ));
        // 带内保持活动：两段 20ms 来自 raw2 与 band
        assert_eq!(sum_dwell(&all), 40_000);
        // raw1（=EXIT）退出
        all.extend(t.feed(
            stamp_at(100_000, BASE_UNIX_US + 100_000),
            TriggerSide::Left,
            raw(1),
        ));
        // 释放帧结算前一间隔（band 活动）
        assert_eq!(sum_dwell(&all), 60_000);
        all.extend(t.feed(
            stamp_at(120_000, BASE_UNIX_US + 120_000),
            TriggerSide::Left,
            raw(1),
        ));
        assert_eq!(sum_dwell(&all), 60_000, "退出后不再累计");
    }

    /// 101 格：端点与 round 语义。
    #[test]
    fn trigger_bin_101_positions_round_clamp() {
        assert_eq!(TRIGGER_BIN_COUNT, 101);
        assert_eq!(trigger_bin(0.0), 0);
        assert_eq!(trigger_bin(1.0), 100);
        assert_eq!(trigger_bin(raw(64)), 25);
        assert_eq!(trigger_bin(raw(255)), 100);
        assert_eq!(trigger_bin(raw(0)), 0);
        assert_eq!(trigger_bin(raw(1)), 0);
        assert_eq!(trigger_bin(raw(2)), 1);
        assert_eq!(trigger_bin(1.5), 100, "越界 clamp");
        assert_eq!(trigger_bin(-0.5), 0);
        // 0.5% 边界 round half-away-from-zero? Rust round 是 half away from 0
        assert_eq!(trigger_bin(0.005), 1); // 0.5 → round to 1
        assert_eq!(trigger_bin(0.004), 0);
    }

    // ------------------------------------------------------------------
    // 跨日 / 断段 / NaN / 250ms 边界
    // ------------------------------------------------------------------

    /// 跨本地午夜：总量守恒、同 bin。
    #[test]
    fn trigger_midnight_split_conserves_total() {
        let mid = clean_midnight_unix_us();
        let mut t = TriggerMotionTracker::new();
        assert!(t
            .feed(stamp_at(0, mid - 5_000), TriggerSide::Left, raw(64))
            .is_empty());
        let out = t.feed(
            stamp_at(20_000, mid + 15_000),
            TriggerSide::Left,
            raw(64),
        );
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].day, format_day(local_day_from_unix_us(mid - 5_000).unwrap()));
        assert_eq!(out[1].day, format_day(local_day_from_unix_us(mid + 15_000).unwrap()));
        assert_eq!(out[0].bins[0].dwell_us, 5_000);
        assert_eq!(out[1].bins[0].dwell_us, 15_000);
        assert_eq!(sum_dwell(&out), 20_000);
        assert_eq!(out[0].bins[0].bin, 25);
        assert_eq!(out[1].bins[0].bin, 25);
    }

    /// dt=250ms 边界可接受；dt=250001 断段；休眠不补热度。
    #[test]
    fn trigger_250ms_boundary_and_sleep_gap() {
        let mut t = TriggerMotionTracker::new();
        assert!(t
            .feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Right, raw(128))
            .is_empty());
        let edge = t.feed(
            stamp_at(250_000, BASE_UNIX_US + 250_000),
            TriggerSide::Right,
            raw(128),
        );
        assert_eq!(sum_dwell(&edge), 250_000, "恰好 250ms 应积分");

        let mut t2 = TriggerMotionTracker::new();
        assert!(t2
            .feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Right, raw(128))
            .is_empty());
        let over = t2.feed(
            stamp_at(250_001, BASE_UNIX_US + 250_001),
            TriggerSide::Right,
            raw(128),
        );
        assert!(over.is_empty(), "250001µs 断段");
        let after = t2.feed(
            stamp_at(270_001, BASE_UNIX_US + 270_001),
            TriggerSide::Right,
            raw(128),
        );
        assert_eq!(sum_dwell(&after), 20_000, "重锚后恢复");

        let mut t3 = TriggerMotionTracker::new();
        let mut all = t3.feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(64));
        all.extend(t3.feed(
            stamp_at(20_000, BASE_UNIX_US + 20_000),
            TriggerSide::Left,
            raw(64),
        ));
        let wake = 20_000 + 3_600_000_000;
        assert!(t3
            .feed(
                stamp_at(wake, BASE_UNIX_US + wake as i64),
                TriggerSide::Left,
                raw(64),
            )
            .is_empty());
        all.extend(t3.feed(
            stamp_at(wake + 20_000, BASE_UNIX_US + (wake + 20_000) as i64),
            TriggerSide::Left,
            raw(64),
        ));
        assert_eq!(sum_dwell(&all), 40_000, "休眠不得补整夜热度");
    }

    /// UTC 倒退 / UTC·mono 偏差 / mono 不递增 → 断段不补。
    #[test]
    fn trigger_clock_anomalies_break_without_fill() {
        let mut t = TriggerMotionTracker::new();
        assert!(t
            .feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(64))
            .is_empty());
        assert!(t
            .feed(
                stamp_at(20_000, BASE_UNIX_US - 1_000_000),
                TriggerSide::Left,
                raw(64),
            )
            .is_empty());
        let after = t.feed(
            stamp_at(40_000, BASE_UNIX_US - 1_000_000 + 20_000),
            TriggerSide::Left,
            raw(64),
        );
        assert_eq!(sum_dwell(&after), 20_000);

        let mut t2 = TriggerMotionTracker::new();
        assert!(t2
            .feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(64))
            .is_empty());
        assert!(t2
            .feed(
                stamp_at(20_000, BASE_UNIX_US + 320_000),
                TriggerSide::Left,
                raw(64),
            )
            .is_empty());
    }

    /// NaN/Inf reset 且无输出；恢复后不跨脏点补热。
    #[test]
    fn trigger_nan_inf_reset_no_output() {
        let mut t = TriggerMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(64));
        all.extend(t.feed(
            stamp_at(20_000, BASE_UNIX_US + 20_000),
            TriggerSide::Left,
            raw(64),
        ));
        assert_eq!(sum_dwell(&all), 20_000);
        assert!(t
            .feed(
                stamp_at(40_000, BASE_UNIX_US + 40_000),
                TriggerSide::Left,
                f64::NAN,
            )
            .is_empty());
        assert!(t
            .feed(
                stamp_at(60_000, BASE_UNIX_US + 60_000),
                TriggerSide::Left,
                raw(255),
            )
            .is_empty());
        all.extend(t.feed(
            stamp_at(80_000, BASE_UNIX_US + 80_000),
            TriggerSide::Left,
            raw(255),
        ));
        assert_eq!(sum_dwell(&all), 40_000);
        assert_eq!(dwell_by_bin(&all).get(&100), Some(&20_000));
        assert!(t
            .feed(
                stamp_at(100_000, BASE_UNIX_US + 100_000),
                TriggerSide::Left,
                f64::INFINITY,
            )
            .is_empty());
    }

    /// 左右、多连接分别积分；side 标签正确。
    #[test]
    fn trigger_left_right_connection_isolation() {
        let mut left = TriggerMotionTracker::new();
        let mut right = TriggerMotionTracker::new();
        let a = feed_hold(
            &mut left,
            TriggerSide::Left,
            raw(64),
            0,
            BASE_UNIX_US,
            20_000,
            51,
        );
        let b = feed_hold(
            &mut right,
            TriggerSide::Right,
            raw(255),
            0,
            BASE_UNIX_US,
            20_000,
            51,
        );
        assert!(a.iter().all(|d| d.side == TriggerSide::Left));
        assert!(b.iter().all(|d| d.side == TriggerSide::Right));
        assert_eq!(dwell_by_bin(&a).get(&25), Some(&1_000_000));
        assert_eq!(dwell_by_bin(&b).get(&100), Some(&1_000_000));
        assert_eq!(dwell_by_bin(&a).get(&100), None);
    }

    /// reset 不补尾；换 side 按断段重锚。
    #[test]
    fn trigger_reset_and_side_switch_reanchor() {
        let mut t = TriggerMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(64));
        all.extend(t.feed(
            stamp_at(20_000, BASE_UNIX_US + 20_000),
            TriggerSide::Left,
            raw(64),
        ));
        t.reset();
        assert!(t
            .feed(
                stamp_at(40_000, BASE_UNIX_US + 40_000),
                TriggerSide::Left,
                raw(64),
            )
            .is_empty());
        all.extend(t.feed(
            stamp_at(60_000, BASE_UNIX_US + 60_000),
            TriggerSide::Left,
            raw(64),
        ));
        assert_eq!(sum_dwell(&all), 40_000, "reset 间隙不补");

        let mut t2 = TriggerMotionTracker::new();
        assert!(t2
            .feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, raw(64))
            .is_empty());
        let a = t2.feed(
            stamp_at(20_000, BASE_UNIX_US + 20_000),
            TriggerSide::Left,
            raw(64),
        );
        assert_eq!(sum_dwell(&a), 20_000);
        assert!(t2
            .feed(
                stamp_at(40_000, BASE_UNIX_US + 40_000),
                TriggerSide::Right,
                raw(255),
            )
            .is_empty());
        let c = t2.feed(
            stamp_at(60_000, BASE_UNIX_US + 60_000),
            TriggerSide::Right,
            raw(255),
        );
        assert_eq!(c[0].side, TriggerSide::Right);
        assert_eq!(sum_dwell(&c), 20_000);
    }

    /// 有限越界 clamp：>1 按满行程积分，<0 视同松开路径经迟滞处理。
    #[test]
    fn trigger_finite_out_of_range_clamped() {
        let mut t = TriggerMotionTracker::new();
        assert!(t
            .feed(stamp_at(0, BASE_UNIX_US), TriggerSide::Left, 1.5)
            .is_empty());
        let all = t.feed(
            stamp_at(20_000, BASE_UNIX_US + 20_000),
            TriggerSide::Left,
            1.5,
        );
        assert_eq!(dwell_by_bin(&all).get(&100), Some(&20_000));
    }
}
