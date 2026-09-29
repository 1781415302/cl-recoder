//! writer —— collector 唯一写入口（PLAN §4.5 契约）。
//!
//! - [`Writer::open`]：建目录 → 连接 → busy_timeout(10s) → migrate（含 WAL）；
//! - [`Writer::get_or_create_device`]：设备不存在则插入（first_seen/last_seen=now），
//!   存在则返回既有 id；`DeviceKey → id` 结果缓存（§4.6 aggregator 热路径零 SQL）；
//! - [`Writer::flush`]：单事务批量 upsert，`ON CONFLICT DO UPDATE count=count+excluded.count`
//!   （app_daily 三个计数列同语义累加）——同键 flush 两次计数翻倍；
//!   失败语义：整批回滚返回 Err，调用方（aggregator）保留聚合桶、下个 tick 重试、日志限频
//!   ——flush 失败绝不丢计数、绝不 panic（PLAN §9.4）；
//! - [`Writer::rebuild_wp_tables`] / [`rebuild_wp_tables_with`]：WhatPulse 导入单事务整体重建
//!   （wp_* 的 SQL 唯一归属本 crate，§2.2；import.rs 只做编排）。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use clrecoder_core::day;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::reader::{
    WpAppDailyRow, WpComboDailyRow, WpKeyDailyRow, WpMetaRow, WpMouseButtonDailyRow,
    WpMouseDailyRow, WpMouseScrollDailyRow,
};
use crate::schema;
use crate::{count_to_i64, kind_to_text, Result};

/// 每 0.5s flush 一次的批量写载荷（近实时；PLAN §4.5 批量语义保留）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FlushBatch {
    /// input_daily 增量：(device_id, day, code, count)。
    pub input: Vec<(i64, String, u16, u64)>,
    /// combo_daily 增量：(day, mods, code, count)。
    pub combos: Vec<(String, u8, u16, u64)>,
    /// app_daily 增量：(day, exe, secs, keys, clicks)。
    pub apps: Vec<(String, String, u64, u64, u64)>,
    /// mouse_move_daily 增量：(device_id, day, distance_inches)。
    pub mouse_move: Vec<(i64, String, f64)>,
}

/// WhatPulse 导入载荷（PLAN §4.5：字段 = 各 wp_* 表行结构 + meta；行结构与 [`crate::reader`] 共用）。
///
/// [`Writer::rebuild_wp_tables`] 单事务清空全部 wp_* 表后写入本批并落 `wp_import_meta(id=1)`
/// ——整体替换语义（§4.8：重复导入即刷新为最新快照）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WpImportBatch {
    /// 导入元数据（`imported_at` 由导入方填写）。
    pub meta: WpMetaRow,
    /// wp_key_daily 行。
    pub keys: Vec<WpKeyDailyRow>,
    /// wp_combo_daily 行。
    pub combos: Vec<WpComboDailyRow>,
    /// wp_app_daily 行。
    pub apps: Vec<WpAppDailyRow>,
    /// wp_mouse_daily 行。
    pub mouse: Vec<WpMouseDailyRow>,
    /// wp_mouse_buttons_daily 行。
    pub mouse_buttons: Vec<WpMouseButtonDailyRow>,
    /// wp_mouse_scroll_daily 行。
    pub mouse_scrolls: Vec<WpMouseScrollDailyRow>,
}

/// collector 用的 SQLite 写手柄（PLAN §4.5）。
///
/// 内部：`Mutex<Connection>`（跨线程安全）+ `DeviceKey → device_id` 缓存。
/// 通过 `Arc<Writer>` 在采集线程与 aggregator 间共享。
pub struct Writer {
    conn: Mutex<Connection>,
    devices: Mutex<HashMap<clrecoder_core::event::DeviceKey, i64>>,
}

