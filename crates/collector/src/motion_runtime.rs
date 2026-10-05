//! 运动运行时（motion-dpi §4.3）——进程内时钟、连接代际分配、鼠标来源注册、DPI 缓存。
//!
//! 职责边界（§2 模块表）：本模块提供**一个**进程内时钟、来源注册与 DPI worker/cache
//! 生命周期，不提供第二条 GUI 管道；DPI 协调节拍的实现在 [`crate::mouse_dpi`]，
//! 本模块持有它读取/写入的缓存（source 表）并对外暴露采集线程需要的四个只读入口
//! （[`MotionRuntime::stamp`] / [`allocate_connection`] / [`dpi_for`] / [`control`]）。
//!
//! 关键语义（§4.1/§4.3 锁定）：
//! - 时钟：持有启动 [`Instant`]，[`MotionRuntime::stamp`] 在**同一采样时刻**成对取得
//!   单调 µs 与 UTC unix µs——两个采集线程共用同一运行时，绝不以 aggregator 到达时间
//!   代替捕获时间；
//! - 连接 ID：[`MotionConnectionId`] 进程内单调分配、断连重连生成新值；与 source_key
//!   （设备身份）不可互换；
//! - 来源注册：[`MotionRuntime::observe_mouse`] 按 source_key 维护单份内存表——新连接
//!   （代际更大）重置探测判定（重连重判）、保留 manual/auto 自然过期；旧代 observe 与
//!   旧代探测结果一律拒绝（同 [`crate::engine_loop`] 的 DB 代际门对应内存侧）；
//! - DPI 取值（§4.3）：auto 未过期优先，其次 manual，否则 `value=None/origin=Unknown`
//!   （不猜 80）；`physical=false`（virtual:unknown 桶）禁自动/手动物理 DPI 换算；
//! - 暂停：[`MotionRuntime::control`] 委托 [`Flags::motion_control`]——一致快照，
//!   producer 与 aggregator 共用；
//! - 停机：[`MotionRuntime::stop_requested`] 暴露 `producer_stop`（注册失败重试等
//!   生产者检查点）；[`MotionRuntime::stop_worker`] 只发协调 worker 停止信号、
//!   **不等待**挂起的原生 IO（执行槽资源由存活线程持有到 IO 收尾或进程退出，§4.3）。
//!
//! 模式：[`MotionRuntime::start`]（生产——附带 writer/tx 启动 DPI 协调 worker）与
//! [`MotionRuntime::offline`]（selftest/fixture——无 DB 依赖、不启动 HID worker，
//! 禁真实 HID/生产 DB）。worker 线程有意分离：停止只经旗标（≤1 个 [`crate::mouse_dpi`]
//! 节拍内退出），JoinHandle 不被 join，进程退出时随进程回收。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use clrecoder_core::event::AggEvent;
use clrecoder_core::motion::{
    DpiOrigin, DpiProbeStatus, EffectiveDpi, MotionConnectionId, MotionControlSnapshot,
    MotionStamp, MouseSourceDescriptor, MouseSourceState,
};
use clrecoder_store::writer::Writer;
use crossbeam_channel::Sender;

use crate::hidpp_dpi::DpiProbeResult;
use crate::ipc_server::Flags;
use crate::mouse_dpi;

/// auto DPI 的有效期（§4.3：4 秒自动数值过期；单位 unix µs）。
pub(crate) const AUTO_TTL_UNIX_US: i64 = 4_000_000;
/// 连接来源状态心跳间隔（§4.3：约每 2 秒一次发布，维持 DB last_seen ≤5s 新鲜度门）。
pub(crate) const HEARTBEAT_UNIX_US: i64 = 2_000_000;

/// 连接 ID 起点：进程内单调分配且断连重连生成新值；从 1 起（0 不用于真实连接）。
const FIRST_CONNECTION_ID: u64 = 1;

