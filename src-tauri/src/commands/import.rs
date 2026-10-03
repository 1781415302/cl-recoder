//! import —— WhatPulse 导入（PLAN §4.8/§5.4；**wp_* 的唯一写入处**，§2.5）。
//!
//! 流程（§5.4 逐字 + correctness-v2 §5.1）：选文件 → 三级路径解析（§4.3）→
//! **复制到 `%TEMP%\clrecoder-wp-{pid}-{ts}-{序号}.db`**（主库 fatal + `{src}-wal`
//! best-effort，**不复制 `-shm`**）→ **RW 打开副本**（`Connection::open`，WAL 恢复写副本
//! 自身；`-shm` 由 SQLite 自动重建）→ **preflight（correctness-v2 §4.5）：`PRAGMA
//! quick_check` 唯一 ok + 来源识别（八张统计表至少一张且列验证通过；applications 仅是
//! 可选展示元数据，不构成来源身份）**，不相关/损坏/畸形来源立即硬失败 → 缺表 warning
//! （缺失统计表允许空类别）→ 全部现存统计数据读取成功后组织批次 → **此时才**依次
//! `db::open_rw`、migrate、`store::writer::rebuild_wp_tables_with` 单事务整体重建
//! （GUI 临时 rw 连接 busy_timeout=10s，wp_* 的 SQL 唯一归属 store）→
//! 删 `{tmp}`/`{tmp}-wal`/`{tmp}-shm` 三件套 → 返回 `ImportReport`。
//!
//! **整体替换语义**：重复导入即刷新为最新快照；合法已识别的空统计表同样成功（导入
//! 零行整体替换），与非法/不相关/损坏来源不同。来源复制/识别/校验/读取/批次准备失败
//! 都先于目标库打开——已有目标不变、不存在目标不被创建。
//!
//! 铁律（§1 原则 6 / §7-1）：WhatPulse 库只读、先复制后打开、**绝不写 WhatPulse 的任何文件**。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rusqlite::Connection;
use serde::Serialize;

use crate::db;
use crate::state::AppState;
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

/// 八张统计表及其必需列（correctness-v2 §4.5 表格逐字；与现有聚合 SQL 一致）。
/// 允许额外列，不要求 profile_id/hour，不要求九表齐全；applications 是单独的可选
/// 展示元数据，不在此列（其失败只 warning + basename 回退）。
const STAT_TABLES: &[(&str, &[&str])] = &[
    ("keypress_frequency", &["day", "key", "count"]),
    ("keycombo_frequency", &["day", "combo", "count"]),
    ("input_per_application", &["day", "path", "keys", "clicks"]),
    ("application_active_hour", &["day", "path", "msec_active"]),
    ("mouseclicks", &["day", "count"]),
    ("mousedistance", &["day", "distance_inches"]),
    ("mouseclicks_frequency", &["day", "button", "count"]),
    ("mousescrolls", &["day", "direction", "count"]),
];

/// 临时副本名的进程内原子序号（correctness-v2 §4.5：pid+毫秒戳可能同毫秒碰撞，
/// 追加序号保证并行回归的内部副本互不冲突；不新增依赖）。
static TMP_COPY_SEQ: AtomicU64 = AtomicU64::new(0);

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

/// preflight 结果（correctness-v2 §4.5）：来源库中已确认存在的 WhatPulse 表集合。
/// 八张统计表只在存在**且必需列验证通过**时入集；applications 仅记录存在性
/// （可选展示元数据，其列/读取失败由聚合路径 warning + basename 回退）。
/// 仅本文件私有使用，内部形状不构成跨模块合同。
#[derive(Debug, Default)]
struct SourceSchema(HashSet<String>);

impl SourceSchema {
    /// 表是否已确认存在（统计表另需已过列验证才会出现在集合中）。
    fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

/// 表的列名集合（`PRAGMA table_info` 的 name 列）。Err 由调用方按合同处理：
/// 统计表 → 硬失败；applications 元数据在聚合路径只 warning。
fn table_columns(conn: &Connection, table: &str) -> Result<HashSet<String>, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    let mut cols = HashSet::new();
    for c in rows {
        cols.insert(c?);
    }
    Ok(cols)
}