impl Writer {
    /// 打开（必要时创建）统计库：建父目录 → 连接 → busy_timeout(10s) → migrate（含 WAL）。
    ///
    /// busy_timeout 先于 migrate 设置：建表/迁移本身也可能与另一进程（GUI 导入临时 rw 连接）争锁。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        schema::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn), devices: Mutex::new(HashMap::new()) })
    }

    /// 取连接。毒化互斥量恢复为可用状态——任何线程 panic 后采集端仍不得 panic（PLAN §9.4）。
    pub(crate) fn lock_conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_devices(&self) -> MutexGuard<'_, HashMap<clrecoder_core::event::DeviceKey, i64>> {
        self.devices.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 设备不存在则插入（first_seen/last_seen=now），存在则返回既有 id（§4.5）。
    ///
    /// 命中进程内缓存时不做任何 SQL；`last_seen` 的刷新**节流到每 flush 一次**
    /// （[`Writer::flush`] 对本批涉及的设备统一刷新）。
    pub fn get_or_create_device(&self, d: &clrecoder_core::event::DeviceKey) -> Result<i64> {
        if let Some(id) = self.lock_devices().get(d) {
            return Ok(*id);
        }
        let conn = self.lock_conn();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM devices WHERE kind = ?1 AND vid = ?2 AND pid = ?3 AND name = ?4",
                params![kind_to_text(d.kind), d.vid, d.pid, d.name],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                let now = day::now_local_rfc3339();
                conn.execute(
                    "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![kind_to_text(d.kind), d.vid, d.pid, d.name, now],
                )?;
                conn.last_insert_rowid()
            }
        };
        self.lock_devices().insert(d.clone(), id);
        Ok(id)
    }

    /// 单事务批量 upsert（PLAN §4.5）：
    /// `INSERT ... ON CONFLICT DO UPDATE count = count + excluded.count`
    /// （app_daily 的 foreground_secs/key_count/click_count 三列同语义累加；
    ///  mouse_move_daily 的 distance_inches 同语义累加）。
    ///
    /// 同时把本批涉及设备的 `last_seen` 刷新一次（"节流：每 flush 一次"的落地处）。
    /// 失败语义：任一步出错整批回滚并返回 Err；调用方保留聚合桶、下个 tick 重试——绝不丢计数。
    pub fn flush(&self, b: &FlushBatch) -> Result<()> {
        if b.input.is_empty() && b.combos.is_empty() && b.apps.is_empty() && b.mouse_move.is_empty() {
            return Ok(());
        }
        let conn = self.lock_conn();
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO input_daily(device_id, day, code, count) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(device_id, day, code) DO UPDATE SET count = count + excluded.count",
            )?;
            for (device_id, day, code, count) in &b.input {
                stmt.execute(params![device_id, day, code, count_to_i64(*count)?])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO combo_daily(day, mods, code, count) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(day, mods, code) DO UPDATE SET count = count + excluded.count",
            )?;
            for (day, mods, code, count) in &b.combos {
                stmt.execute(params![day, mods, code, count_to_i64(*count)?])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO app_daily(day, exe, foreground_secs, key_count, click_count)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(day, exe) DO UPDATE SET
                   foreground_secs = foreground_secs + excluded.foreground_secs,
                   key_count = key_count + excluded.key_count,
                   click_count = click_count + excluded.click_count",
            )?;
            for (day, exe, secs, keys, clicks) in &b.apps {
                stmt.execute(params![
                    day,
                    exe,
                    count_to_i64(*secs)?,
                    count_to_i64(*keys)?,
                    count_to_i64(*clicks)?
                ])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO mouse_move_daily(device_id, day, distance_inches) VALUES (?1, ?2, ?3)
                 ON CONFLICT(device_id, day) DO UPDATE SET
                   distance_inches = distance_inches + excluded.distance_inches",
            )?;
            for (device_id, day, inches) in &b.mouse_move {
                stmt.execute(params![device_id, day, inches])?;
            }
        }
        // last_seen 节流刷新：本批涉及的设备每 flush 恰好一次
        {
            let mut stmt = tx.prepare("UPDATE devices SET last_seen = ?1 WHERE id = ?2")?;
            let now = day::now_local_rfc3339();
            let mut seen = HashSet::new();
            for (device_id, ..) in b.input.iter().map(|(id, ..)| (id,)).chain(
                b.mouse_move.iter().map(|(id, ..)| (id,)),
            ) {
                if seen.insert(*device_id) {
                    stmt.execute(params![now, device_id])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 设置/清空设备昵称（GUI 调用；nickname 为空串或 None 即清除）。
    pub fn set_device_nickname(&self, device_id: i64, nickname: Option<&str>) -> Result<()> {
        let conn = self.lock_conn();
        let nick = nickname.map(str::trim).filter(|s| !s.is_empty());
        conn.execute(
            "UPDATE devices SET nickname = ?1 WHERE id = ?2",
            params![nick, device_id],
        )?;
        Ok(())
    }

    /// WhatPulse 导入整体重建（PLAN §4.5/§5.4）：单事务清空全部 wp_* 表并写入 `b`，
    /// 同时写 `wp_import_meta(id=1)`。重复导入 = 整体替换为最新快照。
    pub fn rebuild_wp_tables(&self, b: &WpImportBatch) -> Result<()> {
        let conn = self.lock_conn();
        rebuild_wp_tables_with(&conn, b)
    }
}

/// [`Writer::rebuild_wp_tables`] 的连接参数版本。
///
/// GUI 导入流程（§5.4）与 collector 分属两个进程：GUI 用自开的临时 rw 连接
/// （busy_timeout=10s）直接调用本函数即可，wp_* 的 SQL 仍唯一归属本 crate（PLAN §2.2/§3）。
pub fn rebuild_wp_tables_with(conn: &Connection, b: &WpImportBatch) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    // 单事务整体清空（无外键依赖 wp_*，删除顺序无关）
    tx.execute_batch(
        "DELETE FROM wp_mouse_scroll_daily;
         DELETE FROM wp_mouse_buttons_daily;
         DELETE FROM wp_mouse_daily;
         DELETE FROM wp_app_daily;
         DELETE FROM wp_combo_daily;
         DELETE FROM wp_key_daily;
         DELETE FROM wp_import_meta;",
    )?;
    {
        let mut stmt = tx
            .prepare("INSERT INTO wp_key_daily(day, qt_key, label, count) VALUES (?1, ?2, ?3, ?4)")?;
        for r in &b.keys {
            stmt.execute(params![r.day, r.qt_key, r.label, count_to_i64(r.count)?])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO wp_combo_daily(day, combo, label, count) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for r in &b.combos {
            stmt.execute(params![r.day, r.combo, r.label, count_to_i64(r.count)?])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO wp_app_daily(day, path, name, seconds, keys, clicks)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for r in &b.apps {
            stmt.execute(params![
                r.day,
                r.path,
                r.name,
                count_to_i64(r.seconds)?,
                count_to_i64(r.keys)?,
                count_to_i64(r.clicks)?
            ])?;
        }
    }
    {
        let mut stmt = tx
            .prepare("INSERT INTO wp_mouse_daily(day, clicks, distance_inches) VALUES (?1, ?2, ?3)")?;
        for r in &b.mouse {
            stmt.execute(params![r.day, count_to_i64(r.clicks)?, r.distance_inches])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO wp_mouse_buttons_daily(day, button_code, label, count)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for r in &b.mouse_buttons {
            stmt.execute(params![r.day, r.button_code, r.label, count_to_i64(r.count)?])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO wp_mouse_scroll_daily(day, direction_code, label, count)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for r in &b.mouse_scrolls {
            stmt.execute(params![r.day, r.direction_code, r.label, count_to_i64(r.count)?])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO wp_import_meta(id, imported_at, source_path, source_size, date_min, date_max, note)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        stmt.execute(params![
            b.meta.imported_at,
            b.meta.source_path,
            b.meta.source_size,
            b.meta.date_min,
            b.meta.date_max,
            b.meta.note
        ])?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDb;
    use clrecoder_core::codes::{mods, DeviceKind};
    use clrecoder_core::event::DeviceKey;

    fn open_writer(tag: &str) -> (TempDb, Writer) {
        let db = TempDb::new(tag);
        let w = Writer::open(db.as_ref()).unwrap();
        (db, w)
    }

    /// open 建父目录（§4.5：建目录）。
    #[test]
    fn open_creates_parent_directories() {
        let base =
            std::env::temp_dir().join(format!("clrecoder-store-dirs-{}", std::process::id()));
        let path = base.join("a/b/stats.db");
        let _ = std::fs::remove_dir_all(&base);
        let w = Writer::open(&path).unwrap();
        assert!(path.is_file());
        drop(w);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// busy_timeout(10s) 按 plan 设置（PRAGMA busy_timeout 单位毫秒）。
    #[test]
    fn open_sets_busy_timeout_10s() {
        let (_db, w) = open_writer("writer-busytimeout");
        let conn = w.lock_conn();
        let ms: i64 = conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0)).unwrap();
        assert_eq!(ms, 10_000);
    }

    /// get_or_create_device：同键幂等（缓存/查库两路一致），四元组任一分量不同即新设备。
    #[test]
    fn get_or_create_device_is_stable_and_distinct() {
        let (db, w) = open_writer("writer-device");
        let kb = DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 0x04D9,
            pid: 0x0169,
            name: "测试键盘".to_string(),
        };
        let id1 = w.get_or_create_device(&kb).unwrap();
        let id2 = w.get_or_create_device(&kb).unwrap(); // 缓存命中
        assert_eq!(id1, id2);

        // 同 vid/pid/name 不同 kind → 不同设备（§4.1：code 唯一性只在种类内成立）
        let mut mouse_same_name = kb.clone();
        mouse_same_name.kind = DeviceKind::Mouse;
        assert_ne!(w.get_or_create_device(&mouse_same_name).unwrap(), id1);
        // 同 kind/vid/pid 不同 name → 不同设备（UNIQUE 四元组）
        let mut renamed = kb.clone();
        renamed.name = "另一个键盘".to_string();
        assert_ne!(w.get_or_create_device(&renamed).unwrap(), id1);
        // 同 kind/name 不同 vid → 不同设备
        let mut other_vid = kb.clone();
        other_vid.vid = 0x1532;
        assert_ne!(w.get_or_create_device(&other_vid).unwrap(), id1);

        // 新 Writer 打开同一库（无缓存）仍命中既有行——不产生重复设备
        let w2 = Writer::open(db.as_ref()).unwrap();
        assert_eq!(w2.get_or_create_device(&kb).unwrap(), id1);
        let n: i64 = {
            let conn = w2.lock_conn();
            conn.query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(n, 4, "恰 4 台设备（kb/mouse/renamed/other_vid）");
    }

    /// §8-S4 验收点：同键 flush 两次计数翻倍（input/combo/app 三表全部累加语义）。
    #[test]
    fn flush_same_key_twice_doubles_count() {
        let (_db, w) = open_writer("writer-double");
        let day = "2026-09-28";
        let dev = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0,
                pid: 0,
                name: "测试键盘".to_string(),
            })
            .unwrap();
        let batch = |keys: u64, secs: u64, k: u64, c: u64| FlushBatch {
            input: vec![(dev, day.to_string(), 0x1E, keys)],
            combos: vec![(day.to_string(), mods::CTRL, 0x2E, 1)],
            apps: vec![(day.to_string(), "code.exe".to_string(), secs, k, c)],
            ..Default::default()
        };
        w.flush(&batch(3, 5, 3, 0)).unwrap();
        w.flush(&batch(3, 5, 2, 1)).unwrap();

        let conn = w.lock_conn();
        let count: i64 = conn
            .query_row(
                "SELECT count FROM input_daily WHERE device_id=?1 AND day=?2 AND code=?3",
                params![dev, day, 0x1E],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 6, "同键 flush 两次必须翻倍（3+3=6）");
        // 是累加而非新增行
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM input_daily WHERE device_id=?1 AND day=?2",
                params![dev, day],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);

        let combo: i64 = conn
            .query_row(
                "SELECT count FROM combo_daily WHERE day=?1 AND mods=?2 AND code=?3",
                params![day, mods::CTRL, 0x2E],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(combo, 2, "组合键计数同样翻倍");

        let (secs, keys, clicks): (i64, i64, i64) = conn
            .query_row(
                "SELECT foreground_secs, key_count, click_count FROM app_daily WHERE day=?1 AND exe=?2",
                params![day, "code.exe"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((secs, keys, clicks), (10, 5, 1), "app_daily 三列累加");
    }

    /// 空批次为 no-op；flush 一次写多设备/多日/多键。
    #[test]
    fn flush_empty_batch_ok_and_batched_writes_land() {
        let (_db, w) = open_writer("writer-batch");
        w.flush(&FlushBatch::default()).unwrap();
        let d1 = DeviceKey { kind: DeviceKind::Mouse, vid: 1, pid: 1, name: "M1".into() };
        let d2 = DeviceKey { kind: DeviceKind::Mouse, vid: 2, pid: 2, name: "M2".into() };
        let id1 = w.get_or_create_device(&d1).unwrap();
        let id2 = w.get_or_create_device(&d2).unwrap();
        w.flush(&FlushBatch {
            input: vec![
                (id1, "2026-09-27".into(), 1, 10),
                (id1, "2026-09-28".into(), 1, 4),
                (id2, "2026-09-27".into(), 2, 7),
            ],
            ..Default::default()
        })
        .unwrap();
        let conn = w.lock_conn();
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM input_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3, "三行一次落库");
    }

    /// last_seen 节流语义：缓存命中的 get_or_create 不刷新；flush 恰好刷新一次。
    #[test]
    fn last_seen_refreshes_per_flush_not_per_call() {
        let (_db, w) = open_writer("writer-lastseen");
        let d = DeviceKey { kind: DeviceKind::Keyboard, vid: 3, pid: 4, name: "K".into() };
        let dev = w.get_or_create_device(&d).unwrap();

        // 人为把 last_seen 拨回过去
        {
            let conn = w.lock_conn();
            conn.execute("UPDATE devices SET last_seen = '2000-01-01T00:00:00+00:00'", [])
                .unwrap();
        }
        // 缓存命中路径：不改 last_seen
        assert_eq!(w.get_or_create_device(&d).unwrap(), dev);
        {
            let conn = w.lock_conn();
            let ls: String = conn
                .query_row("SELECT last_seen FROM devices WHERE id=?1", [dev], |r| r.get(0))
                .unwrap();
            assert_eq!(ls, "2000-01-01T00:00:00+00:00", "get_or_create 缓存命中不得刷新");
        }
        // flush 携带该设备 → 刷新
        w.flush(&FlushBatch {
            input: vec![(dev, "2026-09-28".into(), 0x39, 1)],
            ..Default::default()
        })
        .unwrap();
        {
            let conn = w.lock_conn();
            let ls: String = conn
                .query_row("SELECT last_seen FROM devices WHERE id=?1", [dev], |r| r.get(0))
                .unwrap();
            assert_ne!(ls, "2000-01-01T00:00:00+00:00", "flush 必须刷新 last_seen");
            assert_eq!(ls.len(), 25);
        }
    }

    /// §8-S4 验收点：rebuild_wp_tables 单事务重建——整体替换、meta id=1 唯一、全表清空。
    #[test]
    fn rebuild_wp_tables_replaces_all_and_writes_meta() {
        let (_db, w) = open_writer("writer-wp-rebuild");
        let meta1 = WpMetaRow {
            imported_at: "2026-09-28T10:00:00+08:00".into(),
            source_path: r"C:\wp\whatpulse.db".into(),
            source_size: Some(12_345),
            date_min: Some("2025-01-01".into()),
            date_max: Some("2026-09-27".into()),
            note: "首次导入".into(),
        };
        let b1 = WpImportBatch {
            meta: meta1,
            keys: vec![
                WpKeyDailyRow { day: "2026-09-27".into(), qt_key: 87, label: "W".into(), count: 100 },
                WpKeyDailyRow { day: "2026-09-28".into(), qt_key: 65, label: "A".into(), count: 50 },
            ],
            combos: vec![WpComboDailyRow {
                day: "2026-09-27".into(),
                combo: "control,67".into(),
                label: "Ctrl+C".into(),
                count: 7,
            }],
            apps: vec![WpAppDailyRow {
                day: "2026-09-27".into(),
                path: "c:/dev/editor.exe".into(),
                name: "Editor".into(),
                seconds: 3600,
                keys: 800,
                clicks: 12,
            }],
            mouse: vec![WpMouseDailyRow {
                day: "2026-09-27".into(),
                clicks: 250,
                distance_inches: 123.5,
            }],
            mouse_buttons: vec![WpMouseButtonDailyRow {
                day: "2026-09-27".into(),
                button_code: 0,
                label: "左键".into(),
                count: 200,
            }],
            mouse_scrolls: vec![WpMouseScrollDailyRow {
                day: "2026-09-27".into(),
                direction_code: 1,
                label: "向上".into(),
                count: 30,
            }],
        };
        w.rebuild_wp_tables(&b1).unwrap();

        // 第二次导入：整体替换（旧行必须全部消失）
        let meta2 = WpMetaRow {
            imported_at: "2026-09-28T11:00:00+08:00".into(),
            source_path: r"C:\wp\whatpulse2.db".into(),
            source_size: None,
            date_min: None,
            date_max: None,
            note: String::new(),
        };
        let b2 = WpImportBatch {
            keys: vec![WpKeyDailyRow {
                day: "2026-09-28".into(),
                qt_key: 66,
                label: "B".into(),
                count: 10,
            }],
            mouse_scrolls: vec![WpMouseScrollDailyRow {
                day: "2026-09-28".into(),
                direction_code: 2,
                label: "向下".into(),
                count: 5,
            }],
            meta: meta2,
            ..Default::default()
        };
        w.rebuild_wp_tables(&b2).unwrap();

        let conn = w.lock_conn();
        let key_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM wp_key_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(key_rows, 1, "整体替换：第一次导入的 2 行必须被清空");
        let key_sum: i64 = conn
            .query_row("SELECT COALESCE(SUM(count),0) FROM wp_key_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(key_sum, 10);
        for t in
            ["wp_combo_daily", "wp_app_daily", "wp_mouse_daily", "wp_mouse_buttons_daily"]
        {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "表 {t} 应被清空");
        }
        let (meta_rows, src): (i64, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(source_path) FROM wp_import_meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((meta_rows, src.as_str()), (1, r"C:\wp\whatpulse2.db"));
        // meta 可空列
        let (size, dmin, dmax): (Option<i64>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT source_size, date_min, date_max FROM wp_import_meta WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((size, dmin, dmax), (None, None, None));
    }

    /// flush 出错时整批回滚：batch 前半合法、后半非法（引用不存在设备）→ 前半也不落库。
    #[test]
    fn flush_rolls_back_atomically_on_error() {
        let (_db, w) = open_writer("writer-rollback");
        let dev = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0,
                pid: 0,
                name: "K".into(),
            })
            .unwrap();
        let err = w
            .flush(&FlushBatch {
                input: vec![
                    (dev, "2026-09-28".into(), 0x1E, 5), // 合法
                    (7, "2026-09-28".into(), 0x1F, 1),   // device_id=7 不存在（FK）
                ],
                combos: vec![("2026-09-28".into(), mods::CTRL, 0x2E, 1)],
                apps: vec![("2026-09-28".into(), "code.exe".into(), 1, 1, 1)],
                ..Default::default()
            })
            .unwrap_err();
        let _ = err; // 具体错误类型不强校验（rusqlite FK 错误）
        let conn = w.lock_conn();
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM input_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "FK 失败的批次必须整批回滚，合法前半也不落库");
        let c: i64 =
            conn.query_row("SELECT COUNT(*) FROM combo_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(c, 0);
        let a: i64 =
            conn.query_row("SELECT COUNT(*) FROM app_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(a, 0);
    }
}
