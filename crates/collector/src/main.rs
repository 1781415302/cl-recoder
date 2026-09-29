//! cl-recoder-collector —— 提权采集进程（High IL，计划任务 ONLOGON 自启，PLAN §2.1/§5.1）。
//!
//! 进程形态与启动流程（§5.1）：
//! 1. 全局单实例互斥体（`Local\clrecoder-collector-singleton`，第二实例立即退出）；
//! 2. 打开/迁移统计库 `%LOCALAPPDATA%\ClRecoder\stats.db`（WAL；打开失败 → 日志 + 退出码非 0，§5.3）；
//! 3. 起 raw_input / gamepad / apps 三采集线程 + aggregator（engine_loop）+ pipe 服务端
//!    （`\\.\pipe\clrecoder-control`，status / set_paused / shutdown 经共享 [`Flags`] 生效）；
//! 4. 常驻等 `shutdown`（pipe 命令置位）→ join aggregator（排空 + 终账 + 最后一批 flush）
//!    → join pipe 服务 → 退出。
//!
//! 诊断模式（见 [`selftest`]，均不建互斥体、不起 pipe）：
//! - `--selftest N`：真实采集 N 秒，事件以 `kind|device|code|down` 打印到 stdout 并写库；
//! - `--selftest-inject`：合成事件注入 aggregator→store 全链路（不启动采集线程，先删旧库）。
//!
//! 模块边界（PLAN §2.5 禁止耦合清单）：collector 不做键名翻译、不知道 WhatPulse 存在、
//! 无 UI、无网络。

mod apps;
mod device;
mod engine_loop;
mod gamepad;
mod ipc_server;
mod raw_input;
mod selftest;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clrecoder_core::event::AggEvent;
use clrecoder_store::writer::Writer;
use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;

use crate::apps::{EXE_UNKNOWN, FgState};
use crate::ipc_server::{Flags, RuntimeStatus};

/// 单实例互斥体名（§5.1）。`Local\` 前缀 = 每交互会话一份——采集器按会话采集交互输入，
/// 快速用户切换下各会话实例互不干扰（pipe 名为全局名，多会话并存属未定义部署形态）。
const SINGLE_INSTANCE_MUTEX: &str = "Local\\clrecoder-collector-singleton";

/// 主循环检查 shutdown 旗标的间隔（≤200ms 延迟收尾；实际等待由线程各自承担）。
const SHUTDOWN_POLL: Duration = Duration::from_millis(200);

/// `--selftest N` 的 N 取值域（秒）：0 无意义，上限防呆（一天）。
const SELFTEST_MIN_SECS: u64 = 1;
const SELFTEST_MAX_SECS: u64 = 86_400;

/// 运行模式（参数解析产物）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 常驻采集（无自检参数）
    Run,
    /// `--selftest N`：真实采集 N 秒
    Selftest(u64),
    /// `--selftest-inject`：合成事件注入全链路
    Inject,
    /// `--version`
    Version,
    /// `--help`
    Help,
}

fn main() {
    // 显式退出码贯穿：DB 打开失败非 0（§5.3）、参数错误 2、单实例第二例 0。
    std::process::exit(real_main());
}

/// 生产模式隐藏控制台：采集器是控制台子系统程序，计划任务 ONLOGON 拉起时会在桌面
/// 弹出控制台窗（§2.1 托盘常驻形态不可接受）；selftest 诊断模式保留控制台看输出。
fn hide_console_window() {
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    unsafe {
        let hwnd = GetConsoleWindow();
        if !hwnd.is_invalid() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn real_main() -> i32 {
    init_logger();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, db_override) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("cl-recoder-collector: {msg}");
            print_usage();
            return 2;
        }
    };
    match mode {
        Mode::Version => {
            println!("cl-recoder-collector {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Mode::Help => {
            print_usage();
            0
        }
        // 自检模式 --db 缺省 ./stats.db（任务书 §8-S9）
        Mode::Selftest(n) => selftest::run_live(n, &db_override.unwrap_or_else(default_selftest_db)),
        Mode::Inject => selftest::run_inject(&db_override.unwrap_or_else(default_selftest_db)),
        Mode::Run => {
            hide_console_window();
            run_service(db_override)
        }
    }
}

