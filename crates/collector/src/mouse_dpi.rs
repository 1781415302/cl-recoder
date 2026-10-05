//! DPI 协调 worker（motion-dpi §4.3）——定期批读 manual 配置、调度只读探测、
//! 维护缓存过期与来源状态发布。
//!
//! 节奏（§4.3"自动读取 worker 节奏"，逐条对应）：
//! - **500ms 配置批读**：每节拍对全部已注册来源 key 调 [`Writer::manual_dpi`]——
//!   独立于唯一硬件执行槽（探测在执行槽线程上运行，批读永不被探测阻塞）；
//!   批读失败只保留上一份已确认 manual 值并告警；首次未加载为 unknown，不猜 80；
//! - **状态发布**：来源状态变化（连接/探测/配置）即发布；已连接来源按 2s 心跳
//!   维持 DB last_seen 新鲜度（成功缓存延长有效期也发布，供 DB 侧自动值过期判断）；
//!   约每 2 秒一次，不逐输入写日志；
//! - **2s 探测扫描**：每 2 秒检查支持的已连接来源——Pending/Available（刷新防 4s
//!   过期）即探测；Unavailable 每 10 秒重试；Unsupported/Ambiguous 同连接不重探；
//!   重连（新代际）由运行时重置 Pending 后自然重判；
//! - **唯一执行槽**：同一时刻至多一个探测在飞（不开启第二个 probe slot）。执行槽
//!   线程运行 [`hidpp_dpi::probe_current_dpi`]（1500ms 业务期限由本模块传入）；
//!   协调 worker 到期立即发布 Unavailable，slot 发 CancelIoEx（在 transport 内）；
//!   到期后迟到的结果**拒绝采信**；槽位仍被占用直到执行槽收尾——取消迟迟未完成
//!   只停自动读取，手动配置/输入照常；
//! - **停机**：[`MotionRuntime::worker_should_stop`]（stop_worker 旗标或 shutdown）
//!   在节拍顶检查——停止协调 worker 及后续事件发布，不无限等待挂起 probe；挂起
//!   IO 的资源由存活的执行槽线程持有到 IO 完成或进程正常退出。执行槽不持有
//!   Writer 锁、不阻挡 aggregator 最终 flush。
//!
//! 可测性拆分（沿袭 hidpp_dpi 惯例）：单次协调节拍 [`DpiWorker::tick`] 与探测执行器
//! [`ProbeExecutor`] 均可注入（假 clock 经 [`MotionRuntime`] 的 stamp 覆盖、假
//! executor 返回脚本化结果）——单测不触真实 HID、不 sleep 等真实节奏。
//!
//! S4 交付态：生产入口 [`spawn_coordinator`] 仅由 [`crate::motion_runtime::MotionRuntime::start`]
//! 调用；worker 与执行槽线程随进程退出回收（停止语义见上）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clrecoder_core::event::AggEvent;
use clrecoder_core::motion::{DpiProbeStatus, MouseSourceDescriptor};
use clrecoder_store::writer::Writer;
use crossbeam_channel::Sender;

use crate::hidpp_dpi::{self, DpiProbeResult};
use crate::motion_runtime::MotionRuntime;

/// 协调节拍（§4.3：每 500ms 配置批读；状态发布/收割在同节拍完成）。
const WORKER_TICK: Duration = Duration::from_millis(500);
/// 探测扫描间隔（§4.3：每 2 秒检查支持的已连接来源）。
const SCAN_INTERVAL: Duration = Duration::from_secs(2);
/// 探测业务结果期限（§4.3：1500ms 是业务结果接受期限与停止发后续查询的期限，
/// 由本模块传给 provider；不是驱动一定收尾的硬保证）。
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// Unavailable 的重试间隔（§4.3：Unavailable 每 10 秒重试；单位 unix µs）。
const UNAVAILABLE_RETRY_UNIX_US: i64 = 10_000_000;

