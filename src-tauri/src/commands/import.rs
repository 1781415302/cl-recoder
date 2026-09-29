//! import —— WhatPulse 导入（PLAN §4.8/§5.4；**wp_* 的唯一写入处**，§2.5）。
//!
//! 流程（§5.4 逐字）：选文件 → **复制到 `%TEMP%\clrecoder-wp-{ts}.db`** → 只读打开副本 →
//! 按 §4.8 逐表校验 + Rust 侧聚合（缺表→warning，继续）→ 调
//! `store::writer::rebuild_wp_tables_with` 单事务整体重建（GUI 临时 rw 连接 busy_timeout=10s，
//! wp_* 的 SQL 唯一归属 store）→ 删临时文件 → 返回 `ImportReport`。
//! **整体替换语义**：重复导入即刷新为最新快照。
//!
//! 铁律（§1 原则 6 / §7-1）：WhatPulse 库只读、先复制后打开、绝不写 WhatPulse 的任何文件。

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::db;
use clrecoder_core::{day, qtkeys};
use clrecoder_store::reader::{
    WpAppDailyRow, WpComboDailyRow, WpKeyDailyRow, WpMetaRow, WpMouseButtonDailyRow,
    WpMouseDailyRow, WpMouseScrollDailyRow,
};
use clrecoder_store::writer::WpImportBatch;

/// 导入报告（§4.7 `ImportReport`，camelCase）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// 是否成功（失败原因在 warnings[0]）
    pub ok: bool,
    /// wp_key_daily 行数
    pub keys: u64,
    /// wp_combo_daily 行数
    pub combos: u64,
    /// wp_app_daily 行数
    pub apps: u64,
    /// wp_mouse_daily 行数（= 有鼠标数据的日期数）
    pub mouse_days: u64,
    /// 源数据最早日期（无数据为 null）
    pub date_min: Option<String>,
    /// 源数据最晚日期（无数据为 null）
    pub date_max: Option<String>,
    /// 警告（缺表/回退/推断语义等）
    pub warnings: Vec<String>,
    /// 耗时（毫秒）
    pub duration_ms: u64,
}

/// §4.8 源表清单（缺表 → warning + 跳过该类数据，§4.8/§5.4）。
const SOURCE_TABLES: [&str; 9] = [
    "keypress_frequency",
    "keycombo_frequency",
    "input_per_application",
    "application_active_hour",
    "applications",
    "mouseclicks",
    "mousedistance",
    "mouseclicks_frequency",
    "mousescrolls",
];

/// WhatPulse 按钮码 → 显示名（§4.8 静态表；其余原码直显，§9.3）。
fn mouse_button_label(code: i64) -> String {
    match code {
        0 => "左键".into(),
        1 => "中键".into(),
        2 => "右键".into(),
        99 => "其他".into(),
        other => format!("按钮 {other}"),
    }
}

/// WhatPulse 滚轮方向码 → 显示名（§4.8 推断值，warnings 注明）。
fn mouse_scroll_label(code: i64) -> String {
    match code {
        1 => "向上".into(),
        2 => "向下".into(),
        3 => "向左".into(),
        4 => "向右".into(),
        other => format!("方向 {other}"),
    }
}

/// 组合键原文 → 友好标签（§4.8：格式实测 `"shift,87"` / `"control,65"`，
/// 多修饰为逗号分隔修饰名 + Qt 码；解析修饰名 shift→Shift / control→Ctrl / alt→Alt /
/// meta|win→Win + `qt_key_label`）。解析失败（末段非 Qt 码）→ 保留原文。
fn parse_combo_label(combo: &str) -> String {
    let segs: Vec<&str> = combo.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return combo.to_string();
    }
    let (mod_segs, code_seg) = segs.split_at(segs.len() - 1);
    match code_seg[0].parse::<i64>() {
        Ok(code) => {
            let mut parts: Vec<String> = mod_segs
                .iter()
                .map(|m| match m.to_ascii_lowercase().as_str() {
                    "shift" => "Shift".to_string(),
                    "control" | "ctrl" => "Ctrl".to_string(),
                    "alt" => "Alt".to_string(),
                    "meta" | "win" => "Win".to_string(),
                    other => other.to_string(),
                })
                .collect();
            parts.push(qtkeys::qt_key_label(code));
            parts.join("+")
        }
        Err(_) => combo.to_string(),
    }
}

/// path → basename（源 path 实测小写+正斜杠；兜底两种分隔符）。
fn basename(path: &str) -> String {
    let p = path.trim_end_matches(['/', '\\']);
    match p.rsplit(['/', '\\']).next() {
        Some(base) if !base.is_empty() => base.to_string(),
        _ => path.to_string(),
    }
}

/// 源库是否含指定表。
fn table_exists(conn: &Connection, name: &str) -> Result<bool, rusqlite::Error> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 缺表警告文案（测试断言用）。
fn missing_table_warning(name: &str) -> String {
    format!("源库缺少表 {name}，已跳过该类数据")
}

