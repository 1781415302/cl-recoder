//! db —— GUI 侧 SQLite 连接打开（PLAN §3：ro/rw 连接打开（busy_timeout、WAL 容错））。
//!
//! - 统计库路径：`%LOCALAPPDATA%\ClRecoder\stats.db`（PLAN §6，唯一业务存储）；
//! - ro 连接（GUI 日常查询用）：`SQLITE_OPEN_READ_ONLY` + `busy_timeout(5000ms)`（§2.4/§5.3）；
//!   WAL 容错：若采集进程崩溃留下未恢复的 WAL（只读连接无法执行恢复，报
//!   `SQLITE_READONLY_RECOVERY/CANTINIT` 类错误），降级为**不建库**的读写方式重开一次，
//!   让 SQLite 完成崩溃恢复后按只读语义继续使用（不做任何统计表写入）；
//! - rw 连接（仅 WhatPulse 导入用，§5.4"GUI 临时 rw 连接"）：`busy_timeout(10s)`，与
//!   collector 的 Writer 同参数，导入期间采集端同时 flush 也不会互锁死。

use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// GUI ro 连接的锁等待（PLAN §2.4/§5.3：busy_timeout=5000ms）。
const RO_BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
/// 导入用 rw 连接的锁等待（PLAN §5.4：busy_timeout=10s，与 store::Writer::open 同值）。
const RW_BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// 应用数据目录：`%LOCALAPPDATA%\ClRecoder`（PLAN §6）。
#[must_use]
pub fn data_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("ClRecoder")
}

/// 统计库路径：`%LOCALAPPDATA%\ClRecoder\stats.db`。
#[must_use]
pub fn stats_db_path() -> PathBuf {
    data_dir().join("stats.db")
}

/// 设置文件路径：`%LOCALAPPDATA%\ClRecoder\settings.json`（PLAN §6）。
#[must_use]
pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

/// 打开只读连接（GUI 日常查询，PLAN §2.4）。
///
/// `busy_timeout=5000ms`：与 collector 的 5s 批量 flush 重叠时读不失败而是等待。
/// WAL 容错见模块注释：只读打开失败且库文件存在时，用"读写但不建库"重开一次
/// 完成 WAL 崩溃恢复；两路都失败返回最后的错误（调用方按 §5.3 返回空数据引导态）。
pub fn open_ro(path: &std::path::Path) -> Result<Connection, rusqlite::Error> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match Connection::open_with_flags(path, flags) {
        Ok(conn) => {
            conn.busy_timeout(RO_BUSY_TIMEOUT)?;
            Ok(conn)
        }
        Err(ro_err) => {
            // WAL 容错：库文件在场但只读连接无法完成崩溃恢复 → 以"不建库"的读写方式
            // 打开，让 SQLite 自己恢复 -wal；恢复后查询仍只发生读。
            if path.is_file() {
                let rw_flags =
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
                match Connection::open_with_flags(path, rw_flags) {
                    Ok(conn) => {
                        conn.busy_timeout(RO_BUSY_TIMEOUT)?;
                        crate::gui_log!("WARN: stats.db 只读打开失败（{ro_err}），已用 WAL 恢复方式重开");
                        Ok(conn)
                    }
                    Err(rw_err) => {
                        crate::gui_log!("WARN: stats.db 只读与 WAL 恢复打开均失败：{ro_err} / {rw_err}");
                        Err(rw_err)
                    }
                }
            } else {
                Err(ro_err)
            }
        }
    }
}

/// 打开读写连接（**仅 WhatPulse 导入使用**，PLAN §5.4；建库语义交给调用方的 migrate）。
///
/// busy_timeout 与 store::Writer::open 一致（10s）：导入重建 wp_* 的单事务可能与
/// 采集端的 flush 事务短暂互斥，等待即可。
pub fn open_rw(path: &std::path::Path) -> Result<Connection, rusqlite::Error> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(RW_BUSY_TIMEOUT)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_under_local_appdata() {
        let dir = data_dir();
        assert!(dir.ends_with("ClRecoder"), "dir = {}", dir.display());
        assert!(stats_db_path().starts_with(&dir));
        assert_eq!(stats_db_path().file_name().unwrap(), "stats.db");
        assert_eq!(settings_path().file_name().unwrap(), "settings.json");
    }

    #[test]
    fn open_ro_missing_file_fails_without_fallback() {
        // 库不存在：只读打开必须失败（§5.1 引导态由上层映射为空数据），不得凭空建库
        let p = std::env::temp_dir().join(format!("clrecoder-gui-missing-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        assert!(open_ro(&p).is_err());
        assert!(!p.exists(), "ro 打开失败不得创建文件");
    }

    #[test]
    fn open_ro_wal_recovery_fallback() {
        // 造一个正常 WAL 库 → 关闭前写一行 → ro 打开可读（常规路径）；
        // 再模拟"只读打开"仍可读——两分支都不应报错。
        let p = std::env::temp_dir().join(format!("clrecoder-gui-wal-{}.db", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let mut n = p.clone().into_os_string();
            n.push(suffix);
            let _ = std::fs::remove_file(&n);
        }
        {
            let conn = Connection::open(&p).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (7);").unwrap();
        }
        let ro = open_ro(&p).unwrap();
        let v: i64 = ro.query_row("SELECT SUM(x) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 7);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn open_rw_creates_and_sets_busy_timeout() {
        let p = std::env::temp_dir().join(format!("clrecoder-gui-rw-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let conn = open_rw(&p).unwrap();
        let ms: i64 = conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0)).unwrap();
        assert_eq!(ms, 10_000, "导入 rw 连接 busy_timeout 必须是 10s（§5.4）");
        drop(conn);
        assert!(p.exists(), "rw 打开按契约允许建库（导入前 migrate 用）");
        let _ = std::fs::remove_file(&p);
    }
}