/// 解析命令行。`--db` 在所有模式下可用（自检/调试用途；生产缺省见 [`default_db_path`]）。
fn parse_args(args: &[String]) -> Result<(Mode, Option<PathBuf>), String> {
    let mut mode = Mode::Run;
    let mut db: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" => mode = Mode::Version,
            "--help" | "-h" => mode = Mode::Help,
            "--selftest" => {
                i += 1;
                let Some(raw) = args.get(i) else {
                    return Err("--selftest 需要秒数 N（如 --selftest 10）".to_string());
                };
                let n: u64 = raw
                    .parse()
                    .map_err(|_| format!("--selftest N 不是合法秒数: {raw}"))?;
                if !(SELFTEST_MIN_SECS..=SELFTEST_MAX_SECS).contains(&n) {
                    return Err(format!(
                        "--selftest N 必须在 {SELFTEST_MIN_SECS}..={SELFTEST_MAX_SECS} 秒，实际 {n}"
                    ));
                }
                mode = Mode::Selftest(n);
            }
            "--selftest-inject" => mode = Mode::Inject,
            "--db" => {
                i += 1;
                let Some(p) = args.get(i) else {
                    return Err("--db 需要路径参数（如 --db D:\\tmp\\t.db）".to_string());
                };
                if p.trim().is_empty() {
                    return Err("--db 路径不能为空".to_string());
                }
                db = Some(PathBuf::from(p));
            }
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    Ok((mode, db))
}

/// 常驻采集模式（§5.1 启动流程）。返回进程退出码。
fn run_service(db_override: Option<PathBuf>) -> i32 {
    // 1) 单实例互斥体：第二实例立即退出（§5.1）
    let _instance = match acquire_single_instance() {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            log::info!("已有 collector 实例在运行，本实例退出");
            return 0;
        }
        Err(e) => {
            log::error!("创建单实例互斥体失败: {e}");
            eprintln!("cl-recoder-collector: 创建单实例互斥体失败: {e}");
            return 1;
        }
    };

    // 2) 打开/迁移 DB：失败 → 日志 + 退出码非 0（§5.3"DB 损坏"行）
    let db_path = db_override.unwrap_or_else(default_db_path);
    let writer = match Writer::open(&db_path) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            log::error!("打开统计库 {} 失败: {e}", db_path.display());
            eprintln!("cl-recoder-collector: 打开统计库 {} 失败: {e}", db_path.display());
            return 1;
        }
    };

    // 3) 共享状态：Flags（ipc_server 写 / aggregator+main 读）、RuntimeStatus（aggregator 记 /
    //    ipc_server 读）、FgState（apps 线程写 / aggregator 读）
    let flags = Arc::new(Flags::default());
    let status = Arc::new(RuntimeStatus::new(env!("CARGO_PKG_VERSION")));
    let fg = Arc::new(Mutex::new(FgState { exe: EXE_UNKNOWN.to_string(), since: Instant::now() }));

    // 4) 事件通道 + 线程组装（§5.1 顺序：aggregator 先于采集线程就绪，事件不空跑）
    let (tx, rx) = crossbeam_channel::unbounded::<AggEvent>();
    let aggregator = engine_loop::spawn(rx, Arc::clone(&writer), Arc::clone(&flags), Arc::clone(&fg), Arc::clone(&status));
    let _raw_input = raw_input::spawn(tx.clone());
    let _gamepad = gamepad::spawn(tx.clone());
    let _apps = apps::spawn(tx, Arc::clone(&fg));
    let ipc = ipc_server::spawn(Arc::clone(&flags), Arc::clone(&status));
    log::info!("collector 就绪（版本 {}，库 {}）", env!("CARGO_PKG_VERSION"), db_path.display());

    // 5) 常驻等待 shutdown（pipe `shutdown` 命令置位，§4.4）
    while !flags.shutdown.load(Ordering::Acquire) {
        std::thread::sleep(SHUTDOWN_POLL);
    }

    // 6) 收尾：aggregator 排空余事件 → 前台秒数终账 → 最后一批 flush → 退出；
    //    pipe 服务线程随 shutdown 旗标退出。两者都 join（丢尾批 ≤0.5s 属 §5.3 已接受损耗）。
    let _ = aggregator.join();
    let _ = ipc.join();
    log::info!("collector 已优雅退出");
    0
}

/// 单实例互斥体守卫：句柄有意持到进程退出（不实现 Drop）——进程结束后系统回收句柄、
/// 互斥体随之消失，下一实例自然可建（§5.1"第二实例立即退出"的判据即 ERROR_ALREADY_EXISTS）。
/// 字段无需读取，持存活即是全部语义。
struct SingleInstanceGuard(#[allow(dead_code)] HANDLE);

/// 创建单实例互斥体。`Ok(None)` = 已有实例（§5.1 第二实例立即退出）。
fn acquire_single_instance() -> Result<Option<SingleInstanceGuard>, String> {
    let name = HSTRING::from(SINGLE_INSTANCE_MUTEX);
    // SAFETY: 仅调用 CreateMutexW；name 为本函数内存活的 HSTRING，调用期间有效。
    let handle = unsafe { CreateMutexW(None, false, &name) }.map_err(|e| e.to_string())?;
    // CreateMutexW 对"已存在"仍返回有效句柄，以 ERROR_ALREADY_EXISTS 区分（首个实例的
    // GetLastError 值不受 windows crate Ok 路径影响）。
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: handle 为本调用刚创建的有效句柄，立即关闭放弃。
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(SingleInstanceGuard(handle)))
}

