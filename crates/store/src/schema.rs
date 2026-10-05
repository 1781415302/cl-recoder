//! schema v1 完整 DDL 与幂等迁移（PLAN §4.5 逐字对齐）+ v2 增量（昵称/鼠标移动距离）
//! + v3 增量（motion-dpi §6.1：鼠标运动来源/逐日 DPI 桶、手柄摇杆运动/热度四张新表）。
//!
//! [`migrate`]：建 devices/input_daily/combo_daily/app_daily 与 wp_* 七张导入镜像表 +
//! 索引 idx_input_daily_day；连接级 PRAGMA journal_mode=WAL、synchronous=NORMAL、foreign_keys=ON；
//! `schema_migrations` 表记录版本，重复调用为 no-op（建表幂等）。
//!
//! DDL 在单事务内执行：中途失败整批回滚，不会出现"半套表 + 半条版本记录"的中间态。
//! 每个 vN 增量同为单事务追加：旧表/旧行一律不删除（§6.1"在既有迁移链追加一事务"）。

use clrecoder_core::day;
use rusqlite::Connection;

use crate::{Result, StoreError};

/// 当前 schema 版本（v3 = motion-dpi 四张运动表，§6.1）。
pub const SCHEMA_VERSION: i64 = 3;

/// schema v1 完整 DDL——PLAN §4.5 逐字拷贝（仅去掉块首的 PRAGMA 行：PRAGMA 由 [`migrate`]
/// 以连接级调用执行，不能放进事务内批处理——SQLite 不允许事务内切换 WAL）。
pub(crate) const SCHEMA_V1_SQL: &str = "
CREATE TABLE devices(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL CHECK(kind IN ('keyboard','mouse','gamepad')),
  vid INTEGER NOT NULL DEFAULT 0, pid INTEGER NOT NULL DEFAULT 0,
  name TEXT NOT NULL,
  first_seen TEXT NOT NULL, last_seen TEXT NOT NULL,
  UNIQUE(kind, vid, pid, name)
);
CREATE TABLE input_daily(
  device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  day TEXT NOT NULL, code INTEGER NOT NULL, count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(device_id, day, code)
);
CREATE INDEX idx_input_daily_day ON input_daily(day);
CREATE TABLE combo_daily(
  day TEXT NOT NULL, mods INTEGER NOT NULL, code INTEGER NOT NULL, count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(day, mods, code)
);
CREATE TABLE app_daily(
  day TEXT NOT NULL, exe TEXT NOT NULL,
  foreground_secs INTEGER NOT NULL DEFAULT 0,
  key_count INTEGER NOT NULL DEFAULT 0, click_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(day, exe)
);
-- 以下为 WhatPulse 导入镜像（§4.8；每次导入整体重建）
CREATE TABLE wp_import_meta(id INTEGER PRIMARY KEY CHECK(id=1), imported_at TEXT NOT NULL,
  source_path TEXT NOT NULL, source_size INTEGER, date_min TEXT, date_max TEXT, note TEXT);
CREATE TABLE wp_key_daily(day TEXT NOT NULL, qt_key INTEGER NOT NULL, label TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, qt_key));
CREATE TABLE wp_combo_daily(day TEXT NOT NULL, combo TEXT NOT NULL, label TEXT NOT NULL DEFAULT '',
  count INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, combo));
CREATE TABLE wp_app_daily(day TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL DEFAULT '',
  seconds INTEGER NOT NULL DEFAULT 0, keys INTEGER NOT NULL DEFAULT 0, clicks INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(day, path));
CREATE TABLE wp_mouse_daily(day TEXT NOT NULL, clicks INTEGER NOT NULL DEFAULT 0,
  distance_inches REAL NOT NULL DEFAULT 0, PRIMARY KEY(day));
CREATE TABLE wp_mouse_buttons_daily(day TEXT NOT NULL, button_code INTEGER NOT NULL, label TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, button_code));
CREATE TABLE wp_mouse_scroll_daily(day TEXT NOT NULL, direction_code INTEGER NOT NULL, label TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, direction_code));
";

/// schema v2 增量：设备自定义昵称 + 本软件鼠标移动距离（按设备×天）。
pub(crate) const SCHEMA_V2_SQL: &str = "
ALTER TABLE devices ADD COLUMN nickname TEXT;
CREATE TABLE mouse_move_daily(
  device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  day TEXT NOT NULL,
  distance_inches REAL NOT NULL DEFAULT 0,
  PRIMARY KEY(device_id, day)
);
CREATE INDEX idx_mouse_move_daily_day ON mouse_move_daily(day);
";