/// 自动读取到的 DPI 值及其失效时刻（unix µs）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutoDpi {
    value: u32,
    valid_until_unix_us: i64,
}

/// 鼠标来源的内存态（按 source_key 单份；worker 与采集线程经运行时方法读写）。
#[derive(Debug, Clone)]
struct MouseSourceEntry {
    descriptor: MouseSourceDescriptor,
    /// 该来源当前连接代际（重连换代际；旧代 observe/探测结果拒绝）
    connection: MotionConnectionId,
    connected: bool,
    /// 探测判定（Pending 起步；Unsupported/Ambiguous 同连接不重探，重连重判）
    probe: DpiProbeStatus,
    /// 最近一次已确认的 manual 配置（批读失败保留上一份；None=未配置）
    manual: Option<u32>,
    /// 最近一次自动读取值（4s 过期，过期不继续沿用）
    auto: Option<AutoDpi>,
    /// 状态自上次发布以来有变化（连接/探测/配置）——worker 下个节拍即发布
    dirty: bool,
    /// 上次状态发布时刻（unix µs；心跳 ≥2s 节流）
    last_published_unix_us: i64,
}

impl MouseSourceEntry {
    /// 按连接性/物理性给新条目或新连接的初始探测判定：
    /// 已连接物理设备 → Pending（待探测）；virtual 桶 → Unsupported（无自动读取）；
    /// 断连 → Disconnected。
    fn initial_probe(connected: bool, physical: bool) -> DpiProbeStatus {
        if !connected {
            DpiProbeStatus::Disconnected
        } else if !physical {
            DpiProbeStatus::Unsupported
        } else {
            DpiProbeStatus::Pending
        }
    }

    /// 构造对外发布的 [`MouseSourceState`]。断连时 probe_status 恒为 Disconnected；
    /// auto 值连同失效时刻原样发布（DB 侧过期判断由读取方按 auto_valid_until 执行）。
    fn state(&self, stamp: MotionStamp) -> MouseSourceState {
        MouseSourceState {
            descriptor: self.descriptor.clone(),
            connection: self.connection,
            connected: self.connected,
            stamp,
            probe_status: if self.connected {
                self.probe
            } else {
                DpiProbeStatus::Disconnected
            },
            auto_dpi: self.auto.map(|a| a.value),
            auto_valid_until_unix_us: self.auto.map(|a| a.valid_until_unix_us),
        }
    }
}

/// 来源表快照（worker 探测扫描的读取形态；避免把锁内引用外泄）。
#[derive(Debug, Clone)]
pub(crate) struct SourceSnapshot {
    pub(crate) source_key: String,
    pub(crate) descriptor: MouseSourceDescriptor,
    pub(crate) connection: MotionConnectionId,
    pub(crate) connected: bool,
    pub(crate) probe: DpiProbeStatus,
}

/// 运动运行时（§4.3 设计声明：clock、dpi cache、flags、worker）。
pub struct MotionRuntime {
    /// 启动时刻（单调时钟基准；[`MotionRuntime::stamp`] 从它起算 mono_us）
    started: Instant,
    /// 共享运行开关（暂停快照/producer_stop/shutdown）
    flags: Arc<Flags>,
    /// 连接代际分配器（进程内单调）
    next_connection: AtomicU64,
    /// 来源表：source_key → 内存态（DPI cache + 探测判定 + 心跳簿记）
    sources: Mutex<HashMap<String, MouseSourceEntry>>,
    /// 协调 worker 停止旗标（[`MotionRuntime::stop_worker`] 置位；不等待挂起 IO）
    worker_stop: AtomicBool,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 测试注入的固定采样时刻（假 clock；仅测试构建存在，生产 stamp 走真实时钟）
    #[cfg(test)]
    stamp_override: Mutex<Option<MotionStamp>>,
}