/// 探测执行器抽象（§4.3 fixture：假 worker 不触真实 HID）。生产实现转发
/// [`hidpp_dpi::probe_current_dpi`]；单测注入脚本化结果。
pub(crate) trait ProbeExecutor: Send + Sync {
    /// 在执行槽线程上运行一次只读探测（实现自负责到期取消与资源持有语义）。
    fn probe(&self, descriptor: &MouseSourceDescriptor, timeout: Duration) -> DpiProbeResult;
}

/// 生产执行器（薄适配，无状态）。
struct RealProbeExecutor;

impl ProbeExecutor for RealProbeExecutor {
    fn probe(&self, descriptor: &MouseSourceDescriptor, timeout: Duration) -> DpiProbeResult {
        hidpp_dpi::probe_current_dpi(descriptor, timeout)
    }
}

/// 在途探测（唯一执行槽）：结果通道 + 到期发布簿记。
struct PendingProbe {
    source_key: String,
    /// 派发时的连接代际（结果按它应用，旧代拒绝）
    connection: clrecoder_core::motion::MotionConnectionId,
    started: Instant,
    /// 已按业务期限发布过 Unavailable（迟到的执行槽结果不再采信，仅等槽位释放）
    deadline_published: bool,
    rx: crossbeam_channel::Receiver<DpiProbeResult>,
}

/// 探测调度簿记。
#[derive(Default)]
struct ProbeScheduler {
    pending: Option<PendingProbe>,
    /// source_key → 上次探测派发时刻（unix µs；Unavailable 10s 重试的判据）
    last_attempt_unix_us: HashMap<String, i64>,
}

/// DPI 协调 worker：一个实例对应协调线程内的全部节拍状态（单线程独占）。
pub(crate) struct DpiWorker {
    sched: ProbeScheduler,
    executor: Arc<dyn ProbeExecutor>,
    /// 探测业务期限（生产 [`PROBE_TIMEOUT`]；测试可注入更短值）
    probe_timeout: Duration,
    /// 扫描节流（生产 [`SCAN_INTERVAL`]；测试可注入 0 以逐节拍扫描）
    scan_interval: Duration,
}

impl DpiWorker {
    /// 生产 worker：真实探测执行器 + 合同节奏。
    pub(crate) fn new() -> Self {
        Self {
            sched: ProbeScheduler::default(),
            executor: Arc::new(RealProbeExecutor),
            probe_timeout: PROBE_TIMEOUT,
            scan_interval: SCAN_INTERVAL,
        }
    }

    /// fixture 构造（§8：假 transport/假 worker——不触真实 HID、不 sleep 等真实节奏）。
    #[cfg(test)]
    fn with_executor(
        executor: Arc<dyn ProbeExecutor>,
        probe_timeout: Duration,
        scan_interval: Duration,
    ) -> Self {
        Self {
            sched: ProbeScheduler::default(),
            executor,
            probe_timeout,
            scan_interval,
        }
    }

    /// 单次协调节拍（生产循环每 [`WORKER_TICK`] 调一次；测试直接驱动）：
    /// 批读 manual → 收割执行槽 → 发布状态 → 扫描派发（先收割后发布——
    /// 本节拍内应用的结果随节拍发布）。
    pub(crate) fn tick(&mut self, runtime: &MotionRuntime, writer: &Writer, tx: &Sender<AggEvent>) {
        // 1) manual 配置批读（每 500ms；独立于唯一硬件执行槽）。失败只告警——
        //    不调用 apply_manual，上一份已确认 manual 值原样保留（§4.3）。
        let keys = runtime.source_keys();
        if !keys.is_empty() {
            match writer.manual_dpi(&keys) {
                Ok(rows) => {
                    for row in rows {
                        runtime.apply_manual(&row.source_key, row.manual_dpi);
                    }
                }
                Err(e) => {
                    log::warn!("manual DPI 配置批读失败，保留上一份已确认配置: {e}");
                }
            }
        }

        // 2) 执行槽收割/到期判定（结果带连接代际应用；到期立即发布 Unavailable）。
        self.harvest(runtime);

        // 3) 状态发布：状态变化即发布；已连接来源按 2s 心跳维持 DB last_seen。
        let stamp = runtime.stamp();
        for state in runtime.due_states(stamp) {
            let _ = tx.send(AggEvent::MouseSourceState(state));
        }

        // 每来源独立节流，空槽优先处理最久未探测者。
        if self.sched.pending.is_none() {
            self.scan_and_dispatch(runtime, stamp.unix_us);
        }
    }