/// preflight（correctness-v2 §4.5）：副本完整性检查 + 来源识别 + 现存统计表列验证。
///
/// 固定规则：`PRAGMA quick_check` 必须成功且结果唯一 `ok`；全局 sqlite_master 读取与
/// 八张统计表的存在性/列检查出错均视为硬失败；八张统计表至少一张存在且列验证通过
/// （单独 applications 不构成来源身份）。Err = 硬失败文案，调用方整次导入失败，
/// 绝不触碰目标库。
fn validate_source(conn: &Connection) -> Result<SourceSchema, String> {
    // 1. 完整性：quick_check 唯一 ok（损坏库的多行错误结果 / 查询报错都算失败）
    let mut stmt = conn
        .prepare("PRAGMA quick_check")
        .map_err(|e| format!("副本完整性检查失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| format!("副本完整性检查失败: {e}"))?;
    let mut check: Vec<String> = Vec::new();
    for r in rows {
        check.push(r.map_err(|e| format!("副本完整性检查失败: {e}"))?);
    }
    if check.len() != 1 || check[0] != "ok" {
        return Err(format!("副本完整性检查未通过: {}", check.join("；")));
    }

    // 2. 全局读一次 sqlite_master 拿全部表名，后续存在性判断不再有 SQL 出错面
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .map_err(|e| format!("读取来源表清单失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| format!("读取来源表清单失败: {e}"))?;
    let mut schema = SourceSchema::default();
    for r in rows {
        schema.0.insert(r.map_err(|e| format!("读取来源表清单失败: {e}"))?);
    }

    // 3. 现存统计表逐一验证必需列（存在但缺列必须整次失败，不能跳过后覆写目标）
    for &(name, required) in STAT_TABLES {
        if !schema.0.contains(name) {
            continue; // 缺失统计表：调用方 warning + 空类别（部分合法 schema 仍可导入）
        }
        let actual =
            table_columns(conn, name).map_err(|e| format!("检查表 {name} 列失败: {e}"))?;
        for col in required {
            if !actual.contains(*col) {
                return Err(format!("统计表 {name} 缺少必需列 {col}"));
            }
        }
    }

    // 4. 来源身份：至少一张列验证通过的统计表（单独 applications 不构成身份）
    if !STAT_TABLES
        .iter()
        .any(|&(name, _)| schema.0.contains(name))
    {
        return Err("来源数据库不包含可识别的 WhatPulse 统计表".to_string());
    }
    Ok(schema)
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
/// 属可选展示元数据：先按 PRAGMA 实际列验证 path/name 再 SELECT——缺失列经
/// SELECT 的双引号 DQS 兜底会静默变成字符串字面量，不能只靠 SELECT 报错发现；
/// 任何失败由调用方 warning + basename 回退，不阻断统计导入（correctness-v2 §4.5）。
fn load_applications(conn: &Connection) -> Result<HashMap<String, String>, String> {
    if !table_exists(conn, "applications").map_err(|e| e.to_string())? {
        return Ok(HashMap::new());
    }
    let cols = table_columns(conn, "applications").map_err(|e| e.to_string())?;
    for col in ["path", "name"] {
        if !cols.contains(col) {
            return Err(format!("缺少列 {col}"));
        }
    }
    let mut stmt = conn
        .prepare("SELECT \"path\", \"name\" FROM applications")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut m = HashMap::new();
    for row in rows {
        let (path, name) = row.map_err(|e| e.to_string())?;
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

/// 源路径三级解析（§4.3：参数 > settings 覆盖 > 默认探测）。
/// - `path_arg.trim()` 非空 → 用它；
/// - `settings_wp_db_path.trim()` 非空 → 用它（settings 允许写入空串，须同样 trim 判定）；
/// - 否则 `dirs::data_local_dir()?.join("WhatPulse").join("whatpulse.db")`；
/// - `data_local_dir` 失败 → Err（**绝不**兜底成 `./WhatPulse/whatpulse.db`）。
pub(crate) fn resolve_wp_source(
    path_arg: &str,
    settings_wp_db_path: Option<&str>,
) -> Result<PathBuf, String> {
    let arg = path_arg.trim();
    if !arg.is_empty() {
        return Ok(PathBuf::from(arg));
    }
    if let Some(s) = settings_wp_db_path {
        let s = s.trim();
        if !s.is_empty() {
            return Ok(PathBuf::from(s));
        }
    }
    let base = dirs::data_local_dir().ok_or_else(|| {
        "无法解析本地数据目录（%LOCALAPPDATA%），请在设置中指定 WhatPulse 库路径".to_string()
    })?;
    Ok(base.join("WhatPulse").join("whatpulse.db"))
}

/// 旁路文件名：`{db}{suffix}`（`OsString::push`，非 UTF-8 路径安全）。
fn sidecar_path(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
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
    //    临时名含 pid + 毫秒戳 + 进程内原子序号：并行测试/UI 连点同毫秒也不互相覆盖
    let tmp = std::env::temp_dir().join(format!(
        "clrecoder-wp-{}-{}-{}.db",
        std::process::id(),
        chrono::Local::now().timestamp_millis(),
        TMP_COPY_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp_wal = sidecar_path(&tmp, "-wal");
    let tmp_shm = sidecar_path(&tmp, "-shm");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&tmp_wal);
    let _ = std::fs::remove_file(&tmp_shm);
    if let Err(e) = std::fs::copy(source, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return failed_report(started, warnings, format!("复制源库失败: {e}"));
    }

    // {src}-wal 存在则 best-effort 复制（失败静默——WhatPulse 写入期间锁竞争/checkpoint 竞态属预期，
    // 缺 wal 只是得到更旧快照）。**不复制 -shm**：RW 打开副本时 SQLite 自动重建。
    let src_wal = sidecar_path(source, "-wal");
    if src_wal.is_file() {
        let _ = std::fs::copy(&src_wal, &tmp_wal);
    }

    // 2. RW 打开副本 + preflight 校验/聚合（损坏/不相关/畸形来源硬失败，
    //    失败路径绝不打开目标——已有目标不变、不存在目标不被创建，correctness-v2 §5.1）
    let result = import_from_copy(&tmp, source, source_size, stats_db, &mut warnings);
    // 清理三件套（无论成败，§5.4）
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&tmp_wal);
    let _ = std::fs::remove_file(&tmp_shm);

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

/// 副本校验 + 聚合 + 单事务重建（返回批次与报告数字；Err = 硬失败文案）。
///
/// 失败语义（correctness-v2 §4.5/§5.1）：preflight、聚合读取与批次准备任一失败都
/// **先于** `open_rw(stats_db)` 返回——已有目标不变、不存在目标不被创建；进入目标阶段
/// 后沿用建库/迁移行为，替换失败由 store 的单事务回滚兜底。只有已确认缺失的表才得到
/// 空结果，现存表读取错误绝不降级为空快照覆写目标。
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
    // RW 打开副本：WAL 恢复需要写副本自身（含自动重建 -shm）；源文件绝不写（§7-1）
    let conn = Connection::open(tmp).map_err(|e| format!("打开副本失败: {e}"))?;

    // preflight（correctness-v2 §4.5）：quick_check + 来源识别 + 现存统计表列验证，
    // 任一步失败即硬失败（不相关/损坏/畸形来源立即退出，绝不触碰目标）
    let schema = validate_source(&conn)?;

    // 缺表 warning（§4.8：缺失统计表允许 warning + 空类别；含 applications 元数据）
    for t in SOURCE_TABLES {
        if !schema.contains(t) {
            warnings.push(missing_table_warning(t));
        }
    }

    // Rust 侧聚合（§4.8 六张目标表）：只有已确认缺失的表得到空结果；
    // 现存表的读取/聚合错误一律硬失败传播，绝不以空快照覆写目标（correctness-v2 §4.5）。
    let keys = agg_keys(&conn).map_err(|e| format!("读取按键统计失败: {e}"))?;
    let combos = agg_combos(&conn).map_err(|e| format!("读取组合键统计失败: {e}"))?;
    // agg_apps/agg_mouse 任一来源统计表失败都必须整次失败，不接受"另一张成功所以覆写"
    let apps = agg_apps(&conn, warnings).map_err(|e| format!("读取应用统计失败: {e}"))?;
    let mouse = agg_mouse(&conn).map_err(|e| format!("读取鼠标统计失败: {e}"))?;
    let buttons =
        agg_mouse_buttons(&conn).map_err(|e| format!("读取鼠标按键统计失败: {e}"))?;
    let scrolls = agg_mouse_scrolls(&conn).map_err(|e| format!("读取滚轮统计失败: {e}"))?;
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

    // 批次准备完成（correctness-v2 §4.5：批次全部准备完成前不得 open_rw/migrate/rebuild）
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

    // 此时才打开/迁移目标并单事务整体重建
    // （wp_* 的 SQL 唯一归属 store；GUI 临时 rw 连接 busy_timeout=10s，§5.4）
    let stats = db::open_rw(stats_db).map_err(|e| format!("打开统计库失败: {e}"))?;
    clrecoder_store::schema::migrate(&stats)
        .map_err(|e| format!("统计库迁移失败: {e}"))?;
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

/// WhatPulse 导入（§4.7/§4.3 `import_whatpulse(path) -> ImportReport`）。
/// TS 侧 invoke 键不变：仍只有 `{path}`（`State` 由 Tauri 注入，不进 invoke 参数）。
/// 三级解析：path 参数 > settings.wp_db_path > 默认探测；解析/导入失败一律
/// `ok=false` 报告，不返回 Err。
#[tauri::command]
pub async fn import_whatpulse(
    path: String,
    state: tauri::State<'_, AppState>,
) -> Result<ImportReport, String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        let wp_db_path = st
            .settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wp_db_path
            .clone();
        let report = match resolve_wp_source(&path, wp_db_path.as_deref()) {
            Ok(source) => run_import(&source, &db::stats_db_path()),
            Err(msg) => failed_report(started, Vec::new(), msg),
        };
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
    use rusqlite::OpenFlags;

    const D1: &str = "2026-05-01";
    const D2: &str = "2025-12-31"; // 故意比 D1 早：验证 dateMin 取并集最小值

    /// 按 §4.8 源表 schema 建九张空表（列名与 §4.8"对真实库已验证的列"一致）。
    /// fixture 复用：合法空库 / 元数据反例只建表不插行。
    fn create_wp_tables(conn: &Connection) {
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
    }

    /// 按 §4.8 源表 schema 造合成 fixture 库（列名与 §4.8"对真实库已验证的列"一致）。
    /// 含两个 profile_id 的行——验证"跨 profile_id 直接求和"。
    fn make_fixture(path: &Path) {
        let conn = Connection::open(path).unwrap();
        create_wp_tables(&conn);
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

    /// §4.3 三级路径解析纯函数。
    #[test]
    fn resolve_wp_source_levels() {
        assert_eq!(
            resolve_wp_source(r"C:\x.db", None).unwrap(),
            PathBuf::from(r"C:\x.db"),
            "非空 path_arg 优先"
        );
        assert_eq!(
            resolve_wp_source("  ", Some(r"D:\wp.db")).unwrap(),
            PathBuf::from(r"D:\wp.db"),
            "path_arg 空白 → settings"
        );
        assert_eq!(
            resolve_wp_source("", Some("  ")).unwrap(),
            resolve_wp_source("", None).unwrap(),
            "settings 空白 → 默认探测（须 trim 判定）"
        );
        if let Some(base) = dirs::data_local_dir() {
            let expected = base.join("WhatPulse").join("whatpulse.db");
            assert_eq!(resolve_wp_source("", None).unwrap(), expected);
            assert!(
                resolve_wp_source("", None)
                    .unwrap()
                    .ends_with(std::path::Path::new("WhatPulse").join("whatpulse.db")),
                "默认路径须以 WhatPulse/whatpulse.db 结尾"
            );
        } else {
            assert!(
                resolve_wp_source("", None).is_err(),
                "data_local_dir 不可用时必须 Err，绝不兜底 ./WhatPulse/whatpulse.db"
            );
        }
    }

    /// §4.3：WAL-only 行经 `-wal` 复制被读到（未 checkpoint 的提交数据）。
    #[test]
    fn import_reads_wal_only_rows() {
        // TempFile 先于 conn 声明：Drop 逆序，防 Windows 残留临时文件
        let src = TempFile::new("imp-wal-src", "db");
        let stats = TempFile::new("imp-wal-stats", "db");
        let wal_path = sidecar_path(src.as_ref(), "-wal");

        // fixture：WAL 模式；execute_batch autocommit 提交插入（未提交帧恢复时会被跳过）
        let conn = Connection::open(&src).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE keypress_frequency(profile_id INTEGER, day TEXT, hour INTEGER, key INTEGER, count INTEGER);
             INSERT INTO keypress_frequency VALUES (1,'2026-05-01',0,65,10);",
        )
        .unwrap();
        // 先 checkpoint，使 schema + 基线行落入主库文件
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        // 再插入 wal-only 行（不 checkpoint → 提交数据只存在于 -wal）
        conn.execute_batch("INSERT INTO keypress_frequency VALUES (1,'2026-05-01',1,65,5);")
            .unwrap();

        // 前置条件：src-wal 存在且非空
        assert!(wal_path.is_file(), "fixture 必须生成 -wal: {}", wal_path.display());
        let wal_len = std::fs::metadata(&wal_path).unwrap().len();
        assert!(wal_len > 0, "-wal 必须非空（含未 checkpoint 的提交）");

        // conn 存活跨过 run_import（复制 -wal 后副本侧 WAL 恢复；源侧不受影响）
        let report = run_import(src.as_ref(), stats.as_ref());
        assert!(report.ok, "导入必须成功: {:?}", report.warnings);

        let stats_conn = Connection::open(stats.as_ref()).unwrap();
        let wp_sum: i64 = stats_conn
            .query_row("SELECT COALESCE(SUM(count),0) FROM wp_key_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            wp_sum, 15,
            "wp_key_daily SUM 必须含 wal-only 行（主库 10 + WAL 5）"
        );

        // conn 仍存活时源侧 -wal 应仍在（run_import 绝不写 WhatPulse 源文件；
        // conn drop 后 SQLite 会自行 checkpoint，故断言须在 drop 前）
        assert!(
            wal_path.is_file() && std::fs::metadata(&wal_path).unwrap().len() > 0,
            "源侧 -wal 在 run_import 后、conn 存活时应仍在"
        );
        drop(conn);
    }

    // ===== correctness_v2（S5 导入保护）回归 =====

    /// 目标库六张 wp 数据表 + meta 的内容快照（§4.5：坏来源导入前后必须逐项相等，
    /// 保护断言覆盖全部数据表而非只看按键总数）。
    #[derive(Debug, PartialEq)]
    struct WpTargetSnapshot {
        key_rows: i64,
        key_sum: i64,
        combo_rows: i64,
        app_rows: i64,
        mouse_rows: i64,
        buttons_rows: i64,
        scrolls_rows: i64,
        meta_rows: i64,
        meta_note: String,
    }

    fn wp_target_snapshot(path: &Path) -> WpTargetSnapshot {
        let conn = Connection::open(path).unwrap();
        let count = |t: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        };
        WpTargetSnapshot {
            key_rows: count("wp_key_daily"),
            key_sum: conn
                .query_row("SELECT COALESCE(SUM(count),0) FROM wp_key_daily", [], |r| r.get(0))
                .unwrap(),
            combo_rows: count("wp_combo_daily"),
            app_rows: count("wp_app_daily"),
            mouse_rows: count("wp_mouse_daily"),
            buttons_rows: count("wp_mouse_buttons_daily"),
            scrolls_rows: count("wp_mouse_scroll_daily"),
            meta_rows: count("wp_import_meta"),
            meta_note: conn
                .query_row("SELECT note FROM wp_import_meta WHERE id=1", [], |r| r.get(0))
                .unwrap(),
        }
    }

    /// 来源阶段失败且目标不存在：库文件与 -wal/-shm 旁路都不得被创建（§5.1-6）。
    fn assert_target_absent(stats: &Path) {
        assert!(!stats.exists(), "目标库不得被创建: {}", stats.display());
        for suffix in ["-wal", "-shm"] {
            let side = sidecar_path(stats, suffix);
            assert!(!side.exists(), "目标旁路文件不得被创建: {}", side.display());
        }
    }

    /// F1 反例 1（§1.1 事故）：不相关 SQLite 来源必须 ok=false，已有目标六张数据表
    /// + meta 逐项不变（27→0 事故不再发生），且不存在目标不被创建。
    #[test]
    fn correctness_v2_irrelevant_source_fails_and_preserves_target() {
        let good = TempFile::new("cv2-irrel-good", "db");
        make_fixture(&good);
        let stats = TempFile::new("cv2-irrel-stats", "db");
        assert!(run_import(good.as_ref(), stats.as_ref()).ok, "前置合法导入须成功");
        let before = wp_target_snapshot(stats.as_ref());
        assert_eq!(before.key_sum, 27, "前置：目标已持有 fixture 的 27 个按键");

        let bad = TempFile::new("cv2-irrel-bad", "db");
        {
            let conn = Connection::open(&bad).unwrap();
            conn.execute_batch(
                "CREATE TABLE some_other_tool(data TEXT);
                 INSERT INTO some_other_tool VALUES ('hello');",
            )
            .unwrap();
        }
        let report = run_import(bad.as_ref(), stats.as_ref());
        assert!(!report.ok, "不相关来源必须失败: {:?}", report.warnings);
        assert_eq!(
            (report.keys, report.combos, report.apps, report.mouse_days),
            (0, 0, 0, 0),
            "失败报告数字必须全零"
        );
        assert!(
            report.warnings[0].contains("WhatPulse"),
            "失败原因须指明来源不含 WhatPulse 统计表: {:?}",
            report.warnings
        );
        assert_eq!(
            wp_target_snapshot(stats.as_ref()),
            before,
            "失败导入不得触碰目标（六张数据表+meta 逐项不变）"
        );

        // 来源阶段失败且目标不存在 → 不建库
        let fresh = TempFile::new("cv2-irrel-fresh", "db");
        let report = run_import(bad.as_ref(), fresh.as_ref());
        assert!(!report.ok);
        assert_target_absent(fresh.as_ref());
    }

    /// F1 反例 2：损坏（非 SQLite）来源必须 ok=false，目标不变/不建。
    #[test]
    fn correctness_v2_corrupted_source_fails_and_preserves_target() {
        let good = TempFile::new("cv2-corrupt-good", "db");
        make_fixture(&good);
        let stats = TempFile::new("cv2-corrupt-stats", "db");
        assert!(run_import(good.as_ref(), stats.as_ref()).ok);
        let before = wp_target_snapshot(stats.as_ref());

        let bad = TempFile::new("cv2-corrupt-bad", "db");
        std::fs::write(&bad, vec![0xFFu8; 4096]).unwrap();

        let report = run_import(bad.as_ref(), stats.as_ref());
        assert!(!report.ok, "损坏来源必须失败: {:?}", report.warnings);
        assert_eq!(wp_target_snapshot(stats.as_ref()), before, "损坏来源不得触碰目标");

        let fresh = TempFile::new("cv2-corrupt-fresh", "db");
        assert!(!run_import(bad.as_ref(), fresh.as_ref()).ok);
        assert_target_absent(fresh.as_ref());
    }

    /// F1 反例 3：同名统计表缺必需列 → 整次失败（不得跳过后覆写目标）；
    /// 另一张完整统计表成功不构成豁免。
    #[test]
    fn correctness_v2_existing_stat_table_missing_column_fails() {
        let good = TempFile::new("cv2-misscol-good", "db");
        make_fixture(&good);
        let stats = TempFile::new("cv2-misscol-stats", "db");
        assert!(run_import(good.as_ref(), stats.as_ref()).ok);
        let before = wp_target_snapshot(stats.as_ref());

        let bad = TempFile::new("cv2-misscol-bad", "db");
        {
            let conn = Connection::open(&bad).unwrap();
            // keypress_frequency 同名但缺 count 列；mouseclicks 列完整（不得因它覆写）
            conn.execute_batch(
                "CREATE TABLE keypress_frequency(profile_id INTEGER, day TEXT, hour INTEGER, key INTEGER);
                 INSERT INTO keypress_frequency VALUES (1,'2026-05-01',0,65);
                 CREATE TABLE mouseclicks(profile_id INTEGER, day TEXT, hour INTEGER, count INTEGER);
                 INSERT INTO mouseclicks VALUES (1,'2026-05-01',0,9);",
            )
            .unwrap();
        }
        let report = run_import(bad.as_ref(), stats.as_ref());
        assert!(!report.ok, "现存统计表缺列必须整次失败: {:?}", report.warnings);
        assert_eq!(wp_target_snapshot(stats.as_ref()), before, "缺列来源不得触碰目标");

        let fresh = TempFile::new("cv2-misscol-fresh", "db");
        assert!(!run_import(bad.as_ref(), fresh.as_ref()).ok);
        assert_target_absent(fresh.as_ref());
    }

    /// F1 反例 4：现存统计表列名齐全但行数据无法读取（day 存整数 → Rust 侧 String
    /// 读取失败）→ 整次失败，不降级为空快照覆写目标。
    #[test]
    fn correctness_v2_existing_stat_table_read_error_fails() {
        let good = TempFile::new("cv2-readerr-good", "db");
        make_fixture(&good);
        let stats = TempFile::new("cv2-readerr-stats", "db");
        assert!(run_import(good.as_ref(), stats.as_ref()).ok);
        let before = wp_target_snapshot(stats.as_ref());

        let bad = TempFile::new("cv2-readerr-bad", "db");
        {
            let conn = Connection::open(&bad).unwrap();
            // 列名与 §4.8 一致（preflight 列验证通过），但 day 存整数 → 聚合读取必然出错
            conn.execute_batch(
                "CREATE TABLE keypress_frequency(profile_id INTEGER, day INTEGER, hour INTEGER, key INTEGER, count INTEGER);
                 INSERT INTO keypress_frequency VALUES (1,20260501,0,65,10);",
            )
            .unwrap();
        }
        let report = run_import(bad.as_ref(), stats.as_ref());
        assert!(!report.ok, "现存统计表读取错误必须整次失败: {:?}", report.warnings);
        assert_eq!(wp_target_snapshot(stats.as_ref()), before, "读取错误不得以空快照覆写目标");

        let fresh = TempFile::new("cv2-readerr-fresh", "db");
        assert!(!run_import(bad.as_ref(), fresh.as_ref()).ok);
        assert_target_absent(fresh.as_ref());
    }

    /// §4.5：单独 applications 表不构成来源身份——只有展示元数据、无统计表 → 失败不建库。
    #[test]
    fn correctness_v2_applications_alone_is_not_source_identity() {
        let bad = TempFile::new("cv2-apponly-src", "db");
        {
            let conn = Connection::open(&bad).unwrap();
            conn.execute_batch(
                "CREATE TABLE applications(path TEXT, name TEXT);
                 INSERT INTO applications VALUES ('c:/app/editor.exe','Editor');",
            )
            .unwrap();
        }
        let stats = TempFile::new("cv2-apponly-stats", "db");
        let report = run_import(bad.as_ref(), stats.as_ref());
        assert!(!report.ok, "仅 applications 不构成 WhatPulse 来源: {:?}", report.warnings);
        assert_target_absent(stats.as_ref());
    }

    /// §4.5：合法已识别的空统计表 → 成功导入零行并整体替换（区别于不相关/损坏来源，
    /// 不得以"行数为零"隐式判失败）。
    #[test]
    fn correctness_v2_legal_empty_source_succeeds_and_replaces() {
        let good = TempFile::new("cv2-empty-good", "db");
        make_fixture(&good);
        let stats = TempFile::new("cv2-empty-stats", "db");
        assert!(run_import(good.as_ref(), stats.as_ref()).ok);
        assert_eq!(wp_target_snapshot(stats.as_ref()).key_sum, 27, "前置：目标已有数据");

        let empty = TempFile::new("cv2-empty-src", "db");
        {
            let conn = Connection::open(&empty).unwrap();
            create_wp_tables(&conn); // 九表齐全、零行
        }
        let report = run_import(empty.as_ref(), stats.as_ref());
        assert!(report.ok, "合法空库必须成功: {:?}", report.warnings);
        assert_eq!(
            (report.keys, report.combos, report.apps, report.mouse_days),
            (0, 0, 0, 0),
            "空库报告数字为零"
        );
        assert_eq!(report.date_min, None, "空库无日期范围");
        assert_eq!(report.date_max, None);
        let snap = wp_target_snapshot(stats.as_ref());
        assert_eq!(
            (
                snap.key_rows, snap.combo_rows, snap.app_rows, snap.mouse_rows,
                snap.buttons_rows, snap.scrolls_rows
            ),
            (0, 0, 0, 0, 0, 0),
            "整体替换：旧数据必须被清空"
        );
        assert_eq!(snap.meta_rows, 1, "空库导入仍写 meta（整体替换语义）");
        assert!(
            report.warnings.iter().any(|w| w.contains("未发现任何数据行")),
            "空数据提示照常: {:?}",
            report.warnings
        );
    }

    /// §4.5：applications 是可选展示元数据——存在但缺 name 列只 warning + basename
    /// 回退，不得等同统计损坏阻断导入。
    #[test]
    fn correctness_v2_broken_applications_metadata_warns_and_falls_back() {
        let src = TempFile::new("cv2-appmeta-src", "db");
        {
            let conn = Connection::open(&src).unwrap();
            create_wp_tables(&conn);
            conn.execute_batch("DROP TABLE applications; CREATE TABLE applications(path TEXT);")
                .unwrap();
            conn.execute_batch(
                "INSERT INTO keypress_frequency VALUES (1,'2026-05-01',0,65,10);
                 INSERT INTO input_per_application VALUES (1,'2026-05-01',0,'c:/app/editor.exe',7,1);
                 INSERT INTO application_active_hour VALUES (1,'2026-05-01',0,'c:/app/editor.exe',1000);",
            )
            .unwrap();
        }
        let stats = TempFile::new("cv2-appmeta-stats", "db");
        let report = run_import(src.as_ref(), stats.as_ref());
        assert!(report.ok, "applications 元数据错误不得阻断导入: {:?}", report.warnings);
        assert!(
            report.warnings.iter().any(|w| w.contains("applications")),
            "元数据失败须有警告: {:?}",
            report.warnings
        );
        let conn = Connection::open(stats.as_ref()).unwrap();
        let (name, keys): (String, i64) = conn
            .query_row(
                "SELECT name, keys FROM wp_app_daily LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "editor.exe", "应用名必须回退 basename");
        assert_eq!(keys, 7, "统计数据本身照常导入");
    }
}
