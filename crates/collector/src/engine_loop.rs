//! aggregator：消费事件、跑 Engine、维护日聚合、0.5s flush、跨天、暂停（PLAN §4.6 契约）。
//!
//! 数据流（§2.3 写路径的落点）：三个采集线程的 `AggEvent` → 本线程逐条处理：
//!
//! - **Input**：暂停时直接丢弃（§4.6）；键盘事件先过 [`Engine`] 纯状态机（按下边沿产出
//!   Key/Combo，§4.3），按 `(device_id, day, code)` / `(day, mods, code)` 计数，并按当前
//!   前台 exe 归属 app 的 key_count/click_count（§5.2，仅物理按下边沿——Engine 天然去重
//!   自动重复）；手柄只进 `input_daily`（§5.2 仅键鼠归 app）。
//! - **Foreground**：暂停时也照常处理（保持 exe 归属正确，§4.6）。事件与暂停/恢复旗标
//!   并发时，先在 [`Aggregator::handle_event`] 入口消解未观察的沿（观察延迟至多一个心跳
//!   [`POLL_INTERVAL`]）——否则归账区间会横跨暂停间隔、把暂停秒数记为活动秒数。
//!
//! 聚合结构（§4.6 逐字）：`HashMap<(i64, day, u16), u64>`、`HashMap<(day, mods, code), u64>`、
//! `HashMap<(day, exe), AppAcc>`。每 1s tick 检查 shutdown 与跨天（入桶日期按到达时刷新）；
//! 每 0.5s 若脏 → 构造 [`FlushBatch`]（含前台增量秒数，按天切分，§5.3）→ [`Writer::flush`] →
//! 清桶；**flush 失败保留聚合桶、下个 tick 重试、日志限频——绝不丢计数、绝不 panic**（§4.5/§9.4）。
//!
//! # 前台秒数归账（FgState 增量的无损实现）
//!
//! [`FgState`]（apps 线程在 exe 变化时写 `exe`/`since=now`，且"先落状态再发事件"）与本线程
//! 通过 [`Arc<Mutex>`] 共享。若每次归账都直接读 `FgState.since`，一次前台切换会把旧 exe 的
//! "上次归账 → 切换时刻"尾段从状态里抹掉（切换即重置 since）造成秒数丢失。因此本线程在
//! 单线程内维护与 FgState 同步的归账游标 `fg_cur: (exe, since)`：
//!
//! - `Foreground{exe}` 事件：先把游标尾段 `[since → now]` 归账到旧 exe，再切换游标——
//!   事件有序、单线程处理，无丢失、无重复；
//! - 每 0.5s flush 前：归账 `[游标.since → now]` 到游标 exe（按本地日历日切分，§5.3 跨天切分），
//!   随后推进游标；
//! - **暂停瞬间**（观察到 `paused` 上升沿）：先把增量秒数归账一次，再把游标与 `FgState.since`
//!   一并**冻结在暂停时刻**；**恢复时** `since=now`——暂停区间不产生秒数，全程无减法无负值
//!   （§4.6/§5.3 逐字语义；暂停期间的 Foreground 事件只切换归属、不归账）。
//!
//! 输入事件的 exe 归属直接读 `FgState.exe`（apps 线程先落状态，值最新鲜，§2.3"当前前台"）。
//! 归账游标读取入口见 [`Aggregator::account_seconds_unchecked`]——它正是"FgState 增量秒数"
//! （自上次归账以来未被消费的前台增量）的无损载体。
//!
//! `spawn` 在 PLAN §4.6 四参基础上追加 `status: Arc<RuntimeStatus>`（S8 ipc_server 契约明确
//! "engine_loop 在消费到输入事件时调 record_event"上报 events_seen/last_event_at 的接线点；
//! 最小增量、不影响任何线格式，见 PLAN §9.1-3 兼容原则）。

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, NaiveDateTime, NaiveTime, TimeDelta};
use clrecoder_core::day;
use clrecoder_core::event::{AggEvent, DeviceKey, RawEvent};
use clrecoder_engine::Engine;
use clrecoder_store::writer::{FlushBatch, Writer};
use crossbeam_channel::{Receiver, RecvTimeoutError};

use crate::apps::FgState;
use crate::ipc_server::{Flags, RuntimeStatus};

/// 空闲心跳/暂停旗标观察间隔：100ms 让"暂停瞬间冻结 since"足够及时
/// （§4.6），也保证 shutdown 在 ≤100ms 内被观察到；事件驱动时更密，CPU 开销可忽略。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 1s tick（PLAN §4.6：检查 shutdown/跨天）。
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// 0.5s flush 周期（近实时：配合前端 0.5s 轮询，仪表盘感知延迟 ≤1s；
/// 2026-09-28 用户需求：实时刷新 → 10s→0.5s 轮询、5s→0.5s flush）。
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// flush 失败日志限频：每 60 次（≈2 分钟 @2s 周期）记录一次（§4.5"日志限频"）。
const FLUSH_LOG_EVERY: u64 = 60;

