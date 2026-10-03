//! selftest 模式（PLAN §8-S9 验收点）。
//!
//! 两种自检，均**不创建单实例互斥体、不启动 pipe 服务**（诊断模式，不与常驻实例争抢
//! `\\.\pipe\clrecoder-control`，也不应被正在运行的采集器挡住冒烟关卡）：
//!
//! - [`run_inject`]（`--selftest-inject --db <临时库>`）：**不启动任何采集线程**，把一组
//!   合成 [`AggEvent`] 直接灌入 aggregator→store 全链路（先删除已存在的库文件，含 WAL
//!   旁路），事件脚本覆盖：键盘 a/b/c/Enter 各 1 次（来源 1）、Ctrl+C 1 次、Shift+A 1 次、
//!   鼠标左键 2 次、双来源测试（来源 2 按同型号 B 1 次，并入同型号桶，§4.3）、≥2 次
//!   Foreground 切换、暂停 2s 再恢复——供外部脚本对 `input_daily`/`combo_daily`/
//!   `app_daily` 做 sqlite 断言（行正确、暂停区间秒数为 0、工作集 <100MB）。
//!   注入端同时以与 apps 线程相同的语义（先落状态再发事件）维护 [`FgState`]，使 exe 归属
//!   与真实链路一致。结束时把"期望的库内容"打印到 stdout，供关卡脚本/人工对照。
//! - [`run_live`]（`--selftest N [--db path]`）：**真实采集模式**——完整起 raw_input/
//!   gamepad/apps 三采集线程与 aggregator，main 线程作泵：把收到的事件以
//!   `kind|device|code|down` 行打印到 stdout（`# ` 开头为注释/前台切换/生命周期行）并
//!   **原样转发**给 aggregator 写库（生命周期事件只打印不丢弃，§4.3）；N 秒后置 shutdown
//!   优雅收尾（终账 + 最后一批 flush）退出。`--db` 缺省 `./stats.db`。
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
use clrecoder_store::writer::Writer;

use crate::apps::{EXE_UNKNOWN, FgState};
use crate::engine_loop;
use crate::ipc_server::{Flags, RuntimeStatus};

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
                  鼠标左键×2 → 驻留 2.6s → 暂停 2.0s → 恢复 → Foreground beta.exe → 双来源B(来源2)×1 → \
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

        // alpha 驻留（秒数归账对象；2.6s → ≈2s）
        std::thread::sleep(DWELL_ALPHA);
        // 暂停 2s：暂停区间必须产生 0 秒数（aggregator ≤100ms 内观察到并冻结 since）
        flags.paused.store(true, Ordering::Release);
        std::thread::sleep(PAUSE_FOR);
        // 恢复
        flags.paused.store(false, Ordering::Release);
        // 切换 #2：alpha.exe → beta.exe
        send_fg("beta.exe", &mut sent);
        // 双来源测试（§4.3）：第二个来源（不同正 ID、同 DeviceKey）按 B——同码跨来源独立
        // 计数并入同一个型号桶（input_daily (keyboard,48) 因而为 2）；无修饰键按住 → 无组合
        send_kb(INJECT_SOURCE_ALT, 0x30, true, &mut sent);
        send_kb(INJECT_SOURCE_ALT, 0x30, false, &mut sent);
        // beta 驻留（1.6s → ≈1s）
        std::thread::sleep(DWELL_BETA);
    }

    // 5) 优雅关停：aggregator 排空 → 终账（暂停中不归账）→ 最后一批 flush → 退出
    flags.shutdown.store(true, Ordering::Release);
    drop(tx);
    if agg.join().is_err() {
        eprintln!("cl-recoder-collector: aggregator 线程 panic，注入结果不完整");
        return 1;
    }

    // 6) 期望摘要（stdout，供关卡脚本/人工对照；数值依据见模块文档时间编排）
    let d = day::today_local();
    println!("EXPECT day={d}");
    println!(
        "EXPECT devices: (kind=keyboard,name=Selftest 键盘,vid=4369,pid=1) (kind=mouse,name=Selftest 鼠标,vid=4369,pid=2)"
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
    println!("[inject] 共注入 {sent} 条 AggEvent，已全链路写库: {}", db.display());
    println!("SELFTEST-INJECT OK");
    0
}

/// `--selftest N` 入口：真实采集模式运行 N 秒，事件打印到 stdout 并写库后退出。
/// 返回进程退出码（0=成功）。
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
    let _raw = crate::raw_input::spawn(tx1.clone());
    let _pad = crate::gamepad::spawn(tx1.clone());
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
            Ok(ev) => {
                let line = event_line(&ev);
                // 先打印再转发：stdout 行序与注入 aggregator 的顺序一致
                print_line(&line);
                printed += 1;
                let _ = tx2.send(ev);
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            // 采集线程全部退出（异常情形）：提前收尾
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    // 到时：排空泵内残留 → 优雅关停 aggregator（终账 + 最后一批 flush）
    while let Ok(ev) = rx1.try_recv() {
        let line = event_line(&ev);
        print_line(&line);
        printed += 1;
        let _ = tx2.send(ev);
    }
    flags.shutdown.store(true, Ordering::Release);
    drop(tx2);
    let _ = agg.join();
    println!("# selftest 结束：共打印 {printed} 行，库文件 {}", db.display());
    0
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