/// 聚合 keypress_frequency → wp_key_daily（§4.8：GROUP BY day,key；跨 profile_id 直接求和；
/// label=`qt_key_label(key)`；count=SUM(count)）。
fn agg_keys(conn: &Connection) -> Result<Vec<WpKeyDailyRow>, rusqlite::Error> {
    if !table_exists(conn, "keypress_frequency")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT \"day\", \"key\", COALESCE(SUM(\"count\"), 0) FROM keypress_frequency \
         GROUP BY \"day\", \"key\" ORDER BY \"day\", \"key\"",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(WpKeyDailyRow {
            day: r.get::<_, String>(0)?,
            qt_key: r.get::<_, i64>(1)?,
            label: qtkeys::qt_key_label(r.get::<_, i64>(1)?),
            count: u64::try_from(r.get::<_, i64>(2)?.max(0)).unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// 聚合 keycombo_frequency → wp_combo_daily（§4.8：GROUP BY day,combo；label 解析修饰名）。
fn agg_combos(conn: &Connection) -> Result<Vec<WpComboDailyRow>, rusqlite::Error> {
    if !table_exists(conn, "keycombo_frequency")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT \"day\", \"combo\", COALESCE(SUM(\"count\"), 0) FROM keycombo_frequency \
         GROUP BY \"day\", \"combo\" ORDER BY \"day\", \"combo\"",
    )?;
    let rows = stmt.query_map([], |r| {
        let combo: String = r.get(1)?;
        Ok(WpComboDailyRow {
            day: r.get::<_, String>(0)?,
            label: parse_combo_label(&combo),
            combo,
            count: u64::try_from(r.get::<_, i64>(2)?.max(0)).unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// 聚合应用两表 → wp_app_daily（§4.8：input_per_application + application_active_hour
/// 按 (day,path) 外连接——在 Rust 侧合并；name 按 path 匹配 applications.name，匹配不到
/// 用 basename；seconds=ROUND(SUM(msec_active)/1000.0); keys/clicks=SUM）。
fn agg_apps(
    conn: &Connection,
    warnings: &mut Vec<String>,
) -> Result<Vec<WpAppDailyRow>, rusqlite::Error> {
    // (day, path) → 聚合桶（两表各自守卫：缺其一时另一表照常聚合，§1 原则 3）
    let mut acc: HashMap<(String, String), (u64, u64, f64)> = HashMap::new();
    if table_exists(conn, "input_per_application")? {
        let mut stmt = conn.prepare(
            "SELECT \"day\", \"path\", COALESCE(SUM(\"keys\"), 0), COALESCE(SUM(\"clicks\"), 0) \
             FROM input_per_application GROUP BY \"day\", \"path\"",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                u64::try_from(r.get::<_, i64>(2)?.max(0)).unwrap_or(0),
                u64::try_from(r.get::<_, i64>(3)?.max(0)).unwrap_or(0),
            ))
        })?;
        for row in rows {
            let (day, path, keys, clicks) = row?;
            let e = acc.entry((day, path)).or_insert((0, 0, 0.0));
            e.0 += keys;
            e.1 += clicks;
        }
    }
    if table_exists(conn, "application_active_hour")? {
        let mut stmt = conn.prepare(
            "SELECT \"day\", \"path\", COALESCE(SUM(\"msec_active\"), 0) \
             FROM application_active_hour GROUP BY \"day\", \"path\"",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?))
        })?;
        for row in rows {
            let (day, path, msec) = row?;
            let e = acc.entry((day, path)).or_insert((0, 0, 0.0));
            e.2 += msec.max(0.0);
        }
    }
    // applications 名称匹配（表不可用 → 警告 + basename 回退，§4.8）
    let names: HashMap<String, String> = match load_applications(conn) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(format!(
                "applications 表不可用（{e}），应用名回退为文件名"
            ));
            HashMap::new()
        }
    };
    let mut out: Vec<WpAppDailyRow> = acc
        .into_iter()
        .map(|((day, path), (keys, clicks, msec))| {
            let seconds = (msec / 1000.0).round().max(0.0) as u64; // §4.8：ROUND(SUM/1000.0)
            let name = names
                .get(&path)
                .cloned()
                .unwrap_or_else(|| basename(&path));
            WpAppDailyRow { day, path, name, seconds, keys, clicks }
        })
        .collect();
    out.sort_by(|a, b| a.day.cmp(&b.day).then_with(|| a.path.cmp(&b.path)));
    Ok(out)
}

/// applications 表 → (path → name) 映射（§4.8 "按 path 匹配 applications.name"）。
fn load_applications(conn: &Connection) -> Result<HashMap<String, String>, rusqlite::Error> {
    if !table_exists(conn, "applications")? {
        return Ok(HashMap::new());
    }
    let mut stmt = conn.prepare("SELECT \"path\", \"name\" FROM applications")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut m = HashMap::new();
    for row in rows {
        let (path, name) = row?;
        if !name.is_empty() {
            m.insert(path, name);
        }
    }
    Ok(m)
}