/// 生产统计库路径（PLAN §6）：`%LOCALAPPDATA%\ClRecoder\stats.db`。
/// 定位失败（异常用户配置）退化为当前目录 `stats.db` 并记录警告，绝不 panic。
fn default_db_path() -> PathBuf {
    match dirs::data_local_dir() {
        Some(base) => base.join("ClRecoder").join("stats.db"),
        None => {
            log::warn!("无法定位 %LOCALAPPDATA%，统计库退化到当前目录 stats.db");
            PathBuf::from("stats.db")
        }
    }
}

/// 自检模式缺省库路径（任务书 §8-S9：缺省 stats.db，相对当前目录）。
fn default_selftest_db() -> PathBuf {
    PathBuf::from("stats.db")
}

/// 日志初始化（§9.2：release 默认静默——仅 error 到 stderr，第三方库（gilrs 等）的
/// warn 噪声一并静默；`RUST_LOG=warn/info` 可按需调高）。
fn init_logger() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("error"))
        .try_init();
}

/// 用法说明（stderr）。
fn print_usage() {
    eprintln!(
        "用法: cl-recoder-collector [选项]\n\
         \x20（无参数）         常驻采集模式（单实例；库 %LOCALAPPDATA%\\ClRecoder\\stats.db）\n\
         \x20 --version          打印版本并退出\n\
         \x20 --help             本说明\n\
         \x20 --selftest N       真实采集自检 N 秒：事件以 kind|device|code|down 打印到 stdout 并写库\n\
         \x20 --selftest-inject  合成事件注入 aggregator→store 全链路（不启动采集线程；先删旧库）\n\
         \x20 --db <path>        统计库路径覆盖（自检/调试用；自检模式缺省 ./stats.db）"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_defaults_to_run_mode() {
        let (mode, db) = parse_args(&[]).unwrap();
        assert_eq!(mode, Mode::Run);
        assert_eq!(db, None);
    }

    #[test]
    fn parse_selftest_requires_valid_seconds() {
        let (mode, db) = parse_args(&a(&["--selftest", "10"])).unwrap();
        assert_eq!(mode, Mode::Selftest(10));
        assert_eq!(db, None);
        // 0 与越界拒绝
        assert!(parse_args(&a(&["--selftest", "0"])).is_err());
        assert!(parse_args(&a(&["--selftest", "86401"])).is_err());
        assert!(parse_args(&a(&["--selftest", "abc"])).is_err());
        // 缺参数
        assert!(parse_args(&a(&["--selftest"])).is_err());
        // 上界合法
        assert_eq!(parse_args(&a(&["--selftest", "86400"])).unwrap().0, Mode::Selftest(86_400));
    }

    #[test]
    fn parse_db_takes_next_argument_and_accepts_anywhere() {
        let (mode, db) = parse_args(&a(&["--db", r"D:\tmp\t.db", "--selftest-inject"])).unwrap();
        assert_eq!(mode, Mode::Inject);
        assert_eq!(db, Some(PathBuf::from(r"D:\tmp\t.db")));
        // 自检模式不传 --db → None（main 落到缺省 stats.db）
        let (_, db) = parse_args(&a(&["--selftest", "5"])).unwrap();
        assert_eq!(db, None);
        // 缺路径 / 空路径拒绝
        assert!(parse_args(&a(&["--db"])).is_err());
        assert!(parse_args(&a(&["--db", "  "])).is_err());
    }

    #[test]
    fn parse_version_help_and_unknown() {
        assert_eq!(parse_args(&a(&["--version"])).unwrap().0, Mode::Version);
        assert_eq!(parse_args(&a(&["-V"])).unwrap().0, Mode::Version);
        assert_eq!(parse_args(&a(&["--help"])).unwrap().0, Mode::Help);
        assert!(parse_args(&a(&["--bogus"])).is_err());
        // 重复的 --db：后者覆盖（幂等覆盖语义，非错误）
        let (_, db) = parse_args(&a(&["--db", "a.db", "--db", "b.db"])).unwrap();
        assert_eq!(db, Some(PathBuf::from("b.db")));
    }
}
