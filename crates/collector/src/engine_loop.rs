//! aggregator：消费事件、跑 Engine、维护日聚合、0.5s flush、跨天、暂停（PLAN §4.6 契约）。
//!
//! 数据流（§2.3 写路径的落点）：三个采集线程的 `AggEvent` → 本线程逐条处理：
//!
//! - **Input**：丢弃但键盘边沿仍喂 engine（§4.6：暂停期按来源维护 held/修饰键，输出不
//!   计数）；活动期键盘事件先过 [`Engine`] 纯状态机（按下边沿产出 Key/Combo，§4.3，
//!   `on_key_from` 携带采集层分配的连接 ID——来源维度按物理连接隔离，F3），按
//!   `(device_id, day, code)` / `(day, mods, code)` 计数，并按当前前台 exe 归属 app 的
//!   key_count/click_count（§5.2，仅物理按下边沿——Engine 天然去重自动重复）；
//!   手柄只进 `input_daily`（§5.2 仅键鼠归 app）。
//! - **SourceRemoved / KeyboardSourcesReset**：生命周期控制事件（correctness-v2 §4.3）——
//!   无论 paused 均执行：设备拔出清该来源按下状态（其他来源修饰位保留）、loop 重建清空
//!   全部键盘来源；不写统计、不记 events_seen、不动 paused/shutdown。
//! - **MouseSourceState / MouseTravel**（motion-dpi §4.3/§4.4）：运动事件，无论 paused
//!   均处理、不增加 events_seen/按钮总量/应用键鼠次数。MouseSourceState 注册来源
//!   （按 source_key 幂等）并落库 connected/probe/auto 状态——连接代际门控（旧代
//!   disconnect 不得把新连接标离线；进程重启首见即接受，不与上个进程数字比较）；
//!   MouseTravel 按 (source_key, 捕获日, dpi, origin) 桶累计（暂停过滤在采集侧按捕获
//!   control 完成，此处无条件落库），flush 时注册来源绑定数据库 id——注册失败按
//!   source_key 压缩保留重试，不丢 counts；与旧统计同事务、失败全回滚合桶。
//! - **GamepadMotion / GamepadMotionDisconnected**（motion-dpi §4.3，S5）：手柄摇杆
//!   运动事件，同样不增加 events_seen/按钮总量。每个连接×side 一个
//!   [`StickMotionTracker`]（连接独立）；每帧前读 [`Flags::motion_control`]——当前
//!   epoch 变化 reset 全部摇杆 tracker，捕获 epoch 与当前不一致或任一 paused 时
//!   reset/跳过（不跨短暂停连接坐标）；产出增量按（型号, 日, side）桶累计，flush 时
//!   绑定 device_id（型号注册失败按 DeviceKey×日×side 压缩保留）；断连/读取失败
//!   （`GamepadMotionDisconnected`）清该连接全部 tracker。
//! - **Foreground**：暂停时也照常处理（保持 exe 归属正确，§4.6）。事件与暂停/恢复旗标
//!   并发时，先在 [`Aggregator::handle_event`] 入口消解未观察的沿（观察延迟至多一个心跳
//!   [`POLL_INTERVAL`]）——否则归账区间会横跨暂停间隔、把暂停秒数记为活动秒数。
//!
//! 聚合结构（§4.6 逐字）：`HashMap<(i64, day, u16), u64>`、`HashMap<(day, mods, code), u64>`、
//! `HashMap<(day, exe), AppAcc>`。每 1s tick 检查 shutdown 与跨天（入桶日期按到达时刷新；
//! 跨天时清 ledger 过期日尾差）；每 0.5s 若脏 → 构造 [`FlushBatch`]（含前台增量秒数，按天
//! 切分，§5.3）→ [`Writer::flush`] → 清桶；**flush 失败保留聚合桶、下个 tick 重试、日志限频
//! ——绝不丢计数、绝不 panic**（§4.5/§9.4）。
//!
//! # 前台秒数归账（FgState 增量的无损实现 + 亚秒余数账本，correctness-v2 §4.4）
//!
//! [`FgState`]（apps 线程在 exe 变化时写 `exe`/`since=now`，且"先落状态再发事件"）与本线程
//! 通过 [`Arc<Mutex>`] 共享。若每次归账都直接读 `FgState.since`，一次前台切换会把旧 exe 的
//! "上次归账 → 切换时刻"尾段从状态里抹掉（切换即重置 since）造成秒数丢失。因此本线程在
//! 单线程内维护与 FgState 同步的归账游标 `fg_cur: (exe, since)`：
//!
//! - `Foreground{exe}` 事件：先把游标尾段 `[since → now]` 归账到旧 exe，再切换游标——
//!   事件有序、单线程处理，无丢失、无重复（§5.3-1）；
//! - 每 0.5s flush 前：归账 `[游标.since → now]` 到游标 exe（先按本地日历日拆分再入
//!   ledger，§5.3-2），随后推进游标；
//! - **暂停瞬间**（观察到 `paused` 上升沿）：先把增量秒数归账一次（已赚余数原样保留），
//!   再把游标与 `FgState.since` 一并**冻结在暂停时刻**；**恢复时** `since=now`——暂停区间
//!   不交给 ledger、不产生秒数，全程无减法无负值（§4.6/§5.3 逐字语义；暂停期间的
//!   Foreground 事件只切换归属、不归账）。
//!
//! 归账本体（correctness-v2 §4.4）：aggregator 仍负责 OS 时间——elapsed 取自 Instant，
//! 墙钟 end 取本次 `Local::now()`，`start = end − elapsed` 转 naive 后交
//! [`AppTimeLedger`]：ledger 先把真实区间按本地午夜拆日，再对每个 `(day, exe)` 凑整，
//! 产出的整数秒并入 `AppAcc.secs`（持久整数秒含义不变，§4.6），不足 1 秒余数留账、
//! 跨归账/跨暂停接续。跨天 tick 时先把未归账区间完整归账，再
//! [`AppTimeLedger::discard_before`] 清过期日的尾差（§5.3-3）；flush 失败只回并整数秒桶、
//! ledger 不回滚不清空——重试不重复记秒（§5.3-5）；退出终账后 flush，余数随进程结束
//! 丢弃（§5.3-6）。测试经 `wall_now` 注入固定墙钟——Instant 归账端点与墙钟 end 同时可控，
//! 午夜边界确定性复现（禁止 sleep 逼近）。
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

use chrono::{DateTime, Local};
use clrecoder_core::day;
use clrecoder_core::event::{AggEvent, DeviceKey, RawEvent};
use clrecoder_core::motion::{
    DpiOrigin, EffectiveDpi, GamepadMotionFrame, MotionConnectionId, MouseSourceDescriptor,
    MouseSourceState, MouseTravelDelta, StickBinDelta, StickDayDelta, StickSide,
};
use clrecoder_engine::stick_motion::StickMotionTracker;
use clrecoder_engine::Engine;
use clrecoder_store::motion::{MouseMotionWrite, StickMotionWrite};
use clrecoder_store::writer::{FlushBatch, Writer};
use crossbeam_channel::{Receiver, RecvTimeoutError};

use crate::app_time::AppTimeLedger;
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
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct AppAcc {
    /// 前台秒数（按天切分后累加）
    secs: u64,
    /// 该应用内物理按键数（自动重复不计，§5.2）
    keys: u64,
    /// 该应用内鼠标点击数
    clicks: u64,
}

/// 鼠标运动桶键（motion-dpi §4.3/§4.4）：来源（key 身份）×捕获本地日×有效 DPI 桶。
/// 以 source_key 而非数据库 id 为键：来源注册失败时增量按 key 压缩保留、成功后
/// flush 时绑定 id（§4.4"不丢新 counts、不重新喂旧坐标"）。
type MotionBucketKey = (String, String, u32, DpiOrigin);

/// 鼠标运动来源的登记缓存行：描述（注册入参）+ 数据库 id（None=尚未注册成功）。
#[derive(Debug, Clone)]
struct MouseSourceRef {
    descriptor: MouseSourceDescriptor,
    id: Option<i64>,
}

/// 摇杆运动桶键（motion-dpi §4.4）：型号身份×捕获本地日×摇杆侧。以 DeviceKey 而非
/// 数据库 id 为键——型号注册失败时增量按 DeviceKey×日×side 压缩保留、成功后 flush
/// 绑定 id（"不丢新 counts、不重新喂旧坐标"）。
type StickBucketKey = (DeviceKey, String, StickSide);

