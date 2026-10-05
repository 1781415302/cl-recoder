//! selftest 模式（PLAN §8-S9 验收点）。
//!
//! 两种自检，均**不创建单实例互斥体、不启动 pipe 服务**（诊断模式，不与常驻实例争抢
//! `\\.\pipe\clrecoder-control`，也不应被正在运行的采集器挡住冒烟关卡）：
//!
//! - [`run_inject`]（`--selftest-inject --db <临时库>`）：**不启动任何采集线程**，把一组
//!   合成 [`AggEvent`] 直接灌入 aggregator→store 全链路（先删除已存在的库文件，含 WAL
//!   旁路），事件脚本覆盖：键盘 a/b/c/Enter 各 1 次（来源 1）、Ctrl+C 1 次、Shift+A 1 次、
//!   鼠标左键 2 次、双来源测试（来源 2 按同型号 B 1 次，并入同型号桶，§4.3）、≥2 次
//!   Foreground 切换、暂停 2s 再恢复（经 `Flags::set_paused`，motion-dpi §4.3.1）、
//!   新运动合成场景（来源状态 + 800/1600/unknown 三 DPI 桶 counts，§4.4 精确示例）、
//!   手柄运动场景（连接 1 的 4 帧摇杆轨迹 + 断连，§4.3）——
//!   供外部脚本对 `input_daily`/`combo_daily`/`app_daily`/`mouse_motion_daily`/
//!   `gamepad_motion_daily`/`gamepad_heat_daily` 做 sqlite
//!   断言（行正确、暂停区间秒数为 0、工作集 <100MB）。
//!   注入端同时以与 apps 线程相同的语义（先落状态再发事件）维护 [`FgState`]，使 exe 归属
//!   与真实链路一致。结束时把"期望的库内容"打印到 stdout，供关卡脚本/人工对照。
//! - [`run_live`]（`--selftest N [--db path]`）：**真实采集模式**——完整起 raw_input/
//!   gamepad/apps 三采集线程与 aggregator（运动运行时为 offline 形态：无 DB/HID worker，
//!   fixture 禁真实 HID 探测），main 线程作泵：把收到的事件以 `kind|device|code|down` 行
//!   打印到 stdout（`# ` 开头为注释/前台切换/生命周期/运动行）并**原样转发**给
//!   aggregator 写库（生命周期/运动事件只打印不丢弃，§4.3）；N 秒后按 §4.3.1 退出序
//!   收尾（producer_stop → raw_input stop_and_join 排尾桶 → drained → 终账 + 最后一批
//!   flush）退出。`--db` 缺省 `./stats.db`。
//!
//! 时间编排（注入脚本）：alpha.exe 驻留 2.6s → 暂停 2.0s → beta.exe 驻留 1.6s。aggregator
//! 的暂停观察心跳为 100ms（见 engine_loop），归账秒数取整后 alpha≈2、beta≈1，且暂停的
//! 2s 恒不产生秒数——对 0.5s flush 周期的相位不敏感，断言稳定。

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clrecoder_core::codes::{DeviceKind, MouseButton};
use clrecoder_core::day;
use clrecoder_core::event::{AggEvent, DeviceKey, InputSourceId, RawEvent};
use clrecoder_core::motion::{
    local_day_from_unix_us, DpiOrigin, DpiProbeStatus, EffectiveDpi, GamepadMotionFrame,
    MotionConnectionId, MotionControlSnapshot, MotionStamp, MouseSourceDescriptor,
    MouseSourceState, MouseTravelDelta, StickPoint,
};
use clrecoder_store::writer::Writer;

use crate::apps::{EXE_UNKNOWN, FgState};
use crate::engine_loop;
use crate::ipc_server::{Flags, RuntimeStatus};
use crate::motion_runtime::MotionRuntime;

/// 注入键盘默认来源（§4.3：固定 `InputSourceId(1)`；0 保留给 Engine 单来源兼容入口）。
const INJECT_SOURCE: InputSourceId = InputSourceId(1);
/// 双来源测试的第二来源（§4.3：不同正 ID；与来源 1 同 DeviceKey 并入同型号桶）。
const INJECT_SOURCE_ALT: InputSourceId = InputSourceId(2);