/// 聚合 mouseclicks + mousedistance → wp_mouse_daily（§4.8：clicks=SUM; distance_inches=SUM）。
fn agg_mouse(conn: &Connection) -> Result<Vec<WpMouseDailyRow>, rusqlite::Error> {
    let mut acc: HashMap<String, (u64, f64)> = HashMap::new();
    if table_exists(conn, "mouseclicks")? {
        let mut stmt = conn
            .prepare("SELECT \"day\", COALESCE(SUM(\"count\"), 0) FROM mouseclicks GROUP BY \"day\"")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                u64::try_from(r.get::<_, i64>(1)?.max(0)).unwrap_or(0),
            ))
        })?;
        for row in rows {
            let (day, clicks) = row?;
            acc.entry(day).or_insert((0, 0.0)).0 += clicks;
        }
    }
    if table_exists(conn, "mousedistance")? {
        let mut stmt = conn.prepare(
            "SELECT \"day\", COALESCE(SUM(\"distance_inches\"), 0.0) FROM mousedistance GROUP BY \"day\"",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
        for row in rows {
            let (day, inches) = row?;
            acc.entry(day).or_insert((0, 0.0)).1 += inches.max(0.0);
        }
    }
    let mut out: Vec<WpMouseDailyRow> = acc
        .into_iter()
        .map(|(day, (clicks, distance_inches))| WpMouseDailyRow { day, clicks, distance_inches })
        .collect();
    out.sort_by(|a, b| a.day.cmp(&b.day));
    Ok(out)
}