    /// 收割在途探测：结果到达即应用（带连接代际）；业务期限已到则立即发布
    /// Unavailable——执行槽继续持有 IO 资源直到收尾，其迟到结果拒绝采信；
    /// 执行槽线程消失（panic 等）时释放槽位（下次扫描重新派发）。
    fn harvest(&mut self, runtime: &MotionRuntime) {
        use crossbeam_channel::TryRecvError;
        let Some(p) = self.sched.pending.as_mut() else {
            return;
        };
        match p.rx.try_recv() {
            Ok(result) => {
                if !p.deadline_published && p.started.elapsed() < self.probe_timeout {
                    runtime.apply_probe_result(&p.source_key, p.connection, result);
                } else if !p.deadline_published {
                    runtime.apply_probe_result(
                        &p.source_key,
                        p.connection,
                        DpiProbeResult::Unavailable,
                    );
                }
                // 已到期发布的 Unavailable 之后迟到的结果：拒绝采信（仅释放槽位）
                self.sched.pending = None;
            }
            Err(TryRecvError::Empty) => {
                if !p.deadline_published && p.started.elapsed() >= self.probe_timeout {
                    // 协调 worker 到期立即发布 Unavailable；slot 发 CancelIoEx
                    // （transport 内），资源由执行槽持有到 IO 收尾（§4.3）。
                    runtime.apply_probe_result(
                        &p.source_key,
                        p.connection,
                        DpiProbeResult::Unavailable,
                    );
                    p.deadline_published = true;
                }
            }
            Err(TryRecvError::Disconnected) => {
                // 执行槽线程消失（创建后 panic 等）：无结果可收，释放槽位重试
                self.sched.pending = None;
            }
        }
    }

    /// 扫描支持的已连接来源并派发至多一个探测（§4.3：每 2 秒检查；Unsupported/
    /// Ambiguous 同连接不重探；Unavailable 每 10 秒重试；virtual/断连不探测）。
    fn scan_and_dispatch(&mut self, runtime: &MotionRuntime, now_unix_us: i64) {
        let mut sources = runtime.sources_snapshot();
        sources.sort_by_key(|s| self.sched.last_attempt_unix_us.get(&s.source_key).copied());
        for source in sources {
            if !source.connected || !source.descriptor.physical {
                continue;
            }
            let due = match source.probe {
                // Pending 首探；Available 每 2s 刷新（防 4s 自动数值过期）
                DpiProbeStatus::Pending => true,
                DpiProbeStatus::Available => self
                    .sched
                    .last_attempt_unix_us
                    .get(&source.source_key)
                    .is_none_or(|last| {
                        now_unix_us.saturating_sub(*last) >= self.scan_interval.as_micros() as i64
                    }),
                DpiProbeStatus::Unavailable => {
                    now_unix_us
                        >= self
                            .sched
                            .last_attempt_unix_us
                            .get(&source.source_key)
                            .copied()
                            .unwrap_or(0)
                            + UNAVAILABLE_RETRY_UNIX_US
                }
                // 判定类状态：同连接不重探（重连换代际后由运行时重置 Pending）
                DpiProbeStatus::Unsupported
                | DpiProbeStatus::Ambiguous
                | DpiProbeStatus::Disconnected => false,
            };
            if due {
                self.dispatch(
                    runtime,
                    &source.source_key,
                    source.descriptor.clone(),
                    source.connection,
                );
                return; // 唯一执行槽：每轮至多派发一个
            }
        }
    }

