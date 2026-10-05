//! 摇杆运动 tracker（motion-dpi §4.2）——每个（连接, side）一个实例的**纯函数**状态机。
//!
//! 职责边界与 [`crate::Engine`] 同层：不依赖 windows crate、不做任何 IO、不持有设备；
//! 输入为 MotionRuntime 捕获的摇杆采样（x 向右、y 向上，范围 [-1,1]），输出按本地日
//! 拆分的 [`StickDayDelta`] 增量流。调用方（S5 aggregator）为每个连接×side 各建一个
//! tracker，暂停 epoch 变化/断连时调用 [`StickMotionTracker::reset`]。
//!
//! 输出形态是**每帧回吐增量**而非段末结算：时间积分采用前一完整帧的位置/活动态对
//! `[prev, current)` 累计实际 dt（§4.2），保持同一点无变化事件仍累计——保持帧由此
//! 持续产生热度（§4.4：只有保持帧产生的热度也必须落库），reset 也就无需补排尾。
//!
//! 算法合同（§4.2，阈值常量不得自行换算）：
//! 1) 径向投影：半径 >1 投影到单位圆，不逐轴硬压对角；NaN/Inf 拒绝且 reset。
//! 2) 活动迟滞：静止下半径 ≥0.20 进入活动；活动中半径 ≤0.15 退出；非活动点的行程
//!    有效位置为 (0,0)，只归档活动时间（neutral 不占热力总量），不另做按键计数。
//! 3) 路径去噪：与最近**接受的行程锚点**距离 ≥0.01 R 才接受新锚点并累计欧氏距离；
//!    不与上一原始噪声点累积；退出死区返回 (0,0) 时结算一次返回段；固定点重复
//!    万帧不增行程。
//! 4) 时间积分：微秒归档（无每帧整数秒截断），普通采样约 20ms、用实际捕获 dt。
//! 5) 断段：单调 dt≤0、dt>250ms、UTC 差与单调差相差>250ms、UTC 不递增、日期标签
//!    不可得——不填补该间隔，reset 并将当前点设为新锚点（休眠/时钟跳变/断线不画
//!    长线、不填一夜热度）。
//! 6) 跨日：正常跨本地午夜以 UTC 两端确定午夜切点比例，按该比例分配单调 dt，最后
//!    一段取剩余微秒保证总和；路程增量归当前采样日；chrono 本地日历换算经
//!    [`clrecoder_core::motion`] 助手完成，不固定加 24 小时。

use clrecoder_core::day::format_day;
use clrecoder_core::motion::{
    local_day_from_unix_us, local_midnight_unix_us, MotionStamp, NaiveDate, StickBinDelta,
    StickDayDelta, StickPoint, StickSide,
};

/// 热力图网格边长：25×25 = 625 格（§4.2 分箱 / §4.6 展示共用）。
pub const STICK_GRID_SIZE: usize = 25;

/// 活动进入阈值：静止状态下半径 ≥ 0.20 进入活动（§4.2）。
const STICK_ENTER_RADIUS: f64 = 0.20;
/// 活动退出阈值：活动中半径 ≤ 0.15 退出；(0.15, 0.20) 为迟滞带，保持原状态。
const STICK_EXIT_RADIUS: f64 = 0.15;
/// 行程锚点接受阈值：与最近**接受的锚点**距离 ≥ 0.01 R 才接受新锚点并累计路程。
const STICK_NOISE_RADIUS: f64 = 0.01;
/// 断段阈值：单调 dt 或 UTC/单调偏差超过 250 ms 视为断段（休眠/时钟跳变），不填补。
const MAX_INTERVAL_US: i128 = 250_000;

/// 非活动点的行程有效位置（§4.2：非活动点用于行程的有效位置为 (0,0)）。
const CENTER: StickPoint = StickPoint { x: 0.0, y: 0.0 };

/// 摇杆连接内纯状态（§4.2）：一个实例只服务一个（连接, side）组合。
///
/// 状态只有"上一完整帧 + 行程锚点"两块：时间积分永远用上一完整帧的位置/活动态，
/// 路程只在接受新锚点时累计——两者解耦保证噪声点不进入行程、保持帧持续计停留。
#[derive(Debug, Clone)]
pub struct StickMotionTracker {
    /// 已收到首帧（首帧仅建立锚点，不积分、不计路程）
    has_frame: bool,
    /// 上一完整帧时间戳（积分区间 [prev, current) 的左端点）
    prev_stamp: MotionStamp,
    /// 上一完整帧有效位置：活动=规范化实际点，非活动=(0,0)
    prev_eff: StickPoint,
    /// 上一完整帧活动态（迟滞状态机状态，跨帧保持）
    prev_active: bool,
    /// 最近接受的行程锚点（有效位置空间；仅在接受新锚点时前移）
    anchor: StickPoint,
    /// 最近喂入的 side（混用两侧属调用方错误，按断段处理防串线）
    last_side: Option<StickSide>,
}

impl Default for StickMotionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl StickMotionTracker {
    /// 创建空 tracker：无首帧、静止态、锚点在中心。
    #[must_use]
    pub fn new() -> Self {
        Self {
            has_frame: false,
            prev_stamp: MotionStamp { mono_us: 0, unix_us: 0 },
            prev_eff: CENTER,
            prev_active: false,
            anchor: CENTER,
            last_side: None,
        }
    }