/// schema v3 增量（motion-dpi §6.1 逐字拷贝——字段约束是持久合同）：
/// 鼠标运动来源表（source_key=本机设备路径，仅入库匹配、不外露）、按 来源×日×DPI 桶的
/// 鼠标运动量表、手柄摇杆逐日运动量表与 25×25 热度表。
/// 旧表/旧行一律不删除；生产新路径不再写 v2 的 `mouse_move_daily`（完整保留供 legacy 读数）。
pub(crate) const SCHEMA_V3_SQL: &str = "
CREATE TABLE mouse_motion_sources(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_key TEXT NOT NULL UNIQUE,
  device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  physical INTEGER NOT NULL CHECK(physical IN(0,1)),
  manual_dpi INTEGER CHECK(manual_dpi BETWEEN 1 AND 100000),
  connected INTEGER NOT NULL DEFAULT 0 CHECK(connected IN(0,1)),
  probe_status TEXT NOT NULL DEFAULT 'pending'
    CHECK(probe_status IN('pending','available','unsupported','ambiguous','unavailable','disconnected')),
  auto_dpi INTEGER CHECK(auto_dpi BETWEEN 1 AND 57343),
  auto_valid_until_unix_us INTEGER,
  first_seen TEXT NOT NULL, last_seen TEXT NOT NULL
);
CREATE TABLE mouse_motion_daily(
  source_id INTEGER NOT NULL REFERENCES mouse_motion_sources(id) ON DELETE CASCADE,
  day TEXT NOT NULL, dpi INTEGER NOT NULL CHECK(dpi BETWEEN 0 AND 100000),
  dpi_origin TEXT NOT NULL CHECK(dpi_origin IN('auto','manual','unknown')),
  counts REAL NOT NULL DEFAULT 0 CHECK(counts>=0),
  CHECK((dpi=0 AND dpi_origin='unknown') OR (dpi>0 AND dpi_origin<>'unknown')),
  PRIMARY KEY(source_id,day,dpi,dpi_origin)
);
CREATE INDEX idx_mouse_motion_day ON mouse_motion_daily(day);
CREATE TABLE gamepad_motion_daily(
  device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  day TEXT NOT NULL, stick TEXT NOT NULL CHECK(stick IN('left','right')),
  active_us INTEGER NOT NULL DEFAULT 0 CHECK(active_us>=0),
  travel_r REAL NOT NULL DEFAULT 0 CHECK(travel_r>=0),
  PRIMARY KEY(device_id,day,stick)
);
CREATE TABLE gamepad_heat_daily(
  device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  day TEXT NOT NULL, stick TEXT NOT NULL CHECK(stick IN('left','right')),
  bin INTEGER NOT NULL CHECK(bin BETWEEN 0 AND 624),
  dwell_us INTEGER NOT NULL CHECK(dwell_us>0),
  PRIMARY KEY(device_id,day,stick,bin)
);
CREATE INDEX idx_gamepad_heat_day ON gamepad_heat_daily(day);
";