/// 聚合 mouseclicks_frequency → wp_mouse_buttons_daily（§4.8：GROUP BY day,button + 静态标签表）。
fn agg_mouse_buttons(conn: &Connection) -> Result<Vec<WpMouseButtonDailyRow>, rusqlite::Error> {
    if !table_exists(conn, "mouseclicks_frequency")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT \"day\", \"button\", COALESCE(SUM(\"count\"), 0) FROM mouseclicks_frequency \
         GROUP BY \"day\", \"button\" ORDER BY \"day\", \"button\"",
    )?;
    let rows = stmt.query_map([], |r| {
        let button: i64 = r.get(1)?;
        Ok(WpMouseButtonDailyRow {
            day: r.get::<_, String>(0)?,
            label: mouse_button_label(button),
            button_code: button,
            count: u64::try_from(r.get::<_, i64>(2)?.max(0)).unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// 聚合 mousescrolls → wp_mouse_scroll_daily（§4.8：GROUP BY day,direction + 推断标签）。
fn agg_mouse_scrolls(conn: &Connection) -> Result<Vec<WpMouseScrollDailyRow>, rusqlite::Error> {
    if !table_exists(conn, "mousescrolls")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT \"day\", \"direction\", COALESCE(SUM(\"count\"), 0) FROM mousescrolls \
         GROUP BY \"day\", \"direction\" ORDER BY \"day\", \"direction\"",
    )?;
    let rows = stmt.query_map([], |r| {
        let direction: i64 = r.get(1)?;
        Ok(WpMouseScrollDailyRow {
            day: r.get::<_, String>(0)?,
            label: mouse_scroll_label(direction),
            direction_code: direction,
            count: u64::try_from(r.get::<_, i64>(2)?.max(0)).unwrap_or(0),
        })
    })?;
    rows.collect()
}

/// 汇总各表行集的日期范围（§4.7 ImportReport.dateMin/dateMax）。
fn date_range<'a>(rows: impl IntoIterator<Item = &'a str>) -> (Option<String>, Option<String>) {
    let mut min: Option<String> = None;
    let mut max: Option<String> = None;
    for d in rows {
        let owned = d.to_string();
        min = Some(match min {
            Some(m) if m.as_str() <= d => m,
            _ => owned.clone(),
        });
        max = Some(match max {
            Some(m) if m.as_str() >= d => m,
            _ => owned,
        });
    }
    (min, max)
}

/// 失败报告（耗时照实、ok=false、错误进 warnings[0]）。
fn failed_report(started: Instant, mut warnings: Vec<String>, msg: String) -> ImportReport {
    warnings.insert(0, msg);
    ImportReport {
        ok: false,
        warnings,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        ..Default::default()
    }
}

/// 导入主入口（command 与单测共用）：`source` = WhatPulse 库，`stats_db` = 本软件统计库。
pub fn run_import(source: &Path, stats_db: &Path) -> ImportReport {
    let started = Instant::now();
    let mut warnings: Vec<String> = Vec::new();

    // 0. 源文件在场
    if !source.is_file() {
        return failed_report(started, warnings, format!("源文件不存在: {}", source.display()));
    }
    let source_size = std::fs::metadata(source).map(|m| m.len() as i64).ok();

    // 1. 复制到临时（§5.4：先复制后打开，绝不写 WhatPulse 目录）
    let tmp = std::env::temp_dir().join(format!(
        "clrecoder-wp-{}.db",
        chrono::Local::now().timestamp_millis()
    ));
    let _ = std::fs::remove_file(&tmp); // 清理可能残留的同名旧文件
    if let Err(e) = std::fs::copy(source, &tmp) {
        return failed_report(started, warnings, format!("复制源库失败: {e}"));
    }

    // 2. 只读打开副本 + 逐表校验/聚合（单表失败降级为 warning，§1 原则 3 绝不 crash）
    let result = import_from_copy(&tmp, source, source_size, stats_db, &mut warnings);
    let _ = std::fs::remove_file(&tmp); // 无论成败都删临时文件（§5.4）

    match result {
        Ok((batch, keys, combos, apps, mouse_days, dmin, dmax)) => {
            if keys == 0
                && combos == 0
                && apps == 0
                && mouse_days == 0
                && batch.mouse_buttons.is_empty()
                && batch.mouse_scrolls.is_empty()
            {
                warnings.push("源库未发现任何数据行".to_string());
            }
            ImportReport {
                ok: true,
                keys: keys as u64,
                combos: combos as u64,
                apps: apps as u64,
                mouse_days: mouse_days as u64,
                date_min: dmin,
                date_max: dmax,
                warnings,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            }
        }
        Err(msg) => failed_report(started, warnings, msg),
    }
}

/// 副本聚合 + 单事务重建（返回批次与报告数字；Err = 硬失败文案）。
#[allow(clippy::type_complexity)]
fn import_from_copy(
    tmp: &Path,
    source: &Path,
    source_size: Option<i64>,
    stats_db: &Path,
    warnings: &mut Vec<String>,
) -> Result<
    (WpImportBatch, usize, usize, usize, usize, Option<String>, Option<String>),
    String,
> {
    let conn = Connection::open_with_flags(
        tmp,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("打开副本失败: {e}"))?;

    // 逐表存在性校验（§4.8：缺表跳过并写入 warnings）
    let missing: Vec<&str> = SOURCE_TABLES
        .into_iter()
        .filter(|t| match table_exists(&conn, t) {
            Ok(true) => false,
            Ok(false) => true,
            Err(e) => {
                warnings.push(format!("检查表 {t} 失败: {e}"));
                true // 保守处理：检查失败按缺表跳过（防御性，§1 原则 3）
            }
        })
        .collect();
    for t in &missing {
        warnings.push(missing_table_warning(t));
    }

    // Rust 侧聚合（§4.8 六张目标表）；单表 SQL 失败 → warning + 跳过（防御性）。
    // warnings 只从 run_step 的闭包参数流入，避免同一语句对 warnings 的双重借用。
    let keys = run_step(warnings, "keypress_frequency", |_w| agg_keys(&conn));
    let combos = run_step(warnings, "keycombo_frequency", |_w| agg_combos(&conn));
    let apps = run_step(warnings, "input_per_application/application_active_hour", |w| {
        agg_apps(&conn, w)
    });
    let mouse = run_step(warnings, "mouseclicks/mousedistance", |_w| agg_mouse(&conn));
    let buttons = run_step(warnings, "mouseclicks_frequency", |_w| agg_mouse_buttons(&conn));
    let scrolls = run_step(warnings, "mousescrolls", |_w| agg_mouse_scrolls(&conn));
    if !scrolls.is_empty() {
        // §4.8/§9.3：滚轮方向码语义为推断值，必须在 warnings 声明
        warnings.push("滚轮方向码 1..4 的语义为推断值（WhatPulse 内部编码无公开文档）".into());
    }

    // 日期范围（全部行集的 day 并集）
    let all_days = keys
        .iter()
        .map(|r| r.day.as_str())
        .chain(combos.iter().map(|r| r.day.as_str()))
        .chain(apps.iter().map(|r| r.day.as_str()))
        .chain(mouse.iter().map(|r| r.day.as_str()))
        .chain(buttons.iter().map(|r| r.day.as_str()))
        .chain(scrolls.iter().map(|r| r.day.as_str()));
    let (date_min, date_max) = date_range(all_days);

    // 单事务整体重建（wp_* 的 SQL 唯一归属 store；GUI 临时 rw 连接 busy_timeout=10s，§5.4）
    let stats = db::open_rw(stats_db).map_err(|e| format!("打开统计库失败: {e}"))?;
    clrecoder_store::schema::migrate(&stats)
        .map_err(|e| format!("统计库迁移失败: {e}"))?;
    let batch = WpImportBatch {
        meta: WpMetaRow {
            imported_at: day::now_local_rfc3339(),
            source_path: source.to_string_lossy().into_owned(),
            source_size,
            date_min: date_min.clone(),
            date_max: date_max.clone(),
            note: if warnings.is_empty() { String::new() } else { warnings.join("；") },
        },
        keys,
        combos,
        apps,
        mouse,
        mouse_buttons: buttons,
        mouse_scrolls: scrolls,
    };
    clrecoder_store::writer::rebuild_wp_tables_with(&stats, &batch)
        .map_err(|e| format!("重建 wp_* 表失败: {e}"))?;

    let n = (
        batch.keys.len(),
        batch.combos.len(),
        batch.apps.len(),
        batch.mouse.len(),
    );
    Ok((batch, n.0, n.1, n.2, n.3, date_min, date_max))
}

/// 单表聚合的防御包装：SQL 失败 → warning + 空结果（绝不 crash、绝不丢其他表，§1 原则 3）。
/// 闭包需要写 warnings 时（如 agg_apps 的 applications 回退警告）经参数 `w` 拿到。
fn run_step<T>(
    warnings: &mut Vec<String>,
    what: &str,
    f: impl FnOnce(&mut Vec<String>) -> Result<T, rusqlite::Error>,
) -> T
where
    T: Default,
{
    match f(warnings) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("表 {what} 聚合失败，已跳过: {e}"));
            T::default()
        }
    }
}