    /// 喂入一次摇杆采样，返回本次产生的按日增量（0..=2 条；中性帧为空）。
    ///
    /// - `stamp`：捕获时刻（mono/unix 成对，两采集线程同一时钟）；
    /// - `point`：摇杆点（x 右、y 上，[-1,1]；半径 >1 自动径向投影）；
    /// - NaN/Inf 拒绝且 reset；首帧仅建立锚点（"接入时已推住"不从中心出发计路程）。
    ///
    /// 断段（dt≤0 / dt>250ms / UTC 差与单调差相差>250ms / UTC 不递增）不填补该间隔：
    /// reset 并将当前点设为新锚点。暂停 epoch 变化与断连由调用方先 [`Self::reset`]。
    #[must_use]
    pub fn feed(
        &mut self,
        stamp: MotionStamp,
        side: StickSide,
        point: StickPoint,
    ) -> Vec<StickDayDelta> {
        // NaN/Inf 拒绝且 reset（§4.2）：脏点不得进入积分或锚点。
        if !point.x.is_finite() || !point.y.is_finite() {
            self.reset();
            return Vec::new();
        }
        // 合同用法是每连接×side 独立 tracker；混喂两侧按断段处理，防止两侧坐标串线。
        if self.last_side.is_some_and(|s| s != side) {
            self.reset();
        }
        let norm = normalize_to_unit_circle(point);
        let radius = norm.x.hypot(norm.y);
        // 活动迟滞（§4.2）：进入 ≥0.20、退出 ≤0.15，(0.15, 0.20) 保持原状态。
        let active = if self.prev_active {
            radius > STICK_EXIT_RADIUS
        } else {
            radius >= STICK_ENTER_RADIUS
        };
        let eff = if active { norm } else { CENTER };

        // 首帧仅建立锚点：不把"接入时已推住"算成中心出发，也不积分。
        if !self.has_frame {
            self.establish(stamp, side, eff, active);
            return Vec::new();
        }

        // 时间异常 → 断段：不填补该间隔，reset 并将当前点设为新锚点。
        let dt = i128::from(stamp.mono_us) - i128::from(self.prev_stamp.mono_us);
        let d_utc = i128::from(stamp.unix_us) - i128::from(self.prev_stamp.unix_us);
        let stamp_broken = stamp.unix_us <= self.prev_stamp.unix_us
            || dt <= 0
            || dt > MAX_INTERVAL_US
            || (d_utc - dt).abs() > MAX_INTERVAL_US;
        // 日期标签不可得（超出 chrono 表示范围）同样视作时钟异常。
        let cur_day = match local_day_from_unix_us(stamp.unix_us) {
            Some(d) => d,
            None => {
                self.reset();
                self.establish(stamp, side, eff, active);
                return Vec::new();
            }
        };
        if stamp_broken {
            self.reset();
            self.establish(stamp, side, eff, active);
            return Vec::new();
        }

        // 时间积分：[prev, current) 用上一完整帧的位置/活动态累计实际 dt（µs）。
        let mut deltas: Vec<StickDayDelta> = Vec::new();
        if self.prev_active {
            let prev_day = match local_day_from_unix_us(self.prev_stamp.unix_us) {
                Some(d) => d,
                None => {
                    self.reset();
                    self.establish(stamp, side, eff, active);
                    return Vec::new();
                }
            };
            deltas = split_dwell_by_local_day(
                prev_day,
                cur_day,
                self.prev_stamp.unix_us,
                stamp.unix_us,
                dt as u64,
                self.prev_eff,
                side,
            );
        }

        // 路径去噪（§4.2）：与最近接受的锚点距离 ≥0.01 R 才接受新锚点并累计欧氏距离；
        // 不与上一原始噪声点累积。退出死区返回 (0,0) 时在此结算一次返回段。
        let dist = (eff.x - self.anchor.x).hypot(eff.y - self.anchor.y);
        if dist >= STICK_NOISE_RADIUS {
            // 路程增量归当前采样日：并入同日停留段，否则新建 travel-only 增量。
            let day = format_day(cur_day);
            let mut merged = false;
            if let Some(last) = deltas.last_mut() {
                if last.day == day {
                    last.travel_r += dist;
                    merged = true;
                }
            }
            if !merged {
                deltas.push(StickDayDelta {
                    day,
                    side,
                    active_us: 0,
                    travel_r: dist,
                    bins: Vec::new(),
                });
            }
            self.anchor = eff;
        }

        self.prev_stamp = stamp;
        self.prev_eff = eff;
        self.prev_active = active;
        deltas
    }

    /// 清空全部连接内状态（暂停 epoch 变化/断连重建时由调用方触发，§5）：
    /// 下一帧按首帧规则重建锚点，绝不跨 reset 连线或补积分。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 首帧/断段后重建：只确立时间戳、有效位置与锚点，不积分、不计路程。
    fn establish(&mut self, stamp: MotionStamp, side: StickSide, eff: StickPoint, active: bool) {
        self.has_frame = true;
        self.last_side = Some(side);
        self.prev_stamp = stamp;
        self.prev_eff = eff;
        self.prev_active = active;
        self.anchor = eff;
    }
}

/// 摇杆点 → 热力格号（§4.2）：column=floor((x+1)/2×25)、row=floor((1-y)/2×25)，
/// 边界 clamp 到 0..24，bin=row×25+column；0 在左上、y 正向上。
/// 非有限输入经饱和转换落入 0 格（feed 侧已先行拒绝 NaN/Inf）。
#[must_use]
pub fn stick_bin(point: StickPoint) -> u16 {
    let grid = STICK_GRID_SIZE as f64;
    let last = (STICK_GRID_SIZE - 1) as f64;
    let column = ((point.x + 1.0) / 2.0 * grid).floor().clamp(0.0, last) as u16;
    let row = ((1.0 - point.y) / 2.0 * grid).floor().clamp(0.0, last) as u16;
    row * STICK_GRID_SIZE as u16 + column
}