impl MotionRuntime {
    fn new(flags: Arc<Flags>) -> Self {
        Self {
            started: Instant::now(),
            flags,
            next_connection: AtomicU64::new(FIRST_CONNECTION_ID),
            sources: Mutex::new(HashMap::new()),
            worker_stop: AtomicBool::new(false),
            worker: Mutex::new(None),
            #[cfg(test)]
            stamp_override: Mutex::new(None),
        }
    }

    /// 生产模式构造（motion-dpi §4.3）：附带 writer 与事件通道启动 DPI 协调 worker
    /// （manual 批读/探测/状态发布）。协调线程由stop_worker收尾，probe IO独立收尾。
    pub fn start(writer: Arc<Writer>, flags: Arc<Flags>, tx: Sender<AggEvent>) -> Arc<Self> {
        let runtime = Arc::new(Self::new(flags));
        let handle = mouse_dpi::spawn_coordinator(Arc::clone(&runtime), writer, tx);
        *runtime
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = handle;
        runtime
    }

    /// offline 模式构造（§4.3：selftest——无 DB/HID worker）：manual/auto 恒空、
    /// 不探测、不发布；时钟/连接分配/控制快照照常可用（fixture 禁真实 HID/生产 DB）。
    pub fn offline(flags: Arc<Flags>) -> Arc<Self> {
        Arc::new(Self::new(flags))
    }

    /// 采样时刻（§4.1/§4.3）：单调 µs 与 UTC unix µs 成对取得——同一采样时刻，
    /// 不得分开取或以 aggregator 到达时间补齐。
    pub fn stamp(&self) -> MotionStamp {
        #[cfg(test)]
        {
            let guard = self
                .stamp_override
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(s) = guard.as_ref().copied() {
                return s;
            }
        }
        let mono_us = self.started.elapsed().as_micros() as u64;
        let unix_us = unix_now_micros();
        MotionStamp { mono_us, unix_us }
    }

    /// 分配运动连接代际（§4.1）：进程内单调、断连重连生成新值；与 source_key 不可互换。
    pub fn allocate_connection(&self) -> MotionConnectionId {
        MotionConnectionId(self.next_connection.fetch_add(1, Ordering::Relaxed))
    }

    /// 登记一次鼠标来源观察（§4.3：注册或重连时查询；接口路径/ContainerID 只在此刻解析）。
    ///
    /// - 新来源：插入内存表，探测判定按 [`MouseSourceEntry::initial_probe`]；
    /// - 同连接：仅同步 connected（状态变化置脏，worker 下个节拍发布）；
    /// - 更大代际（重连）：换代际、刷新描述、探测判定重置（重连重判），manual/auto
    ///   保留manual后备，清除上一连接的auto读数；
    /// - 更小代际（旧连接的迟到观察）：忽略。
    pub fn observe_mouse(
        &self,
        descriptor: MouseSourceDescriptor,
        connection: MotionConnectionId,
        connected: bool,
    ) {
        let key = descriptor.source_key.clone();
        let stamp_unix_us = self.stamp().unix_us;
        let mut guard = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get_mut(&key) {
            Some(e) if e.connection == connection => {
                if e.connected != connected {
                    e.connected = connected;
                    e.auto = None;
                    e.probe = MouseSourceEntry::initial_probe(connected, e.descriptor.physical);
                    e.dirty = true;
                }
            }
            Some(e) if connection.0 > e.connection.0 => {
                e.connection = connection;
                e.descriptor = descriptor;
                e.connected = connected;
                e.auto = None;
                e.probe = MouseSourceEntry::initial_probe(connected, e.descriptor.physical);
                e.dirty = true;
            }
            // 旧代 observe：忽略（同连接已换代际，不回退）
            Some(_) => {}
            // 新来源（进程首见，不与既有代际比较）：插入并按连接性/物理性给初始判定
            None => {
                let physical = descriptor.physical;
                guard.insert(
                    key,
                    MouseSourceEntry {
                        descriptor,
                        connection,
                        connected,
                        probe: MouseSourceEntry::initial_probe(connected, physical),
                        manual: None,
                        auto: None,
                        dirty: true,
                        last_published_unix_us: stamp_unix_us.saturating_sub(HEARTBEAT_UNIX_US),
                    },
                );
            }
        }
    }

