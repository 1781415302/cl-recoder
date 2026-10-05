//! writer —— collector 唯一写入口（PLAN §4.5 契约）。
//!
//! - [`Writer::open`]：建目录 → 连接 → busy_timeout(10s) → migrate（含 WAL）；
//! - [`Writer::get_or_create_device`]：设备不存在则插入（first_seen/last_seen=now），
//!   存在则返回既有 id；`DeviceKey → id` 结果缓存（§4.6 aggregator 热路径零 SQL）；
//! - [`Writer::flush`]：单事务批量 upsert，`ON CONFLICT DO UPDATE count=count+excluded.count`
//!   （app_daily 三个计数列同语义累加）——同键 flush 两次计数翻倍；
//!   失败语义：整批回滚返回 Err，调用方（aggregator）保留聚合桶、下个 tick 重试、日志限频
//!   ——flush 失败绝不丢计数、绝不 panic（PLAN §9.4）；
//!   motion-dpi §4.4：新两类运动增量（mouse_motion/stick_motion）与旧统计**同一个事务**，
//!   任一部分失败全回滚；空批次提前返回判定纳入运动字段（纯热度批也必须落库）；
//! - [`Writer::register_mouse_source`] / [`Writer::update_mouse_source_state`] /
//!   [`Writer::manual_dpi`]：来源元数据/手动配置（不是增量，幂等可重试；SQL 在 [`crate::motion`]）；
//! - [`Writer::rebuild_wp_tables`] / [`rebuild_wp_tables_with`]：WhatPulse 导入单事务整体重建
//!   （wp_* 的 SQL 唯一归属本 crate，§2.2；import.rs 只做编排）。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use clrecoder_core::day;
use clrecoder_core::motion::{MouseSourceDescriptor, MouseSourceState};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::motion::{
    self as motion_sql, dpi_origin_to_text, stick_side_to_text, validate_mouse_motion_write,
    validate_finite_nonneg, MouseConfigRow, MouseMotionWrite, StickMotionWrite,
};
use crate::reader::{
    WpAppDailyRow, WpComboDailyRow, WpKeyDailyRow, WpMetaRow, WpMouseButtonDailyRow,
    WpMouseDailyRow, WpMouseScrollDailyRow,
};
use crate::schema;
use crate::{count_to_i64, kind_to_text, Result, StoreError};

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
    /// mouse_motion_daily 增量（motion-dpi §4.4：来源×日×DPI 桶；与旧统计同事务）。
    pub mouse_motion: Vec<MouseMotionWrite>,
    /// gamepad_motion_daily + gamepad_heat_daily 增量（同事务写入；空 bin 不落行）。
    pub stick_motion: Vec<StickMotionWrite>,
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
    /// motion-dpi §4.4：`mouse_motion`（按 来源×日×DPI 桶）与 `stick_motion`
    /// （motion 行 + 热度 bins，空 bin 不落行）在同一事务内追加——任何部分失败全回滚；
    /// 空批次提前返回判定纳入两类运动字段（只有保持帧产生的热度也必须落库）。
    ///
    /// 同时把本批涉及设备的 `last_seen` 刷新一次（"节流：每 flush 一次"的落地处）。
    /// 失败语义：任一步出错整批回滚并返回 Err；调用方保留聚合桶、下个 tick 重试——绝不丢计数。
    pub fn flush(&self, b: &FlushBatch) -> Result<()> {
        if b.input.is_empty()
            && b.combos.is_empty()
            && b.apps.is_empty()
            && b.mouse_move.is_empty()
            && b.mouse_motion.is_empty()
            && b.stick_motion.is_empty()
        {
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
        // motion-dpi §4.4：鼠标运动按（来源, 日, DPI 桶）累加——与旧统计同事务，失败全回滚；
        // Rust 先行校验 counts 有限非负与 dpi×origin 配对（§6.1 不只依赖 CHECK），坏值报错回滚。
        {
            let mut stmt = tx.prepare(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(source_id, day, dpi, dpi_origin) DO UPDATE SET
                   counts = counts + excluded.counts",
            )?;
            for w in &b.mouse_motion {
                validate_mouse_motion_write(w)?;
                stmt.execute(params![
                    w.source_id,
                    w.day,
                    w.dpi,
                    dpi_origin_to_text(w.origin),
                    w.counts
                ])?;
            }
        }
        // 手柄摇杆逐日运动行（active_us/travel_r 累加；travel_r 同样 Rust 先行校验）
        {
            let mut stmt = tx.prepare(
                "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(device_id, day, stick) DO UPDATE SET
                   active_us = active_us + excluded.active_us,
                   travel_r = travel_r + excluded.travel_r",
            )?;
            for w in &b.stick_motion {
                validate_finite_nonneg(w.travel_r)?;
                stmt.execute(params![
                    w.device_id,
                    w.day,
                    stick_side_to_text(w.side),
                    count_to_i64(w.active_us)?,
                    w.travel_r
                ])?;
            }
        }
        // 手柄停留热力行（§6.1"空 bin 不写行"：dwell_us=0 的增量跳过；其余按格累加）
        {
            let mut stmt = tx.prepare(
                "INSERT INTO gamepad_heat_daily(device_id, day, stick, bin, dwell_us)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(device_id, day, stick, bin) DO UPDATE SET
                   dwell_us = dwell_us + excluded.dwell_us",
            )?;
            for w in &b.stick_motion {
                let side = stick_side_to_text(w.side);
                for bin in &w.bins {
                    if bin.dwell_us == 0 {
                        continue;
                    }
                    let bin_id = i64::from(bin.bin);
                    if bin_id > 624 {
                        return Err(StoreError::InvalidMotionField(format!(
                            "热力格号 {bin_id} 超出 0..=624"
                        )));
                    }
                    stmt.execute(params![w.device_id, w.day, side, bin_id, count_to_i64(bin.dwell_us)?])?;
                }
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

    /// 注册鼠标运动来源（motion-dpi §4.4）：model 行经既有设备缓存解析/创建
    /// （"model id 复用 Writer 缓存"），来源行按 source_key 幂等、重注册返回既有 id。
    /// 元数据写入不是增量：失败可重试、不涉及任何 counts。
    pub fn register_mouse_source(&self, descriptor: &MouseSourceDescriptor) -> Result<i64> {
        let device_id = self.get_or_create_device(&descriptor.model)?;
        let conn = self.lock_conn();
        motion_sql::register_mouse_source(&conn, descriptor, device_id, &day::now_local_rfc3339())
    }

    /// 应用鼠标来源状态快照（motion-dpi §4.4 心跳）：connected 为缓存证据，
    /// 读取侧另有 last_seen≤5 秒新鲜度门（§6.1）。同来源连接代际的旧代结果拒绝
    /// 由 aggregator 侧在调用前完成（连接 ID 只在内存）。
    pub fn update_mouse_source_state(&self, id: i64, state: &MouseSourceState) -> Result<()> {
        let conn = self.lock_conn();
        motion_sql::update_mouse_source_state(&conn, id, state)
    }

    /// 批量读取来源手动 DPI 配置（motion-dpi §4.4：DPI worker ≤500ms 批读；
    /// 未注册的 key 不在返回中，已注册未配置的 manual_dpi=None）。
    pub fn manual_dpi(&self, keys: &[String]) -> Result<Vec<MouseConfigRow>> {
        let conn = self.lock_conn();
        motion_sql::read_manual_dpi(&conn, keys)
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
    use crate::motion::{MouseMotionWrite, StickMotionWrite};
    use crate::testutil::TempDb;
    use clrecoder_core::codes::{mods, DeviceKind};
    use clrecoder_core::event::DeviceKey;
    use clrecoder_core::motion::{
        DpiOrigin, DpiProbeStatus, MotionConnectionId, MotionStamp, MouseSourceDescriptor,
        MouseSourceState, StickBinDelta, StickSide,
    };

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

    // ---------------- motion-dpi §8-S2：FlushBatch 运动字段与同事务写入 ----------------

    const MOTION_DAY: &str = "2026-09-28";

    /// 物理鼠标来源描述。
    fn mouse_descriptor(key: &str, model_name: &str) -> MouseSourceDescriptor {
        MouseSourceDescriptor {
            source_key: key.to_string(),
            model: DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0x1532,
                pid: 0x0045,
                name: model_name.to_string(),
            },
            interface_path: None,
            physical: true,
        }
    }

    /// §8-S2 验收点："空批次提前返回"判定纳入运动字段——只有运动增量（无任何旧统计）
    /// 的批次也必须写库（"只有保持帧产生的热度也必须落库"）。
    #[test]
    fn motion_dpi_flush_pure_motion_batch_is_not_treated_as_empty() {
        let (_db, w) = open_writer("motion-pure-batch");
        let sid = w.register_mouse_source(&mouse_descriptor("k1", "鼠标")).unwrap();
        // 纯鼠标运动批
        w.flush(&FlushBatch {
            mouse_motion: vec![MouseMotionWrite {
                source_id: sid,
                day: MOTION_DAY.into(),
                dpi: 0,
                origin: DpiOrigin::Unknown,
                counts: 5.0,
            }],
            ..Default::default()
        })
        .unwrap();
        // 纯摇杆热度批（保持帧）：无 input/apps/mouse_move 也必须落库
        w.flush(&FlushBatch {
            stick_motion: vec![StickMotionWrite {
                device_id: 1,
                day: MOTION_DAY.into(),
                side: StickSide::Left,
                active_us: 250_000,
                travel_r: 0.0,
                bins: vec![StickBinDelta { bin: 312, dwell_us: 250_000 }],
            }],
            ..Default::default()
        })
        .unwrap();
        let conn = w.lock_conn();
        let (mm, gm, gh): (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM mouse_motion_daily),
                        (SELECT COUNT(*) FROM gamepad_motion_daily),
                        (SELECT COUNT(*) FROM gamepad_heat_daily)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((mm, gm, gh), (1, 1, 1), "纯运动批必须落库");
    }

    /// §8-S2 验收点：800/1600/unknown 三桶写入并按桶累加（同键再 flush 翻倍，
    /// 与旧表累加语义一致）；dpi×origin 配对由写入层校验。
    #[test]
    fn motion_dpi_flush_mouse_motion_buckets_accumulate() {
        let (_db, w) = open_writer("motion-mm-accum");
        let sid = w.register_mouse_source(&mouse_descriptor("k1", "鼠标")).unwrap();
        let batch = |extra: f64| FlushBatch {
            mouse_motion: vec![
                MouseMotionWrite {
                    source_id: sid,
                    day: MOTION_DAY.into(),
                    dpi: 800,
                    origin: DpiOrigin::Manual,
                    counts: 800.0 + extra,
                },
                MouseMotionWrite {
                    source_id: sid,
                    day: MOTION_DAY.into(),
                    dpi: 1600,
                    origin: DpiOrigin::Manual,
                    counts: 1600.0,
                },
                MouseMotionWrite {
                    source_id: sid,
                    day: MOTION_DAY.into(),
                    dpi: 0,
                    origin: DpiOrigin::Unknown,
                    counts: 400.0,
                },
            ],
            ..Default::default()
        };
        w.flush(&batch(0.0)).unwrap();
        w.flush(&batch(100.0)).unwrap(); // 同桶再 flush：累加而非新增行
        let conn = w.lock_conn();
        let mut rows: Vec<(i64, String, f64)> = conn
            .prepare(
                "SELECT dpi, dpi_origin, counts FROM mouse_motion_daily
                 WHERE source_id=?1 AND day=?2 ORDER BY dpi",
            )
            .unwrap()
            .query_map(params![sid, MOTION_DAY], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        rows.sort_by_key(|(dpi, _, _)| *dpi);
        assert_eq!(
            rows,
            vec![
                (0, "unknown".to_string(), 800.0), // 400+400 累加
                (800, "manual".to_string(), 1700.0), // 800+900 累加
                (1600, "manual".to_string(), 3200.0),
            ]
        );
    }

    /// §8-S2 验收点：stick motion 行与热度 bins 按键累加；空 bin（dwell_us=0）不写行。
    #[test]
    fn motion_dpi_flush_stick_motion_and_heat_accumulate_skip_empty_bins() {
        let (_db, w) = open_writer("motion-stick-accum");
        // device_id=1 需存在（FK）：注册一个来源即建出第一台设备
        w.register_mouse_source(&mouse_descriptor("k1", "鼠标")).unwrap();
        let stick = |bins: Vec<StickBinDelta>| StickMotionWrite {
            device_id: 1,
            day: MOTION_DAY.into(),
            side: StickSide::Right,
            active_us: 500_000,
            travel_r: 0.25,
            bins,
        };
        w.flush(&FlushBatch {
            stick_motion: vec![stick(vec![
                StickBinDelta { bin: 312, dwell_us: 300_000 },
                StickBinDelta { bin: 313, dwell_us: 200_000 },
                StickBinDelta { bin: 400, dwell_us: 0 }, // 空 bin：不得落行
            ])],
            ..Default::default()
        })
        .unwrap();
        w.flush(&FlushBatch {
            stick_motion: vec![stick(vec![StickBinDelta { bin: 312, dwell_us: 250_000 }])],
            ..Default::default()
        })
        .unwrap();
        let conn = w.lock_conn();
        let (active, travel): (i64, f64) = conn
            .query_row(
                "SELECT active_us, travel_r FROM gamepad_motion_daily
                 WHERE device_id=1 AND day=?1 AND stick='right'",
                params![MOTION_DAY],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((active, travel), (1_000_000, 0.5), "motion 行同键累加");
        let mut bins: Vec<(i64, i64)> = conn
            .prepare(
                "SELECT bin, dwell_us FROM gamepad_heat_daily
                 WHERE device_id=1 AND day=?1 AND stick='right' ORDER BY bin",
            )
            .unwrap()
            .query_map(params![MOTION_DAY], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        bins.sort();
        assert_eq!(bins, vec![(312, 550_000), (313, 200_000)], "dwell_us=0 的 bin 不写行");
    }

    /// §8-S2 验收点（bin/summary 原子）：运动增量与旧统计同事务——任一部分失败全回滚，
    /// 已执行的合法前半（input 桶）同样回滚，绝不部分提交。
    #[test]
    fn motion_dpi_flush_motion_and_legacy_buckets_rollback_together() {
        let (_db, w) = open_writer("motion-atomic");
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
                input: vec![(dev, MOTION_DAY.into(), 0x1E, 5)], // 合法前半
                mouse_motion: vec![MouseMotionWrite {
                    source_id: 999, // 不存在的来源 → FK 失败
                    day: MOTION_DAY.into(),
                    dpi: 800,
                    origin: DpiOrigin::Manual,
                    counts: 10.0,
                }],
                stick_motion: vec![StickMotionWrite {
                    device_id: dev,
                    day: MOTION_DAY.into(),
                    side: StickSide::Left,
                    active_us: 1_000,
                    travel_r: 0.1,
                    bins: vec![StickBinDelta { bin: 0, dwell_us: 1_000 }],
                }],
                ..Default::default()
            })
            .unwrap_err();
        let _ = err; // FK 错误（rusqlite），不强校验具体类型
        let conn = w.lock_conn();
        for table in
            ["input_daily", "mouse_motion_daily", "gamepad_motion_daily", "gamepad_heat_daily"]
        {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "失败批次的 {table} 必须整批回滚");
        }
    }

    /// §6.1"不只依赖 REAL CHECK 拒绝 NaN"：counts/travel_r 非有限非负在 Rust 侧拒绝、
    /// 整批回滚（含批内合法的旧统计行）。
    #[test]
    fn motion_dpi_flush_rejects_nonfinite_and_negative_motion_values() {
        let (_db, w) = open_writer("motion-nonfinite");
        let sid = w.register_mouse_source(&mouse_descriptor("k1", "鼠标")).unwrap();
        let dev = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0,
                pid: 0,
                name: "K".into(),
            })
            .unwrap();
        let mm = |counts: f64| MouseMotionWrite {
            source_id: sid,
            day: MOTION_DAY.into(),
            dpi: 800,
            origin: DpiOrigin::Manual,
            counts,
        };
        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            let err = w
                .flush(&FlushBatch {
                    input: vec![(dev, MOTION_DAY.into(), 0x1E, 3)],
                    mouse_motion: vec![mm(bad)],
                    ..Default::default()
                })
                .unwrap_err();
            assert!(err.to_string().contains("运动计数值非法"), "bad={bad}: {err}");
        }
        // travel_r 负数同样拒绝
        let err = w
            .flush(&FlushBatch {
                stick_motion: vec![StickMotionWrite {
                    device_id: dev,
                    day: MOTION_DAY.into(),
                    side: StickSide::Left,
                    active_us: 1_000,
                    travel_r: -0.5,
                    bins: vec![],
                }],
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("运动计数值非法"), "{err}");
        // 全部失败后库内无残留（合法 input 前半也回滚）
        let conn = w.lock_conn();
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM input_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "校验失败的批次必须整批回滚");
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM mouse_motion_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    /// §8-S2：Writer 三个来源元数据助手——register 复用设备缓存（同型号两来源同 device_id）、
    /// update_mouse_source_state 落库、manual_dpi 批读往返。
    #[test]
    fn motion_dpi_writer_source_helpers_roundtrip() {
        let (_db, w) = open_writer("motion-writer-helpers");
        let d1 = mouse_descriptor("k1", "同型号鼠标");
        let d2 = mouse_descriptor("k2", "同型号鼠标");
        let s1 = w.register_mouse_source(&d1).unwrap();
        let s2 = w.register_mouse_source(&d2).unwrap();
        assert_ne!(s1, s2, "不同路径 = 不同来源");
        let (dev1, dev2): (i64, i64) = {
            let conn = w.lock_conn();
            conn.query_row(
                "SELECT MAX(device_id), MIN(device_id) FROM mouse_motion_sources",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(dev1, dev2, "同型号两来源共用同一 device 行（model id 复用缓存）");

        // update_mouse_source_state：心跳落库（经 list_mouse_sources 验证）
        let st = MouseSourceState {
            descriptor: d1.clone(),
            connection: MotionConnectionId(1),
            connected: true,
            stamp: MotionStamp { mono_us: 0, unix_us: 1_780_272_000_000_000 },
            probe_status: DpiProbeStatus::Available,
            auto_dpi: Some(800),
            auto_valid_until_unix_us: Some(1_780_272_004_000_000),
        };
        w.update_mouse_source_state(s1, &st).unwrap();
        // manual_dpi 批读：set_manual_dpi 走 free function（GUI 例外路径，S6 接线）
        {
            let conn = w.lock_conn();
            crate::motion::set_manual_dpi(&conn, s2, Some(1600)).unwrap();
        }
        let rows = w.manual_dpi(&["k1".to_string(), "k2".to_string(), "ghost".to_string()]).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], MouseConfigRow { source_key: "k1".into(), manual_dpi: None });
        assert_eq!(rows[1], MouseConfigRow { source_key: "k2".into(), manual_dpi: Some(1600) });

        // 状态已落库：connected 证据 + auto 值可经 list 读出（last_seen=stamp 时刻，
        // 以 stamp 对应的 unix µs 作为 now 校验新鲜窗口内）
        let rows = {
            let conn = w.lock_conn();
            crate::motion::list_mouse_sources(&conn, 1_780_272_000_000_000).unwrap()
        };
        let r1 = rows.iter().find(|r| r.id == s1).unwrap();
        assert!(r1.connected);
        assert_eq!(r1.auto_dpi, Some(800));
        assert_eq!(r1.effective_dpi, Some(800));
    }
}