/// 连接级 PRAGMA（PLAN §4.5）：
/// - `journal_mode=WAL`：持久化在库文件中，两进程并发读写的数据集成点（§9.1-1）；
/// - `synchronous=NORMAL`、`foreign_keys=ON`：连接级，每次打开连接都要重设。
///
/// 必须在事务外执行——SQLite 不允许事务内切换 WAL，且事务内设置 foreign_keys 是 no-op。
/// WAL 设置失败（返回非 "wal"）按错误上报：两进程架构依赖 WAL，静默降级会造成读写互阻塞。
fn apply_pragmas(conn: &Connection) -> Result<()> {
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if mode != "wal" {
        return Err(StoreError::WalUnavailable(mode));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

/// 幂等迁移到 [`SCHEMA_VERSION`]：
/// 1. 应用连接级 PRAGMA（WAL / synchronous=NORMAL / foreign_keys=ON）；
/// 2. 建 `schema_migrations`（自身幂等：`IF NOT EXISTS`）；
/// 3. 按版本步进：v0→v1→v2→v3…，每步单事务；已是当前版本则 no-op。
///
/// 可在任意**可写**连接上重复调用（`Writer::open` 与测试均复用）；只读连接会因 WAL/建表失败而报错
/// ——GUI 的 ro 连接按 §5.1 由 src-tauri 侧自行容错，不走本函数。
pub fn migrate(conn: &Connection) -> Result<()> {
    apply_pragmas(conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(
           version INTEGER PRIMARY KEY,
           applied_at TEXT NOT NULL
         );",
    )?;
    let mut current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?;
    while current < SCHEMA_VERSION {
        let next = current + 1;
        // unchecked_transaction：&Connection 即可起事务；出错时 Transaction 被 drop 自动回滚。
        let tx = conn.unchecked_transaction()?;
        match next {
            1 => tx.execute_batch(SCHEMA_V1_SQL)?,
            2 => tx.execute_batch(SCHEMA_V2_SQL)?,
            3 => tx.execute_batch(SCHEMA_V3_SQL)?,
            _ => return Err(StoreError::WalUnavailable(format!("未知 schema 版本 {next}"))),
        }
        tx.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![next, day::now_local_rfc3339()],
        )?;
        tx.commit()?;
        current = next;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDb;
    use rusqlite::params;

    /// §8-S4 验收点：建表幂等——两次 migrate 均成功，版本记录为当前版本，
    /// 全部业务表 + 索引存在（v1 七张 wp + v2 mouse_move_daily + v3 四张运动表）。
    #[test]
    fn migrate_is_idempotent_and_creates_all_objects() {
        let db = TempDb::new("schema-idempotent");
        let conn = Connection::open(db.as_ref()).unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap(); // 第二次必须无害

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let applied: String = conn
            .query_row("SELECT applied_at FROM schema_migrations WHERE version = ?1", [SCHEMA_VERSION], |r| r.get(0))
            .unwrap();
        assert_eq!(applied.len(), 25, "RFC3339 秒级带时区偏移: {applied}");

        const TABLES: [&str; 16] = [
            "devices",
            "input_daily",
            "combo_daily",
            "app_daily",
            "mouse_move_daily",
            "wp_import_meta",
            "wp_key_daily",
            "wp_combo_daily",
            "wp_app_daily",
            "wp_mouse_daily",
            "wp_mouse_buttons_daily",
            "wp_mouse_scroll_daily",
            "mouse_motion_sources",
            "mouse_motion_daily",
            "gamepad_motion_daily",
            "gamepad_heat_daily",
        ];
        for name in TABLES {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [name],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "缺表 {name}");
        }
        // nickname 列存在
        let mut stmt = conn.prepare("PRAGMA table_info(devices)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(cols.iter().any(|c| c == "nickname"), "devices 缺 nickname 列: {cols:?}");
    }

    /// WAL + synchronous=NORMAL + foreign_keys=ON 按 plan 落实。
    #[test]
    fn migrate_sets_wal_synchronous_and_foreign_keys() {
        let db = TempDb::new("schema-pragma");
        let conn = Connection::open(db.as_ref()).unwrap();
        migrate(&conn).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "synchronous=NORMAL");
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1, "foreign_keys=ON");
    }

    /// 迁移后再次打开同一库文件（模拟 GUI 第二进程 ro 前的语义与 collector 重启）：
    /// WAL 持久化生效、migrate 直接 no-op、数据可见。
    #[test]
    fn reopen_same_db_is_wal_and_noop() {
        let db = TempDb::new("schema-reopen");
        {
            let conn = Connection::open(db.as_ref()).unwrap();
            migrate(&conn).unwrap();
            conn.execute(
                "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
                 VALUES ('keyboard', 1, 2, 'K', '2026-01-01', '2026-01-01')",
                [],
            )
            .unwrap();
        }
        let conn2 = Connection::open(db.as_ref()).unwrap();
        migrate(&conn2).unwrap();
        let mode: String = conn2
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let n: i64 = conn2
            .query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let version: i64 = conn2
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    /// DDL 约束逐字生效：kind CHECK、UNIQUE(kind,vid,pid,name)、input_daily 外键。
    #[test]
    fn ddl_constraints_hold() {
        let db = TempDb::new("schema-constraints");
        let conn = Connection::open(db.as_ref()).unwrap();
        migrate(&conn).unwrap();
        // 非法 kind 被 CHECK 拒绝
        assert!(conn
            .execute(
                "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
                 VALUES ('joystick', 0, 0, 'X', '2026-01-01', '2026-01-01')",
                []
            )
            .is_err());
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('keyboard', 0x04D9, 0x0169, 'K', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();
        // UNIQUE(kind, vid, pid, name) 冲突被拒绝
        assert!(conn
            .execute(
                "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
                 VALUES ('keyboard', 0x04D9, 0x0169, 'K', '2026-01-01', '2026-01-01')",
                []
            )
            .is_err());
        // foreign_keys=ON：指向不存在设备的 input_daily 行被拒绝
        assert!(conn
            .execute(
                "INSERT INTO input_daily(device_id, day, code, count) VALUES (999, '2026-01-01', 30, 1)",
                []
            )
            .is_err());
        // wp_import_meta 的 CHECK(id=1)：id=2 被拒绝
        assert!(conn
            .execute(
                "INSERT INTO wp_import_meta(id, imported_at, source_path, note)
                 VALUES (2, '2026-01-01T00:00:00+00:00', 'p', '')",
                []
            )
            .is_err());
    }

    /// 手工搭出 v2 库（生产升级前的真实状态：v1+v2 DDL + 版本记录 1/2，不经 migrate）。
    fn open_v2_db(tag: &str) -> (TempDb, Connection) {
        let db = TempDb::new(tag);
        let conn = Connection::open(db.as_ref()).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations(
               version INTEGER PRIMARY KEY,
               applied_at TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute_batch(SCHEMA_V1_SQL).unwrap();
        conn.execute_batch(SCHEMA_V2_SQL).unwrap();
        conn.execute_batch(
            "INSERT INTO schema_migrations(version, applied_at)
             VALUES (1, '2026-01-01T00:00:00+00:00'), (2, '2026-01-02T00:00:00+00:00');",
        )
        .unwrap();
        (db, conn)
    }

    /// §8-S2 验收点：v2 生产库升级 v3——migrate 只追加 v3 四张表与两个索引，
    /// 旧表/旧行原样（不删除、不回写），重复迁移幂等。
    #[test]
    fn motion_dpi_v2_db_upgrade_appends_v3_and_preserves_old_rows() {
        let (_db, conn) = open_v2_db("schema-v2-upgrade");
        // 升级前写入旧数据行（含旧行为语义的 mouse_move_daily）
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 0x1532, 0x0045, '旧鼠标', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO input_daily(device_id, day, code, count) VALUES (1, '2026-09-28', 30, 7)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mouse_move_daily(device_id, day, distance_inches) VALUES (1, '2026-09-28', 1.25)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, nickname, first_seen, last_seen)
             VALUES ('keyboard', 1, 2, '旧键盘', '昵称', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        // 版本推进到 3，且 1/2 两条历史记录保留
        let versions: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect()
        };
        assert_eq!(versions, vec![1, 2, SCHEMA_VERSION]);

        // v3 四张表 + 两个索引存在
        for (kind, name) in [
            ("table", "mouse_motion_sources"),
            ("table", "mouse_motion_daily"),
            ("table", "gamepad_motion_daily"),
            ("table", "gamepad_heat_daily"),
            ("index", "idx_mouse_motion_day"),
            ("index", "idx_gamepad_heat_day"),
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type=?1 AND name=?2",
                    [kind, name],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "缺 {kind} {name}");
        }

        // 旧表旧行原样（不删除、不回写：距离仍是英寸原值）
        let (dev_n, nickname): (i64, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*), MAX(nickname) FROM devices",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((dev_n, nickname.as_deref()), (2, Some("昵称")));
        let (input_sum, mv_rows, inches): (i64, i64, f64) = conn
            .query_row(
                "SELECT (SELECT SUM(count) FROM input_daily),
                        (SELECT COUNT(*) FROM mouse_move_daily),
                        (SELECT MAX(distance_inches) FROM mouse_move_daily)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((input_sum, mv_rows), (7, 1));
        assert_eq!(inches, 1.25);

        // 幂等：再次迁移无害，旧行仍原样
        migrate(&conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM mouse_move_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    /// §6.1"字段约束是持久合同（逐字）"：v3 四张表的 CHECK/外键/组合约束逐项锚定。
    #[test]
    fn motion_dpi_v3_ddl_constraints_hold() {
        let db = TempDb::new("schema-v3-constraints");
        let conn = Connection::open(db.as_ref()).unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 0x1532, 0x0045, '鼠标', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('gamepad', 3, 4, '手柄', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mouse_motion_sources(source_key, device_id, physical, first_seen, last_seen)
             VALUES ('k1', 1, 1, 't', 't')",
            [],
        )
        .unwrap();
        // source_key UNIQUE：重复注册同 key 被拒（幂等由写入层 upsert 语义负责）
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_sources(source_key, device_id, physical, first_seen, last_seen)
                 VALUES ('k1', 1, 1, 't', 't')",
                []
            )
            .is_err());
        // physical CHECK(0,1)
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_sources(source_key, device_id, physical, first_seen, last_seen)
                 VALUES ('k2', 1, 2, 't', 't')",
                []
            )
            .is_err());
        // manual_dpi CHECK(1..=100000)：0 与 100001 均拒绝
        for bad in [0, 100_001] {
            assert!(conn
                .execute(
                    "INSERT INTO mouse_motion_sources(source_key, device_id, physical, manual_dpi, first_seen, last_seen)
                     VALUES (?1, 1, 1, ?2, 't', 't')",
                    params![format!("m{bad}"), bad],
                )
                .is_err(),
            "manual_dpi={bad} 必须被 CHECK 拒绝");
        }
        // probe_status CHECK 六值
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_sources(source_key, device_id, physical, probe_status, first_seen, last_seen)
                 VALUES ('k3', 1, 1, 'bogus', 't', 't')",
                []
            )
            .is_err());
        // auto_dpi CHECK(1..=57343)
        for bad in [0i64, 57_344] {
            assert!(conn
                .execute(
                    "INSERT INTO mouse_motion_sources(source_key, device_id, physical, auto_dpi, first_seen, last_seen)
                     VALUES (?1, 1, 1, ?2, 't', 't')",
                    params![format!("a{bad}"), bad],
                )
                .is_err(),
            "auto_dpi={bad} 必须被 CHECK 拒绝");
        }
        // mouse_motion_daily：dpi=0 只允许 unknown origin；dpi>0 禁 unknown
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (1, '2026-09-28', 0, 'manual', 1.0)",
                []
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (1, '2026-09-28', 800, 'unknown', 1.0)",
                []
            )
            .is_err());
        // counts CHECK(>=0)
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (1, '2026-09-28', 800, 'manual', -0.5)",
                []
            )
            .is_err());
        // dpi CHECK(0..=100000)
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (1, '2026-09-28', 100_001, 'manual', 1.0)",
                []
            )
            .is_err());
        // 外键：source_id=999 不存在被拒
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (999, '2026-09-28', 800, 'manual', 1.0)",
                []
            )
            .is_err());
        // 合法组合可以写入（unknown 桶 dpi=0）
        conn.execute(
            "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
             VALUES (1, '2026-09-28', 0, 'unknown', 4.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
             VALUES (1, '2026-09-28', 800, 'manual', 8.0)",
            [],
        )
        .unwrap();
        // PRIMARY KEY(source_id,day,dpi,dpi_origin)：同桶裸 INSERT 冲突被拒
        assert!(conn
            .execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (1, '2026-09-28', 800, 'manual', 1.0)",
                []
            )
            .is_err());
        // gamepad_motion_daily：stick CHECK、active_us/travel_r 非负、外键
        assert!(conn
            .execute(
                "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
                 VALUES (2, '2026-09-28', 'center', 1, 0.0)",
                []
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
                 VALUES (2, '2026-09-28', 'left', -1, 0.0)",
                []
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
                 VALUES (2, '2026-09-28', 'left', 1, -0.5)",
                []
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
                 VALUES (999, '2026-09-28', 'left', 1, 0.0)",
                []
            )
            .is_err());
        // gamepad_heat_daily：bin 0..=624、dwell_us>0
        assert!(conn
            .execute(
                "INSERT INTO gamepad_heat_daily(device_id, day, stick, bin, dwell_us)
                 VALUES (2, '2026-09-28', 'left', 625, 1)",
                []
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO gamepad_heat_daily(device_id, day, stick, bin, dwell_us)
                 VALUES (2, '2026-09-28', 'left', 312, 0)",
                []
            )
            .is_err());
        conn.execute(
            "INSERT INTO gamepad_heat_daily(device_id, day, stick, bin, dwell_us)
             VALUES (2, '2026-09-28', 'left', 624, 1)",
            [],
        )
        .unwrap();
        // bin=624 边界可写；再验证 bin=0 边界
        conn.execute(
            "INSERT INTO gamepad_heat_daily(device_id, day, stick, bin, dwell_us)
             VALUES (2, '2026-09-28', 'right', 0, 1)",
            [],
        )
        .unwrap();
    }
}