    /// 取该来源当前有效 DPI（§4.3）：auto 未过期优先，其次 manual，否则 unknown
    /// （不猜 80）。virtual 桶（physical=false）禁物理 DPI 换算——恒 unknown。
    pub fn dpi_for(&self, key: &str) -> EffectiveDpi {
        let guard = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(e) = guard.get(key) else {
            return EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown,
            };
        };
        if !e.descriptor.physical {
            return EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown,
            };
        }
        let now_unix_us = self.stamp().unix_us;
        if e.connected && e.probe == DpiProbeStatus::Available {
            if let Some(a) = e.auto {
                if a.valid_until_unix_us > now_unix_us {
                    return EffectiveDpi {
                        value: Some(a.value),
                        origin: DpiOrigin::Auto,
                    };
                }
            }
        }
        if let Some(m) = e.manual {
            return EffectiveDpi {
                value: Some(m),
                origin: DpiOrigin::Manual,
            };
        }
        EffectiveDpi {
            value: None,
            origin: DpiOrigin::Unknown,
        }
    }

    /// 暂停控制快照（§4.3）：委托 [`Flags::motion_control`]——producer 与 aggregator
    /// 共用同一一致快照，不分开读 AtomicBool＋epoch。
    pub fn control(&self) -> MotionControlSnapshot {
        self.flags.motion_control()
    }

    /// 生产者停机请求（§4.3.1）：raw_input 注册失败重试等检查点读取；
    /// 停止唤醒只发本进程自有消息线程，不改系统输入。
    pub fn stop_requested(&self) -> bool {
        self.flags.producer_stop.load(Ordering::Acquire)
    }

    /// 等待协调worker结束事件发布，然后才允许聚合器最终排空。
    /// 不等待不可取消的probe IO，其资源由执行槽持有至收尾或进程退出。
    pub fn stop_worker(&self) {
        self.worker_stop.store(true, Ordering::Release);
        let handle = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            if handle.join().is_err() {
                log::error!("DPI 协调线程异常退出，进入聚合收尾");
            }
        }
    }

    /// worker 停止判定（协调 worker 每节拍检查）：显式停止或进程 shutdown 均停止
    /// 发布与探测（§4.3"shutdown 停止协调 worker 及后续事件发布"）。
    pub(crate) fn worker_should_stop(&self) -> bool {
        self.worker_stop.load(Ordering::Acquire) || self.flags.shutdown.load(Ordering::Acquire)
    }

    /// worker：全部来源 key（manual 配置批读入参）。
    pub(crate) fn source_keys(&self) -> Vec<String> {
        let guard = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        guard.keys().cloned().collect()
    }

    /// worker：来源表快照（探测扫描用）。
    pub(crate) fn sources_snapshot(&self) -> Vec<SourceSnapshot> {
        let guard = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .values()
            .map(|e| SourceSnapshot {
                source_key: e.descriptor.source_key.clone(),
                descriptor: e.descriptor.clone(),
                connection: e.connection,
                connected: e.connected,
                probe: e.probe,
            })
            .collect()
    }

    /// worker：应用一次 manual 配置批读结果（§4.3：批读失败时调用方不调用本方法——
    /// 保留上一份已确认 manual 值；值未变化不置脏，避免无意义的心跳发布）。
    pub(crate) fn apply_manual(&self, key: &str, manual: Option<u32>) {
        let mut guard = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(e) = guard.get_mut(key) else { return };
        if e.manual != manual {
            e.manual = manual;
            e.dirty = true;
        }
    }

    /// worker：应用一次探测结果（§4.3：结果带连接代际——旧代结果拒绝，不覆盖新连接
    /// 状态）。Available刷新auto与有效期；失败立即清除auto并退回manual。
    pub(crate) fn apply_probe_result(
        &self,
        key: &str,
        connection: MotionConnectionId,
        result: DpiProbeResult,
    ) {
        let mut guard = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(e) = guard.get_mut(key) else { return };
        if e.connection != connection || !e.connected {
            return; // 旧代结果拒绝（重连重判）
        }
        match result {
            DpiProbeResult::Available(dpi) => {
                e.probe = DpiProbeStatus::Available;
                e.auto = Some(AutoDpi {
                    value: dpi,
                    valid_until_unix_us: self.stamp().unix_us + AUTO_TTL_UNIX_US,
                });
            }
            DpiProbeResult::Unsupported => {
                e.probe = DpiProbeStatus::Unsupported;
                e.auto = None;
            }
            DpiProbeResult::Ambiguous => {
                e.probe = DpiProbeStatus::Ambiguous;
                e.auto = None;
            }
            DpiProbeResult::Unavailable => {
                e.probe = DpiProbeStatus::Unavailable;
                e.auto = None;
            }
        }
        e.dirty = true;
    }

    /// worker：收集本节拍应发布的来源状态（§4.3：状态变化即发布；已连接来源按
    /// [`HEARTBEAT_UNIX_US`] 心跳维持 DB last_seen 新鲜度；断连源发布一次即静默）。
    pub(crate) fn due_states(&self, stamp: MotionStamp) -> Vec<MouseSourceState> {
        let mut guard = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for e in guard.values_mut() {
            let due = e.dirty
                || (e.connected && stamp.unix_us - e.last_published_unix_us >= HEARTBEAT_UNIX_US);
            if due {
                out.push(e.state(stamp));
                e.dirty = false;
                e.last_published_unix_us = stamp.unix_us;
            }
        }
        out
    }
}