/// 设备解析失败日志限频：每 60 次记录一次（热路径失败时的防刷屏）。
const DEVICE_LOG_EVERY: u64 = 60;

/// `app_daily (day, exe)` 的内存聚合桶（§4.6 的 `AppAcc`）。
#[derive(Debug, Default, Clone)]
struct AppAcc {
    /// 前台秒数（按天切分后累加）
    secs: u64,
    /// 该应用内物理按键数（自动重复不计，§5.2）
    keys: u64,
    /// 该应用内鼠标点击数
    clicks: u64,
}

/// 启动 aggregator 线程（PLAN §4.6 逐字契约）。
///
/// - `rx`：采集事件通道（main/selftest 创建，采集线程持发送端）；
/// - `writer`：store 写入口（内部自带连接互斥与设备缓存）；
/// - `flags`：与 ipc_server 共享的暂停/关停开关（§4.6）；
/// - `fg`：与 apps 线程共享的前台状态（§4.6）；
/// - `status`：pipe `status` 命令的运行时数据源（S8 契约：消费到输入事件时调
///   [`RuntimeStatus::record_event`]）。
///
/// 线程体 panic 时限频记录并重建聚合状态继续（§9.4 catch_unwind 兜底；丢失的仅是
/// ≤0.5s 的未 flush 内存增量，`FgState`/`Flags` 不受影响）；收到 shutdown 或通道断开
/// 时排空余事件、终账、最后一批 flush 后正常退出。
#[must_use = "aggregator 线程必须被 join（main/selftest 等待终账与最后一批 flush）"]
pub fn spawn(
    rx: Receiver<AggEvent>,
    writer: Arc<Writer>,
    flags: Arc<Flags>,
    fg: Arc<Mutex<FgState>>,
    status: Arc<RuntimeStatus>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("aggregator".to_string())
        .spawn(move || run(rx, writer, flags, fg, status))
        .expect("aggregator 线程创建失败（内存耗尽等进程级错误）")
}