/// 摇杆运动累计（motion-dpi §4.4）：活动微秒 + 路程（R）+ 各格停留微秒。
/// 不变量 `active_us == Σbins.dwell_us` 由 tracker 逐帧保证、累计保持。
#[derive(Debug, Default, Clone)]
struct StickAccum {
    active_us: u64,
    travel_r: f64,
    bins: HashMap<u16, u64>,
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
/// ≤0.5s 的未 flush 内存增量与 ledger 不足整秒余数——余数本就不落库，§5.3-6 同类语义，
/// `FgState`/`Flags` 不受影响）；收到 shutdown 或通道断开时排空余事件、终账、最后一批
/// flush 后正常退出。
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
        // panic 重建即全新聚合状态（≤0.5s 未 flush 增量 + ledger 各 (day,exe) <1s 余数），
        // 共享的 Flags/FgState/Writer 无恙。
        match catch_unwind(AssertUnwindSafe(|| aggregate_loop(&rx, &writer, &flags, &fg, &status)))
        {
            Ok(()) => return,
            Err(_) => {
                log::error!("aggregator 线程 panic（已捕获，重建聚合状态继续；≤0.5s 未 flush 增量与 ledger 余数丢失）");
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
        mouse_sources: HashMap::new(),
        mouse_connections: HashMap::new(),
        mouse_motion: HashMap::new(),
        motion_register_fail_streak: 0,
        stick_trackers: HashMap::new(),
        motion_epoch_observed: 0,
        stick_motion: HashMap::new(),
        ledger: AppTimeLedger::new(),
        wall_now: Local::now,
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

        // shutdown：排空余事件 → 终账（暂停中则冻结区间不归账）→ 最后一批 flush → 退出。
        // motion-dpi §4.3.1：看到 shutdown 但生产者未 drained 时继续接收已捕获增量，
        // drained 后才排空并最后 flush（不得提前 finish）。
        if flags.shutdown.load(Ordering::Acquire)
            && flags.producers_drained.load(Ordering::Acquire)
        {
            agg.finish(rx);
            return;
        }

        // 1s tick：刷新入桶日期（跨天检查，§4.6/§5.3——事件按到达时的本地日期入桶）。
        // 跨天时先把仍指向前一日的未归账区间完整交给 ledger（整秒与余数就位），再清
        // 过期日的不足 1 秒尾差——correctness-v2 §4.4/§5.3-3：discard_before 只能在完整
        // 归账区间处理完后调用；此后区间只可能从新日起算，旧日余数再无接续机会。
        if now.duration_since(agg.last_tick) >= TICK_INTERVAL {
            agg.last_tick = now;
            let today = day::today_local();
            if today != agg.today {
                agg.account_seconds(now);
                agg.ledger.discard_before(&today);
                agg.today = today;
            }
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
    /// 鼠标运动来源缓存：source_key → (描述, 数据库 id——None=尚未注册成功)
    /// （motion-dpi §4.4：注册失败按 source_key 压缩保留，成功后绑定 id 再 flush）
    mouse_sources: HashMap<String, MouseSourceRef>,
    /// 每来源已应用的最新连接代际（§4.4 代际门：旧代状态拒绝；进程重启首见即接受）
    mouse_connections: HashMap<String, u64>,
    /// `mouse_motion_daily` 桶：(source_key, day, dpi, origin) → counts
    /// （按 source_key 而非数据库 id 分桶：注册失败压缩保留，成功后 flush 绑定 id）
    mouse_motion: HashMap<MotionBucketKey, f64>,
    /// 鼠标来源注册连续失败次数（日志限频用）
    motion_register_fail_streak: u64,
    /// 摇杆 tracker：连接 → [左, 右]（motion-dpi §4.3：连接独立 tracker；暂停 epoch
    /// 变化/断连 reset——不跨短暂停连接坐标、不跨断点连线）
    stick_trackers: HashMap<MotionConnectionId, [StickMotionTracker; 2]>,
    /// 摇杆已观察的控制快照 epoch（§4.3.1：当前 epoch 变化 reset 全部摇杆 tracker）
    motion_epoch_observed: u64,
    /// `gamepad_motion_daily`/`gamepad_heat_daily` 桶：型号×日×side → 累计
    /// （按 DeviceKey 而非数据库 id 分桶：注册失败压缩保留，成功后 flush 绑定 id）
    stick_motion: HashMap<StickBucketKey, StickAccum>,
    /// 前台时长的亚秒余数账本（correctness-v2 §4.4）：按 `(day, exe)` 累计不足整秒零头、
    /// 凑满整秒经 [`AppTimeLedger::account_interval`] 产出并入 `AppAcc.secs`。与统计桶
    /// 生命周期分离——flush 成功不清账、失败回并桶时也不回滚/清账（§5.3-5）。
    ledger: AppTimeLedger,
    /// 墙钟读取器：归账 end 的来源（§4.4 aggregator 负责 OS 时间）。生产为 `Local::now`；
    /// 测试注入固定端点，与 Instant 归账端点同时确定——禁止一端真实时钟造成午夜偶发。
    wall_now: fn() -> DateTime<Local>,
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
            AggEvent::Foreground { exe } => self.on_foreground(exe, paused, Instant::now()),
            AggEvent::Input(raw) => {
                // 收到即计（§4.4 StatusData.events_seen = "累计收到的事件数"，含暂停丢弃的）
                self.status.record_event();
                if paused {
                    // 键盘边沿照常喂状态机（按来源维护 held/修饰键，输出丢弃不计数），
                    // 防止暂停期松开/按下的键在恢复后被误判（漏计首按 + 幽灵组合键）。
                    // 其余 RawEvent 变体无内部状态可陈旧，照旧整条丢弃。
                    if let RawEvent::Keyboard { source, sc, down, .. } = raw {
                        let _ = self.engine.on_key_from(source, sc, down); // EngineOut 带 #[must_use]
                    }
                    return;
                }
                self.on_input(raw);
            }
            // 生命周期控制事件（correctness-v2 §4.3/§5.2）：无论 paused 均执行——暂停中
            // 拔出设备/loop 重建同样要清 Engine 来源按下状态，否则恢复后产生幽灵组合键；
            // 不写统计、不记 events_seen、不动 paused/shutdown（"全局清空不能替代来源移除"）。
            AggEvent::SourceRemoved { source } => self.engine.remove_source(source),
            AggEvent::KeyboardSourcesReset => self.engine.clear_sources(),
            // 运动事件（motion-dpi §4.1/§4.3）：无论 paused 均处理（暂停过滤在采集侧按
            // 捕获 control 完成，已捕获为非暂停的增量无条件落库）；不增加 events_seen、
            // 不写按钮统计、不计入应用键鼠次数。
            AggEvent::MouseSourceState(state) => self.on_mouse_source_state(state),
            AggEvent::MouseTravel(delta) => self.on_mouse_travel(delta),
            // 手柄运动事件（motion-dpi §4.3，S5）：同样不增加 events_seen/按钮总量
            AggEvent::GamepadMotion(frame) => self.on_gamepad_motion(frame),
            AggEvent::GamepadMotionDisconnected { connection } => {
                // 断连/读取失败/上下文重建：清该连接全部摇杆 tracker（不跨断点连线）。
                // 未知连接为 no-op（防御兜底，绝不 panic）。
                self.stick_trackers.remove(&connection);
            }
        }
    }

    /// Foreground 事件：先把旧 exe 的未归账尾段落桶，再切换归属（Foreground 照常处理，
    /// 保持 exe 归属正确，§4.6）。暂停期间只切换归属、不归账（区间冻结）。
    /// 调用前 `handle_event` 已完成沿消解——`paused` 即当前真实沿状态。
    /// `now` 为事件消费时刻（§5.3-1 沿用既有实现；私有测试入口：与墙钟 end 同时可控）。
    fn on_foreground(&mut self, exe: String, paused: bool, now: Instant) {
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
            RawEvent::Keyboard { source, device, sc, down } => {
                // 引擎状态先行：up 边沿/自动重复在此消化（§4.3；按来源隔离，F3）
                let out = self.engine.on_key_from(source, sc, down);
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

    // ---------- 鼠标运动事件（motion-dpi §4.3/§4.4） ----------

    /// MouseSourceState：注册来源（按 source_key 幂等）并落库状态快照。
    ///
    /// 连接代际门（§4.4）：旧代 disconnect 不得把新连接标离线——已应用的代际更大时
    /// 整条拒绝；进程重启首见即接受（内存表为空，不与上个进程数字比较）。元数据非
    /// 增量：写入失败不丢计数，约 2s 心跳会重发（§4.4"幂等写入失败可重试"）。
    fn on_mouse_source_state(&mut self, state: MouseSourceState) {
        let key = state.descriptor.source_key.clone();
        if let Some(&current) = self.mouse_connections.get(&key) {
            if state.connection.0 < current {
                return; // 旧代状态拒绝（不回退新连接状态）
            }
        }
        self.mouse_connections.insert(key.clone(), state.connection.0);
        self.note_descriptor(&state.descriptor);
        let Some(id) = self.mouse_source_id(&key) else { return };
        if let Err(e) = self.writer.update_mouse_source_state(id, &state) {
            log::warn!("鼠标来源状态写入失败（心跳会重试，不影响计数）: {e}");
        }
    }

    /// MouseTravel：按 (source_key, 捕获日, dpi, origin) 桶累计。暂停过滤在采集侧
    /// 按捕获 control 完成——此处无条件落库（§4.3"已捕获为非暂停的独立 counts 增量
    /// 可落库"）。dpi×origin 配对归一到 store 约束（dpi=0 ↔ unknown）。
    fn on_mouse_travel(&mut self, delta: MouseTravelDelta) {
        if !(delta.counts.is_finite() && delta.counts > 0.0) {
            return; // 防御兜底：非有限/零增量不入桶（§1 原则 3）
        }
        self.note_descriptor(&delta.descriptor);
        let (dpi, origin) = match delta.dpi {
            EffectiveDpi { value: Some(v), origin } if origin != DpiOrigin::Unknown && v > 0 => {
                (v, origin)
            }
            _ => (0, DpiOrigin::Unknown),
        };
        *self
            .mouse_motion
            .entry((delta.descriptor.source_key, delta.day, dpi, origin))
            .or_insert(0.0) += delta.counts;
    }

    // ---------- 手柄摇杆运动事件（motion-dpi §4.3/§4.4，S5） ----------

    /// GamepadMotion：喂连接独立的摇杆 tracker（§4.3.1 暂停条款逐字）。
    ///
    /// - 每帧前读 [`Flags::motion_control`]：当前 epoch 变化 → reset **全部**摇杆
    ///   tracker（短暂停再恢复也换代际，不跨暂停积分）；
    /// - 捕获 epoch 与当前不一致，或任一 paused → reset/跳过该帧——不跨短暂停连接坐标；
    /// - 其余帧喂 [左, 右] 两个 tracker（首帧仅建锚点），产出增量按（型号, 日, side）
    ///   入桶——绑定 device_id 延后到 flush（注册失败压缩保留）。
    fn on_gamepad_motion(&mut self, frame: GamepadMotionFrame) {
        let current = self.flags.motion_control();
        if current.epoch != self.motion_epoch_observed {
            self.motion_epoch_observed = current.epoch;
            self.stick_trackers.clear();
        }
        if frame.control.epoch != current.epoch || frame.control.paused || current.paused {
            self.stick_trackers.remove(&frame.connection);
            return;
        }
        let mut deltas: Vec<StickDayDelta> = Vec::new();
        {
            let pair = self
                .stick_trackers
                .entry(frame.connection)
                .or_insert_with(|| [StickMotionTracker::new(), StickMotionTracker::new()]);
            deltas.extend(pair[0].feed(frame.stamp, StickSide::Left, frame.left));
            deltas.extend(pair[1].feed(frame.stamp, StickSide::Right, frame.right));
        }
        for delta in deltas {
            self.accumulate_stick(&frame.device, delta);
        }
    }

    /// 摇杆增量入桶（型号×捕获日×side）。tracker 保证增量有限非负且
    /// `active_us == Σbins.dwell_us`；累计保持该不变量（dwell=0 的格不入桶）。
    fn accumulate_stick(&mut self, device: &DeviceKey, delta: StickDayDelta) {
        let entry = self
            .stick_motion
            .entry((device.clone(), delta.day, delta.side))
            .or_default();
        entry.active_us = entry.active_us.saturating_add(delta.active_us);
        entry.travel_r += delta.travel_r;
        for b in delta.bins {
            if b.dwell_us > 0 {
                *entry.bins.entry(b.bin).or_insert(0) += b.dwell_us;
            }
        }
    }

    /// 登记来源描述（首次见到时缓存；注册/绑定延后到 flush）。
    fn note_descriptor(&mut self, descriptor: &MouseSourceDescriptor) {
        self.mouse_sources
            .entry(descriptor.source_key.clone())
            .or_insert_with(|| MouseSourceRef { descriptor: descriptor.clone(), id: None });
    }

    /// source_key → 数据库 id（缓存命中零 SQL）。注册失败限频记录并返回 None——
    /// 增量按 source_key 压缩保留在桶内，注册成功后的 flush 再绑定（§4.4）。
    fn mouse_source_id(&mut self, source_key: &str) -> Option<i64> {
        if let Some(r) = self.mouse_sources.get(source_key) {
            if let Some(id) = r.id {
                return Some(id);
            }
        }
        let descriptor = self.mouse_sources.get(source_key)?.descriptor.clone();
        match self.writer.register_mouse_source(&descriptor) {
            Ok(id) => {
                self.motion_register_fail_streak = 0;
                if let Some(r) = self.mouse_sources.get_mut(source_key) {
                    r.id = Some(id);
                }
                Some(id)
            }
            Err(e) => {
                self.motion_register_fail_streak += 1;
                if self.motion_register_fail_streak == 1
                    || self.motion_register_fail_streak.is_multiple_of(DEVICE_LOG_EVERY)
                {
                    log::error!(
                        "鼠标运动来源注册失败（连续第 {} 次），增量按 source_key 保留重试: {e}",
                        self.motion_register_fail_streak
                    );
                }
                None
            }
        }
    }

    // ---------- 前台秒数归账 ----------

    /// 暂停/恢复沿处理（§4.6 逐字语义；correctness-v2 §4.4：暂停沿先归账、冻结游标、
    /// 保留已赚余数——余数不因暂停丢弃，恢复后同日同 exe 接续凑整）。
    fn on_pause_transition(&mut self, paused: bool, now: Instant) {
        self.paused_observed = paused;
        if paused {
            // 暂停瞬间：先把 FgState 增量秒数归账一次，再把 since 冻结在暂停时刻。
            // （旗标置位到本线程观察到的 ≤100ms 滞后按暂停前时间计入，属可忽略斜差。）
            // 已赚的不足整秒余数原样留在 ledger（§4.4），恢复后接续凑整。
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

    /// 归账核心：游标尾段 `[since → now]` 交 [`AppTimeLedger`] 处理（correctness-v2 §4.4）
    /// ——ledger 先把真实区间按本地午夜拆日、再对每个 `(day, exe)` 凑整，产出的整数秒
    /// 并入 `app_daily` 桶（`AppAcc.secs` 持久整数秒含义不变），不足 1 秒余数留账、
    /// 跨归账/跨暂停接续；随后推进游标。
    ///
    /// OS 时间归属（§4.4）：elapsed 取自 Instant，墙钟 end 经 [`Aggregator::wall_now`]
    /// （生产 `Local::now()`，测试注入固定端点），`start = end − elapsed` 转 naive。
    /// 零长区间不归账；`Instant` 反向差值饱和为 0、走同一路径——绝不产生负值（§4.6）。
    fn account_seconds_unchecked(&mut self, now: Instant) {
        let Some((exe, since)) = self.fg_cur.as_ref().map(|(e, s)| (e.clone(), *s)) else {
            return;
        };
        let elapsed = now.duration_since(since);
        if elapsed.is_zero() {
            return;
        }
        // 以本次墙钟为 end、减去 elapsed 得 start（§4.4 逐字）；DateTime − Duration 为
        // instant 语义（与旧 TimeDelta 减法一致），转 naive 后交 ledger 按本地午夜拆分
        let end_wall = (self.wall_now)();
        let start_wall = end_wall - elapsed;
        for delta in self
            .ledger
            .account_interval(&exe, start_wall.naive_local(), end_wall.naive_local())
        {
            self.app(&delta.day, &delta.exe).secs += delta.seconds;
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
    ///
    /// motion-dpi §4.4：鼠标运动增量与摇杆运动增量同旧统计**同一个事务**——任何部分
    /// 失败全回滚、全部合回；运动行先经来源/型号注册绑定数据库 id，注册失败的 key 按
    /// source_key / DeviceKey×日×side 压缩保留在桶内（不下批、不丢 counts），注册成功
    /// 后随下批 flush。空批次提前返回判定纳入两类运动字段（纯摇杆热度批也必须落库）。
    fn flush_buckets(&mut self) {
        let inputs = std::mem::take(&mut self.inputs);
        let combos = std::mem::take(&mut self.combos);
        let apps = std::mem::take(&mut self.apps);
        let mouse_move = std::mem::take(&mut self.mouse_move);
        let mouse_motion = std::mem::take(&mut self.mouse_motion);
        let stick_motion = std::mem::take(&mut self.stick_motion);
        if inputs.is_empty()
            && combos.is_empty()
            && apps.is_empty()
            && mouse_move.is_empty()
            && mouse_motion.is_empty()
            && stick_motion.is_empty()
        {
            return; // 不脏：无写库（§4.6"若脏"）
        }
        // 来源注册 + 行装配：解析成功的进批次（行与桶键同行携带，便于失败整桶回并），
        // 失败的压缩保留（重试后再绑定）
        let mut rows: Vec<(MotionBucketKey, MouseMotionWrite)> =
            Vec::with_capacity(mouse_motion.len());
        let mut kept = HashMap::new();
        for (key, counts) in mouse_motion {
            match self.mouse_source_id(&key.0) {
                Some(id) => rows.push((
                    key.clone(),
                    MouseMotionWrite {
                        source_id: id,
                        day: key.1.clone(),
                        dpi: key.2,
                        origin: key.3,
                        counts,
                    },
                )),
                None => {
                    kept.insert(key, counts);
                }
            }
        }
        // 摇杆行装配（型号注册 + bins 按格号升序装配，确定性批序）
        let mut stick_rows: Vec<(StickBucketKey, StickMotionWrite)> =
            Vec::with_capacity(stick_motion.len());
        let mut stick_kept: HashMap<StickBucketKey, StickAccum> = HashMap::new();
        for (key, acc) in stick_motion {
            match self.device_id(&key.0) {
                Some(id) => {
                    let mut bins: Vec<StickBinDelta> = acc
                        .bins
                        .iter()
                        .map(|(bin, dwell_us)| StickBinDelta { bin: *bin, dwell_us: *dwell_us })
                        .collect();
                    bins.sort_by_key(|b| b.bin);
                    stick_rows.push((
                        key.clone(),
                        StickMotionWrite {
                            device_id: id,
                            day: key.1.clone(),
                            side: key.2,
                            active_us: acc.active_us,
                            travel_r: acc.travel_r,
                            bins,
                        },
                    ));
                }
                None => {
                    stick_kept.insert(key, acc);
                }
            }
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
            mouse_motion: rows.iter().map(|(_, w)| w.clone()).collect(),
            stick_motion: stick_rows.iter().map(|(_, w)| w.clone()).collect(),
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
            // 运动行合回（同键压缩累计）：已装配行与注册失败保留的 kept 都不丢
            for (key, w) in rows {
                *self.mouse_motion.entry(key).or_insert(0.0) += w.counts;
            }
            for (key, counts) in kept {
                *self.mouse_motion.entry(key).or_insert(0.0) += counts;
            }
            for (key, w) in stick_rows {
                let entry = self.stick_motion.entry(key).or_default();
                entry.active_us = entry.active_us.saturating_add(w.active_us);
                entry.travel_r += w.travel_r;
                for b in w.bins {
                    if b.dwell_us > 0 {
                        *entry.bins.entry(b.bin).or_insert(0) += b.dwell_us;
                    }
                }
            }
            for (key, acc) in stick_kept {
                let entry = self.stick_motion.entry(key).or_default();
                entry.active_us = entry.active_us.saturating_add(acc.active_us);
                entry.travel_r += acc.travel_r;
                for (bin, dwell_us) in acc.bins {
                    if dwell_us > 0 {
                        *entry.bins.entry(bin).or_insert(0) += dwell_us;
                    }
                }
            }
        } else {
            self.flush_fail_streak = 0;
            // 成功：注册失败保留的部分继续留在桶内（下个 tick 重试绑定）
            self.mouse_motion = kept;
            self.stick_motion = stick_kept;
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
/// 参数多是 flush 回滚语义的直接映射（四类桶 × 当前+回滚）；私有辅助，不改聚合契约。
#[allow(clippy::too_many_arguments)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::path::{Path, PathBuf};

    use clrecoder_core::codes::{mods, DeviceKind};
    use clrecoder_core::event::InputSourceId;
    use clrecoder_core::motion::{
        local_day_from_unix_us, GamepadMotionFrame, MotionConnectionId, MotionControlSnapshot,
        MotionStamp, StickPoint, StickSide,
    };

    // ---------------- correctness-v2 §4.4：前台时长 ledger 接线（S4/F4） ----------------
    //
    // 旧 fg_residual_ms 标量与整秒拆分 helper（split_secs_by_day_naive）退役：余数改由
    // AppTimeLedger 按 (day, exe) 记账、先拆日再凑整。下列测试把原拆分 helper 的跨日/
    // 反向/边界意图迁移到 aggregator 集成路径——墙钟经 `wall_now` 注入、Instant 端点经
    // 参数注入，两侧同时确定（§4.4：禁止一端真实时钟造成午夜偶发、禁止 sleep 逼近）。

    /// `wall_noon` 注入的固定"今日"（app 桶断言键；`day::format_day` 输出同形）。
    const WALL_DAY: &str = "2026-09-28";

    /// 固定本地墙钟构造（`with_ymd_and_hms` 整点秒，避开 DST 歧义时刻）。
    fn wall_at(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    /// 默认注入端点：2026-09-28 12:00:00（同日测试用）。
    fn wall_noon() -> DateTime<Local> {
        wall_at(2026, 9, 28, 12, 0, 0)
    }

    /// 跨午夜 end：2026-09-29 00:00:01。
    fn wall_just_past_midnight() -> DateTime<Local> {
        wall_at(2026, 9, 29, 0, 0, 1)
    }

    /// 恰好午夜 end：2026-09-29 00:00:00（区间右开语义）。
    fn wall_at_midnight() -> DateTime<Local> {
        wall_at(2026, 9, 29, 0, 0, 0)
    }

    /// 年界 end：2027-01-01 00:00:01。
    fn wall_year_boundary() -> DateTime<Local> {
        wall_at(2027, 1, 1, 0, 0, 1)
    }

    /// 月界 end：2027-03-01 00:00:01。
    fn wall_month_boundary() -> DateTime<Local> {
        wall_at(2027, 3, 1, 0, 0, 1)
    }

    /// 休眠跨多天 end：2026-09-30 06:00:00。
    fn wall_multiday_end() -> DateTime<Local> {
        wall_at(2026, 9, 30, 6, 0, 0)
    }

    /// 汇总 app 桶全部整数秒（跨日守恒断言用）。
    fn total_secs(agg: &Aggregator) -> u64 {
        agg.apps.values().map(|a| a.secs).sum()
    }

    /// 回归（原 half_second_flush_segments_accumulate_via_residual 意图迁移）：0.5s 一段地
    /// 归账时，余数必须经 ledger 把两个 0.5s 接成 1 秒，而不是各截断成 0；flush 成功不清
    /// 余数——余下零头继续接续凑整（§5.3-3）。
    #[test]
    fn correctness_v2_half_second_flushes_accumulate_via_ledger_remainder() {
        let (mut agg, db) = agg_with_frozen_cursor("ledger-residual", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let t0 = Instant::now();
        agg.fg_cur = Some(("code.exe".to_string(), t0));
        // 模拟 0.5s 周期 flush 的逐次归账（每段 elapsed 恰 500ms）
        agg.account_seconds(t0 + Duration::from_millis(500));
        assert_eq!(total_secs(&agg), 0, "首段 0.5s 不足整秒，不得产出");
        agg.account_seconds(t0 + Duration::from_millis(1000));
        assert_eq!(total_secs(&agg), 1, "两个 0.5s 必须接续凑出 1 秒");
        // flush 成功保留余数：第三次归账余 500ms，不产出新整秒
        agg.account_seconds(t0 + Duration::from_millis(1500));
        assert_eq!(total_secs(&agg), 1, "flush 成功不清 ledger 余数");
        agg.account_seconds(t0 + Duration::from_millis(2000));
        assert_eq!(total_secs(&agg), 2, "余数接续凑出第 2 秒");
        drop(agg);
        cleanup_db(&db);
    }

    /// §4.4 精确示例：A 400ms / B 600ms 前台交替 20 轮 → A 8 秒、B 12 秒。
    /// Foreground 切换先按旧 exe 归账再移动游标（§5.3-1），余数跨切换分段接续。
    #[test]
    fn correctness_v2_foreground_switch_ab_alternating_20_rounds_settle_8_and_12_seconds() {
        let (mut agg, db) = agg_with_frozen_cursor("fg-ab-8-12", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let t0 = Instant::now();
        agg.fg_cur = Some(("a.exe".to_string(), t0));
        for round in 0..20u32 {
            let base = u64::from(round) * 1000;
            // A 用满 400ms 后切到 B：先归账 A 的尾段，再切游标
            agg.on_foreground("b.exe".to_string(), false, t0 + Duration::from_millis(base + 400));
            // B 用满 600ms 后切回 A
            agg.on_foreground("a.exe".to_string(), false, t0 + Duration::from_millis(base + 1000));
        }
        let a = &agg.apps[&(WALL_DAY.to_string(), "a.exe".to_string())];
        let b = &agg.apps[&(WALL_DAY.to_string(), "b.exe".to_string())];
        assert_eq!(a.secs, 8, "20×400ms 必须凑出整 8 秒");
        assert_eq!(b.secs, 12, "20×600ms 必须凑出整 12 秒");
        assert_eq!(agg.apps.len(), 2, "同日恰两个 (day,exe) 桶: {:?}", agg.apps);
        drop(agg);
        cleanup_db(&db);
    }

    /// §4.4 精确示例（暂停接续）：暂停沿先归账 A 400ms 并冻结游标（已赚余数保留），
    /// 暂停 100 秒不归账，恢复重设游标后 A 600ms → 恰 1 秒（暂停区间不交给 ledger）。
    #[test]
    fn correctness_v2_pause_edge_accounts_then_freezes_and_remainder_carries_over() {
        let (mut agg, db) = agg_with_frozen_cursor("pause-remainder", Duration::ZERO);
        agg.paused_observed = false;
        let t0 = Instant::now();
        agg.fg_cur = Some(("a.exe".to_string(), t0));
        // 暂停沿（pipe 已置位）在 t0+400ms：先归账 400ms（不足整秒→余数留账），再冻结
        agg.flags.paused.store(true, Ordering::Release);
        agg.on_pause_transition(true, t0 + Duration::from_millis(400));
        assert!(agg.paused_observed);
        assert_eq!(total_secs(&agg), 0, "400ms 不足整秒，整数秒桶为空");
        // 暂停 100 秒：区间不归账；恢复沿（pipe 已复位）重设游标
        let t_resume = t0 + Duration::from_millis(400) + Duration::from_secs(100);
        agg.flags.paused.store(false, Ordering::Release);
        agg.on_pause_transition(false, t_resume);
        // 恢复后 A 600ms：与暂停前 400ms 接续凑整 → 恰 1 秒，暂停 100s 不计入
        agg.account_seconds(t_resume + Duration::from_millis(600));
        let a = &agg.apps[&(WALL_DAY.to_string(), "a.exe".to_string())];
        assert_eq!(a.secs, 1, "400ms+600ms 接续凑出 1 秒");
        assert_eq!(agg.apps.len(), 1, "暂停区间与恢复时刻不得产生其他桶: {:?}", agg.apps);
        drop(agg);
        cleanup_db(&db);
    }

    /// 原 midnight_crossing_splits_into_two_days 意图迁移：归账区间横跨本地午夜时，
    /// 整数秒按日分桶——先拆日再凑整，两日桶各自独立成秒、不互相借零头（§5.3-2）。
    #[test]
    fn correctness_v2_cross_midnight_interval_splits_into_two_day_buckets() {
        let (mut agg, db) = agg_with_frozen_cursor("cross-midnight", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.wall_now = wall_just_past_midnight; // end = 2026-09-29 00:00:01
        let t0 = Instant::now();
        agg.fg_cur = Some(("a.exe".to_string(), t0));
        // elapsed 5s → naive 区间 [2026-09-28 23:59:56 → 2026-09-29 00:00:01]
        agg.account_seconds(t0 + Duration::from_secs(5));
        let secs_of = |day: &str| agg.apps[&(day.to_string(), "a.exe".to_string())].secs;
        assert_eq!(secs_of("2026-09-28"), 4, "午夜前的 4 秒归前一日");
        assert_eq!(secs_of("2026-09-29"), 1, "午夜后的 1 秒归次日");
        assert_eq!(agg.apps.len(), 2, "恰两个日桶: {:?}", agg.apps);
        drop(agg);
        cleanup_db(&db);
    }

    /// 原 end_exactly_at_midnight_belongs_to_previous_day 意图迁移：区间右开——恰好
    /// 结束于午夜零点的时长全归前一日，次日不产生桶。
    #[test]
    fn correctness_v2_interval_ending_exactly_at_midnight_belongs_to_previous_day() {
        let (mut agg, db) = agg_with_frozen_cursor("end-at-midnight", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.wall_now = wall_at_midnight; // end = 2026-09-29 00:00:00
        let t0 = Instant::now();
        agg.fg_cur = Some(("a.exe".to_string(), t0));
        // elapsed 2s → [2026-09-28 23:59:58 → 2026-09-29 00:00:00]
        agg.account_seconds(t0 + Duration::from_secs(2));
        assert_eq!(
            agg.apps[&("2026-09-28".to_string(), "a.exe".to_string())].secs,
            2,
            "右开区间整段归前一日"
        );
        assert!(
            !agg.apps.contains_key(&("2026-09-29".to_string(), "a.exe".to_string())),
            "次日零点时刻不属于次日: {:?}",
            agg.apps
        );
        drop(agg);
        cleanup_db(&db);
    }

    /// 原 multi_day_span_is_split_per_calendar_day 意图迁移：休眠跨多天（Instant 连续
    /// 计时、墙钟端点固定）按日历日逐段切分，各日独立成秒、总量守恒。
    #[test]
    fn correctness_v2_multi_day_span_splits_per_calendar_day() {
        let (mut agg, db) = agg_with_frozen_cursor("multi-day", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.wall_now = wall_multiday_end; // end = 2026-09-30 06:00:00
        let t0 = Instant::now();
        agg.fg_cur = Some(("a.exe".to_string(), t0));
        // elapsed 56h → naive 区间 [2026-09-28 22:00 → 2026-09-30 06:00]
        let elapsed_secs: u64 = 2 * 3600 + 86_400 + 6 * 3600;
        agg.account_seconds(t0 + Duration::from_secs(elapsed_secs));
        assert_eq!(agg.apps[&("2026-09-28".to_string(), "a.exe".to_string())].secs, 2 * 3600);
        assert_eq!(agg.apps[&("2026-09-29".to_string(), "a.exe".to_string())].secs, 86_400);
        assert_eq!(agg.apps[&("2026-09-30".to_string(), "a.exe".to_string())].secs, 6 * 3600);
        assert_eq!(total_secs(&agg), elapsed_secs, "切分不丢总量");
        drop(agg);
        cleanup_db(&db);
    }

    /// 原 year_boundary_and_month_boundary_split_correctly 意图迁移：年界/月界
    /// （2027-02-28 → 03-01，非闰年）同样按日历日切分。
    #[test]
    fn correctness_v2_year_and_month_boundary_accounting_split_correctly() {
        // 年界：end 2027-01-01 00:00:01、elapsed 2s → 2026-12-31 1 秒 + 2027-01-01 1 秒
        {
            let (mut agg, db) = agg_with_frozen_cursor("year-boundary", Duration::ZERO);
            agg.paused_observed = false;
            agg.flags.paused.store(false, Ordering::Release);
            agg.wall_now = wall_year_boundary;
            let t0 = Instant::now();
            agg.fg_cur = Some(("a.exe".to_string(), t0));
            agg.account_seconds(t0 + Duration::from_secs(2));
            assert_eq!(agg.apps[&("2026-12-31".to_string(), "a.exe".to_string())].secs, 1);
            assert_eq!(agg.apps[&("2027-01-01".to_string(), "a.exe".to_string())].secs, 1);
            drop(agg);
            cleanup_db(&db);
        }
        // 月界：end 2027-03-01 00:00:01、elapsed 2s → 2027-02-28 1 秒 + 2027-03-01 1 秒
        {
            let (mut agg, db) = agg_with_frozen_cursor("month-boundary", Duration::ZERO);
            agg.paused_observed = false;
            agg.flags.paused.store(false, Ordering::Release);
            agg.wall_now = wall_month_boundary;
            let t0 = Instant::now();
            agg.fg_cur = Some(("a.exe".to_string(), t0));
            agg.account_seconds(t0 + Duration::from_secs(2));
            assert_eq!(agg.apps[&("2027-02-28".to_string(), "a.exe".to_string())].secs, 1);
            assert_eq!(agg.apps[&("2027-03-01".to_string(), "a.exe".to_string())].secs, 1);
            drop(agg);
            cleanup_db(&db);
        }
    }

    /// 原 inverted_or_zero_interval_yields_nothing_and_never_negative 意图迁移：零长区间
    /// 不归账、不扰动余数（Instant 端点经 duration_since 对反向差值饱和为 0、走同一路径，
    /// 绝不负值；ledger 侧的反向区间语义由 app_time::tests 覆盖）。
    #[test]
    fn correctness_v2_zero_elapsed_accounts_nothing_and_keeps_remainder() {
        let (mut agg, db) = agg_with_frozen_cursor("zero-elapsed", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let t0 = Instant::now();
        agg.fg_cur = Some(("a.exe".to_string(), t0));
        // 先赚 400ms 余数
        agg.account_seconds(t0 + Duration::from_millis(400));
        assert_eq!(total_secs(&agg), 0);
        // 零长区间（now == since）：不产出、游标与余数原样
        agg.account_seconds(t0 + Duration::from_millis(400));
        assert_eq!(total_secs(&agg), 0, "零长区间不得产出");
        // 余数未受扰动：补 600ms 恰好凑整 1 秒
        agg.account_seconds(t0 + Duration::from_millis(1000));
        assert_eq!(total_secs(&agg), 1, "零长区间不得扰动余数（400ms 应原样接续）");
        assert_eq!(agg.apps.len(), 1, "零长区间不得产生其他桶");
        drop(agg);
        cleanup_db(&db);
    }

    /// §5.3-5/§8.3：flush 失败（输入桶放入不存在的 device_id → 现有外键事务失败）回并
    /// app 整数秒桶、ledger 不回滚不清空；清除标记重试后成功清桶、整数秒不重复
    /// （重试批次内容在其发送前逐点断言；库级事务原子性由 store 侧既有测试覆盖）。
    #[test]
    fn correctness_v2_failed_flush_merges_app_secs_back_and_retry_does_not_double_count() {
        let (mut agg, db) = agg_with_frozen_cursor("failed-flush", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let day = WALL_DAY.to_string();
        let t0 = Instant::now();
        agg.fg_cur = Some(("a.exe".to_string(), t0));
        // 归账 1.5s：1 整秒入桶、500ms 余数留 ledger
        agg.account_seconds(t0 + Duration::from_millis(1500));
        assert_eq!(agg.apps[&(day.clone(), "a.exe".to_string())].secs, 1);
        // 测试标记：输入桶放入不存在的 device_id，令现有外键事务失败（§8.3 允许手法）
        let ghost_device = 7_654_321i64;
        agg.inputs.insert((ghost_device, day.clone(), 0x1E), 1);
        agg.flush_buckets();
        assert_eq!(agg.flush_fail_streak, 1, "外键事务必须失败");
        // app 整数秒桶回并不丢；ledger 不回滚不清空——再归账 500ms 与留账余数接续，
        // 恰好新产出 1 整秒（若失败时清空/重复提取 ledger，这里不会恰为 2）
        assert_eq!(agg.apps[&(day.clone(), "a.exe".to_string())].secs, 1, "失败必须回并 app 桶");
        agg.account_seconds(t0 + Duration::from_secs(2));
        assert_eq!(agg.apps[&(day.clone(), "a.exe".to_string())].secs, 2);
        // 清除测试标记重试：成功（streak 清零）；成功路径清桶——已写库的整数秒不残留
        // 桶内，下一轮 flush 不会重复发送（§5.3-5"重试不会重复记秒"的聚合侧语义；
        // §8.3：校验全部走聚合桶内存态，不为读库引入 rusqlite）
        agg.inputs.remove(&(ghost_device, day.clone(), 0x1E));
        agg.flush_buckets();
        assert_eq!(agg.flush_fail_streak, 0, "重试应成功");
        assert!(agg.apps.is_empty(), "成功 flush 必须清桶，不残留已写库的整秒: {:?}", agg.apps);
        assert!(agg.inputs.is_empty(), "ghost 标记清除后不得残留输入行: {:?}", agg.inputs);
        // 失败-重试周期后管道照常：余数精确耗尽（1.5s+0.5s=2s），两段 500ms 接续凑出
        // 第 3 秒并成功落库清桶——后续归账与 flush 不受失败周期影响
        agg.account_seconds(t0 + Duration::from_millis(2500));
        assert!(agg.apps.is_empty(), "余数精确耗尽后 500ms 不得提前产出: {:?}", agg.apps);
        agg.account_seconds(t0 + Duration::from_millis(3000));
        assert_eq!(
            agg.apps.get(&(day, "a.exe".to_string())).map(|a| a.secs),
            Some(1),
            "余数接续凑出第 3 秒: {:?}",
            agg.apps
        );
        agg.flush_buckets();
        assert!(agg.apps.is_empty(), "第 3 秒应成功落库并清桶");
        drop(agg);
        cleanup_db(&db);
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
    /// 墙钟固定在 wall_noon（2026-09-28 12:00:00）——归账端点两侧同时确定；
    /// 需要跨天/年月界端点的测试再覆写 `wall_now`。
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
            mouse_sources: HashMap::new(),
            mouse_connections: HashMap::new(),
            mouse_motion: HashMap::new(),
            motion_register_fail_streak: 0,
            stick_trackers: HashMap::new(),
            motion_epoch_observed: 0,
            stick_motion: HashMap::new(),
            ledger: AppTimeLedger::new(),
            wall_now: wall_noon,
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

    fn cleanup_db(db: &Path) {
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
        // （归账桶的日期取注入墙钟 wall_noon 的固定日）
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(AggEvent::Foreground { exe: "beta.exe".to_string() });
        let alpha = &agg.apps[&(WALL_DAY.to_string(), "alpha.exe".to_string())];
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
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(1),
            device: dev.clone(),
            sc: 0x1E,
            down: true,
        }));
        let dev_id = agg.devices.get(&dev).copied().unwrap();
        let day = day::today_local();
        assert_eq!(agg.inputs.get(&(dev_id, day.clone(), 0x1E)), Some(&1), "恢复后的按键应计数");
        // 暂停冻结区间不得入账（app 桶断言键取注入墙钟的固定日）
        assert!(
            !agg.apps.contains_key(&(WALL_DAY.to_string(), "alpha.exe".to_string())),
            "暂停区间不得入账: {:?}",
            agg.apps
        );
        drop(agg);
        cleanup_db(&db);
    }

    /// 暂停期键盘边沿仍喂 Engine（held/修饰键维护），输出丢弃不计数；
    /// 恢复后首按不被当自动重复吞掉，且不产生幽灵组合键。
    #[test]
    fn paused_keyboard_edges_keep_engine_state_fresh() {
        let (mut agg, db) = agg_with_frozen_cursor("paused-kb", Duration::from_secs(2));
        // helper 只设 paused_observed:true、flag 默认 false——必须显式置位，
        // 否则首条事件会走恢复沿消解，测试假绿。
        agg.flags.paused.store(true, Ordering::Release);

        let dev = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 1,
            pid: 2,
            name: "测试键盘".to_string(),
        };
        let day = day::today_local();

        // 暂停中键盘按下：边沿仍喂 engine，但 inputs/combos/apps 全空
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(1),
            device: dev.clone(),
            sc: 0x1E,
            down: true,
        }));
        assert!(agg.inputs.is_empty(), "暂停中按下不得入 input_daily: {:?}", agg.inputs);
        assert!(agg.combos.is_empty(), "暂停中不得入 combo_daily: {:?}", agg.combos);
        assert!(agg.apps.is_empty(), "暂停中不得入 app_daily: {:?}", agg.apps);

        // 恢复：先 up 再 down——首按恰计 1（paused 期间的 down 边沿已喂 engine，
        // 恢复后的 down 不会被当自动重复吞掉，也不重复计）
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(1),
            device: dev.clone(),
            sc: 0x1E,
            down: false,
        }));
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(1),
            device: dev.clone(),
            sc: 0x1E,
            down: true,
        }));
        let dev_id = agg.devices.get(&dev).copied().unwrap();
        assert_eq!(agg.inputs.get(&(dev_id, day.clone(), 0x1E)), Some(&1), "恢复后首按应恰计 1");
        assert!(agg.combos.is_empty(), "单键不得产生组合: {:?}", agg.combos);

        // 推荐附加：暂停前 Ctrl down → 暂停期 Ctrl up → 恢复后 C down → combos 仍空
        // （暂停期 up 边沿必须喂 engine 维护 held，否则 held 残留 Ctrl 会幽灵 Ctrl+C）
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(1),
            device: dev.clone(),
            sc: 0x1D,
            down: true,
        }));
        agg.flags.paused.store(true, Ordering::Release);
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(1),
            device: dev.clone(),
            sc: 0x1D,
            down: false,
        }));
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(1),
            device: dev.clone(),
            sc: 0x2E,
            down: true,
        }));
        assert!(agg.combos.is_empty(), "暂停期松开的 Ctrl 不得产生幽灵 Ctrl+C: {:?}", agg.combos);

        drop(agg);
        cleanup_db(&db);
    }

    // ---------------- correctness-v2 §4.3/§5.2：物理来源与生命周期接线（F3/F5） ----------------

    /// §5.2 边界：同型号两键盘（不同来源、相同 DeviceKey）各产 1 个 Key，
    /// 并入同一个型号桶（Writer 侧再按型号归并）。
    #[test]
    fn correctness_v2_same_model_two_sources_merge_into_one_device_bucket() {
        let (mut agg, db) = agg_with_frozen_cursor("same-model", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let dev = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 7,
            pid: 8,
            name: "测试键盘".to_string(),
        };
        // 两个物理连接（来源 101/102）按同一个码：各自独立计数（Engine 来源维度），
        // 聚合桶仍按 DeviceKey 归并
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(101),
            device: dev.clone(),
            sc: 0x1E,
            down: true,
        }));
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(102),
            device: dev.clone(),
            sc: 0x1E,
            down: true,
        }));
        let dev_id = agg.devices.get(&dev).copied().unwrap();
        assert_eq!(
            agg.inputs.get(&(dev_id, day::today_local(), 0x1E)),
            Some(&2),
            "同型号两键盘的按键并入同一个型号桶"
        );
        drop(agg);
        cleanup_db(&db);
    }

    /// §5.2-5：移除一个来源只清该来源 held，另一个仍按住的同码修饰键保留——
    /// SourceRemoved 后其余来源的 C 仍产 Ctrl+C。
    #[test]
    fn correctness_v2_source_removed_keeps_other_source_modifier() {
        let (mut agg, db) = agg_with_frozen_cursor("remove-keeps-mod", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let dev = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 7,
            pid: 8,
            name: "测试键盘".to_string(),
        };
        // 两个键盘都按住 Ctrl
        for source in [InputSourceId(101), InputSourceId(102)] {
            agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
                source,
                device: dev.clone(),
                sc: 0x1D,
                down: true,
            }));
        }
        // 101 拔出：只清 101 的 held
        agg.handle_event(AggEvent::SourceRemoved { source: InputSourceId(101) });
        // 102 的 C：仍有 Ctrl+C（其他来源修饰位保留）
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(102),
            device: dev.clone(),
            sc: 0x2E,
            down: true,
        }));
        let day = day::today_local();
        assert_eq!(
            agg.combos.get(&(day, mods::CTRL, 0x2E)),
            Some(&1),
            "SourceRemoved 不得清掉其他来源的修饰位: {:?}",
            agg.combos
        );
        drop(agg);
        cleanup_db(&db);
    }

    /// §5.2 边界"暂停中移除同样处理生命周期"：SourceRemoved 无论 paused 均执行——
    /// 暂停中 101 Ctrl down（held 维护）→ 暂停中拔出 101 → 恢复后 102 C down 无幽灵 Ctrl。
    #[test]
    fn correctness_v2_source_removed_during_pause_clears_held_state() {
        let (mut agg, db) = agg_with_frozen_cursor("paused-removal", Duration::from_secs(2));
        // helper 只设 paused_observed:true、flag 默认 false——必须显式置位保持暂停沿一致
        agg.flags.paused.store(true, Ordering::Release);
        let dev = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 7,
            pid: 8,
            name: "测试键盘".to_string(),
        };
        // 暂停中 101 Ctrl down：边沿喂 engine（不计数）
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(101),
            device: dev.clone(),
            sc: 0x1D,
            down: true,
        }));
        // 暂停中拔出来源 101：生命周期分支必须执行（否则恢复后 102 的 C 带幽灵 Ctrl）
        agg.handle_event(AggEvent::SourceRemoved { source: InputSourceId(101) });
        // 恢复后 102 C down：正常计数、无组合
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(102),
            device: dev.clone(),
            sc: 0x2E,
            down: true,
        }));
        let dev_id = agg.devices.get(&dev).copied().unwrap();
        assert!(agg.combos.is_empty(), "移除来源的 Ctrl 不得产生幽灵组合: {:?}", agg.combos);
        assert_eq!(
            agg.inputs.get(&(dev_id, day::today_local(), 0x2E)),
            Some(&1),
            "恢复后 102 的 C 应正常计数"
        );
        drop(agg);
        cleanup_db(&db);
    }

    /// §5.2-6：loop 重建的 KeyboardSourcesReset 清空全部来源 held（含修饰位与同源
    /// 去重表）——reset 后同码 down 重新计数且无旧修饰键，随后同源 repeat 正常去重。
    #[test]
    fn correctness_v2_keyboard_sources_reset_clears_all_held() {
        let (mut agg, db) = agg_with_frozen_cursor("reset-all", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let dev = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 7,
            pid: 8,
            name: "测试键盘".to_string(),
        };
        // 101 Ctrl、102 Shift 按住
        for (source, sc) in [(InputSourceId(101), 0x1Du16), (InputSourceId(102), 0x2A)] {
            agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
                source,
                device: dev.clone(),
                sc,
                down: true,
            }));
        }
        agg.handle_event(AggEvent::KeyboardSourcesReset);
        // reset 后 101 A down：重新计数，且不继承任何来源的旧修饰键
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(101),
            device: dev.clone(),
            sc: 0x1E,
            down: true,
        }));
        let dev_id = agg.devices.get(&dev).copied().unwrap();
        let day = day::today_local();
        assert_eq!(agg.inputs.get(&(dev_id, day.clone(), 0x1E)), Some(&1), "reset 后同码 down 重新计数");
        assert!(agg.combos.is_empty(), "reset 必须清掉全部来源修饰位: {:?}", agg.combos);
        // reset 重建的同源去重表生效：101 A 的自动重复不计数
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(101),
            device: dev.clone(),
            sc: 0x1E,
            down: true,
        }));
        assert_eq!(agg.inputs.get(&(dev_id, day, 0x1E)), Some(&1), "同源 repeat 仍去重");
        drop(agg);
        cleanup_db(&db);
    }

    /// 生命周期事件不写统计、不记 events_seen（§4.3）：SourceRemoved/Reset 前后
    /// 全部统计桶逐键相等（只动 Engine 按下状态，不动任何计数）。
    #[test]
    fn correctness_v2_lifecycle_events_do_not_count_or_record_status() {
        let (mut agg, db) = agg_with_frozen_cursor("lifecycle-clean", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let dev = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 7,
            pid: 8,
            name: "测试键盘".to_string(),
        };
        agg.handle_event(AggEvent::Input(RawEvent::Keyboard {
            source: InputSourceId(1),
            device: dev.clone(),
            sc: 0x1E,
            down: true,
        }));
        assert_eq!(agg.status.events_seen.load(Ordering::Relaxed), 1, "正常输入记 events_seen");
        let dev_id = agg.devices.get(&dev).copied().unwrap();
        let inputs_before = agg.inputs.clone();
        let combos_before = agg.combos.clone();
        let apps_before = agg.apps.clone();
        let mouse_move_before = agg.mouse_move.clone();

        agg.handle_event(AggEvent::SourceRemoved { source: InputSourceId(1) });
        agg.handle_event(AggEvent::KeyboardSourcesReset);
        assert_eq!(
            agg.status.events_seen.load(Ordering::Relaxed),
            1,
            "生命周期事件不得增加 events_seen"
        );
        assert_eq!(agg.inputs, inputs_before, "生命周期事件不得写 input_daily 桶");
        assert_eq!(agg.combos, combos_before, "生命周期事件不得写 combo_daily 桶");
        assert_eq!(agg.apps, apps_before, "生命周期事件不得写 app_daily 桶");
        assert_eq!(agg.mouse_move, mouse_move_before, "生命周期事件不得写 mouse_move 桶");
        // 已入桶计数保持不变（幂等引用：dev_id 桶仍存在）
        assert_eq!(agg.inputs.get(&(dev_id, day::today_local(), 0x1E)), Some(&1));
        drop(agg);
        cleanup_db(&db);
    }

    // ---------------- motion-dpi §4.3/§4.4：鼠标运动事件消费 ----------------

    const MOTION_DAY: &str = "2026-09-28";

    /// 合成鼠标来源描述（§8：唯一 key，不与真实/未知桶冲突）。
    fn motion_descriptor(key: &str) -> MouseSourceDescriptor {
        MouseSourceDescriptor {
            source_key: key.to_string(),
            model: DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0x046D,
                pid: 0xC08B,
                name: "测试鼠标".to_string(),
            },
            interface_path: None,
            physical: true,
        }
    }

    fn travel(descriptor: &MouseSourceDescriptor, counts: f64, dpi: u32) -> AggEvent {
        AggEvent::MouseTravel(MouseTravelDelta {
            descriptor: descriptor.clone(),
            connection: MotionConnectionId(1),
            day: MOTION_DAY.to_string(),
            counts,
            dpi: if dpi == 0 {
                EffectiveDpi { value: None, origin: DpiOrigin::Unknown }
            } else {
                EffectiveDpi { value: Some(dpi), origin: DpiOrigin::Manual }
            },
            control: MotionControlSnapshot { epoch: 0, paused: false },
        })
    }

    /// 运动事件不增加 events_seen、不写按钮/应用统计（§4.1：生命周期/运动不是按钮事件）。
    #[test]
    fn motion_dpi_motion_events_do_not_touch_status_or_button_buckets() {
        let (mut agg, db) = agg_with_frozen_cursor("motion-status", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let desc = motion_descriptor("k1");
        agg.handle_event(travel(&desc, 100.0, 800));
        agg.handle_event(AggEvent::MouseSourceState(MouseSourceState {
            descriptor: desc.clone(),
            connection: MotionConnectionId(1),
            connected: true,
            stamp: MotionStamp { mono_us: 0, unix_us: 0 },
            probe_status: clrecoder_core::motion::DpiProbeStatus::Pending,
            auto_dpi: None,
            auto_valid_until_unix_us: None,
        }));
        assert_eq!(
            agg.status.events_seen.load(Ordering::Relaxed),
            0,
            "运动事件不得增加 events_seen"
        );
        assert!(agg.inputs.is_empty() && agg.combos.is_empty() && agg.apps.is_empty());
        assert!(agg.mouse_move.is_empty(), "运动事件不得写旧 mouse_move 桶");
        drop(agg);
        cleanup_db(&db);
    }

    /// MouseTravel 按 (source_key, 日, dpi, origin) 桶累计；flush 注册来源绑定 id 并清桶；
    /// 同键再送再 flush 继续累加（写库累加语义由 store 侧覆盖，此处锚定桶级装配）。
    #[test]
    fn motion_dpi_mouse_travel_accumulates_by_key_and_flush_binds_source_id() {
        let (mut agg, db) = agg_with_frozen_cursor("motion-flush", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let desc = motion_descriptor("k1");
        // 同桶两笔（800）+ 异桶两笔（1600、unknown 400）
        agg.handle_event(travel(&desc, 500.0, 800));
        agg.handle_event(travel(&desc, 300.0, 800));
        agg.handle_event(travel(&desc, 1600.0, 1600));
        agg.handle_event(travel(&desc, 400.0, 0));
        let key800 = ("k1".to_string(), MOTION_DAY.to_string(), 800u32, DpiOrigin::Manual);
        let key1600 = ("k1".to_string(), MOTION_DAY.to_string(), 1600u32, DpiOrigin::Manual);
        let key_unknown = ("k1".to_string(), MOTION_DAY.to_string(), 0u32, DpiOrigin::Unknown);
        assert_eq!(agg.mouse_motion[&key800], 800.0, "同桶累计");
        assert_eq!(agg.mouse_motion[&key1600], 1600.0);
        assert_eq!(agg.mouse_motion[&key_unknown], 400.0, "unknown 桶编码 dpi=0");

        agg.flush_buckets();
        assert!(agg.mouse_motion.is_empty(), "成功 flush 后清桶");
        let id = agg.mouse_sources["k1"].id.expect("注册成功必须绑定数据库 id");
        assert!(id >= 1);
        assert_eq!(agg.flush_fail_streak, 0);

        // 同键再送：绑定的 id 复用（缓存命中，不再注册）
        agg.handle_event(travel(&desc, 100.0, 800));
        agg.flush_buckets();
        assert_eq!(agg.mouse_sources["k1"].id, Some(id), "来源 id 缓存复用");
        drop(agg);
        cleanup_db(&db);
    }

    /// flush 失败（§8.3 手法：ghost 输入行令事务失败）→ 运动桶整批合回（同键压缩累计），
    /// 清除标记重试后成功清桶——注册成功的行不丢 counts、不重复提交。
    #[test]
    fn motion_dpi_flush_failure_merges_motion_bucket_back_and_retry_succeeds() {
        let (mut agg, db) = agg_with_frozen_cursor("motion-rollback", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let desc = motion_descriptor("k1");
        agg.handle_event(travel(&desc, 800.0, 800));
        // ghost 标记：输入桶引用不存在的 device_id → 外键事务失败（§8.3 允许手法）
        let ghost = 7_654_321i64;
        agg.inputs.insert((ghost, MOTION_DAY.to_string(), 0x1E), 1);
        agg.flush_buckets();
        assert_eq!(agg.flush_fail_streak, 1, "ghost 标记必须令事务失败");
        let key800 = ("k1".to_string(), MOTION_DAY.to_string(), 800u32, DpiOrigin::Manual);
        assert_eq!(agg.mouse_motion[&key800], 800.0, "运动桶必须整批合回");
        // 清除标记重试：成功清桶
        agg.inputs.remove(&(ghost, MOTION_DAY.to_string(), 0x1E));
        agg.flush_buckets();
        assert_eq!(agg.flush_fail_streak, 0);
        assert!(agg.mouse_motion.is_empty(), "重试成功后清桶");
        drop(agg);
        cleanup_db(&db);
    }

    /// 暂停期间到达的 MouseTravel 照常落库（§4.3：已捕获为非暂停的独立 counts 增量
    /// 可落库——暂停过滤在采集侧按捕获 control 完成，aggregator 不按当前暂停态丢弃）。
    #[test]
    fn motion_dpi_travel_lands_while_paused() {
        let (mut agg, db) = agg_with_frozen_cursor("motion-paused", Duration::ZERO);
        // 模拟暂停（§4.3.1：selftest 场景统一经 set_paused 携带代际）
        agg.flags.set_paused(true);
        let desc = motion_descriptor("k1");
        agg.handle_event(travel(&desc, 250.0, 1600));
        let key = ("k1".to_string(), MOTION_DAY.to_string(), 1600u32, DpiOrigin::Manual);
        assert_eq!(agg.mouse_motion[&key], 250.0, "捕获为非暂停的增量必须落库");
        drop(agg);
        cleanup_db(&db);
    }

    /// 连接代际门（§4.4）：旧代 disconnect 不得把新连接标离线——
    /// 先应用新代际（connected），再来的旧代断连状态整条拒绝。
    #[test]
    fn motion_dpi_stale_generation_state_is_rejected() {
        let (mut agg, db) = agg_with_frozen_cursor("motion-generation", Duration::ZERO);
        let desc = motion_descriptor("k1");
        let state_of = |connection: u64, connected: bool| MouseSourceState {
            descriptor: desc.clone(),
            connection: MotionConnectionId(connection),
            connected,
            stamp: MotionStamp { mono_us: 0, unix_us: 0 },
            probe_status: clrecoder_core::motion::DpiProbeStatus::Disconnected,
            auto_dpi: None,
            auto_valid_until_unix_us: None,
        };
        // 新连接（代际 6）在线状态：接受并应用
        agg.on_mouse_source_state(state_of(6, true));
        assert_eq!(agg.mouse_connections["k1"], 6);
        // 旧代（代际 5）断连：拒绝——代际表不回退
        agg.on_mouse_source_state(state_of(5, false));
        assert_eq!(agg.mouse_connections["k1"], 6, "旧代状态不得回退代际表");
        // 更新代际（7）：接受
        agg.on_mouse_source_state(state_of(7, false));
        assert_eq!(agg.mouse_connections["k1"], 7);
        // 进程首见即接受（不与既有数字比较方向的另一侧：更小首见值也接受）
        let (mut agg2, db2) = agg_with_frozen_cursor("motion-generation-first", Duration::ZERO);
        agg2.on_mouse_source_state(state_of(3, true));
        assert_eq!(agg2.mouse_connections["k1"], 3, "进程重启首见即接受");
        drop(agg);
        cleanup_db(&db);
        drop(agg2);
        cleanup_db(&db2);
    }

    // ---------------- motion-dpi §4.3：手柄摇杆运动消费（S5） ----------------

    /// 基准 UTC µs：2026-06-01T00:00:00Z（帧跨度 60ms，任意真实时区下四帧同属一个本地日
    /// ——时区偏移均为 15 分钟整数倍，不可能落在该窗口内的本地午夜 ±60ms）。
    const GP_BASE_UNIX_US: i64 = 1_780_272_000_000_000;

    /// 手柄帧的本地归属日（由 stamp 换算，与 tracker 的日归属口径一致）。
    fn gp_day() -> String {
        local_day_from_unix_us(GP_BASE_UNIX_US)
            .map(day::format_day)
            .expect("基准时刻应可换算本地日期")
    }

    /// 合成手柄设备（独立 vid/pid/name，不与真实 gilrs "Xbox Controller" 行冲突）。
    fn pad_device() -> DeviceKey {
        DeviceKey {
            kind: DeviceKind::Gamepad,
            vid: 0x045E,
            pid: 0x028E,
            name: "测试手柄".to_string(),
        }
    }

    /// 构造一帧摇杆运动（stamp 单调 µs 自 GP_BASE_UNIX_US 起步进）。
    fn gp_frame(
        connection: u64,
        mono_us: u64,
        left: (f64, f64),
        right: (f64, f64),
        control: MotionControlSnapshot,
    ) -> AggEvent {
        AggEvent::GamepadMotion(GamepadMotionFrame {
            device: pad_device(),
            connection: MotionConnectionId(connection),
            stamp: MotionStamp { mono_us, unix_us: GP_BASE_UNIX_US + mono_us as i64 },
            left: StickPoint { x: left.0, y: left.1 },
            right: StickPoint { x: right.0, y: right.1 },
            control,
        })
    }

    /// 取某侧摇杆桶（None=该侧无任何增量）。
    fn stick_side_bucket(agg: &Aggregator, side: StickSide) -> Option<&StickAccum> {
        agg.stick_motion.iter().find(|(k, _)| k.2 == side).map(|(_, a)| a)
    }

    /// 验收点：(0.5,0.5) 中心出发 ≈0.707107R——中心锚点后一帧 (0.5,0.5) 的欧氏路程。
    #[test]
    fn motion_dpi_gamepad_center_to_half_half_travels_0_707107() {
        let (mut agg, db) = agg_with_frozen_cursor("gp-half-half", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(gp_frame(1, 0, (0.0, 0.0), (0.0, 0.0), MotionControlSnapshot::default()));
        agg.handle_event(gp_frame(
            1,
            20_000,
            (0.5, 0.5),
            (0.0, 0.0),
            MotionControlSnapshot::default(),
        ));
        let left = stick_side_bucket(&agg, StickSide::Left).expect("左摇杆应有桶");
        assert!(
            (left.travel_r - 0.5f64.hypot(0.5)).abs() < 1e-9,
            "(0.5,0.5) 中心出发应 ≈0.707107R，实际 {}",
            left.travel_r
        );
        assert_eq!(left.active_us, 0, "[中心, (0.5,0.5)) 区间静止，无停留");
        assert!(left.bins.is_empty(), "静止区间不产出停留格");
        assert!(stick_side_bucket(&agg, StickSide::Right).is_none(), "右摇杆全程中性无桶");
        assert_eq!(agg.stick_motion.len(), 1, "桶按 side 分立");
        drop(agg);
        cleanup_db(&db);
    }

    /// 验收点：原始 (1,1)（半径 >1）径向规范为 1R——不逐轴硬压对角。
    #[test]
    fn motion_dpi_gamepad_raw_one_one_radially_normalized_to_1r() {
        let (mut agg, db) = agg_with_frozen_cursor("gp-one-one", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(gp_frame(1, 0, (0.0, 0.0), (0.0, 0.0), MotionControlSnapshot::default()));
        agg.handle_event(gp_frame(
            1,
            20_000,
            (1.0, 1.0),
            (0.0, 0.0),
            MotionControlSnapshot::default(),
        ));
        let left = stick_side_bucket(&agg, StickSide::Left).expect("左摇杆应有桶");
        assert!(
            (left.travel_r - 1.0).abs() < 1e-9,
            "原始 (1,1) 径向规范后应为 1R，实际 {}",
            left.travel_r
        );
        drop(agg);
        cleanup_db(&db);
    }

    /// 验收点：静止连续帧保持时间——固定点逐帧累计停留（保持帧产生热度）、行程 0。
    #[test]
    fn motion_dpi_gamepad_static_consecutive_frames_keep_time() {
        let (mut agg, db) = agg_with_frozen_cursor("gp-static", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        for i in 0..4u64 {
            agg.handle_event(gp_frame(
                1,
                i * 20_000,
                (1.0, 0.0),
                (0.0, 0.0),
                MotionControlSnapshot::default(),
            ));
        }
        let left = stick_side_bucket(&agg, StickSide::Left).expect("左摇杆应有桶");
        assert_eq!(left.active_us, 60_000, "3 个 20ms 保持区间全部累计");
        assert!((left.travel_r).abs() < 1e-12, "固定点不得增加行程");
        assert_eq!(left.bins.get(&324), Some(&60_000), "满幅右 = row12×col24 = 324");
        assert_eq!(left.bins.len(), 1, "停留只落实际采样格");
        assert!(stick_side_bucket(&agg, StickSide::Right).is_none(), "右摇杆全程中性");
        drop(agg);
        cleanup_db(&db);
    }

    /// 验收点：20ms 两帧间短暂停再恢复不跨段——当前 epoch 变化 reset 全部 tracker，
    /// 恢复首帧仅重锚（即使坐标跳到对侧）；捕获 epoch 与当前不一致或捕获 paused
    /// 的帧 reset/跳过。
    #[test]
    fn motion_dpi_gamepad_pause_between_20ms_frames_does_not_cross() {
        let (mut agg, db) = agg_with_frozen_cursor("gp-pause-cross", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        let active = MotionControlSnapshot { epoch: 0, paused: false };
        agg.handle_event(gp_frame(1, 0, (1.0, 0.0), (0.0, 0.0), active));
        agg.handle_event(gp_frame(1, 20_000, (1.0, 0.0), (0.0, 0.0), active));
        // 20ms 两帧之间真实暂停又恢复（selftest/生产同款合法入口 set_paused）：
        // epoch 0→1→2，短暂停也换代际
        agg.flags.set_paused(true);
        agg.flags.set_paused(false);
        // 恢复后首帧：tracker 已被当前 epoch 变化 reset——坐标跳到对侧也不画 2R 连线
        let resumed = MotionControlSnapshot { epoch: 2, paused: false };
        agg.handle_event(gp_frame(1, 40_000, (-1.0, 0.0), (0.0, 0.0), resumed));
        {
            let left = stick_side_bucket(&agg, StickSide::Left).expect("左摇杆应有桶");
            assert!(left.travel_r.abs() < 1e-12, "短暂停不得跨段连线：{}", left.travel_r);
            assert_eq!(left.active_us, 20_000, "暂停区间不补停留");
            assert_eq!(left.bins.get(&324), Some(&20_000), "暂停前停留保持");
            assert!(!left.bins.contains_key(&300), "恢复后首帧仅重锚，不积分");
        }
        // 恢复后正常积分：对侧格 20ms
        agg.handle_event(gp_frame(1, 60_000, (-1.0, 0.0), (0.0, 0.0), resumed));
        {
            let left = stick_side_bucket(&agg, StickSide::Left).expect("左摇杆应有桶");
            assert_eq!(left.active_us, 40_000);
            assert_eq!(left.bins.get(&300), Some(&20_000));
            assert!(left.travel_r.abs() < 1e-12);
        }
        // 捕获 epoch 与当前不一致（旧捕获的迟到帧）→ reset/跳过
        agg.handle_event(gp_frame(
            1,
            80_000,
            (-1.0, 0.0),
            (0.0, 0.0),
            MotionControlSnapshot { epoch: 1, paused: false },
        ));
        // 捕获 paused 的帧 → 同样 reset/跳过
        agg.handle_event(gp_frame(
            1,
            100_000,
            (-1.0, 0.0),
            (0.0, 0.0),
            MotionControlSnapshot { epoch: 2, paused: true },
        ));
        // 两次跳过后正常帧仅重锚：无跨跳过帧的积分或连线
        agg.handle_event(gp_frame(1, 120_000, (-1.0, 0.0), (0.0, 0.0), resumed));
        {
            let left = stick_side_bucket(&agg, StickSide::Left).expect("左摇杆应有桶");
            assert_eq!(left.active_us, 40_000, "捕获 epoch 不一致/paused 的帧必须跳过");
            assert!(left.travel_r.abs() < 1e-12, "跳过帧后重锚不得画线");
        }
        drop(agg);
        cleanup_db(&db);
    }

    /// 验收点：重连不连线——断连（Disconnected）清该连接 tracker，新连接首帧重锚，
    /// 新旧连接的停留不串、跨断点无路程。
    #[test]
    fn motion_dpi_gamepad_reconnect_does_not_connect_lines() {
        let (mut agg, db) = agg_with_frozen_cursor("gp-reconnect", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(gp_frame(1, 0, (1.0, 0.0), (0.0, 0.0), MotionControlSnapshot::default()));
        agg.handle_event(gp_frame(
            1,
            20_000,
            (1.0, 0.0),
            (0.0, 0.0),
            MotionControlSnapshot::default(),
        ));
        agg.handle_event(AggEvent::GamepadMotionDisconnected { connection: MotionConnectionId(1) });
        assert!(
            !agg.stick_trackers.contains_key(&MotionConnectionId(1)),
            "断连必须清该连接全部 tracker"
        );
        // 重连（新连接代际 2）：位置跳到对侧——不得与断点连线
        agg.handle_event(gp_frame(2, 40_000, (-1.0, 0.0), (0.0, 0.0), MotionControlSnapshot::default()));
        agg.handle_event(gp_frame(
            2,
            60_000,
            (-1.0, 0.0),
            (0.0, 0.0),
            MotionControlSnapshot::default(),
        ));
        let left = stick_side_bucket(&agg, StickSide::Left).expect("左摇杆应有桶");
        assert!(left.travel_r.abs() < 1e-12, "重连不得与断点连线：{}", left.travel_r);
        assert_eq!(left.active_us, 40_000, "断点两侧停留各自归档");
        assert_eq!(left.bins.get(&324), Some(&20_000), "断连前停留保持");
        assert_eq!(left.bins.get(&300), Some(&20_000), "重连后停留归对侧格");
        drop(agg);
        cleanup_db(&db);
    }

    /// 验收点：motion 不增 events_seen、不写按钮/应用统计（手柄运动同鼠标运动条款）。
    #[test]
    fn motion_dpi_gamepad_motion_events_do_not_touch_status_or_button_buckets() {
        let (mut agg, db) = agg_with_frozen_cursor("gp-status", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        agg.handle_event(gp_frame(1, 0, (1.0, 0.0), (0.5, 0.5), MotionControlSnapshot::default()));
        agg.handle_event(gp_frame(
            1,
            20_000,
            (1.0, 0.0),
            (0.5, 0.5),
            MotionControlSnapshot::default(),
        ));
        agg.handle_event(AggEvent::GamepadMotionDisconnected { connection: MotionConnectionId(1) });
        assert_eq!(
            agg.status.events_seen.load(Ordering::Relaxed),
            0,
            "手柄运动事件不得增加 events_seen"
        );
        assert!(agg.inputs.is_empty() && agg.combos.is_empty() && agg.apps.is_empty());
        assert!(agg.mouse_move.is_empty() && agg.mouse_motion.is_empty());
        drop(agg);
        cleanup_db(&db);
    }

    /// 纯摇杆批可 flush：空批次提前返回判定纳入 stick_motion——只有保持帧产生的热度
    /// 也必须落库（无任何旧统计的批次不得被当作"不脏"跳过）。
    #[test]
    fn motion_dpi_gamepad_pure_stick_batch_flushes_and_binds_device() {
        let (mut agg, db) = agg_with_frozen_cursor("gp-pure-flush", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        for i in 0..4u64 {
            agg.handle_event(gp_frame(
                1,
                i * 20_000,
                (1.0, 0.0),
                (0.0, 0.0),
                MotionControlSnapshot::default(),
            ));
        }
        agg.flush_buckets();
        assert!(agg.stick_motion.is_empty(), "纯摇杆批必须写库并清桶");
        assert_eq!(agg.flush_fail_streak, 0, "纯摇杆批不得 flush 失败");
        assert!(agg.devices.contains_key(&pad_device()), "flush 时绑定型号 device_id");
        // 后续保持帧再次入桶 → flush 复用同一 device_id（缓存命中）
        agg.handle_event(gp_frame(
            1,
            80_000,
            (1.0, 0.0),
            (0.0, 0.0),
            MotionControlSnapshot::default(),
        ));
        agg.flush_buckets();
        assert!(agg.stick_motion.is_empty());
        drop(agg);
        cleanup_db(&db);
    }

    /// flush 失败（§8.3 手法：ghost 输入行令事务失败）→ 摇杆桶整批合回（同键压缩
    /// 累计、bins 不丢），清除标记重试后成功清桶。
    #[test]
    fn motion_dpi_flush_failure_merges_stick_bucket_back_and_retry_succeeds() {
        let (mut agg, db) = agg_with_frozen_cursor("gp-rollback", Duration::ZERO);
        agg.paused_observed = false;
        agg.flags.paused.store(false, Ordering::Release);
        for i in 0..4u64 {
            agg.handle_event(gp_frame(
                1,
                i * 20_000,
                (1.0, 0.0),
                (0.0, 0.0),
                MotionControlSnapshot::default(),
            ));
        }
        // ghost 标记：输入桶引用不存在的 device_id → 外键事务失败（§8.3 允许手法）
        let ghost = 7_654_321i64;
        agg.inputs.insert((ghost, gp_day(), 0x1E), 1);
        agg.flush_buckets();
        assert_eq!(agg.flush_fail_streak, 1, "ghost 标记必须令事务失败");
        {
            let left = stick_side_bucket(&agg, StickSide::Left).expect("失败后摇杆桶必须合回");
            assert_eq!(left.active_us, 60_000, "active_us 整批合回不丢");
            assert_eq!(left.bins.get(&324), Some(&60_000), "停留格合回不丢");
        }
        // 清除标记重试：成功清桶
        agg.inputs.remove(&(ghost, gp_day(), 0x1E));
        agg.flush_buckets();
        assert_eq!(agg.flush_fail_streak, 0);
        assert!(agg.stick_motion.is_empty(), "重试成功后清桶");
        drop(agg);
        cleanup_db(&db);
    }
}