/// UTC unix µs（§4.1）。系统时钟早于纪元（异常环境）退化为 0——日归属与过期判断
/// 退化为保守值，绝不 panic（§1 原则 3）。
fn unix_now_micros() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_micros() as i64,
        Err(_) => 0,
    }
}

#[cfg(test)]
impl MotionRuntime {
    /// 测试注入固定采样时刻（假 clock）：后续 [`MotionRuntime::stamp`] 恒返回该值，
    /// 直到再次注入。仅影响 stamp 消费方（日归属/有效期/心跳节流）。
    pub(crate) fn set_stamp_override_for_tests(&self, stamp: Option<MotionStamp>) {
        *self
            .stamp_override
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = stamp;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use clrecoder_core::codes::DeviceKind;
    use clrecoder_core::event::DeviceKey;

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

    fn virtual_descriptor() -> MouseSourceDescriptor {
        MouseSourceDescriptor {
            source_key: "virtual:unknown".to_string(),
            model: DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0,
                pid: 0,
                name: "未知/虚拟设备".to_string(),
            },
            interface_path: None,
            physical: false,
        }
    }

    #[test]
    fn motion_dpi_reconnect_requires_fresh_auto_reading() {
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, 1_000_000_000);
        let first = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("mouse"), first, true);
        runtime.apply_manual("mouse", Some(1600));
        runtime.apply_probe_result("mouse", first, DpiProbeResult::Available(800));
        runtime.observe_mouse(physical_descriptor("mouse"), first, false);
        assert_eq!(runtime.dpi_for("mouse").origin, DpiOrigin::Manual);
        runtime.apply_probe_result("mouse", first, DpiProbeResult::Available(800));
        assert_eq!(runtime.dpi_for("mouse").origin, DpiOrigin::Manual);
        let second = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("mouse"), second, true);
        assert_eq!(runtime.dpi_for("mouse").value, Some(1600));
        runtime.apply_probe_result("mouse", second, DpiProbeResult::Available(1200));
        assert_eq!(runtime.dpi_for("mouse").value, Some(1200));
        runtime.apply_probe_result("mouse", second, DpiProbeResult::Unavailable);
        assert_eq!(runtime.dpi_for("mouse").value, Some(1600));
    }

    #[test]
    fn motion_dpi_stop_worker_waits_for_publisher_to_exit() {
        let dir = std::env::temp_dir().join(format!(
            "clrec-dpi-stop-{}-{}",
            std::process::id(),
            unix_now_micros()
        ));
        let writer = Arc::new(Writer::open(&dir.join("stats.db")).unwrap());
        let (tx, rx) = crossbeam_channel::unbounded();
        let runtime = MotionRuntime::start(Arc::clone(&writer), flags(), tx);
        runtime.stop_worker();
        assert!(matches!(
            rx.try_recv(),
            Err(crossbeam_channel::TryRecvError::Disconnected)
        ));
        drop(runtime);
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// stamp 覆盖为给定 unix µs（假 clock；mono 保持运行时真实值无所谓）。
    fn override_unix(runtime: &MotionRuntime, unix_us: i64) {
        runtime.set_stamp_override_for_tests(Some(MotionStamp {
            mono_us: 0,
            unix_us,
        }));
    }

    // ---------- 时钟与连接代际 ----------

    /// stamp 成对字段：mono 为启动起算的非负单调值；unix 与系统当前时间同量级（±1 分钟）。
    #[test]
    fn motion_dpi_stamp_pairs_mono_and_unix() {
        let runtime = MotionRuntime::offline(flags());
        let s = runtime.stamp();
        let now_unix = unix_now_micros();
        assert!(
            s.unix_us.abs_diff(now_unix) < 60_000_000,
            "unix µs 应接近当前时刻"
        );
        let s2 = runtime.stamp();
        assert!(s2.mono_us >= s.mono_us, "单调 µs 不得回退");
        assert!(s2.unix_us >= s.unix_us, "unix µs 不得回退");
    }

    /// 连接 ID 单调分配、非零；断连重连（再次分配）必得新值（§4.1）。
    #[test]
    fn motion_dpi_allocate_connection_is_monotonic_and_positive() {
        let runtime = MotionRuntime::offline(flags());
        let a = runtime.allocate_connection();
        let b = runtime.allocate_connection();
        assert_ne!(a, b);
        assert!(
            a.0 >= 1 && b.0 > a.0,
            "连接 ID 必须严格递增且非零: {a:?} → {b:?}"
        );
        // 两个运行时各自独立分配（进程内共享的是同一实例；offline/start 均从 1 起）
        let other = MotionRuntime::offline(flags());
        assert_eq!(other.allocate_connection(), MotionConnectionId(1));
    }

    /// control 委托 Flags.motion_control：set_paused 的代际变化在运行时快照可见。
    #[test]
    fn motion_dpi_control_delegates_to_flags_snapshot() {
        let f = flags();
        let runtime = MotionRuntime::offline(StdArc::clone(&f));
        assert_eq!(
            runtime.control(),
            MotionControlSnapshot {
                epoch: 0,
                paused: false
            }
        );
        f.set_paused(true);
        assert_eq!(
            runtime.control(),
            MotionControlSnapshot {
                epoch: 1,
                paused: true
            }
        );
    }

    // ---------- 来源注册与 DPI 取值 ----------

    /// 首次 observe 即登记（无需移动）：dpi_for 未配置 → Unknown（不猜 80）。
    #[test]
    fn motion_dpi_observe_registers_and_dpi_starts_unknown() {
        let runtime = MotionRuntime::offline(flags());
        let conn = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn, true);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            },
            "首次未加载为 unknown"
        );
        // 未知 key 同样 unknown
        assert_eq!(
            runtime.dpi_for("ghost"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            }
        );
    }

    /// DPI 取值优先级（§4.3）：auto 未过期优先于 manual；auto 过期退 manual；
    /// 两者皆无退 unknown。
    #[test]
    fn motion_dpi_dpi_for_priority_auto_then_manual_then_unknown() {
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, 1_000_000_000);
        let conn = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn, true);
        runtime.apply_manual("k1", Some(1600));
        // 仅 manual
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: Some(1600),
                origin: DpiOrigin::Manual
            }
        );
        // 探测成功：auto 在有效期内 → auto 优先
        runtime.apply_probe_result("k1", conn, DpiProbeResult::Available(800));
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: Some(800),
                origin: DpiOrigin::Auto
            }
        );
        // 4s 内仍 auto
        override_unix(&runtime, 1_000_000_000 + AUTO_TTL_UNIX_US - 1);
        assert_eq!(runtime.dpi_for("k1").origin, DpiOrigin::Auto);
        // 过期（valid_until ≤ now）→ 退 manual
        override_unix(&runtime, 1_000_000_000 + AUTO_TTL_UNIX_US);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: Some(1600),
                origin: DpiOrigin::Manual
            }
        );
        // manual 也清掉 → unknown
        runtime.apply_manual("k1", None);
        assert_eq!(
            runtime.dpi_for("k1"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            }
        );
    }

    /// virtual 桶（physical=false）禁物理 DPI 换算：即使误配 manual/auto 也恒 unknown。
    #[test]
    fn motion_dpi_dpi_for_rejects_dpi_for_virtual_sources() {
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, 1_000_000_000);
        let conn = runtime.allocate_connection();
        runtime.observe_mouse(virtual_descriptor(), conn, true);
        runtime.apply_manual("virtual:unknown", Some(800));
        runtime.apply_probe_result("virtual:unknown", conn, DpiProbeResult::Available(800));
        assert_eq!(
            runtime.dpi_for("virtual:unknown"),
            EffectiveDpi {
                value: None,
                origin: DpiOrigin::Unknown
            },
            "physical=false 仅保留原始量，禁自动/手动物理 DPI"
        );
    }

    /// 重连重判（§4.3）：更大代际 observe 重置探测判定并保留 manual；
    /// 旧代探测结果拒绝（不覆盖新连接状态）；旧代 observe 忽略。
    #[test]
    fn motion_dpi_observe_reconnect_resets_probe_and_rejects_stale_results() {
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, 1_000_000_000);
        let c1 = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), c1, true);
        runtime.apply_probe_result("k1", c1, DpiProbeResult::Available(800));
        runtime.apply_manual("k1", Some(1600));

        // 重连重新探测，旧自动值不再属于当前连接；manual后备保留。
        let c2 = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), c2, true);
        let snap = runtime
            .sources_snapshot()
            .into_iter()
            .find(|s| s.source_key == "k1")
            .unwrap();
        assert_eq!(snap.connection, c2, "重连必须换代际");
        assert_eq!(snap.probe, DpiProbeStatus::Pending, "重连重判");
        assert_eq!(
            runtime.dpi_for("k1").value,
            Some(1600),
            "重连后等待新自动值，使用manual后备"
        );

        // 旧代探测结果（c1）拒绝：不得把新连接置为 Available
        runtime.apply_probe_result("k1", c1, DpiProbeResult::Unavailable);
        let snap = runtime
            .sources_snapshot()
            .into_iter()
            .find(|s| s.source_key == "k1")
            .unwrap();
        assert_eq!(
            snap.probe,
            DpiProbeStatus::Pending,
            "旧代结果不得覆盖新连接"
        );
        assert_eq!(snap.connection, c2);

        // 旧代 observe（c1 断连）忽略：新连接不得被标离线
        runtime.observe_mouse(physical_descriptor("k1"), c1, false);
        let snap = runtime
            .sources_snapshot()
            .into_iter()
            .find(|s| s.source_key == "k1")
            .unwrap();
        assert!(snap.connected, "旧代 disconnect 不得把新连接标离线");
    }

    /// 断连发布（§4.1）：同连接 connected=false 置脏，due_states 发布 Disconnected；
    /// 断连后不再心跳（发布一次即静默）。
    #[test]
    fn motion_dpi_observe_disconnect_publishes_disconnected_state() {
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, 1_000_000_000);
        let conn = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn, true);
        let states = runtime.due_states(runtime.stamp());
        assert_eq!(states.len(), 1, "新来源置脏即发布");
        assert!(states[0].connected);

        // 断连：状态发布 connected=false + Disconnected
        runtime.observe_mouse(physical_descriptor("k1"), conn, false);
        let states = runtime.due_states(runtime.stamp());
        assert_eq!(states.len(), 1);
        assert!(!states[0].connected);
        assert_eq!(states[0].probe_status, DpiProbeStatus::Disconnected);

        // 未变化且已断连：不再心跳
        assert!(runtime.due_states(runtime.stamp()).is_empty());
    }

    /// 心跳节流（§4.3：约每 2 秒一次）：连接中来源 ≥2s 未发布则补发；窗口内不重发。
    #[test]
    fn motion_dpi_heartbeat_publishes_every_two_seconds() {
        let runtime = MotionRuntime::offline(flags());
        override_unix(&runtime, 1_000_000_000);
        let conn = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), conn, true);
        assert_eq!(runtime.due_states(runtime.stamp()).len(), 1, "首次发布");
        // 2s 窗口内：不重发
        override_unix(&runtime, 1_000_000_000 + HEARTBEAT_UNIX_US - 1);
        assert!(runtime.due_states(runtime.stamp()).is_empty());
        // ≥2s：补发心跳
        override_unix(&runtime, 1_000_000_000 + HEARTBEAT_UNIX_US);
        let states = runtime.due_states(runtime.stamp());
        assert_eq!(states.len(), 1);
        assert!(states[0].connected);
        assert_eq!(states[0].stamp.unix_us, 1_000_000_000 + HEARTBEAT_UNIX_US);
    }

    /// worker 读取面：source_keys / sources_snapshot 形状与注册一致。
    #[test]
    fn motion_dpi_source_keys_and_snapshot_shape() {
        let runtime = MotionRuntime::offline(flags());
        let c1 = runtime.allocate_connection();
        runtime.observe_mouse(physical_descriptor("k1"), c1, true);
        runtime.observe_mouse(virtual_descriptor(), runtime.allocate_connection(), true);
        let mut keys = runtime.source_keys();
        keys.sort();
        assert_eq!(keys, vec!["k1".to_string(), "virtual:unknown".to_string()]);
        let snaps = runtime.sources_snapshot();
        assert_eq!(snaps.len(), 2);
        let k1 = snaps.iter().find(|s| s.source_key == "k1").unwrap();
        assert!(k1.connected);
        assert!(k1.descriptor.physical);
        assert_eq!(k1.probe, DpiProbeStatus::Pending);
        let v = snaps
            .iter()
            .find(|s| s.source_key == "virtual:unknown")
            .unwrap();
        assert_eq!(v.probe, DpiProbeStatus::Unsupported, "virtual 桶无自动读取");
    }

    /// stop_requested 反映 producer_stop；stop_worker 置位 worker 停止判定（offline
    /// 无 worker 线程，旗标语义仍可验证）。
    #[test]
    fn motion_dpi_stop_flags_surface_through_runtime() {
        let f = flags();
        let runtime = MotionRuntime::offline(StdArc::clone(&f));
        assert!(!runtime.stop_requested());
        f.producer_stop.store(true, Ordering::Release);
        assert!(runtime.stop_requested());
        assert!(!runtime.worker_should_stop());
        runtime.stop_worker();
        assert!(runtime.worker_should_stop());
        // shutdown 同样停止 worker（§4.3"shutdown 停止协调 worker 及后续事件发布"）
        let f2 = flags();
        let runtime2 = MotionRuntime::offline(StdArc::clone(&f2));
        f2.shutdown.store(true, Ordering::Release);
        assert!(runtime2.worker_should_stop());
    }
}