/// 线程主体：聚合循环 panic → 限频日志 + 1s 后重建聚合状态继续；正常退出（shutdown/
/// 通道断开）则直接返回。
fn run(
    rx: Receiver<AggEvent>,
    writer: Arc<Writer>,
    flags: Arc<Flags>,
    fg: Arc<Mutex<FgState>>,
    status: Arc<RuntimeStatus>,
) {
    loop {
        // Gilrs 式兜底：闭包捕获非 UnwindSafe 的 channel/Writer，断言后跨 catch_unwind 使用；
        // panic 重建即全新聚合状态（≤0.5s 增量），共享的 Flags/FgState/Writer 无恙。
        match catch_unwind(AssertUnwindSafe(|| aggregate_loop(&rx, &writer, &flags, &fg, &status)))
        {
            Ok(()) => return,
            Err(_) => {
                log::error!("aggregator 线程 panic（已捕获，重建聚合状态继续；≤0.5s 未 flush 增量丢失）");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// 聚合主循环：收事件 → 暂停沿检测 → shutdown 检查 → 1s tick（跨天）→ 0.5s flush。
/// 返回即线程退出（shutdown 或通道断开，均已终账落库）。
fn aggregate_loop(
    rx: &Receiver<AggEvent>,
    writer: &Arc<Writer>,
    flags: &Arc<Flags>,
    fg: &Arc<Mutex<FgState>>,
    status: &Arc<RuntimeStatus>,
) {
    let mut agg = Aggregator {
        writer: Arc::clone(writer),
        flags: Arc::clone(flags),
        fg: Arc::clone(fg),
        status: Arc::clone(status),
        engine: Engine::new(),
        devices: HashMap::new(),
        inputs: HashMap::new(),
        combos: HashMap::new(),
        apps: HashMap::new(),
        mouse_move: HashMap::new(),
        today: day::today_local(),
        fg_cur: None,
        paused_observed: flags.paused.load(Ordering::Acquire),
        last_tick: Instant::now(),
        last_flush: Instant::now(),
        flush_fail_streak: 0,
        device_fail_streak: 0,
    };
    // 归账游标从 FgState 的初始 exe 起算（"unknown"占位，首个 Foreground 事件即接管；
    // 初始零头不足 1 秒，不产生 app_daily 行）。
    agg.fg_cur = Some((agg.current_exe(), Instant::now()));

    loop {
        // 收事件：等到第一条后一次性排空积压（try_recv），避免高吞吐下的事件饥饿
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(first) => {
                agg.handle_event(first);
                while let Ok(ev) = rx.try_recv() {
                    agg.handle_event(ev);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // 全部发送端关闭：排空余事件 → 终账 → 最后一批 flush → 退出
            Err(RecvTimeoutError::Disconnected) => {
                agg.finish(rx);
                return;
            }
        }

        let now = Instant::now();

        // 暂停/恢复沿检测（§4.6：暂停瞬间先归账并冻结 since，恢复时 since=now）。
        // 每次迭代检查（POLL_INTERVAL=100ms）保证"瞬间"语义足够及时。
        let paused = flags.paused.load(Ordering::Acquire);
        if paused != agg.paused_observed {
            agg.on_pause_transition(paused, now);
        }

        // shutdown：排空余事件 → 终账（暂停中则冻结区间不归账）→ 最后一批 flush → 退出
        if flags.shutdown.load(Ordering::Acquire) {
            agg.finish(rx);
            return;
        }

        // 1s tick：刷新入桶日期（跨天检查，§4.6/§5.3——事件按到达时的本地日期入桶）
        if now.duration_since(agg.last_tick) >= TICK_INTERVAL {
            agg.last_tick = now;
            agg.today = day::today_local();
        }

        // 0.5s flush：若脏（任一聚合桶非空）→ 构造 FlushBatch（含前台增量秒数，按天切分）
        // → writer.flush → 清桶；失败保留聚合桶下个 tick 重试（§4.6）
        if now.duration_since(agg.last_flush) >= FLUSH_INTERVAL {
            agg.last_flush = now;
            agg.account_seconds(now);
            agg.flush_buckets();
        }
    }
}

/// 聚合器状态（单线程独占，无锁）。
struct Aggregator {
    writer: Arc<Writer>,
    flags: Arc<Flags>,
    fg: Arc<Mutex<FgState>>,
    status: Arc<RuntimeStatus>,
    /// 键盘纯状态机（§4.3：按下状态表/修饰键/组合键判定）
    engine: Engine,
    /// DeviceKey → device_id 缓存（§4.6"由 aggregator 维护"，底层 Writer 另有持久缓存）
    devices: HashMap<DeviceKey, i64>,
    /// `input_daily` 桶：(device_id, day, code) → count
    inputs: HashMap<(i64, String, u16), u64>,
    /// `combo_daily` 桶：(day, mods, code) → count
    combos: HashMap<(String, u8, u16), u64>,
    /// `app_daily` 桶：(day, exe) → AppAcc
    apps: HashMap<(String, String), AppAcc>,
    /// `mouse_move_daily` 桶：(device_id, day) → distance_inches
    mouse_move: HashMap<(i64, String), f64>,
    /// 入桶日期缓存（每 1s tick 刷新，跨天自动切换，§5.3）
    today: String,
    /// 前台秒数归账游标 (exe, 未归账起点)——FgState 增量的单线程无损载体（见模块文档）
    fg_cur: Option<(String, Instant)>,
    /// 上一次观察到的 paused 值（沿检测）
    paused_observed: bool,
    last_tick: Instant,
    last_flush: Instant,
    /// flush 连续失败次数（日志限频用；成功清零）
    flush_fail_streak: u64,
    /// 设备解析连续失败次数（日志限频用）
    device_fail_streak: u64,
}

impl Aggregator {
    // ---------- 事件处理 ----------

    /// 处理一条聚合事件。
    ///
    /// 先行消解尚未观察到的暂停/恢复沿（事件与 pipe 旗标翻转并发时，本迭代内到达的事件
    /// 可能携带"已恢复但尚未观察"的状态）：若不先对齐沿状态，Foreground 的归账区间会横跨
    /// 整个暂停间隔、把暂停秒数记为活动秒数（违反 §4.6"暂停区间不产生秒数"）。
    /// 沿观察延迟至多一个心跳（[`POLL_INTERVAL`]），两侧归属与秒数不受影响。
    fn handle_event(&mut self, ev: AggEvent) {
        let paused = self.flags.paused.load(Ordering::Acquire);
        if paused != self.paused_observed {
            self.on_pause_transition(paused, Instant::now());
        }
        match ev {
            AggEvent::Foreground { exe } => self.on_foreground(exe, paused),
            AggEvent::Input(raw) => {
                // 收到即计（§4.4 StatusData.events_seen = "累计收到的事件数"，含暂停丢弃的）
                self.status.record_event();
                // 暂停：Input 事件丢弃（§4.6），Foreground 照常处理
                if paused {
                    return;
                }
                self.on_input(raw);
            }
        }
    }

    /// Foreground 事件：先把旧 exe 的未归账尾段落桶，再切换归属（Foreground 照常处理，
    /// 保持 exe 归属正确，§4.6）。暂停期间只切换归属、不归账（区间冻结）。
    /// 调用前 `handle_event` 已完成沿消解——`paused` 即当前真实沿状态。
    fn on_foreground(&mut self, exe: String, paused: bool) {
        let now = Instant::now();
        if paused {
            self.fg_cur = Some((exe, now));
            return;
        }
        self.account_seconds_unchecked(now);
        self.fg_cur = Some((exe, now));
    }

    /// Input 事件入桶（§5.2）。设备解析失败时整条跳过（防御兜底：绝不 panic、
    /// 不影响其他事件，§1 原则 3），但引擎状态始终前进——否则后续组合键判定被污染。
    fn on_input(&mut self, raw: RawEvent) {
        let day = self.today.clone();
        match raw {
            RawEvent::Keyboard { device, sc, down } => {
                // 引擎状态先行：up 边沿/自动重复在此消化（§4.3）
                let out = self.engine.on_key(sc, down);
                let Some(code) = out.key else { return }; // 无按键计数：up 边沿或自动重复
                let Some(dev_id) = self.device_id(&device) else { return };
                *self.inputs.entry((dev_id, day.clone(), code)).or_insert(0) += 1;
                // 当前前台 exe 的 key_count +1（仅物理按下边沿，§5.2）
                let exe = self.current_exe();
                self.app(&day, &exe).keys += 1;
                // 组合键计数（§4.3：mods 为按住修饰键的位或）
                if let Some((mods, combo_code)) = out.combo {
                    *self.combos.entry((day.clone(), mods, combo_code)).or_insert(0) += 1;
                }
            }
            RawEvent::MouseClick { device, button } => {
                let Some(dev_id) = self.device_id(&device) else { return };
                *self.inputs.entry((dev_id, day.clone(), u16::from(button))).or_insert(0) += 1;
                // 当前前台 exe 的 click_count +1（§5.2）
                let exe = self.current_exe();
                self.app(&day, &exe).clicks += 1;
            }
            RawEvent::GamepadPress { device, button } => {
                let Some(dev_id) = self.device_id(&device) else { return };
                *self.inputs.entry((dev_id, day.clone(), u16::from(button))).or_insert(0) += 1;
                // 手柄不计入 exe 的 key_count/click_count（§5.2 仅键鼠归 app；R8 语义）
            }
            RawEvent::MouseMove { device, distance_inches } => {
                if !(distance_inches.is_finite() && distance_inches > 0.0) {
                    return;
                }
                let Some(dev_id) = self.device_id(&device) else { return };
                *self.mouse_move.entry((dev_id, day.clone())).or_insert(0.0) += distance_inches;
            }
        }
    }

    // ---------- 前台秒数归账 ----------

    /// 暂停/恢复沿处理（§4.6 逐字语义）。
    fn on_pause_transition(&mut self, paused: bool, now: Instant) {
        self.paused_observed = paused;
        if paused {
            // 暂停瞬间：先把 FgState 增量秒数归账一次，再把 since 冻结在暂停时刻。
            // （旗标置位到本线程观察到的 ≤100ms 滞后按暂停前时间计入，属可忽略斜差。）
            self.account_seconds_unchecked(now);
        }
        // 恢复（与暂停冻结的写回）：游标与 FgState.since 一律重置为 now——
        // 暂停区间不产生秒数，全程无减法无负值（§4.6）。
        if let Some((_, since)) = self.fg_cur.as_mut() {
            *since = now;
        }
        self.freeze_fg_since(now);
    }

    /// 归账一次前台增量秒数（暂停期跳过：游标已冻结，区间属暂停时间）。
    fn account_seconds(&mut self, now: Instant) {
        if self.flags.paused.load(Ordering::Acquire) {
            return;
        }
        self.account_seconds_unchecked(now);
    }

    /// 归账核心：游标尾段 `[since → now]` 按本地日历日切分入 `app_daily` 桶，随后推进游标。
    /// 异常情形（时钟回拨/极小间隔）宁可丢弃零头也绝不产生负值（§4.6）。
    fn account_seconds_unchecked(&mut self, now: Instant) {
        let Some((exe, since)) = self.fg_cur.as_ref().map(|(e, s)| (e.clone(), *s)) else {
            return;
        };
        let elapsed = now.duration_since(since);
        if elapsed.is_zero() {
            return;
        }
        let end_wall = Local::now();
        let Ok(delta) = TimeDelta::from_std(elapsed) else {
            return;
        };
        let start_wall = end_wall - delta;
        for (seg_day, secs) in split_seconds_by_day(start_wall, end_wall) {
            if secs > 0 {
                // i64 秒数已验证为正，try_from 失败按 0 兜底（防御，实际不可达）
                let secs = u64::try_from(secs).unwrap_or(0);
                self.app(&seg_day, &exe).secs += secs;
            }
        }
        if let Some((_, since)) = self.fg_cur.as_mut() {
            *since = now;
        }
    }

    /// 把暂停冻结/恢复语义同步写回 `FgState.since`（§4.6"将 since 冻结在暂停时刻，
    /// 恢复时 since=now"——apps 线程仅在 exe 变化时重写，两者互斥锁下串行无竞争）。
    fn freeze_fg_since(&self, now: Instant) {
        let mut st = match self.fg.lock() {
            Ok(g) => g,
            // 锁毒化不阻塞统计（§1 原则 3：绝不 crash）
            Err(poisoned) => poisoned.into_inner(),
        };
        st.since = now;
    }

    // ---------- 桶与 flush ----------

    /// `app_daily` 桶入口。
    fn app(&mut self, day: &str, exe: &str) -> &mut AppAcc {
        self.apps.entry((day.to_string(), exe.to_string())).or_default()
    }

    /// 当前前台 exe（输入归属用；锁毒化时取原值继续，§1 原则 3）。
    fn current_exe(&self) -> String {
        match self.fg.lock() {
            Ok(g) => g.exe.clone(),
            Err(poisoned) => poisoned.into_inner().exe.clone(),
        }
    }

    /// DeviceKey → device_id（§4.6：缓存调 `writer.get_or_create_device`）。
    /// 解析失败限频记录并返回 None（调用方整条跳过，绝不 panic）。
    fn device_id(&mut self, d: &DeviceKey) -> Option<i64> {
        if let Some(id) = self.devices.get(d) {
            return Some(*id);
        }
        match self.writer.get_or_create_device(d) {
            Ok(id) => {
                self.device_fail_streak = 0;
                self.devices.insert(d.clone(), id);
                Some(id)
            }
            Err(e) => {
                self.device_fail_streak += 1;
                if self.device_fail_streak == 1
                    || self.device_fail_streak.is_multiple_of(DEVICE_LOG_EVERY)
                {
                    log::error!("设备注册失败（连续第 {} 次），本条事件跳过: {e}", self.device_fail_streak);
                }
                None
            }
        }
    }

    /// 取出全部聚合桶构造 [`FlushBatch`] 单事务 flush；成功清桶、失败原样合并回桶
    /// （下个 tick 重试——绝不丢计数，§4.5），日志限频。
    fn flush_buckets(&mut self) {
        let inputs = std::mem::take(&mut self.inputs);
        let combos = std::mem::take(&mut self.combos);
        let apps = std::mem::take(&mut self.apps);
        let mouse_move = std::mem::take(&mut self.mouse_move);
        if inputs.is_empty() && combos.is_empty() && apps.is_empty() && mouse_move.is_empty() {
            return; // 不脏：无写库（§4.6"若脏"）
        }
        let batch = FlushBatch {
            input: inputs
                .iter()
                .map(|((dev, d, code), n)| (*dev, d.clone(), *code, *n))
                .collect(),
            combos: combos
                .iter()
                .map(|((d, mods, code), n)| (d.clone(), *mods, *code, *n))
                .collect(),
            apps: apps
                .iter()
                .map(|((d, exe), a)| (d.clone(), exe.clone(), a.secs, a.keys, a.clicks))
                .collect(),
            mouse_move: mouse_move
                .iter()
                .map(|((dev, d), inches)| (*dev, d.clone(), *inches))
                .collect(),
        };
        if let Err(e) = self.writer.flush(&batch) {
            self.flush_fail_streak += 1;
            if self.flush_fail_streak == 1 || self.flush_fail_streak.is_multiple_of(FLUSH_LOG_EVERY) {
                log::error!(
                    "flush 失败（连续第 {} 次），聚合桶已保留、下个 tick 重试: {e}",
                    self.flush_fail_streak
                );
            }
            merge_buckets_back(
                &mut self.inputs,
                &mut self.combos,
                &mut self.apps,
                &mut self.mouse_move,
                inputs,
                combos,
                apps,
                mouse_move,
            );
        } else {
            self.flush_fail_streak = 0;
        }
    }

    /// 退出前的收尾（shutdown / 通道断开共用）：排空竞态窗口内的残余事件 →
    /// 终账前台秒数（暂停中则冻结区间不归账）→ 最后一批 flush。
    fn finish(&mut self, rx: &Receiver<AggEvent>) {
        while let Ok(ev) = rx.try_recv() {
            self.handle_event(ev);
        }
        self.account_seconds(Instant::now());
        self.flush_buckets();
    }
}

/// flush 失败时把已取出的桶原样合并回去（计数累加，键合并——后续增量接着累计）。
fn merge_buckets_back(
    inputs: &mut HashMap<(i64, String, u16), u64>,
    combos: &mut HashMap<(String, u8, u16), u64>,
    apps: &mut HashMap<(String, String), AppAcc>,
    mouse_move: &mut HashMap<(i64, String), f64>,
    back_inputs: HashMap<(i64, String, u16), u64>,
    back_combos: HashMap<(String, u8, u16), u64>,
    back_apps: HashMap<(String, String), AppAcc>,
    back_mouse_move: HashMap<(i64, String), f64>,
) {
    for (k, n) in back_inputs {
        *inputs.entry(k).or_insert(0) += n;
    }
    for (k, n) in back_combos {
        *combos.entry(k).or_insert(0) += n;
    }
    for (k, a) in back_apps {
        let acc = apps.entry(k).or_default();
        acc.secs += a.secs;
        acc.keys += a.keys;
        acc.clicks += a.clicks;
    }
    for (k, d) in back_mouse_move {
        *mouse_move.entry(k).or_insert(0.0) += d;
    }
}

/// 把墙钟区间 `[start, end]` 按本地日历日切分为 `(day, 秒数)` 段（§5.3"FgState 秒数在
/// 日期边界切分到两个 (day,exe)"；系统休眠跨多天时自然切出多段）。
///
/// 纯函数：在 naive 本地时间上运算——DST 切换日的段长按墙钟差计（±1 小时偏差，符合
/// §9.3"前台墙钟时长"语义）；`end <= start`（时钟回拨等异常）返回空——宁可不记，绝不负值。
fn split_seconds_by_day(start: DateTime<Local>, end: DateTime<Local>) -> Vec<(String, i64)> {
    split_secs_by_day_naive(start.naive_local(), end.naive_local())
}

/// [`split_seconds_by_day`] 的 naive 核心（可脱离时区单测）。
fn split_secs_by_day_naive(start: NaiveDateTime, end: NaiveDateTime) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    if end <= start {
        return out;
    }
    let mut seg_start = start;
    loop {
        let seg_day = seg_start.date();
        // 次日零点为段界；无次日（理论不可达：end > start 保证日期可推进）即止
        let Some(next_day) = seg_day.succ_opt() else { break };
        let seg_end = next_day.and_time(NaiveTime::MIN).min(end);
        let secs = (seg_end - seg_start).num_seconds();
        if secs > 0 {
            out.push((day::format_day(seg_day), secs));
        }
        if seg_end >= end {
            break;
        }
        seg_start = seg_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::path::PathBuf;

    use clrecoder_core::codes::DeviceKind;

    /// 便捷构造 naive 时刻。
    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, s).unwrap()
    }

    /// 汇总各段秒数（断言总量守恒用）。
    fn total(segs: &[(String, i64)]) -> i64 {
        segs.iter().map(|(_, s)| *s).sum()
    }

    #[test]
    fn same_day_interval_is_single_segment() {
        let segs = split_secs_by_day_naive(at(2026, 9, 28, 10, 0, 0), at(2026, 9, 28, 10, 0, 30));
        assert_eq!(segs, vec![("2026-09-28".to_string(), 30)]);
    }

    #[test]
    fn midnight_crossing_splits_into_two_days() {
        // §5.3：FgState 秒数在日期边界切分到两个 (day,exe)
        let segs = split_secs_by_day_naive(at(2026, 9, 28, 23, 59, 58), at(2026, 9, 29, 0, 0, 3));
        assert_eq!(
            segs,
            vec![("2026-09-28".to_string(), 2), ("2026-09-29".to_string(), 3)]
        );
    }

    #[test]
    fn end_exactly_at_midnight_belongs_to_previous_day() {
        // 区间右开：恰好到零点的秒数全归前一日
        let segs = split_secs_by_day_naive(at(2026, 9, 28, 23, 59, 58), at(2026, 9, 29, 0, 0, 0));
        assert_eq!(segs, vec![("2026-09-28".to_string(), 2)]);
    }

    #[test]
    fn multi_day_span_is_split_per_calendar_day() {
        // 休眠跨多天（Instant 连续计时、墙钟跳跃）：按日历日逐段切分
        let segs = split_secs_by_day_naive(at(2026, 9, 28, 22, 0, 0), at(2026, 9, 30, 6, 0, 0));
        assert_eq!(
            segs,
            vec![
                ("2026-09-28".to_string(), 2 * 3600),
                ("2026-09-29".to_string(), 86_400),
                ("2026-09-30".to_string(), 6 * 3600),
            ]
        );
        assert_eq!(total(&segs), 2 * 3600 + 86_400 + 6 * 3600);
    }

    #[test]
    fn year_boundary_and_month_boundary_split_correctly() {
        let segs = split_secs_by_day_naive(at(2026, 12, 31, 23, 59, 59), at(2027, 1, 1, 0, 0, 1));
        assert_eq!(
            segs,
            vec![("2026-12-31".to_string(), 1), ("2027-01-01".to_string(), 1)]
        );
        let segs = split_secs_by_day_naive(at(2027, 2, 28, 23, 59, 59), at(2027, 3, 1, 0, 0, 1));
        assert_eq!(
            segs,
            vec![("2027-02-28".to_string(), 1), ("2027-03-01".to_string(), 1)]
        );
    }

    #[test]
    fn inverted_or_zero_interval_yields_nothing_and_never_negative() {
        // 时钟回拨（start > end）与零长区间：空结果，绝不产生负秒数（§4.6）
        assert!(split_secs_by_day_naive(at(2026, 9, 28, 1, 0, 0), at(2026, 9, 28, 0, 0, 0)).is_empty());
        assert!(split_secs_by_day_naive(at(2026, 9, 28, 8, 0, 0), at(2026, 9, 28, 8, 0, 0)).is_empty());
    }

    #[test]
    fn sub_second_truncation_keeps_total_consistent() {
        // 毫秒级区间：秒数截断为 0，不产生空段
        let start = at(2026, 9, 28, 0, 0, 0);
        let end = start + TimeDelta::try_milliseconds(999).unwrap();
        assert!(split_secs_by_day_naive(start, end).is_empty());
        // 恰好 1 秒 → 1 段 1 秒
        let end = start + TimeDelta::try_seconds(1).unwrap();
        assert_eq!(split_secs_by_day_naive(start, end), vec![("2026-09-28".to_string(), 1)]);
    }

    #[test]
    fn app_acc_default_is_all_zero() {
        let a = AppAcc::default();
        assert_eq!((a.secs, a.keys, a.clicks), (0, 0, 0));
    }

    #[test]
    fn merge_buckets_back_accumulates_into_existing_entries() {
        // flush 失败合并回桶：同键累加、新键插入（§4.5"保留聚合桶、下个 tick 重试"）
        let mut inputs = HashMap::from([((1i64, "2026-09-28".to_string(), 0x1Eu16), 5u64)]);
        let mut combos = HashMap::new();
        let mut apps = HashMap::from([(
            ("2026-09-28".to_string(), "a.exe".to_string()),
            AppAcc { secs: 2, keys: 1, clicks: 0 },
        )]);
        let mut mouse_move = HashMap::from([((1i64, "2026-09-28".to_string()), 1.5f64)]);
        let back_inputs = HashMap::from([
            ((1i64, "2026-09-28".to_string(), 0x1Eu16), 3u64),
            ((2i64, "2026-09-28".to_string(), 1u16), 2u64),
        ]);
        let back_combos = HashMap::from([(("2026-09-28".to_string(), 1u8, 0x2Eu16), 1u64)]);
        let back_apps = HashMap::from([
            (
                ("2026-09-28".to_string(), "a.exe".to_string()),
                AppAcc { secs: 1, keys: 2, clicks: 3 },
            ),
            (
                ("2026-09-28".to_string(), "b.exe".to_string()),
                AppAcc { secs: 9, keys: 0, clicks: 0 },
            ),
        ]);
        let back_mouse_move = HashMap::from([
            ((1i64, "2026-09-28".to_string()), 0.5f64),
            ((2i64, "2026-09-28".to_string()), 2.0f64),
        ]);
        merge_buckets_back(
            &mut inputs,
            &mut combos,
            &mut apps,
            &mut mouse_move,
            back_inputs,
            back_combos,
            back_apps,
            back_mouse_move,
        );
        assert_eq!(inputs.get(&(1, "2026-09-28".to_string(), 0x1E)), Some(&8));
        assert_eq!(inputs.get(&(2, "2026-09-28".to_string(), 1)), Some(&2));
        assert_eq!(combos.get(&("2026-09-28".to_string(), 1, 0x2E)), Some(&1));
        let a = &apps[&("2026-09-28".to_string(), "a.exe".to_string())];
        assert_eq!((a.secs, a.keys, a.clicks), (3, 3, 3));
        let b = &apps[&("2026-09-28".to_string(), "b.exe".to_string())];
        assert_eq!((b.secs, b.keys, b.clicks), (9, 0, 0));
        assert!((mouse_move[&(1, "2026-09-28".to_string())] - 2.0).abs() < 1e-9);
        assert!((mouse_move[&(2, "2026-09-28".to_string())] - 2.0).abs() < 1e-9);
    }

    // ---------------- 暂停沿消解回归（事件与 pipe 旗标并发） ----------------
    //
    // 真实缺陷锚点：恢复旗标置位后、沿被观察到之前到达的 Foreground 事件，会以"活动期"
    // 分支归账 [暂停冻结点 → now]——把整段暂停秒数记给旧 exe（违反 §4.6"暂停区间不产生
    // 秒数"）。以下用例确定性复现该竞态（无 sleep、无线程），守住修复。

    /// 带冻结游标的 aggregator（临时库；tag+pid 保证路径唯一，结束清理）。
    fn agg_with_frozen_cursor(tag: &str, frozen_for: Duration) -> (Aggregator, PathBuf) {
        let db =
            std::env::temp_dir().join(format!("clrecoder-agg-{tag}-{}.db", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let mut name = db.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(&name);
        }
        let flags = Arc::new(Flags::default());
        let agg = Aggregator {
            writer: Arc::new(Writer::open(&db).unwrap()),
            flags: Arc::clone(&flags),
            fg: Arc::new(Mutex::new(FgState { exe: "unknown".to_string(), since: Instant::now() })),
            status: Arc::new(RuntimeStatus::new("0.1.0")),
            engine: Engine::new(),
            devices: HashMap::new(),
            inputs: HashMap::new(),
            combos: HashMap::new(),
            apps: HashMap::new(),
            mouse_move: HashMap::new(),
            today: day::today_local(),
            // 游标停在 alpha.exe，且"暂停已冻结"2 秒（旗标翻转发生在冻结之后）
            fg_cur: Some(("alpha.exe".to_string(), Instant::now() - frozen_for)),
            paused_observed: true,
            last_tick: Instant::now(),
            last_flush: Instant::now(),
            flush_fail_streak: 0,
            device_fail_streak: 0,
        };
        (agg, db)
    }

    fn cleanup_db(db: &PathBuf) {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = db.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(&name);
        }
    }

    #[test]
    fn foreground_during_unobserved_resume_discards_paused_interval() {
        let (mut agg, db) = agg_with_frozen_cursor("resume-edge", Duration::from_secs(2));
        // pipe 已置恢复，但 aggregator 尚未观察到沿（paused_observed 仍为 true）
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(AggEvent::Foreground { exe: "beta.exe".to_string() });
        // 沿已消解、游标已切到 beta；冻结的 2s 暂停区间必须 0 秒入账
        assert!(!agg.paused_observed, "沿应被消解");
        assert!(agg.apps.is_empty(), "暂停区间不得入账，实际 {:?}", agg.apps);
        assert_eq!(agg.fg_cur.as_ref().unwrap().0, "beta.exe");
        drop(agg);
        cleanup_db(&db);
    }

    #[test]
    fn foreground_while_still_paused_switches_cursor_without_accounting() {
        let (mut agg, db) = agg_with_frozen_cursor("paused-fg", Duration::from_secs(2));
        // 旗标与观察一致（仍暂停）：Foreground 照常处理=仅切换归属，不归账（§4.6）
        agg.flags.paused.store(true, Ordering::Release);
        agg.handle_event(AggEvent::Foreground { exe: "beta.exe".to_string() });
        assert!(agg.paused_observed, "仍处暂停沿");
        assert!(agg.apps.is_empty(), "暂停期间不得归账: {:?}", agg.apps);
        assert_eq!(agg.fg_cur.as_ref().unwrap().0, "beta.exe");
        drop(agg);
        cleanup_db(&db);
    }

    #[test]
    fn foreground_while_active_accounts_old_exe_tail() {
        let (mut agg, db) = agg_with_frozen_cursor("active-fg", Duration::from_secs(2));
        // 活动期（沿已对齐为未暂停）：旧 exe 的 2s 尾段正常归账后切换游标
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(AggEvent::Foreground { exe: "beta.exe".to_string() });
        let alpha = &agg.apps[&(day::today_local(), "alpha.exe".to_string())];
        assert_eq!(alpha.secs, 2, "旧 exe 尾段应无损归账");
        assert_eq!(agg.fg_cur.as_ref().unwrap().0, "beta.exe");
        drop(agg);
        cleanup_db(&db);
    }

    #[test]
    fn input_during_unobserved_resume_is_counted_after_edge_reconciles() {
        // 恢复沿未观察时到达的键盘事件：沿消解后按活动期正常计数（修复前会被丢弃到下次 tick）
        let (mut agg, db) = agg_with_frozen_cursor("resume-input", Duration::from_millis(1500));
        agg.flags.paused.store(false, Ordering::Release);
        let dev = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 7,
            pid: 8,
            name: "测试键盘".to_string(),
        };
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard { device: dev.clone(), sc: 0x1E, down: true }));
        let dev_id = agg.devices.get(&dev).copied().unwrap();
        let day = day::today_local();
        assert_eq!(agg.inputs.get(&(dev_id, day.clone(), 0x1E)), Some(&1), "恢复后的按键应计数");
        // 暂停冻结区间不得入账
        assert!(!agg.apps.contains_key(&(day, "alpha.exe".to_string())));
        drop(agg);
        cleanup_db(&db);
    }
}