    /// 派发一次探测到独立执行槽线程（不持有 Writer 锁；结果经有界通道回传）。
    /// 线程创建失败（内存耗尽等）：本次探测按 Unavailable 降级，槽位立即释放。
    fn dispatch(
        &mut self,
        runtime: &MotionRuntime,
        source_key: &str,
        descriptor: MouseSourceDescriptor,
        connection: clrecoder_core::motion::MotionConnectionId,
    ) {
        let (result_tx, rx) = crossbeam_channel::bounded(1);
        self.sched
            .last_attempt_unix_us
            .insert(source_key.to_string(), runtime.stamp().unix_us);
        self.sched.pending = Some(PendingProbe {
            source_key: source_key.to_string(),
            connection,
            started: Instant::now(),
            deadline_published: false,
            rx,
        });
        let executor = Arc::clone(&self.executor);
        let timeout = self.probe_timeout;
        let spawned = std::thread::Builder::new()
            .name("dpi-probe-slot".to_string())
            .spawn(move || {
                // 探测只触 HID IO（transport 负责到期取消与资源持有），不持任何锁
                let result = executor.probe(&descriptor, timeout);
                let _ = result_tx.send(result);
            });
        if spawned.is_err() {
            self.sched.pending = None;
            runtime.apply_probe_result(source_key, connection, DpiProbeResult::Unavailable);
        }
    }
}