/// 注入用合成键盘设备（devices 表 UNIQUE 四元组，与未知桶/真实设备均不冲突）。
fn inject_keyboard() -> DeviceKey {
    DeviceKey { kind: DeviceKind::Keyboard, vid: 0x1111, pid: 0x0001, name: "Selftest 键盘".to_string() }
}

/// 注入用合成鼠标设备。
fn inject_mouse() -> DeviceKey {
    DeviceKey { kind: DeviceKind::Mouse, vid: 0x1111, pid: 0x0002, name: "Selftest 鼠标".to_string() }
}

/// 注入用合成鼠标运动来源（motion-dpi §4.3 新运动场景；key 唯一，不与真实/未知桶冲突）。
fn inject_motion_source() -> MouseSourceDescriptor {
    MouseSourceDescriptor {
        source_key: "selftest://inject-motion-mouse".to_string(),
        model: inject_mouse(),
        interface_path: None,
        physical: true,
    }
}

/// 注入用合成手柄设备（motion-dpi §4.3 手柄运动场景；独立 vid/pid/name，
/// 不与真实 gilrs "Xbox Controller" 行冲突）。
fn inject_gamepad() -> DeviceKey {
    DeviceKey {
        kind: DeviceKind::Gamepad,
        vid: 0x1111,
        pid: 0x0003,
        name: "Selftest 手柄".to_string(),
    }
}

/// 手柄运动场景的基准 UTC µs：2026-06-01T00:00:00Z（帧跨度 60ms，任意真实时区下
/// 四帧同属一个本地日——时区偏移均为 15 分钟整数倍）。
const GP_BASE_UNIX_US: i64 = 1_780_272_000_000_000;

/// 手柄运动场景的本地归属日（由 stamp 换算，与 tracker 的日归属口径一致）。
fn gp_day() -> String {
    local_day_from_unix_us(GP_BASE_UNIX_US)
        .map(day::format_day)
        .unwrap_or_else(day::today_local)
}

/// alpha.exe 驻留时长（暂停前活动段；归账取整后 ≈2s）。
const DWELL_ALPHA: Duration = Duration::from_millis(2600);
/// 暂停时长（该区间必须产生 0 秒数）。
const PAUSE_FOR: Duration = Duration::from_secs(2);
/// beta.exe 驻留时长（恢复后活动段；归账取整后 ≈1s）。
const DWELL_BETA: Duration = Duration::from_millis(1600);