/// WhatPulse 导入（§4.7 `import_whatpulse(path: String) -> ImportReport`）。
/// 失败不返回 Err——统一以 `ok=false` + warnings 表达（TS 契约有 ok 字段）。
#[tauri::command]
pub async fn import_whatpulse(path: String) -> Result<ImportReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let report = run_import(Path::new(&path), &db::stats_db_path());
        crate::gui_log!(
            "INFO: WhatPulse 导入完成: ok={} keys={} 耗时 {}ms",
            report.ok,
            report.keys,
            report.duration_ms
        );
        report
    })
    .await
    .map_err(|e| format!("导入任务失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_core::qtkeys::qt_key_label;

    const D1: &str = "2026-05-01";
    const D2: &str = "2025-12-31"; // 故意比 D1 早：验证 dateMin 取并集最小值

    /// 按 §4.8 源表 schema 造合成 fixture 库（列名与 §4.8"对真实库已验证的列"一致）。
    /// 含两个 profile_id 的行——验证"跨 profile_id 直接求和"。
    fn make_fixture(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE keypress_frequency(profile_id INTEGER, day TEXT, hour INTEGER, key INTEGER, count INTEGER);
             CREATE TABLE keycombo_frequency(profile_id INTEGER, day TEXT, hour INTEGER, combo TEXT, count INTEGER);
             CREATE TABLE input_per_application(profile_id INTEGER, day TEXT, hour INTEGER, path TEXT, keys INTEGER, clicks INTEGER);
             CREATE TABLE application_active_hour(profile_id INTEGER, day TEXT, hour INTEGER, path TEXT, msec_active INTEGER);
             CREATE TABLE applications(path TEXT, name TEXT);
             CREATE TABLE mouseclicks(profile_id INTEGER, day TEXT, hour INTEGER, count INTEGER);
             CREATE TABLE mousedistance(profile_id INTEGER, day TEXT, hour INTEGER, distance_inches REAL);
             CREATE TABLE mouseclicks_frequency(profile_id INTEGER, day TEXT, hour INTEGER, button INTEGER, count INTEGER);
             CREATE TABLE mousescrolls(profile_id INTEGER, day TEXT, hour INTEGER, direction INTEGER, count INTEGER);",
        )
        .unwrap();
        // 键：跨 profile、跨 hour 直接求和
        // profile 1：A(D1,0h,10)+A(D1,1h,5)；profile 2：A(D2,1h,2)、B(D1,2h,7)、Escape(D2,0h,3)
        // SUM = 10+5+2+7+3 = 27（§5.4 校验锚点）
        conn.execute_batch(&format!(
            "INSERT INTO keypress_frequency VALUES (1,'{D1}',0,65,10),(1,'{D1}',1,65,5),\
             (2,'{D2}',1,65,2),(2,'{D1}',2,66,7),(1,'{D2}',0,16777216,3);"
        ))
        .unwrap();
        // 组合键：单修饰、多修饰
        conn.execute_batch(&format!(
            "INSERT INTO keycombo_frequency VALUES (1,'{D1}',0,'shift,87',4),(1,'{D1}',1,'control,65',2),\
             (2,'{D2}',0,'control,shift,90',1);"
        ))
        .unwrap();
        // 应用：三路径覆盖外连接三象限（双边/仅活跃时长/仅输入）
        conn.execute_batch(&format!(
            "INSERT INTO input_per_application VALUES (1,'{D1}',0,'c:/app/editor.exe',30,2),\
             (1,'{D2}',0,'c:/app/only.exe',1,1);\
             INSERT INTO application_active_hour VALUES (1,'{D1}',0,'c:/app/editor.exe',1500),\
             (1,'{D1}',1,'c:/app/editor.exe',500),(1,'{D2}',0,'c:/app/game.exe',3600000);\
             INSERT INTO applications VALUES ('c:/app/editor.exe','Editor');"
        ))
        .unwrap();
        // 鼠标逐日
        conn.execute_batch(&format!(
            "INSERT INTO mouseclicks VALUES (1,'{D1}',0,100),(1,'{D1}',1,50),(1,'{D2}',0,25);\
             INSERT INTO mousedistance VALUES (1,'{D1}',0,10.5),(1,'{D2}',0,4.25);"
        ))
        .unwrap();
        // 按钮：静态表 0/2/99 + 未知码 300
        conn.execute_batch(&format!(
            "INSERT INTO mouseclicks_frequency VALUES (1,'{D1}',0,0,70),(1,'{D1}',1,2,30),\
             (1,'{D2}',0,99,5),(1,'{D2}',1,300,2);"
        ))
        .unwrap();
        // 滚轮：1/2/3 三方向
        conn.execute_batch(&format!(
            "INSERT INTO mousescrolls VALUES (1,'{D1}',0,1,12),(1,'{D1}',1,2,8),(1,'{D2}',0,3,2);"
        ))
        .unwrap();
        conn.pragma_update(None, "journal_mode", "DELETE").unwrap(); // 单文件便于复制
    }

    /// **§5.4 主断言**：对合成 fixture 库导入后 `wp_key_daily` 总数 == 源 SUM(count)。
    /// 另覆盖六张目标表的映射细节与 ImportReport 字段。
    #[test]
    fn import_synthetic_fixture_wp_key_total_equals_source_sum() {
        let src = TempFile::new("imp-src", "db");
        make_fixture(&src);
        let stats = TempFile::new("imp-stats", "db");

        let report = run_import(src.as_ref(), stats.as_ref());
        assert!(report.ok, "导入必须成功: {:?}", report.warnings);
        assert!(report.warnings.iter().any(|w| w.contains("推断")), "滚轮推断语义须进 warnings");

        // —— 校验锚点（§5.4）：wp_key_daily 总数 == 源 SUM(count) ——
        let src_conn = Connection::open_with_flags(
            src.as_ref(),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let source_sum: i64 = src_conn
            .query_row("SELECT COALESCE(SUM(\"count\"),0) FROM keypress_frequency", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source_sum, 27, "fixture 源 SUM 应为 27");
        let stats_conn = Connection::open(stats.as_ref()).unwrap();
        let wp_sum: i64 = stats_conn
            .query_row("SELECT COALESCE(SUM(count),0) FROM wp_key_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(wp_sum, source_sum, "wp_key_daily 总数必须等于源 SUM（§5.4 校验锚点）");

        // —— 行数与行内容 ——
        let n: i64 = stats_conn.query_row("SELECT COUNT(*) FROM wp_key_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 4, "A-D1 / A-D2 / B-D1 / Escape-D2 四行（跨 profile、跨 hour 已合并）");
        let rows: Vec<(String, i64, String, i64)> = {
            let mut s = stats_conn
                .prepare("SELECT day, qt_key, label, count FROM wp_key_daily ORDER BY day, qt_key")
                .unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            rows,
            vec![
                (D2.into(), 65, "A".into(), 2), // D2 早于 D1（ORDER BY day 升序）
                (D2.into(), 0x01000000, qt_key_label(0x01000000), 3), // Escape
                (D1.into(), 65, "A".into(), 15), // 跨 profile 2 行 + 跨 hour 合并
                (D1.into(), 66, "B".into(), 7),
            ]
        );

        // 组合键标签（§4.8 解析规则）
        let combos: Vec<(String, String, i64)> = {
            let mut s = stats_conn
                .prepare("SELECT combo, label, count FROM wp_combo_daily ORDER BY combo")
                .unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            combos,
            vec![
                ("control,65".into(), "Ctrl+A".into(), 2),
                ("control,shift,90".into(), "Ctrl+Shift+Z".into(), 1),
                ("shift,87".into(), "Shift+W".into(), 4),
            ]
        );

        // 应用：外连接三象限 + ROUND 秒数 + 名称匹配/basename 回退
        let apps: Vec<(String, String, i64, i64, i64)> = {
            let mut s = stats_conn
                .prepare("SELECT path, name, seconds, keys, clicks FROM wp_app_daily ORDER BY path")
                .unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            apps,
            vec![
                ("c:/app/editor.exe".into(), "Editor".into(), 2, 30, 2), // ROUND(2000/1000)=2
                ("c:/app/game.exe".into(), "game.exe".into(), 3600, 0, 0), // 仅活跃时长；basename 回退
                ("c:/app/only.exe".into(), "only.exe".into(), 0, 1, 1),    // 仅输入
            ]
        );

        // 鼠标逐日：SUM 合并
        let mouse: Vec<(String, i64, f64)> = {
            let mut s = stats_conn
                .prepare("SELECT day, clicks, distance_inches FROM wp_mouse_daily ORDER BY day")
                .unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(mouse, vec![(D2.into(), 25, 4.25), (D1.into(), 150, 10.5)]);

        // 按钮/滚轮静态标签
        let buttons: Vec<(i64, String, i64)> = {
            let mut s = stats_conn
                .prepare("SELECT button_code, label, count FROM wp_mouse_buttons_daily ORDER BY button_code")
                .unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            buttons,
            vec![(0, "左键".into(), 70), (2, "右键".into(), 30), (99, "其他".into(), 5), (300, "按钮 300".into(), 2)]
        );
        let scrolls: Vec<(i64, String, i64)> = {
            let mut s = stats_conn
                .prepare("SELECT direction_code, label, count FROM wp_mouse_scroll_daily ORDER BY direction_code")
                .unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(scrolls, vec![(1, "向上".into(), 12), (2, "向下".into(), 8), (3, "向左".into(), 2)]);

        // —— ImportReport 字段（§4.7：行数语义）——
        assert_eq!(report.keys, 4);
        assert_eq!(report.combos, 3);
        assert_eq!(report.apps, 3);
        assert_eq!(report.mouse_days, 2);
        assert_eq!(report.date_min.as_deref(), Some(D2), "dateMin 取全部行集并集最小");
        assert_eq!(report.date_max.as_deref(), Some(D1));
        assert!(report.duration_ms < 60_000);
        assert_eq!(report.warnings.len(), 1, "齐全 fixture 只应有滚轮推断一条警告: {:?}", report.warnings);

        // meta 行
        let (src_path, size, dmin, dmax): (String, i64, String, String) = stats_conn
            .query_row(
                "SELECT source_path, source_size, date_min, date_max FROM wp_import_meta WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(src_path, src.as_ref().to_string_lossy());
        assert_eq!(size, std::fs::metadata(src.as_ref()).unwrap().len() as i64);
        assert_eq!((dmin.as_str(), dmax.as_str()), (D2, D1));

        // 自有表未被导入触碰（GUI 只写 wp_*，§2.2）
        let devices: i64 = stats_conn.query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0)).unwrap();
        assert_eq!(devices, 0);
    }

    /// 整体替换语义（§4.8）：重复导入刷新为最新快照，计数不翻倍。
    #[test]
    fn import_twice_replaces_not_doubles() {
        let src = TempFile::new("imp-twice-src", "db");
        make_fixture(&src);
        let stats = TempFile::new("imp-twice-stats", "db");
        let r1 = run_import(src.as_ref(), stats.as_ref());
        assert!(r1.ok);
        let r2 = run_import(src.as_ref(), stats.as_ref());
        assert!(r2.ok);
        let conn = Connection::open(stats.as_ref()).unwrap();
        let sum: i64 = conn
            .query_row("SELECT COALESCE(SUM(count),0) FROM wp_key_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sum, 27, "重复导入必须整体替换，不得翻倍");
        let meta_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM wp_import_meta", [], |r| r.get(0)).unwrap();
        assert_eq!(meta_rows, 1);
        assert_eq!(r2.keys, r1.keys);
    }

    /// 缺表容错（§4.8：缺表跳过并写入 warnings，导入继续成功）。
    #[test]
    fn import_missing_tables_produce_warnings_and_partial_data() {
        let src = TempFile::new("imp-miss-src", "db");
        let conn = Connection::open(&src).unwrap();
        conn.execute_batch(
            "CREATE TABLE keypress_frequency(profile_id INTEGER, day TEXT, hour INTEGER, key INTEGER, count INTEGER);
             CREATE TABLE mouseclicks(profile_id INTEGER, day TEXT, hour INTEGER, count INTEGER);
             INSERT INTO keypress_frequency VALUES (1,'2026-05-01',0,65,9);
             INSERT INTO mouseclicks VALUES (1,'2026-05-01',0,4);",
        )
        .unwrap();
        let stats = TempFile::new("imp-miss-stats", "db");
        let report = run_import(src.as_ref(), stats.as_ref());
        assert!(report.ok, "缺表必须仍成功: {:?}", report.warnings);
        for t in [
            "keycombo_frequency",
            "input_per_application",
            "application_active_hour",
            "applications",
            "mousedistance",
            "mouseclicks_frequency",
            "mousescrolls",
        ] {
            assert!(
                report.warnings.iter().any(|w| w.contains(t)),
                "warnings 必须提到缺表 {t}: {:?}",
                report.warnings
            );
        }
        let conn = Connection::open(stats.as_ref()).unwrap();
        let keys: i64 =
            conn.query_row("SELECT COALESCE(SUM(count),0) FROM wp_key_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(keys, 9);
        let clicks: i64 =
            conn.query_row("SELECT COALESCE(SUM(clicks),0) FROM wp_mouse_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(clicks, 4);
        let combos: i64 =
            conn.query_row("SELECT COUNT(*) FROM wp_combo_daily", [], |r| r.get(0)).unwrap();
        assert_eq!(combos, 0);
        assert_eq!(report.keys, 1);
        assert_eq!(report.mouse_days, 1);
    }

    /// 源文件不存在 → ok=false 报告（不 panic、不返回 Err）。
    #[test]
    fn import_missing_source_file_reports_failure() {
        let stats = TempFile::new("imp-nosrc-stats", "db");
        let report = run_import(Path::new("Z:/definitely/not/here.db"), stats.as_ref());
        assert!(!report.ok);
        assert!(report.warnings[0].contains("源文件不存在"));
    }

    /// 真实库测试（§8-S10）：`%LOCALAPPDATA%\WhatPulse\whatpulse.db` 在场时执行
    /// §5.4 校验锚点；**文件缺失时 skip**。全程只读（源文件字节数导入前后必须一致）。
    #[test]
    fn import_real_whatpulse_db_when_present() {
        let Some(local) = dirs::data_local_dir() else {
            eprintln!("skip: 无法解析 %LOCALAPPDATA%");
            return;
        };
        let src = local.join("WhatPulse").join("whatpulse.db");
        if !src.is_file() {
            eprintln!("skip: 真实 WhatPulse 库不存在（{}）——按 §8-S10 跳过", src.display());
            return;
        }
        let size_before = std::fs::metadata(&src).unwrap().len();

        // 先确认锚点表存在（不存在则本机环境不适用，skip）
        {
            let ro = Connection::open_with_flags(&src, OpenFlags::SQLITE_OPEN_READ_ONLY);
            let Ok(ro) = ro else { eprintln!("skip: 真实库只读打开失败"); return };
            match table_exists(&ro, "keypress_frequency") {
                Ok(true) => {}
                _ => { eprintln!("skip: 真实库无 keypress_frequency 表"); return; }
            }
        }

        let stats = TempFile::new("imp-real-stats", "db");
        let report = run_import(&src, stats.as_ref());
        assert!(report.ok, "真实库导入必须成功: {:?}", report.warnings);

        // §5.4 校验锚点
        let ro = Connection::open_with_flags(&src, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let source_sum: i64 = ro
            .query_row("SELECT COALESCE(SUM(\"count\"),0) FROM keypress_frequency", [], |r| r.get(0))
            .unwrap();
        let stats_conn = Connection::open_with_flags(stats.as_ref(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        let wp_sum: i64 = stats_conn
            .query_row("SELECT COALESCE(SUM(count),0) FROM wp_key_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(wp_sum, source_sum, "真实库 wp_key_daily 总数必须等于源 SUM（§5.4）");
        // 只读铁律：源文件不得被触碰
        assert_eq!(std::fs::metadata(&src).unwrap().len(), size_before);
    }

    /// 纯函数：组合键标签解析（§4.8 全部分支）。
    #[test]
    fn parse_combo_label_rules() {
        assert_eq!(parse_combo_label("shift,87"), "Shift+W");
        assert_eq!(parse_combo_label("control,65"), "Ctrl+A");
        assert_eq!(parse_combo_label("control,shift,90"), "Ctrl+Shift+Z");
        assert_eq!(parse_combo_label("alt,65"), "Alt+A");
        // 特殊 Qt 码：16777223 = 0x01000007 = Delete
        assert_eq!(parse_combo_label("control,16777223"), "Ctrl+Delete");
        assert_eq!(parse_combo_label("meta,65"), "Win+A");
        assert_eq!(parse_combo_label("win,65"), "Win+A");
        assert_eq!(parse_combo_label("65"), "A", "无修饰的 Qt 码");
        assert_eq!(parse_combo_label("shift,not-a-code"), "shift,not-a-code", "解析失败保留原文");
        assert_eq!(parse_combo_label(""), "", "空原文");
    }

    /// 纯函数：basename 与日期范围。
    #[test]
    fn basename_and_date_range() {
        assert_eq!(basename("c:/app/editor.exe"), "editor.exe");
        assert_eq!(basename(r"C:\app\game.exe"), "game.exe");
        assert_eq!(basename("editor.exe"), "editor.exe");
        assert_eq!(basename("c:/"), "c:", "根路径无文件名 → 修剪后整体（展示兜底）");
        assert_eq!(date_range(["2026-01-02", "2025-12-31", "2026-05-01"]).0.as_deref(), Some("2025-12-31"));
        assert_eq!(date_range(["2026-01-02", "2025-12-31", "2026-05-01"]).1.as_deref(), Some("2026-05-01"));
        assert_eq!(date_range(Vec::<&str>::new()), (None, None));
    }

    /// 按钮码标签静态表逐字（§4.8）。
    #[test]
    fn wp_button_and_scroll_labels() {
        assert_eq!(mouse_button_label(0), "左键");
        assert_eq!(mouse_button_label(1), "中键");
        assert_eq!(mouse_button_label(2), "右键");
        assert_eq!(mouse_button_label(99), "其他");
        assert_eq!(mouse_button_label(123), "按钮 123");
        assert_eq!(mouse_scroll_label(1), "向上");
        assert_eq!(mouse_scroll_label(2), "向下");
        assert_eq!(mouse_scroll_label(3), "向左");
        assert_eq!(mouse_scroll_label(4), "向右");
        assert_eq!(mouse_scroll_label(7), "方向 7");
    }
}