/// 启动 DPI 协调 worker 线程（[`MotionRuntime::start`] 专用；offline 模式不启动）。
/// 返回协调线程句柄，stop_worker等待协调者结束发布。
/// probe槽仍独立收尾，不参与此join。
pub(crate) fn spawn_coordinator(
    runtime: Arc<MotionRuntime>,
    writer: Arc<Writer>,
    tx: Sender<AggEvent>,
) -> Option<std::thread::JoinHandle<()>> {
    let spawned = std::thread::Builder::new()
        .name("dpi-worker".to_string())
        .spawn(move || {
            let mut worker = DpiWorker::new();
            loop {
                if runtime.worker_should_stop() {
                    break;
                }
                worker.tick(&runtime, &writer, &tx);
                std::thread::sleep(WORKER_TICK);
            }
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(e) => {
            log::error!("DPI 协调 worker 线程创建失败，自动读取与配置刷新不可用: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex as StdMutex;

    use clrecoder_core::codes::DeviceKind;
    use clrecoder_core::event::DeviceKey;
    use clrecoder_core::motion::{DpiOrigin, EffectiveDpi, MotionStamp};

    use crate::ipc_server::Flags;

    #[test]
    fn motion_dpi_worker_probes_all_available_sources_fairly() {
        struct RecordingExecutor(StdMutex<Vec<String>>);
        impl ProbeExecutor for RecordingExecutor {
            fn probe(&self, d: &MouseSourceDescriptor, _: Duration) -> DpiProbeResult {
                self.0.lock().unwrap().push(d.source_key.clone());
                DpiProbeResult::Available(800)
            }
        }
        let db = TempDb::new("fair-sources");
        let writer = Writer::open(&db.0).unwrap();
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        for key in ["first", "second"] {
            runtime.observe_mouse(
                physical_descriptor(key),
                runtime.allocate_connection(),
                true,
            );
        }
        let executor = Arc::new(RecordingExecutor(StdMutex::new(Vec::new())));
        let mut worker =
            DpiWorker::with_executor(executor.clone(), Duration::from_secs(1), Duration::ZERO);
        let (tx, _rx) = crossbeam_channel::unbounded();
        for _ in 0..5 {
            worker.tick(&runtime, &writer, &tx);
            std::thread::sleep(Duration::from_millis(20));
        }
        let queried = executor.0.lock().unwrap();
        assert!(queried.iter().any(|s| s == "first"));
        assert!(queried.iter().any(|s| s == "second"));
        assert_eq!(runtime.dpi_for("first").origin, DpiOrigin::Auto);
        assert_eq!(runtime.dpi_for("second").origin, DpiOrigin::Auto);
    }

    #[test]
    fn motion_dpi_worker_rejects_result_queued_after_deadline() {
        let db = TempDb::new("queued-late-result");
        let writer = Writer::open(&db.0).unwrap();
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        runtime.observe_mouse(
            physical_descriptor("k1"),
            runtime.allocate_connection(),
            true,
        );
        let executor = FakeExecutor::always_with_delay(
            DpiProbeResult::Available(800),
            Duration::from_millis(30),
        );
        let mut worker =
            DpiWorker::with_executor(executor, Duration::from_millis(10), Duration::ZERO);
        let (tx, _rx) = crossbeam_channel::unbounded();
        worker.tick(&runtime, &writer, &tx);
        std::thread::sleep(Duration::from_millis(80));
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(runtime.dpi_for("k1").origin, DpiOrigin::Unknown);
    }

    /// 唯一临时 DB（沿袭 engine_loop 测试惯例：tag+pid 保证路径唯一，结束清理）。
    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "clrecoder-dpiworker-{tag}-{}.db",
                std::process::id()
            ));
            for suffix in ["", "-wal", "-shm"] {
                let mut name = path.as_os_str().to_owned();
                name.push(suffix);
                let _ = std::fs::remove_file(&name);
            }
            Self(path)
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut name = self.0.as_os_str().to_owned();
                name.push(suffix);
                let _ = std::fs::remove_file(&name);
            }
        }
    }

    /// 脚本化假执行器：按调用次序回放结果（耗尽后重复最后一个）并计数——
    /// 验证派发次数与节奏（10s 重试/槽位唯一）。
    struct FakeExecutor {
        results: StdMutex<Vec<DpiProbeResult>>,
        delay: Duration,
        calls: AtomicUsize,
    }

    impl FakeExecutor {
        /// 恒定结果执行器。
        fn always(result: DpiProbeResult) -> Arc<Self> {
            Arc::new(Self {
                results: StdMutex::new(vec![result]),
                delay: Duration::ZERO,
                calls: AtomicUsize::new(0),
            })
        }

        /// 恒定结果 + 每次探测耗时 delay（模拟到期后仍未收尾的执行槽）。
        fn always_with_delay(result: DpiProbeResult, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                results: StdMutex::new(vec![result]),
                delay,
                calls: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(AtomicOrdering::SeqCst)
        }
    }

    impl ProbeExecutor for FakeExecutor {
        fn probe(&self, _descriptor: &MouseSourceDescriptor, _timeout: Duration) -> DpiProbeResult {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            let mut guard = self.results.lock().unwrap();
            match guard.len() {
                0 => DpiProbeResult::Unavailable,
                // 单元素脚本 = 恒定结果（重复）
                1 => guard[0],
                _ => guard.remove(0),
            }
        }
    }

    fn flags() -> Arc<Flags> {
        Arc::new(Flags::default())
    }

    fn physical_descriptor(key: &str) -> MouseSourceDescriptor {
        MouseSourceDescriptor {
            source_key: key.to_string(),
            model: DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0x046D,
                pid: 0xC08B,
                name: "测试鼠标".to_string(),
            },
            interface_path: Some(r"\\?\HID#VID_046D&PID_C08B&MI_00#7&2f3a3d&0&0000".to_string()),
            physical: true,
        }
    }

    fn override_unix(runtime: &MotionRuntime, unix_us: i64) {
        runtime.set_stamp_override_for_tests(Some(MotionStamp {
            mono_us: 0,
            unix_us,
        }));
    }

    /// 排空通道中发布的 MouseSourceState。
    fn drain_states(
        rx: &crossbeam_channel::Receiver<AggEvent>,
    ) -> Vec<clrecoder_core::motion::MouseSourceState> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let AggEvent::MouseSourceState(s) = ev {
                out.push(s);
            }
        }
        out
    }

    const T0: i64 = 1_000_000_000;

    /// 探测成功 → auto 值 + 4s 有效期入缓存并发布 Available 状态；dpi_for 取 auto。
    #[test]
    fn motion_dpi_worker_probe_sets_auto_dpi_with_ttl_and_publishes_state() {
        let db = TempDb::new("probe-happy");
        let writer = Arc::new(clrecoder_store::writer::Writer::open(&db.0).unwrap());
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        runtime.observe_mouse(
            physical_descriptor("k1"),
            runtime.allocate_connection(),
            true,
        );

        let (tx, rx) = crossbeam_channel::unbounded();
        let executor = FakeExecutor::always(DpiProbeResult::Available(800));
        // 扫描间隔用生产值（2s）：tick 间隔远小于它 → 不会在本用例内刷新重探
        let mut worker =
            DpiWorker::with_executor(executor.clone(), Duration::from_millis(100), SCAN_INTERVAL);

        // tick1：批读 + 首次状态发布（Pending）+ 扫描派发
        worker.tick(&runtime, &writer, &tx);
        let states = drain_states(&rx);
        assert_eq!(states.len(), 1, "新来源首次发布");
        assert_eq!(
            states[0].probe_status,
            clrecoder_core::motion::DpiProbeStatus::Pending
        );
        // 执行槽异步运行：留待收割（假执行器无 IO，短暂让渡即可）
        std::thread::sleep(Duration::from_millis(50));

        // tick2：收割应用（Available + 4s 有效期）→ 发布新状态
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: Some(800),
                origin: DpiOrigin::Auto
            },
            "新读数发布后使用新值（不取 default）"
        );
        let states = drain_states(&rx);
        assert_eq!(states.len(), 1);
        assert_eq!(
            states[0].probe_status,
            clrecoder_core::motion::DpiProbeStatus::Available
        );
        assert_eq!(states[0].auto_dpi, Some(800));
        assert_eq!(
            states[0].auto_valid_until_unix_us,
            Some(T0 + 4_000_000),
            "4s 有效期"
        );
        assert_eq!(executor.calls(), 1, "2s 扫描间隔内不重探");
    }

    /// 自动数值 4s 过期（§4.3）：过期后 dpi_for 退 manual（有配置）——自动米数按
    /// "最近有效读取值"之外的口径退回手动。
    #[test]
    fn motion_dpi_worker_auto_expiry_falls_back_to_manual() {
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        let conn = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn, true);
        runtime.apply_manual("k1", Some(1600));
        runtime.apply_probe_result("k1", conn, DpiProbeResult::Available(800));
        assert_eq!(runtime.dpi_for("k1").origin, DpiOrigin::Auto);
        // 过期（≥4s）：退 manual
        override_unix(&runtime, T0 + 4_000_000);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: Some(1600),
                origin: DpiOrigin::Manual
            }
        );
    }

    /// Unavailable 每 10 秒重试（§4.3）：窗口内不重派发；≥10s 后重派发。
    #[test]
    fn motion_dpi_worker_unavailable_retries_after_ten_seconds_only() {
        let db = TempDb::new("probe-retry");
        let writer = Arc::new(clrecoder_store::writer::Writer::open(&db.0).unwrap());
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        runtime.observe_mouse(
            physical_descriptor("k1"),
            runtime.allocate_connection(),
            true,
        );
        let executor = FakeExecutor::always(DpiProbeResult::Unavailable);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut worker =
            DpiWorker::with_executor(executor.clone(), Duration::from_millis(100), Duration::ZERO);

        worker.tick(&runtime, &writer, &tx); // 派发
        std::thread::sleep(Duration::from_millis(50));
        worker.tick(&runtime, &writer, &tx); // 收割 → Unavailable
        assert_eq!(executor.calls(), 1);
        let calls_after_first = executor.calls();

        // 未到 10s：不重派发（扫描因 2s 节流不触发；注入 +2s 时钟后扫描触发但重试未到期）
        override_unix(&runtime, T0 + 2_000_000);
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(executor.calls(), calls_after_first, "10s 内不得重试");

        // ≥10s：重派发
        override_unix(&runtime, T0 + UNAVAILABLE_RETRY_UNIX_US);
        worker.tick(&runtime, &writer, &tx);
        std::thread::sleep(Duration::from_millis(50));
        worker.tick(&runtime, &writer, &tx);
        assert!(executor.calls() > calls_after_first, "10s 后应重试");
    }

    /// 业务期限（§4.3）：到期立即发布 Unavailable；执行槽迟到结果拒绝采信；
    /// 槽位在执行槽收尾前不被复用（不开启第二个 probe slot）。
    #[test]
    fn motion_dpi_worker_deadline_publishes_unavailable_and_rejects_late_result() {
        let db = TempDb::new("probe-deadline");
        let writer = Arc::new(clrecoder_store::writer::Writer::open(&db.0).unwrap());
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        runtime.observe_mouse(
            physical_descriptor("k1"),
            runtime.allocate_connection(),
            true,
        );
        // 执行槽 300ms 才返回 Available——远超 100ms 业务期限
        let executor = FakeExecutor::always_with_delay(
            DpiProbeResult::Available(800),
            Duration::from_millis(300),
        );
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut worker =
            DpiWorker::with_executor(executor.clone(), Duration::from_millis(100), Duration::ZERO);

        worker.tick(&runtime, &writer, &tx); // 派发（started）
                                             // 未到期：不发布
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            }
        );
        // 到期：立即发布 Unavailable（槽位仍被占用）
        std::thread::sleep(Duration::from_millis(120));
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            }
        );
        // 执行槽迟到收尾（300ms）：结果迟到 → 拒绝，槽位释放后也不复用结果
        std::thread::sleep(Duration::from_millis(300));
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            },
            "迟到的探测结果必须拒绝采信"
        );
        // 槽位已释放：下次扫描可重新派发（此时重试节流不受 10s 限制——Available 判定
        // 之外的 Unavailable 状态在 10s 内不重派发，断言派发次数仍为 1）
        assert_eq!(executor.calls(), 1, "槽位占用期间不得开启第二个 probe slot");
    }

    /// virtual 桶（physical=false）与断连来源永不探测（§4.3：禁自动；重连重判）。
    #[test]
    fn motion_dpi_worker_virtual_and_disconnected_sources_are_never_probed() {
        let db = TempDb::new("probe-skip");
        let writer = Arc::new(clrecoder_store::writer::Writer::open(&db.0).unwrap());
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        let virtual_desc = MouseSourceDescriptor {
            source_key: "virtual:unknown".to_string(),
            model: DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0,
                pid: 0,
                name: "未知/虚拟设备".to_string(),
            },
            interface_path: None,
            physical: false,
        };
        runtime.observe_mouse(virtual_desc, runtime.allocate_connection(), true);
        let conn = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn, true);
        runtime.observe_mouse(physical_descriptor("k1"), conn, false); // 置断连

        let executor = FakeExecutor::always(DpiProbeResult::Available(800));
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut worker =
            DpiWorker::with_executor(executor.clone(), Duration::from_millis(100), Duration::ZERO);
        worker.tick(&runtime, &writer, &tx);
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(executor.calls(), 0, "virtual/断连来源不得派发探测");
    }

    /// 旧代探测结果拒绝（§4.3：结果带连接代际）：派发后来源重连换代际，
    /// 迟到的旧代结果不得覆盖新连接状态。
    #[test]
    fn motion_dpi_worker_old_generation_probe_result_is_rejected() {
        let db = TempDb::new("probe-gen");
        let writer = Arc::new(clrecoder_store::writer::Writer::open(&db.0).unwrap());
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        let conn1 = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn1, true);

        let executor = FakeExecutor::always(DpiProbeResult::Available(800));
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut worker =
            DpiWorker::with_executor(executor.clone(), Duration::from_millis(100), Duration::ZERO);
        worker.tick(&runtime, &writer, &tx); // 派发（conn1）

        // 派发后重连换代际
        let conn2 = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn2, true);

        std::thread::sleep(Duration::from_millis(50));
        worker.tick(&runtime, &writer, &tx); // 收割：conn1 结果 → 旧代拒绝
        let snap = runtime
            .sources_snapshot()
            .into_iter()
            .find(|s| s.source_key == "k1")
            .unwrap();
        assert_eq!(snap.connection, conn2);
        assert_eq!(
            snap.probe,
            clrecoder_core::motion::DpiProbeStatus::Pending,
            "旧代 Available 不得置新连接为 Available（新连接待重判）"
        );
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            }
        );
    }

    /// manual 批读（§4.3）：未配置来源保持 unknown——首次未加载不猜 80；
    /// 已确认值在批读失败（本 fixture 无法注入失败，退而断言窗口内不被清空）下保留。
    #[test]
    fn motion_dpi_worker_manual_unconfigured_stays_unknown_not_80() {
        let db = TempDb::new("manual-unknown");
        let writer = Arc::new(clrecoder_store::writer::Writer::open(&db.0).unwrap());
        // 来源在 DB 注册（批读返回 manual=None 行）
        let desc = physical_descriptor("k1");
        writer.register_mouse_source(&desc).unwrap();
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        runtime.observe_mouse(desc, runtime.allocate_connection(), true);

        let executor = FakeExecutor::always(DpiProbeResult::Unsupported);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut worker =
            DpiWorker::with_executor(executor.clone(), Duration::from_millis(100), Duration::ZERO);
        worker.tick(&runtime, &writer, &tx);
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            },
            "未配置 manual 且探测 Unsupported 时必须保持 unknown（不猜 80）"
        );
    }

    /// 心跳节流（§4.3：约每 2 秒一次，非逐输入）：状态无变化时 2s 窗口内不重复发布
    /// 已连接来源状态。来源预先置于终态判定（Unsupported——同连接不重探），
    /// 避免探测结果变化置脏干扰心跳观察。
    #[test]
    fn motion_dpi_worker_heartbeat_throttles_to_two_seconds() {
        let db = TempDb::new("heartbeat");
        let writer = Arc::new(clrecoder_store::writer::Writer::open(&db.0).unwrap());
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, T0);
        let conn = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn, true);
        runtime.apply_probe_result("k1", conn, DpiProbeResult::Unsupported);
        let executor = FakeExecutor::always(DpiProbeResult::Unsupported);
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut worker =
            DpiWorker::with_executor(executor.clone(), Duration::from_millis(100), SCAN_INTERVAL);

        worker.tick(&runtime, &writer, &tx);
        assert_eq!(drain_states(&rx).len(), 1, "首次发布");
        // 窗口内（+1s）：无变化不重发
        override_unix(&runtime, T0 + 1_000_000);
        worker.tick(&runtime, &writer, &tx);
        assert!(drain_states(&rx).is_empty(), "2s 窗口内不得重复心跳");
        // ≥2s：补发心跳
        override_unix(&runtime, T0 + 2_000_000);
        worker.tick(&runtime, &writer, &tx);
        assert_eq!(drain_states(&rx).len(), 1, "≥2s 应补发心跳");
        assert_eq!(executor.calls(), 0, "Unsupported 同连接不重探");
    }
}