/// `--selftest-inject` 入口：合成事件注入 aggregator→store 全链路后退出。
/// 返回进程退出码（0=成功）。
pub fn run_inject(db: &Path) -> i32 {
    // 1) 先删除已存在的库文件（任务书 §8-S9：启动时先删除；含 WAL 旁路）
    if let Err(e) = remove_db_files(db) {
        eprintln!("cl-recoder-collector: 删除旧库 {} 失败: {e}", db.display());
        return 1;
    }
    println!("[inject] 已重置库文件: {}", db.display());

    // 2) 打开库（建目录 + migrate + WAL）
    let writer = match Writer::open(db) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            eprintln!("cl-recoder-collector: 打开库 {} 失败: {e}", db.display());
            return 1;
        }
    };

    // 3) 共享状态 + 通道；**只起 aggregator——不启动任何采集线程**（任务书）
    let (tx, rx) = crossbeam_channel::unbounded::<AggEvent>();
    let flags = Arc::new(Flags::default());
    let status = Arc::new(RuntimeStatus::new(env!("CARGO_PKG_VERSION")));
    let fg = Arc::new(Mutex::new(FgState { exe: EXE_UNKNOWN.to_string(), since: Instant::now() }));
    let agg = engine_loop::spawn(rx, writer, Arc::clone(&flags), Arc::clone(&fg), status);

    // 4) 注入脚本（单一发送端 → 通道 FIFO，aggregator 处理顺序与发送顺序一致）
    let kb_dev = inject_keyboard();
    let mouse_dev = inject_mouse();
    let mut sent = 0u64;
    {
        // Foreground 注入与 apps 线程同语义：先落 FgState 再发事件（engine_loop 收到时状态已一致）
        let send_fg = |exe: &str, sent: &mut u64| {
            if let Ok(mut st) = fg.lock() {
                st.exe = exe.to_string();
                st.since = Instant::now();
            }
            let _ = tx.send(AggEvent::Foreground { exe: exe.to_string() });
            *sent += 1;
        };
        let send_kb = |source: InputSourceId, sc: u16, down: bool, sent: &mut u64| {
            let _ = tx.send(AggEvent::Input(RawEvent::Keyboard {
                source,
                device: kb_dev.clone(),
                sc,
                down,
            }));
            *sent += 1;
        };
        let send_click = |button: MouseButton, sent: &mut u64| {
            let _ = tx.send(AggEvent::Input(RawEvent::MouseClick {
                device: mouse_dev.clone(),
                button,
            }));
            *sent += 1;
        };

        println!("[inject] 脚本：Foreground alpha.exe → 键盘 a/b/c/Enter 各1次(来源1) → Ctrl+C → Shift+A → \
                  鼠标左键×2 → 运动场景(来源状态+800/1600/unknown 三桶counts) → \
                  手柄运动场景(4帧+断连) → 驻留 2.6s → \
                  暂停 2.0s(set_paused) → 恢复 → Foreground beta.exe → 双来源B(来源2)×1 → \
                  驻留 1.6s → shutdown");

        // 切换 #1：unknown → alpha.exe
        send_fg("alpha.exe", &mut sent);
        // 键盘 a / b / c / Enter 各 1 次（按下+抬起；默认来源 1）
        for sc in [0x1Eu16, 0x30, 0x2E, 0x1C] {
            send_kb(INJECT_SOURCE, sc, true, &mut sent);
            send_kb(INJECT_SOURCE, sc, false, &mut sent);
        }
        // Ctrl+C 1 次 → Key(Ctrl) + Key(C) + Combo(CTRL, C)
        send_kb(INJECT_SOURCE, 0x1D, true, &mut sent);
        send_kb(INJECT_SOURCE, 0x2E, true, &mut sent);
        send_kb(INJECT_SOURCE, 0x2E, false, &mut sent);
        send_kb(INJECT_SOURCE, 0x1D, false, &mut sent);
        // Shift+A 1 次 → Key(Shift) + Key(A) + Combo(SHIFT, A)
        send_kb(INJECT_SOURCE, 0x2A, true, &mut sent);
        send_kb(INJECT_SOURCE, 0x1E, true, &mut sent);
        send_kb(INJECT_SOURCE, 0x1E, false, &mut sent);
        send_kb(INJECT_SOURCE, 0x2A, false, &mut sent);
        // 鼠标左键 2 次
        send_click(MouseButton::Left, &mut sent);
        send_click(MouseButton::Left, &mut sent);

        // motion-dpi §4.3 新运动合成场景：来源状态 + 三 DPI 桶 counts（§4.4 精确示例：
        // 800 桶 800 counts ＋ 1600 桶 1600 counts ＋ unknown 桶 400 counts →
        // rawCounts 2800、meters 0.0508、unconfiguredCounts 400、coverage ≈0.857143）。
        // 探测状态 Unsupported（手动配置鼠标）与 manual 桶一致；事件均为"已捕获非暂停"
        // 增量——aggregator 无条件落库。
        let motion_desc = inject_motion_source();
        let motion_conn = MotionConnectionId(1);
        let _ = tx.send(AggEvent::MouseSourceState(MouseSourceState {
            descriptor: motion_desc.clone(),
            connection: motion_conn,
            connected: true,
            stamp: MotionStamp { mono_us: 0, unix_us: 0 },
            probe_status: DpiProbeStatus::Unsupported,
            auto_dpi: None,
            auto_valid_until_unix_us: None,
        }));
        sent += 1;
        for (counts, dpi) in [(800.0, Some(800u32)), (1600.0, Some(1600)), (400.0, None)] {
            let _ = tx.send(AggEvent::MouseTravel(MouseTravelDelta {
                descriptor: motion_desc.clone(),
                connection: motion_conn,
                day: day::today_local(),
                counts,
                dpi: match dpi {
                    Some(v) => EffectiveDpi { value: Some(v), origin: DpiOrigin::Manual },
                    None => EffectiveDpi { value: None, origin: DpiOrigin::Unknown },
                },
                control: MotionControlSnapshot { epoch: 0, paused: false },
            }));
            sent += 1;
        }

        // motion-dpi §4.3 手柄运动合成场景（S5）：连接 1 的四帧（stamp 取固定历史时刻
        // GP_BASE_UNIX_US、20ms 步进——日归属由 tracker 按本地日历换算确定）：
        //   f1 中心（首帧仅建锚点）→ f2 左满幅右/右上（进入段：左 1R、右 1R）→
        //   f3 同点保持（20ms 停留）→ f4 左回中（返回段 1R）。
        // tracker 产出：左 active 40ms、travel 2.0R（进入段 1R + 返回段 1R，§4.2 中心往返）、
        // bin 324 共 40ms；右 active 40ms、travel 1.0R（进入段）、bin 12 共 40ms。
        // 随后断连事件复位该连接 tracker（无库效果）。
        let pad_dev = inject_gamepad();
        let gp_frame = |mono_us: u64, left: (f64, f64), right: (f64, f64)| {
            AggEvent::GamepadMotion(GamepadMotionFrame {
                device: pad_dev.clone(),
                connection: MotionConnectionId(1),
                stamp: MotionStamp { mono_us, unix_us: GP_BASE_UNIX_US + mono_us as i64 },
                left: StickPoint { x: left.0, y: left.1 },
                right: StickPoint { x: right.0, y: right.1 },
                control: MotionControlSnapshot { epoch: 0, paused: false },
            })
        };
        for (mono, l, r) in [
            (0u64, (0.0, 0.0), (0.0, 0.0)),
            (20_000, (1.0, 0.0), (0.0, 1.0)),
            (40_000, (1.0, 0.0), (0.0, 1.0)),
            (60_000, (0.0, 0.0), (0.0, 1.0)),
        ] {
            let _ = tx.send(gp_frame(mono, l, r));
            sent += 1;
        }
        let _ = tx.send(AggEvent::GamepadMotionDisconnected { connection: MotionConnectionId(1) });
        sent += 1;

        // alpha 驻留（秒数归账对象；2.6s → ≈2s）
        std::thread::sleep(DWELL_ALPHA);
        // 暂停 2s：暂停区间必须产生 0 秒数（aggregator ≤100ms 内观察到并冻结 since）。
        // motion-dpi §4.3.1：selftest 模拟暂停统一改用 set_paused（携带代际的合法入口，
        // 不直接修改 motion 快照/AtomicBool）。
        flags.set_paused(true);
        std::thread::sleep(PAUSE_FOR);
        // 恢复（epoch 再 +1，短暂停再恢复也换代际）
        flags.set_paused(false);
        // 切换 #2：alpha.exe → beta.exe
        send_fg("beta.exe", &mut sent);
        // 双来源测试（§4.3）：第二个来源（不同正 ID、同 DeviceKey）按 B——同码跨来源独立
        // 计数并入同一个型号桶（input_daily (keyboard,48) 因而为 2）；无修饰键按住 → 无组合
        send_kb(INJECT_SOURCE_ALT, 0x30, true, &mut sent);
        send_kb(INJECT_SOURCE_ALT, 0x30, false, &mut sent);
        // beta 驻留（1.6s → ≈1s）
        std::thread::sleep(DWELL_BETA);
    }

    // 5) 优雅关停：发布 producers_drained（注入模式无生产者线程）→ aggregator 排空 →
    //    终账（暂停中不归账）→ 最后一批 flush → 退出
    flags.shutdown.store(true, Ordering::Release);
    flags.producers_drained.store(true, Ordering::Release);
    drop(tx);
    if agg.join().is_err() {
        eprintln!("cl-recoder-collector: aggregator 线程 panic，注入结果不完整");
        return 1;
    }

    // 6) 期望摘要（stdout，供关卡脚本/人工对照；数值依据见模块文档时间编排）
    let d = day::today_local();
    println!("EXPECT day={d}");
    println!(
        "EXPECT devices: (kind=keyboard,name=Selftest 键盘,vid=4369,pid=1) (kind=mouse,name=Selftest 鼠标,vid=4369,pid=2) (kind=gamepad,name=Selftest 手柄,vid=4369,pid=3)"
    );
    println!(
        "EXPECT input_daily(code=十进制): (keyboard,30)=2 (keyboard,48)=2 (keyboard,46)=2 (keyboard,28)=1 (keyboard,29)=1 (keyboard,42)=1 (mouse,1)=2"
    );
    println!("EXPECT combo_daily: (mods=1,code=46)=1 (mods=2,code=30)=1");
    println!(
        "EXPECT app_daily: (exe=alpha.exe,keys=8,clicks=2,secs≈2) (exe=beta.exe,keys=1,clicks=0,secs≈1)"
    );
    println!(
        "EXPECT 暂停区间({PAUSE_FOR:?})秒数贡献为 0：app_daily.foreground_secs 合计应 ≤ 5（活动驻留 2.6+1.6s、按秒取整 ≈3；暂停的 2s 恒不入账）"
    );
    println!(
        "EXPECT mouse_motion_sources: (source_key=selftest://inject-motion-mouse,physical=1,probe_status=unsupported,connected=1)"
    );
    println!(
        "EXPECT mouse_motion_daily(day={d}): (dpi=800,manual)=800 (dpi=1600,manual)=1600 (dpi=0,unknown)=400 \
         → rawCounts=2800 meters=0.0508 unconfiguredCounts=400 coverage≈0.857143（§4.4 精确示例）"
    );
    let gp = gp_day();
    println!(
        "EXPECT gamepad_motion_daily(day={gp}): (left,active_us=40000,travel_r=2.0) (right,active_us=40000,travel_r=1.0)"
    );
    println!(
        "EXPECT gamepad_heat_daily(day={gp}): (left,bin=324,dwell_us=40000) (right,bin=12,dwell_us=40000)"
    );
    println!("[inject] 共注入 {sent} 条 AggEvent，已全链路写库: {}", db.display());
    println!("SELFTEST-INJECT OK");
    0
}