/// 径向投影：半径 >1 投影到单位圆（§4.2：不逐轴硬压对角）；≤1 原样保留。
fn normalize_to_unit_circle(p: StickPoint) -> StickPoint {
    let r = p.x.hypot(p.y);
    if r > 1.0 {
        StickPoint { x: p.x / r, y: p.y / r }
    } else {
        p
    }
}

/// 停留时长按本地午夜切点拆日（§4.2）：以 UTC 两端确定午夜切点比例，按该比例分配
/// 单调 dt，最后一段取剩余微秒保证总和；每段一个 bin（原位置格），满足
/// `active_us = Σbins.dwell_us`。不跨日时输出单段；切点被 clamp 进区间，
/// 任何 DST 歧义下总量守恒。
#[must_use]
fn split_dwell_by_local_day(
    prev_day: NaiveDate,
    cur_day: NaiveDate,
    prev_unix_us: i64,
    cur_unix_us: i64,
    dwell_us: u64,
    at: StickPoint,
    side: StickSide,
) -> Vec<StickDayDelta> {
    let piece = |day: String, us: u64| StickDayDelta {
        day,
        side,
        active_us: us,
        travel_r: 0.0,
        bins: vec![StickBinDelta { bin: stick_bin(at), dwell_us: us }],
    };
    if prev_day == cur_day {
        return vec![piece(format_day(prev_day), dwell_us)];
    }
    // 跨本地午夜：切点取 cur_day 的本地 00:00，clamp 进 [prev, cur] 后按比例分配。
    let midnight = local_midnight_unix_us(cur_day, prev_unix_us)
        .clamp(prev_unix_us, cur_unix_us);
    let span = (i128::from(cur_unix_us) - i128::from(prev_unix_us)).max(1);
    let first = ((i128::from(midnight) - i128::from(prev_unix_us)) * i128::from(dwell_us) / span)
        as u64;
    let second = dwell_us - first;
    let mut out = Vec::with_capacity(2);
    if first > 0 {
        out.push(piece(format_day(prev_day), first));
    }
    if second > 0 {
        out.push(piece(format_day(cur_day), second));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// 基准 UTC µs：2026-06-15T00:00:00Z（测试只作相对时间/日历扫描起点）。
    const BASE_UNIX_US: i64 = 1_780_272_000_000_000;

    fn stamp_at(mono_us: u64, unix_us: i64) -> MotionStamp {
        MotionStamp { mono_us, unix_us }
    }

    fn pt(x: f64, y: f64) -> StickPoint {
        StickPoint { x, y }
    }

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn sum_active(deltas: &[StickDayDelta]) -> u64 {
        deltas.iter().map(|d| d.active_us).sum()
    }

    fn sum_travel(deltas: &[StickDayDelta]) -> f64 {
        deltas.iter().map(|d| d.travel_r).sum()
    }

    /// 不变量锚定（§4.2）：每日 active_us = Σbins.dwell_us。
    fn assert_dwell_invariant(deltas: &[StickDayDelta]) {
        for d in deltas {
            let bins: u64 = d.bins.iter().map(|b| b.dwell_us).sum();
            assert_eq!(d.active_us, bins, "每日 active_us 必须等于 Σbins.dwell_us");
        }
    }

    /// 累计 (bin → dwell µs) 表。
    fn dwell_by_bin(deltas: &[StickDayDelta]) -> BTreeMap<u16, u64> {
        let mut m: BTreeMap<u16, u64> = BTreeMap::new();
        for d in deltas {
            for b in &d.bins {
                *m.entry(b.bin).or_default() += b.dwell_us;
            }
        }
        m
    }

    /// 以固定间隔连续喂入 `n` 帧同一坐标（mono 自 `mono0`、unix 自 `unix0` 起）。
    fn feed_hold(
        tracker: &mut StickMotionTracker,
        side: StickSide,
        point: StickPoint,
        mono0: u64,
        unix0: i64,
        dt_us: u64,
        n: usize,
    ) -> Vec<StickDayDelta> {
        let mut out = Vec::new();
        for i in 0..n {
            let stamp = MotionStamp {
                mono_us: mono0 + (i as u64) * dt_us,
                unix_us: unix0 + (i as u64 * dt_us) as i64,
            };
            out.extend(tracker.feed(stamp, side, point));
        }
        out
    }

    /// 取一个本地 00:00 边界干净的午夜（转换唯一、两侧日期相邻）：
    /// 从基准日起逐日向后扫描，避开本机时区可能的 DST 切换日。
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
    // §4.2 示例（原样）：中心往返 / 保持 / 两帧不补
    // ------------------------------------------------------------------

    /// 完整帧 t=0 中心；t=20ms 满幅右；每 20ms 采样同一点直到 t=1020ms；
    /// t=1040ms 回中心 → 路程 2R、右侧累计活动 1020ms、neutral 不入热力。
    #[test]
    fn motion_dpi_center_round_trip_two_r() {
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0));
        for i in 1..=51u64 {
            all.extend(t.feed(
                stamp_at(i * 20_000, BASE_UNIX_US + (i * 20_000) as i64),
                StickSide::Left,
                pt(1.0, 0.0),
            ));
        }
        all.extend(t.feed(
            stamp_at(52 * 20_000, BASE_UNIX_US + 1_040_000),
            StickSide::Left,
            pt(0.0, 0.0),
        ));
        assert_dwell_invariant(&all);
        assert!(approx(sum_travel(&all), 2.0, 1e-9), "中心往返应为 2R，实际 {}", sum_travel(&all));
        assert_eq!(sum_active(&all), 1_020_000, "右侧累计活动应为 1020ms");
        // 热力只含右侧格；neutral 时间不入热力总量
        let dwell = dwell_by_bin(&all);
        assert_eq!(dwell.len(), 1, "热力只应有右侧一格");
        assert_eq!(dwell.get(&324), Some(&1_020_000), "满幅右 = row12×col24 = 324");
        assert_eq!(dwell.get(&stick_bin(pt(0.0, 0.0))), None, "neutral 不占热力总量");
        // 首帧（接入帧）不产增量
        let mut t2 = StickMotionTracker::new();
        assert!(t2.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0)).is_empty());
    }

    /// 单位圆逐度采样整圈：路程近 2π（弦长和），活动时间逐帧累计。
    #[test]
    fn motion_dpi_full_circle_travel_near_two_pi() {
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Right, pt(1.0, 0.0));
        for i in 1..=360u64 {
            let angle = (i as f64).to_radians();
            all.extend(t.feed(
                stamp_at(i * 20_000, BASE_UNIX_US + (i * 20_000) as i64),
                StickSide::Right,
                pt(angle.cos(), angle.sin()),
            ));
        }
        assert_dwell_invariant(&all);
        let travel = sum_travel(&all);
        assert!(approx(travel, std::f64::consts::TAU, 1e-3), "整圆弦长和应近 2π，实际 {travel}");
        assert_eq!(sum_active(&all), 360 * 20_000, "360 个 20ms 区间全部活动");
    }

    /// 固定活动点重复一万帧：行程 0R（锚点不前移），停留逐帧累计。
    #[test]
    fn motion_dpi_fixed_point_ten_thousand_frames_zero_travel() {
        let mut t = StickMotionTracker::new();
        let all = feed_hold(&mut t, StickSide::Left, pt(0.7, 0.3), 0, BASE_UNIX_US, 20_000, 10_000);
        assert_dwell_invariant(&all);
        assert!(approx(sum_travel(&all), 0.0, 1e-12), "固定点一万帧不得增加行程");
        assert_eq!(sum_active(&all), 9_999 * 20_000);
        let dwell = dwell_by_bin(&all);
        assert_eq!(dwell.len(), 1);
        assert_eq!(dwell.get(&stick_bin(pt(0.7, 0.3))), Some(&(9_999 * 20_000)));
    }

    /// 从已有满幅点首次建立锚点、每 20ms 采样保持 1 秒：0 R、1 秒活动
    /// （"接入时已推住"不从中心出发）。
    #[test]
    fn motion_dpi_hold_from_deflection_one_second_zero_travel() {
        let mut t = StickMotionTracker::new();
        // 首帧即满幅：仅建立锚点，无增量
        assert!(t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(1.0, 0.0)).is_empty());
        let all = feed_hold(&mut t, StickSide::Left, pt(1.0, 0.0), 20_000, BASE_UNIX_US + 20_000, 20_000, 50);
        assert_dwell_invariant(&all);
        assert!(approx(sum_travel(&all), 0.0, 1e-12), "原地点保持不得产生路程");
        assert_eq!(sum_active(&all), 1_000_000, "保持 1 秒应累计 1 秒活动");
    }

    /// 仅给两帧且相隔 1 秒：dt>250ms 断段，不补停留时间，重新以当前点为锚点。
    #[test]
    fn motion_dpi_two_frames_one_second_apart_do_not_fill_dwell() {
        let mut t = StickMotionTracker::new();
        assert!(t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Right, pt(1.0, 0.0)).is_empty());
        let gap = t.feed(stamp_at(1_000_000, BASE_UNIX_US + 1_000_000), StickSide::Right, pt(1.0, 0.0));
        assert!(gap.is_empty(), "1 秒间隔属于断段，不得补停留");
        assert!(approx(sum_travel(&gap), 0.0, 1e-12));
        // 断段后当前点成为新锚点：后续 20ms 恢复正常积分且无路程
        let after = t.feed(stamp_at(1_020_000, BASE_UNIX_US + 1_020_000), StickSide::Right, pt(1.0, 0.0));
        assert_dwell_invariant(&after);
        assert_eq!(sum_active(&after), 20_000);
        assert!(approx(sum_travel(&after), 0.0, 1e-12), "断段重锚不得把间隙画成线");
    }

    // ------------------------------------------------------------------
    // 活动迟滞 / 路径去噪 / 返回段
    // ------------------------------------------------------------------

    /// 死区与迟滞：<0.20 不进入活动（无停留无路程）；0.20 进入；0.17 带内保持；
    /// 0.15 退出；噪声 <0.01R 不累积（从锚点而非上一原始点计）。
    #[test]
    fn motion_dpi_deadzone_hysteresis_and_noise() {
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0));
        // (a) 死区内摆动：不进入活动
        all.extend(t.feed(stamp_at(20_000, BASE_UNIX_US + 20_000), StickSide::Left, pt(0.1, 0.0)));
        all.extend(t.feed(stamp_at(40_000, BASE_UNIX_US + 40_000), StickSide::Left, pt(0.19, 0.0)));
        assert!(all.is_empty(), "死区内不得有停留或路程");
        // (b) 0.20 进入活动 → 0.17 迟滞带保持 → 0.15 退出
        all.extend(t.feed(stamp_at(60_000, BASE_UNIX_US + 60_000), StickSide::Left, pt(0.20, 0.0)));
        all.extend(t.feed(stamp_at(80_000, BASE_UNIX_US + 80_000), StickSide::Left, pt(0.20, 0.0)));
        all.extend(t.feed(stamp_at(100_000, BASE_UNIX_US + 100_000), StickSide::Left, pt(0.17, 0.0)));
        all.extend(t.feed(stamp_at(120_000, BASE_UNIX_US + 120_000), StickSide::Left, pt(0.17, 0.0)));
        all.extend(t.feed(stamp_at(140_000, BASE_UNIX_US + 140_000), StickSide::Left, pt(0.15, 0.0)));
        all.extend(t.feed(stamp_at(160_000, BASE_UNIX_US + 160_000), StickSide::Left, pt(0.15, 0.0)));
        // 进入 0.2R + 带内 0.03R + 返回段 0.17R；活动 4×20ms（0.15 退出后不再累计）
        assert!(approx(sum_travel(&all), 0.2 + 0.03 + 0.17, 1e-9), "实际 {}", sum_travel(&all));
        assert_eq!(sum_active(&all), 80_000, "迟滞带保持活动、退出后停止累计");
        // (c) 噪声不与上一原始点累积：0.008 拒绝，0.012 从锚点 0.2 计（而非 0.008+0.012）
        let before = sum_travel(&all);
        all.extend(t.feed(stamp_at(180_000, BASE_UNIX_US + 180_000), StickSide::Left, pt(0.20, 0.0)));
        all.extend(t.feed(stamp_at(200_000, BASE_UNIX_US + 200_000), StickSide::Left, pt(0.208, 0.0)));
        all.extend(t.feed(stamp_at(220_000, BASE_UNIX_US + 220_000), StickSide::Left, pt(0.212, 0.0)));
        assert!(approx(sum_travel(&all) - before, 0.2 + 0.012, 1e-9),
            "噪声必须从锚点计 0.012R，实际 {}", sum_travel(&all) - before);
        assert_dwell_invariant(&all);
        // 停留格只落在实际采样点（噪声停留仍归档，去噪只作用于行程）
        let dwell = dwell_by_bin(&all);
        let allowed = [stick_bin(pt(0.20, 0.0)), stick_bin(pt(0.17, 0.0)), stick_bin(pt(0.208, 0.0))];
        assert!(dwell.keys().all(|b| allowed.contains(b)), "dwell = {dwell:?}");
    }

    /// 退出死区返回 (0,0) 结算一次返回段；之后 neutral 长住不再累计。
    #[test]
    fn motion_dpi_return_settles_once_and_neutral_stay_is_free() {
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0));
        all.extend(t.feed(stamp_at(20_000, BASE_UNIX_US + 20_000), StickSide::Left, pt(1.0, 0.0)));
        all.extend(t.feed(stamp_at(40_000, BASE_UNIX_US + 40_000), StickSide::Left, pt(0.0, 0.0)));
        for i in 3..=102u64 {
            all.extend(t.feed(
                stamp_at(i * 20_000, BASE_UNIX_US + (i * 20_000) as i64),
                StickSide::Left,
                pt(0.0, 0.0),
            ));
        }
        assert_dwell_invariant(&all);
        assert!(approx(sum_travel(&all), 2.0, 1e-9), "去 1R + 返回段 1R = 2R，实际 {}", sum_travel(&all));
        assert_eq!(sum_active(&all), 20_000, "回中后 99 帧中性停留不产生活动时间");
    }

    // ------------------------------------------------------------------
    // 分箱
    // ------------------------------------------------------------------

    /// 625 格方向：0 在左上、y 正向上、bin=row×25+column、边界 clamp、越界钳制。
    #[test]
    fn motion_dpi_stick_bin_grid_orientation_625() {
        assert_eq!(STICK_GRID_SIZE, 25);
        assert_eq!(stick_bin(pt(-1.0, 1.0)), 0, "左上是 0");
        assert_eq!(stick_bin(pt(1.0, 1.0)), 24, "右上 col=24");
        assert_eq!(stick_bin(pt(-1.0, -1.0)), 600, "左下 row=24");
        assert_eq!(stick_bin(pt(1.0, -1.0)), 624, "右下 = 24×25+24");
        assert_eq!(stick_bin(pt(0.0, 0.0)), 312, "中心 = 12×25+12");
        assert_eq!(stick_bin(pt(0.0, 1.0)), 12, "上中 row=0");
        assert_eq!(stick_bin(pt(0.0, -1.0)), 612, "下中 row=24");
        assert_eq!(stick_bin(pt(-1.0, 0.0)), 300, "左中 col=0");
        assert_eq!(stick_bin(pt(1.0, 0.0)), 324, "右中 col=24");
        // 越界 clamp 到 0..24
        assert_eq!(stick_bin(pt(-1.5, 1.5)), 0);
        assert_eq!(stick_bin(pt(1.5, -1.5)), 624);
        // x 向右列号严格不减；y 向上行号严格不增
        let mut prev = None;
        for i in 0..=20u32 {
            let x = -1.0 + 0.1 * f64::from(i);
            let b = stick_bin(pt(x, 0.0));
            if let Some(p) = prev {
                assert!(b > p, "x 增大列号应严格增大：x={x} bin={b} prev={p}");
            }
            prev = Some(b);
        }
        let mut prev = None;
        for i in 0..=20u32 {
            let y = 1.0 - 0.1 * f64::from(i);
            let b = stick_bin(pt(0.0, y));
            if let Some(p) = prev {
                assert!(b > p, "y 减小行号应严格增大：y={y} bin={b} prev={p}");
            }
            prev = Some(b);
        }
    }

    // ------------------------------------------------------------------
    // 时间：微秒归档 / 跨日守恒与比例分配 / 路程归当前采样日
    // ------------------------------------------------------------------

    /// 微秒归档：奇数 dt 逐帧累计，无每帧整数秒截断。
    #[test]
    fn motion_dpi_microsecond_dwell_no_second_truncation() {
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0));
        all.extend(t.feed(stamp_at(123_456, BASE_UNIX_US + 123_456), StickSide::Left, pt(1.0, 0.0)));
        all.extend(t.feed(stamp_at(246_912, BASE_UNIX_US + 246_912), StickSide::Left, pt(1.0, 0.0)));
        all.extend(t.feed(stamp_at(370_369, BASE_UNIX_US + 370_369), StickSide::Left, pt(1.0, 0.0)));
        assert_dwell_invariant(&all);
        assert_eq!(sum_active(&all), 246_913, "停留按微秒归档：123456+123457");
        let dwell = dwell_by_bin(&all);
        assert_eq!(dwell.len(), 1);
        assert_eq!(dwell.get(&stick_bin(pt(1.0, 0.0))), Some(&246_913));
    }

    /// 正常跨本地午夜：20ms 区间按 UTC 切点比例拆两日（5ms+15ms），总量守恒、同格。
    #[test]
    fn motion_dpi_midnight_split_conserves_total_us() {
        let mid = clean_midnight_unix_us();
        let mut t = StickMotionTracker::new();
        assert!(t.feed(stamp_at(0, mid - 5_000), StickSide::Left, pt(1.0, 0.0)).is_empty());
        let out = t.feed(stamp_at(20_000, mid + 15_000), StickSide::Left, pt(1.0, 0.0));
        assert_dwell_invariant(&out);
        assert_eq!(out.len(), 2, "应拆成午夜前后两段：{out:?}");
        assert_eq!(out[0].day, format_day(local_day_from_unix_us(mid - 5_000).unwrap()));
        assert_eq!(out[1].day, format_day(local_day_from_unix_us(mid + 15_000).unwrap()));
        assert_eq!(out[0].active_us, 5_000);
        assert_eq!(out[1].active_us, 15_000, "最后一段取剩余微秒保证总和");
        assert_eq!(sum_active(&out), 20_000, "跨日时间守恒");
        assert_eq!(out[0].bins[0].bin, out[1].bins[0].bin, "两段同属原位置格");
        assert!(approx(sum_travel(&out), 0.0, 1e-12));
    }

    /// §4.2 示例（原样）：UTC 差 120ms、mono 差 20ms 且午夜居中 → 各归 10ms，不归 120ms。
    #[test]
    fn motion_dpi_midnight_proportional_split_with_utc_skew() {
        let mid = clean_midnight_unix_us();
        let mut t = StickMotionTracker::new();
        assert!(t.feed(stamp_at(0, mid - 60_000), StickSide::Left, pt(1.0, 0.0)).is_empty());
        let out = t.feed(stamp_at(20_000, mid + 60_000), StickSide::Left, pt(1.0, 0.0));
        assert_dwell_invariant(&out);
        assert_eq!(out.len(), 2, "午夜居中应两段：{out:?}");
        assert_eq!(out[0].active_us, 10_000, "前半 10ms");
        assert_eq!(out[1].active_us, 10_000, "后半 10ms——按 mono dt 分配，不归 120ms");
        assert_eq!(sum_active(&out), 20_000);
    }

    /// 路程增量归当前采样日：跨午夜帧接受新锚点时，路程落在午夜后的当日段。
    #[test]
    fn motion_dpi_travel_attributed_to_current_sampling_day() {
        let mid = clean_midnight_unix_us();
        let mut t = StickMotionTracker::new();
        assert!(t.feed(stamp_at(0, mid - 10_000), StickSide::Right, pt(0.4, 0.0)).is_empty());
        let out = t.feed(stamp_at(20_000, mid + 10_000), StickSide::Right, pt(0.9, 0.0));
        assert_dwell_invariant(&out);
        assert_eq!(out.len(), 2, "停留拆两日：{out:?}");
        assert_eq!(out[0].day, format_day(local_day_from_unix_us(mid - 10_000).unwrap()));
        assert_eq!(out[1].day, format_day(local_day_from_unix_us(mid + 10_000).unwrap()));
        assert!(approx(out[0].travel_r, 0.0, 1e-12), "前日段不携带本帧路程");
        assert!(approx(out[1].travel_r, 0.5, 1e-9), "路程 0.5R 归当前采样日");
        assert_eq!(sum_active(&out), 20_000);
    }

    // ------------------------------------------------------------------
    // 暂停 reset / 连接隔离 / 休眠与时钟跳变 / NaN
    // ------------------------------------------------------------------

    /// 暂停 reset（§5：调用方在暂停 epoch 变化时 reset）：不跨暂停连线、不补停留。
    #[test]
    fn motion_dpi_pause_reset_does_not_draw_across_pause() {
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0));
        all.extend(t.feed(stamp_at(20_000, BASE_UNIX_US + 20_000), StickSide::Left, pt(1.0, 0.0)));
        all.extend(feed_hold(&mut t, StickSide::Left, pt(1.0, 0.0), 40_000, BASE_UNIX_US + 40_000, 20_000, 4));
        // 真实暂停发生在两采样之间：epoch 变化，调用方 reset（暂停 5 秒）
        t.reset();
        let resume_mono = 100_000 + 5_000_000;
        // 恢复后首帧建立新锚点：即使坐标跳到对侧，也不产生 (1,0)→(-1,0) 的 2R 连线
        assert!(t.feed(stamp_at(resume_mono, BASE_UNIX_US + resume_mono as i64), StickSide::Left, pt(-1.0, 0.0)).is_empty());
        let after = t.feed(stamp_at(resume_mono + 20_000, BASE_UNIX_US + resume_mono as i64 + 20_000), StickSide::Left, pt(-1.0, 0.0));
        assert_dwell_invariant(&after);
        all.extend(after);
        assert!(approx(sum_travel(&all), 1.0, 1e-9), "只有暂停前的 1R 进入段，无跨暂停连线：{}", sum_travel(&all));
        assert_eq!(sum_active(&all), 100_000, "暂停区间（5 秒）不补停留：80ms+20ms");
        let dwell = dwell_by_bin(&all);
        assert_eq!(dwell.get(&324), Some(&80_000), "暂停前全部在右侧格");
        assert_eq!(dwell.get(&300), Some(&20_000), "恢复后停留归对侧格，与暂停前不串");
    }

    /// 连接×side 隔离：两个独立 tracker 互不影响，side 标签正确。
    #[test]
    fn motion_dpi_connection_side_isolation() {
        let mut left = StickMotionTracker::new();
        let mut right = StickMotionTracker::new();
        // 连接 A 左摇杆满幅右保持 1 秒；连接 B 右摇杆满幅下保持 1 秒
        let a = feed_hold(&mut left, StickSide::Left, pt(1.0, 0.0), 0, BASE_UNIX_US, 20_000, 51);
        let b = feed_hold(&mut right, StickSide::Right, pt(0.0, -1.0), 0, BASE_UNIX_US, 20_000, 51);
        assert_dwell_invariant(&a);
        assert_dwell_invariant(&b);
        assert!(a.iter().all(|d| d.side == StickSide::Left));
        assert!(b.iter().all(|d| d.side == StickSide::Right));
        assert_eq!(sum_active(&a), 1_000_000);
        assert_eq!(sum_active(&b), 1_000_000);
        assert!(approx(sum_travel(&a), 0.0, 1e-12));
        assert!(approx(sum_travel(&b), 0.0, 1e-12));
        let da = dwell_by_bin(&a);
        let db = dwell_by_bin(&b);
        assert_eq!(da.get(&324), Some(&1_000_000), "左 tracker 全在右侧格");
        assert_eq!(db.get(&612), Some(&1_000_000), "右 tracker 全在下侧格");
        assert_eq!(da.get(&612), None, "两 tracker 停留不串");
    }

    /// 休眠（1 小时单调间隔）不补段：断段重锚后恢复，无长线、无整夜热度。
    #[test]
    fn motion_dpi_sleep_gap_does_not_fill() {
        const SLEEP_US: u64 = 3_600_000_000;
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0));
        all.extend(t.feed(stamp_at(20_000, BASE_UNIX_US + 20_000), StickSide::Left, pt(1.0, 0.0)));
        all.extend(t.feed(stamp_at(40_000, BASE_UNIX_US + 40_000), StickSide::Left, pt(1.0, 0.0)));
        // 休眠 1 小时：dt > 250ms → 不填补
        let wake_mono = 40_000 + SLEEP_US;
        let wake = t.feed(stamp_at(wake_mono, BASE_UNIX_US + wake_mono as i64), StickSide::Left, pt(1.0, 0.0));
        assert!(wake.is_empty(), "休眠间隔不得补停留");
        // 醒后 20ms 恢复：从新锚点继续，无路程
        all.extend(t.feed(stamp_at(wake_mono + 20_000, BASE_UNIX_US + (wake_mono + 20_000) as i64), StickSide::Left, pt(1.0, 0.0)));
        all.extend(t.feed(stamp_at(wake_mono + 40_000, BASE_UNIX_US + (wake_mono + 40_000) as i64), StickSide::Left, pt(0.0, 0.0)));
        assert_dwell_invariant(&all);
        assert!(approx(sum_travel(&all), 2.0, 1e-9), "仅去 1R + 醒后回中 1R：{}", sum_travel(&all));
        assert_eq!(sum_active(&all), 60_000, "休眠 1 小时不得变成活动时间");
    }

    /// 时钟跳变：UTC 倒退 reset 不连线；UTC/单调差 >250ms 断段不补。
    #[test]
    fn motion_dpi_clock_jump_does_not_fill() {
        // (a) UTC 倒退（NTP 步进）：不递增 → reset，且不得把跳变画成线
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0));
        all.extend(t.feed(stamp_at(20_000, BASE_UNIX_US - 5_000_000), StickSide::Left, pt(1.0, 0.0)));
        assert!(approx(sum_travel(&all), 0.0, 1e-12), "UTC 倒退帧不得产生进入段路程");
        // reset 后以当前点重锚：正常帧恢复积分
        all.extend(t.feed(stamp_at(40_000, BASE_UNIX_US - 5_000_000 + 20_000), StickSide::Left, pt(1.0, 0.0)));
        assert_eq!(sum_active(&all), 20_000);
        assert!(approx(sum_travel(&all), 0.0, 1e-12));
        // (b) UTC 与单调差 >250ms（时钟跳变 300ms）：断段不补
        let mut t2 = StickMotionTracker::new();
        assert!(t2.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0)).is_empty());
        let jumped = t2.feed(stamp_at(20_000, BASE_UNIX_US + 320_000), StickSide::Left, pt(1.0, 0.0));
        assert!(jumped.is_empty(), "UTC/单调差 300ms 属断段");
        let after = t2.feed(stamp_at(40_000, BASE_UNIX_US + 340_000), StickSide::Left, pt(1.0, 0.0));
        assert_dwell_invariant(&after);
        assert_eq!(sum_active(&after), 20_000, "重锚后恢复正常积分");
        assert!(approx(sum_travel(&after), 0.0, 1e-12));
    }

    /// NaN/Inf 拒绝且 reset：脏点后不得跨脏点连线。
    #[test]
    fn motion_dpi_nan_inf_rejected_and_resets() {
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(0.0, 0.0));
        all.extend(t.feed(stamp_at(20_000, BASE_UNIX_US + 20_000), StickSide::Left, pt(1.0, 0.0)));
        all.extend(t.feed(stamp_at(40_000, BASE_UNIX_US + 40_000), StickSide::Left, pt(1.0, 0.0)));
        assert!(approx(sum_travel(&all), 1.0, 1e-9));
        // NaN 拒绝且 reset
        assert!(t.feed(stamp_at(60_000, BASE_UNIX_US + 60_000), StickSide::Left, pt(f64::NAN, 0.0)).is_empty());
        // reset 后同帧附近恢复：不产生 (1,0)→(-1,0) 连线
        assert!(t.feed(stamp_at(80_000, BASE_UNIX_US + 80_000), StickSide::Left, pt(-1.0, 0.0)).is_empty());
        let after = t.feed(stamp_at(100_000, BASE_UNIX_US + 100_000), StickSide::Left, pt(-1.0, 0.0));
        assert_dwell_invariant(&after);
        all.extend(after);
        assert!(approx(sum_travel(&all), 1.0, 1e-9), "脏点后不得跨点连线：{}", sum_travel(&all));
        assert_eq!(dwell_by_bin(&all).get(&300), Some(&20_000), "对侧格停留正常归档");
        // Inf 同样拒绝；恢复后中性点不再累计
        assert!(t.feed(stamp_at(120_000, BASE_UNIX_US + 120_000), StickSide::Left, pt(f64::INFINITY, 0.0)).is_empty());
        assert!(t.feed(stamp_at(140_000, BASE_UNIX_US + 140_000), StickSide::Left, pt(0.0, 0.0)).is_empty());
        assert!(t.feed(stamp_at(160_000, BASE_UNIX_US + 160_000), StickSide::Left, pt(0.0, 0.0)).is_empty());
        assert!(approx(sum_travel(&all), 1.0, 1e-9));
    }

    // ------------------------------------------------------------------
    // 径向投影 / 混用 side 防御
    // ------------------------------------------------------------------

    /// 半径 >1 径向投影到单位圆（不逐轴硬压对角）：(1,1) 归一后路程 1R；
    /// 半径 ≤1 不投影：(0.5,0.5) 路程 0.707107R。
    #[test]
    fn motion_dpi_radial_projection_not_axis_clamped() {
        let mut t = StickMotionTracker::new();
        let mut all = t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Right, pt(0.0, 0.0));
        all.extend(t.feed(stamp_at(20_000, BASE_UNIX_US + 20_000), StickSide::Right, pt(1.0, 1.0)));
        assert_dwell_invariant(&all);
        assert!(approx(sum_travel(&all), 1.0, 1e-9), "(1,1) 径向规范后应为 1R，实际 {}", sum_travel(&all));
        let mut t2 = StickMotionTracker::new();
        let mut all2 = t2.feed(stamp_at(0, BASE_UNIX_US), StickSide::Right, pt(0.0, 0.0));
        all2.extend(t2.feed(stamp_at(20_000, BASE_UNIX_US + 20_000), StickSide::Right, pt(0.5, 0.5)));
        assert!(approx(sum_travel(&all2), 0.5_f64.hypot(0.5), 1e-9),
            "(0.5,0.5) 应为 0.707107R，实际 {}", sum_travel(&all2));
    }

    /// 同一 tracker 混喂两侧属调用方错误：按断段处理，绝不跨侧连线（防御合同）。
    #[test]
    fn motion_dpi_side_switch_reanchors_like_first_frame() {
        let mut t = StickMotionTracker::new();
        assert!(t.feed(stamp_at(0, BASE_UNIX_US), StickSide::Left, pt(1.0, 0.0)).is_empty());
        let a = t.feed(stamp_at(20_000, BASE_UNIX_US + 20_000), StickSide::Left, pt(1.0, 0.0));
        assert_eq!(sum_active(&a), 20_000);
        // 换 side：断段重锚，无跨侧路程
        assert!(t.feed(stamp_at(40_000, BASE_UNIX_US + 40_000), StickSide::Right, pt(-1.0, 0.0)).is_empty());
        let c = t.feed(stamp_at(60_000, BASE_UNIX_US + 60_000), StickSide::Right, pt(-1.0, 0.0));
        assert_dwell_invariant(&c);
        assert_eq!(sum_active(&c), 20_000);
        assert_eq!(c[0].side, StickSide::Right, "增量 side 取当前喂入侧");
        let all: Vec<_> = a.into_iter().chain(c).collect();
        assert!(approx(sum_travel(&all), 0.0, 1e-12), "换侧不得产生跨侧路程");
    }
}