/// `--selftest N` 入口：真实采集模式运行 N 秒，事件打印到 stdout 并写库后退出。
/// 返回进程退出码（0=成功）。运动运行时为 offline 形态（§4.3：无 DB/HID worker——
/// fixture 禁真实 HID 探测；DPI 恒 unknown 桶），raw_input 经 [`RawInputRunner`]
/// 停止并排出尾桶。
pub fn run_live(seconds: u64, db: &Path) -> i32 {
    let writer = match Writer::open(db) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            eprintln!("cl-recoder-collector: 打开库 {} 失败: {e}", db.display());
            return 1;
        }
    };
    let flags = Arc::new(Flags::default());
    let status = Arc::new(RuntimeStatus::new(env!("CARGO_PKG_VERSION")));
    let fg = Arc::new(Mutex::new(FgState { exe: EXE_UNKNOWN.to_string(), since: Instant::now() }));

    // 双通道泵：采集线程 → ch1 → main（打印）→ ch2 → aggregator（全链路写库）
    let (tx1, rx1) = crossbeam_channel::unbounded::<AggEvent>();
    let (tx2, rx2) = crossbeam_channel::unbounded::<AggEvent>();
    let agg = engine_loop::spawn(rx2, writer, Arc::clone(&flags), Arc::clone(&fg), status);
    let motion = MotionRuntime::offline(Arc::clone(&flags));
    let raw = crate::raw_input::spawn(tx1.clone(), Arc::clone(&motion));
    let _pad = crate::gamepad::spawn(tx1.clone(), Arc::clone(&motion));
    let _apps = crate::apps::spawn(tx1.clone(), Arc::clone(&fg));
    drop(tx1); // 采集线程各自持有克隆；main 只作泵

    println!("# selftest {seconds}s db={}（行格式 kind|device|code|down；'# ' 开头为注释）", db.display());
    let mut printed = 0u64;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if Instant::now() >= deadline {
            break;
        }
        match rx1.recv_timeout(Duration::from_millis(200)) {
            Ok(ev) => pump_event(&mut printed, &tx2, ev),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            // 采集线程全部退出（异常情形）：提前收尾
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    // 到时：§4.3.1 生产者收尾——producer_stop → raw_input stop_and_join（窗口线程
    // 排出已捕获尾桶）→ 排空泵内残留 → drained → 优雅关停 aggregator（终账 + 最后一批 flush）
    flags.producer_stop.store(true, Ordering::Release);
    if raw.stop_and_join().is_err() {
        eprintln!("cl-recoder-collector: raw_input 生产者线程异常退出（已捕获 join 错误，进入收尾）");
    }
    while let Ok(ev) = rx1.try_recv() {
        pump_event(&mut printed, &tx2, ev);
    }
    flags.shutdown.store(true, Ordering::Release);
    flags.producers_drained.store(true, Ordering::Release);
    drop(tx2);
    let _ = agg.join();
    println!("# selftest 结束：共打印 {printed} 行，库文件 {}", db.display());
    0
}

/// 泵一条事件：先打印（stdout 行序与注入 aggregator 的顺序一致）再转发。
fn pump_event(printed: &mut u64, tx2: &crossbeam_channel::Sender<AggEvent>, ev: AggEvent) {
    let line = event_line(&ev);
    print_line(&line);
    *printed += 1;
    let _ = tx2.send(ev);
}

/// 事件 → stdout 行（纯函数，单测锚点）。
/// Input 事件 = `kind|device|code|down`（down: 1=按下 0=抬起；鼠标/手柄恒为按下边沿 1；
/// 来源 ID 不进入四段线格式，§4.3"正常 stdout 四段完全不改"）；
/// Foreground 与生命周期事件 = `# ` 开头注释行（不属于四段格式，不追加第五段）。
fn event_line(ev: &AggEvent) -> String {
    match ev {
        AggEvent::Input(RawEvent::Keyboard { device, sc, down, .. }) => {
            format!("keyboard|{}|{}|{}", device.name, sc, u8::from(*down))
        }
        AggEvent::Input(RawEvent::MouseClick { device, button }) => {
            format!("mouse|{}|{}|1", device.name, u16::from(*button))
        }
        AggEvent::Input(RawEvent::GamepadPress { device, button }) => {
            format!("gamepad|{}|{}|1", device.name, u16::from(*button))
        }
        AggEvent::Input(RawEvent::MouseMove { device, distance_inches }) => {
            format!("mousemove|{}|{:.4}|1", device.name, distance_inches)
        }
        AggEvent::Foreground { exe } => format!("# fg|{exe}"),
        // 生命周期控制事件（§4.3）：只打注释行；live 泵仍原样转发给 aggregator
        AggEvent::SourceRemoved { source } => format!("# source_removed|{}", source.0),
        AggEvent::KeyboardSourcesReset => "# kb_sources_reset".to_string(),
        // 运动事件（motion-dpi §4.3）：同样只打注释行——四段格式不含运动/元数据事件
        AggEvent::MouseSourceState(state) => format!(
            "# mouse_state|{}|{}|{}",
            state.descriptor.source_key,
            state.connection.0,
            if state.connected { "connected" } else { "disconnected" },
        ),
        AggEvent::MouseTravel(delta) => format!(
            "# mouse_travel|{}|{}|{:.0}",
            delta.descriptor.source_key, delta.day, delta.counts,
        ),
        // 手柄运动事件（motion-dpi §4.3，S5）：同样只打注释行——四段格式不含运动事件
        AggEvent::GamepadMotion(frame) => format!(
            "# gamepad_motion|{}|{}|{:.3},{:.3}|{:.3},{:.3}",
            frame.device.name,
            frame.connection.0,
            frame.left.x,
            frame.left.y,
            frame.right.x,
            frame.right.y,
        ),
        AggEvent::GamepadMotionDisconnected { connection } => {
            format!("# gamepad_motion_disconnected|{}", connection.0)
        }
    }
}

/// 打印一行并立即 flush（selftest 常被重定向到管道，逐行可见性是硬要求）。
fn print_line(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// 删除库文件与 WAL 旁路文件（`-wal`/`-shm`）。文件不存在视为成功；其他错误原样上抛。
fn remove_db_files(p: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = p.as_os_str().to_owned();
        name.push(suffix);
        match std::fs::remove_file(&name) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::codes::GamepadButton;

    /// 行格式锚点：kind|device|code|down（十进制 code；down 1/0）。
    #[test]
    fn event_line_formats_four_fields() {
        let kb = inject_keyboard();
        let mouse = inject_mouse();
        let pad = DeviceKey {
            kind: DeviceKind::Gamepad,
            vid: 0,
            pid: 0,
            name: "Xbox Controller".to_string(),
        };
        assert_eq!(
            event_line(&AggEvent::Input(RawEvent::Keyboard {
                source: INJECT_SOURCE,
                device: kb.clone(),
                sc: 0x1E,
                down: true,
            })),
            format!("keyboard|{}|30|1", kb.name)
        );
        assert_eq!(
            event_line(&AggEvent::Input(RawEvent::Keyboard {
                source: INJECT_SOURCE,
                device: kb,
                sc: 0xE11D,
                down: false,
            })),
            "keyboard|Selftest 键盘|57629|0"
        );
        assert_eq!(
            event_line(&AggEvent::Input(RawEvent::MouseClick { device: mouse, button: MouseButton::WheelUp })),
            "mouse|Selftest 鼠标|6|1"
        );
        assert_eq!(
            event_line(&AggEvent::Input(RawEvent::GamepadPress {
                device: pad,
                button: GamepadButton::DPadLeft
            })),
            "gamepad|Xbox Controller|16|1"
        );
        assert_eq!(
            event_line(&AggEvent::Foreground { exe: "explorer.exe".to_string() }),
            "# fg|explorer.exe"
        );
    }

    /// 注入设备身份：固定 vid/pid/name（devices UNIQUE 四元组，可重复运行不产生新行——
    /// 库先删后建，这里锚定值不漂移）。
    #[test]
    fn inject_devices_are_stable() {
        let kb = inject_keyboard();
        assert_eq!((kb.kind, kb.vid, kb.pid, kb.name.as_str()),
            (DeviceKind::Keyboard, 0x1111, 0x0001, "Selftest 键盘"));
        let mouse = inject_mouse();
        assert_eq!((mouse.kind, mouse.vid, mouse.pid, mouse.name.as_str()),
            (DeviceKind::Mouse, 0x1111, 0x0002, "Selftest 鼠标"));
    }

    /// remove_db_files：不存在的文件按成功处理；存在的文件被删除。
    #[test]
    fn remove_db_files_handles_missing_and_existing() {
        let base = std::env::temp_dir().join(format!("clrecoder-selftest-{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&base);
        // 全部不存在 → Ok
        remove_db_files(&base).expect("不存在的文件应视为成功");
        // 写入主文件 → 删除成功且旁路路径一并尝试
        std::fs::write(&base, b"x").unwrap();
        remove_db_files(&base).expect("存在的文件应被删除");
        assert!(!base.exists());
        // 只读目录场景不做断言（Windows 语义差异大），错误传播路径由调用方处理
    }

    /// §4.3：生命周期事件输出为 `# ` 开头注释行（不追加第五段）；
    /// live 泵对它们照常原样转发（转发逻辑在 run_live 主循环，见模块文档）。
    #[test]
    fn correctness_v2_lifecycle_events_print_as_comment_lines() {
        let removed = event_line(&AggEvent::SourceRemoved { source: InputSourceId(7) });
        assert_eq!(removed, "# source_removed|7");
        assert!(removed.starts_with("# "), "生命周期行必须以注释前缀开头");
        let reset = event_line(&AggEvent::KeyboardSourcesReset);
        assert_eq!(reset, "# kb_sources_reset");
        assert!(reset.starts_with("# "), "生命周期行必须以注释前缀开头");
    }

    /// motion-dpi §4.3：运动事件输出为 `# ` 开头注释行（不进四段格式）；
    /// mouse_travel 行携带 source_key/捕获日/整数 counts。
    #[test]
    fn motion_dpi_motion_events_print_as_comment_lines() {
        let desc = inject_motion_source();
        let state_line = event_line(&AggEvent::MouseSourceState(MouseSourceState {
            descriptor: desc.clone(),
            connection: MotionConnectionId(4),
            connected: true,
            stamp: MotionStamp { mono_us: 0, unix_us: 0 },
            probe_status: DpiProbeStatus::Available,
            auto_dpi: Some(800),
            auto_valid_until_unix_us: Some(0),
        }));
        assert_eq!(
            state_line,
            format!("# mouse_state|{}|4|connected", desc.source_key)
        );
        assert!(state_line.starts_with("# "), "运动事件必须以注释前缀开头");
        let travel_line = event_line(&AggEvent::MouseTravel(MouseTravelDelta {
            descriptor: desc.clone(),
            connection: MotionConnectionId(4),
            day: "2026-06-15".to_string(),
            counts: 1234.0,
            dpi: EffectiveDpi { value: Some(800), origin: DpiOrigin::Auto },
            control: MotionControlSnapshot { epoch: 0, paused: false },
        }));
        assert_eq!(travel_line, format!("# mouse_travel|{}|2026-06-15|1234", desc.source_key));
        assert!(travel_line.starts_with("# "), "运动事件必须以注释前缀开头");
    }

    /// motion-dpi §4.3（S5）：手柄运动事件输出为 `# ` 开头注释行（不进四段格式）——
    /// gamepad_motion 行携带型号名/连接代际/左右摇杆点，断连行携带连接代际。
    #[test]
    fn motion_dpi_gamepad_motion_events_print_as_comment_lines() {
        let pad = inject_gamepad();
        let frame_line = event_line(&AggEvent::GamepadMotion(GamepadMotionFrame {
            device: pad.clone(),
            connection: MotionConnectionId(9),
            stamp: MotionStamp { mono_us: 20_000, unix_us: GP_BASE_UNIX_US + 20_000 },
            left: StickPoint { x: 1.0, y: 0.0 },
            right: StickPoint { x: 0.0, y: -1.0 },
            control: MotionControlSnapshot { epoch: 0, paused: false },
        }));
        assert_eq!(
            frame_line,
            format!("# gamepad_motion|{}|9|1.000,0.000|0.000,-1.000", pad.name)
        );
        assert!(frame_line.starts_with("# "), "手柄运动事件必须以注释前缀开头");
        let disconnected_line = event_line(&AggEvent::GamepadMotionDisconnected {
            connection: MotionConnectionId(9),
        });
        assert_eq!(disconnected_line, "# gamepad_motion_disconnected|9");
        assert!(disconnected_line.starts_with("# "), "手柄运动事件必须以注释前缀开头");
    }

    /// §4.3：双来源（不同正 ID）键盘事件的四段输出完全一致——来源 ID 不进入线格式，
    /// 注入键盘默认固定 InputSourceId(1)，双来源测试用不同正 ID。
    #[test]
    fn correctness_v2_dual_source_keyboard_lines_keep_four_field_format() {
        let kb = inject_keyboard();
        let s1 = event_line(&AggEvent::Input(RawEvent::Keyboard {
            source: INJECT_SOURCE,
            device: kb.clone(),
            sc: 0x30,
            down: true,
        }));
        let s2 = event_line(&AggEvent::Input(RawEvent::Keyboard {
            source: INJECT_SOURCE_ALT,
            device: kb,
            sc: 0x30,
            down: true,
        }));
        assert_eq!(s1, s2, "来源 ID 不得改变四段线格式");
        assert_eq!(s1, "keyboard|Selftest 键盘|48|1");
        // 注入来源常量契约：默认 1，双来源用不同正 ID（0 保留给 Engine 兼容入口）
        assert_eq!(INJECT_SOURCE, InputSourceId(1));
        assert_eq!(INJECT_SOURCE_ALT, InputSourceId(2));
        assert_ne!(INJECT_SOURCE, INJECT_SOURCE_ALT);
    }
}
