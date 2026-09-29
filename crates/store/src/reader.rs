//! reader —— GUI 全部查询 SQL 的唯一归属（PLAN §4.5 契约）。
//!
//! 每个函数只做查询与行映射，不做跨页业务逻辑；调用方（GUI）持 ro 连接
//! （busy_timeout=5000ms 由 src-tauri 的 db.rs 设置，§2.4/§5.1）。
//! 行类型不含 GUI 展示字段：`label` 一律由 GUI 的 keylabel（GetKeyNameTextW，§3）补齐，
//! 本模块返回的 `TopKeyRow.label` 为空串。
//!
//! 日期范围 `[from, to]` 为闭区间、`YYYY-MM-DD` 闭区间字典序比较（§4 全部 day 为该格式）。

use clrecoder_core::codes::DeviceKind;
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::{i64_to_count, i64_to_u16, i64_to_u8, kind_from_text, kind_to_text, Result};

// ---------------------------------------------------------------------------
// 行类型（§4.5 逐字对齐；内部结构不 camelCase，DTO 映射在 src-tauri）
// ---------------------------------------------------------------------------

/// 设备行（含 lifetime 总计数）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRow {
    /// 设备 id（devices.id）
    pub id: i64,
    /// 设备种类
    pub kind: DeviceKind,
    /// Vendor ID（未知为 0）
    pub vid: u16,
    /// Product ID（未知为 0）
    pub pid: u16,
    /// 显示名
    pub name: String,
    /// 用户自定义昵称（可空；展示优先于 name）
    pub nickname: Option<String>,
    /// 首次见到（RFC3339）
    pub first_seen: String,
    /// 最近见到（RFC3339）
    pub last_seen: String,
    /// lifetime 总计数（全部天数 SUM，从未输入的设备为 0）
    pub total: u64,
}

/// 单日总量。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayCount {
    /// 日期 `YYYY-MM-DD`
    pub day: String,
    /// 当日总量
    pub total: u64,
}

/// 键盘逐日行（label 由 GUI keylabel 补）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyDailyRow {
    /// 日期
    pub day: String,
    /// 归一化 scancode
    pub code: u16,
    /// 当日次数
    pub count: u64,
}

/// 键盘 Top-N 行。`label` 由 GUI keylabel 拼接（§4.5："由 SQL 外拼接"），本模块返回空串。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopKeyRow {
    /// 归一化 scancode
    pub code: u16,
    /// 范围内总次数
    pub total: u64,
    /// 显示名（GUI 填充）
    pub label: String,
}

/// 应用聚合行（范围内求和，按前台秒数降序）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRow {
    /// 前台 exe 小写 basename
    pub exe: String,
    /// 前台秒数合计
    pub seconds: u64,
    /// 按键数合计
    pub keys: u64,
    /// 点击数合计
    pub clicks: u64,
}

/// 组合键聚合行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComboRow {
    /// 修饰键位掩码（`clrecoder_core::codes::mods` 位或组合）
    pub mods: u8,
    /// 非修饰键 scancode
    pub code: u16,
    /// 范围内总次数
    pub total: u64,
}

/// 应用逐日明细行（CSV/JSON 导出用，§4.10）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDayRow {
    /// 日期
    pub day: String,
    /// 前台 exe
    pub exe: String,
    /// 前台秒数
    pub seconds: u64,
    /// 按键数
    pub keys: u64,
    /// 点击数
    pub clicks: u64,
}

/// 组合键逐日明细行（导出用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComboDayRow {
    /// 日期
    pub day: String,
    /// 修饰键位掩码
    pub mods: u8,
    /// 非修饰键 scancode
    pub code: u16,
    /// 当日次数
    pub count: u64,
}

/// 仪表盘组合查询（§4.7 Overview 的 store 侧形状）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewData {
    /// 范围内逐日总量（全部种类合计）
    pub days: Vec<DayCount>,
    /// `to` 日按设备种类拆分（GUI 仪表盘把它当"今日"展示）
    pub today: TodaySplit,
    /// 设备列表（lifetime total，与 [`devices`] 同口径）
    pub devices: Vec<OverviewDeviceRow>,
}

/// 今日按设备种类拆分（§4.7：keys/clicks/gamepad）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TodaySplit {
    /// 键盘按键合计
    pub keys: u64,
    /// 鼠标点击（含滚轮）合计
    pub clicks: u64,
    /// 手柄按键合计
    pub gamepad: u64,
}

/// 仪表盘设备行（§4.7 Overview.devices 形状）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewDeviceRow {
    /// 设备 id
    pub id: i64,
    /// 设备种类
    pub kind: DeviceKind,
    /// 显示名
    pub name: String,
    /// lifetime 总计数
    pub total: u64,
}

// ---------------------------------------------------------------------------
// WhatPulse 行类型（§4.7 同名 TS 类型的 store 侧来源；wp_* 表行结构同时供导入批次复用）
// ---------------------------------------------------------------------------

/// WhatPulse 导入元数据（`wp_import_meta`，id 恒为 1，不外露）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WpMetaRow {
    /// 导入时刻（RFC3339，导入方填写）
    pub imported_at: String,
    /// 源库路径
    pub source_path: String,
    /// 源库大小（字节；未知为 None）
    pub source_size: Option<i64>,
    /// 源数据最早日期
    pub date_min: Option<String>,
    /// 源数据最晚日期
    pub date_max: Option<String>,
    /// 备注/警告补充
    pub note: String,
}

/// `wp_key_daily` 行（导入载荷与逐日查询共用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpKeyDailyRow {
    /// 日期
    pub day: String,
    /// Qt 键码（§4.8）
    pub qt_key: i64,
    /// 显示名（`qt_key_label`，导入时固化）
    pub label: String,
    /// 当日次数
    pub count: u64,
}

/// `wp_combo_daily` 行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpComboDailyRow {
    /// 日期
    pub day: String,
    /// 组合键原文（如 `"shift,87"`）
    pub combo: String,
    /// 解析后的友好格式
    pub label: String,
    /// 当日次数
    pub count: u64,
}

/// `wp_app_daily` 行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpAppDailyRow {
    /// 日期
    pub day: String,
    /// 源 path 原样
    pub path: String,
    /// 应用显示名
    pub name: String,
    /// 前台秒数
    pub seconds: u64,
    /// 按键数
    pub keys: u64,
    /// 点击数
    pub clicks: u64,
}

/// `wp_mouse_daily` 行（`distance_inches` 原样返回；米换算 ×0.0254 在 GUI 层，§4.7）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpMouseDailyRow {
    /// 日期
    pub day: String,
    /// 当日点击数
    pub clicks: u64,
    /// 当日移动距离（英寸）
    pub distance_inches: f64,
}

/// `wp_mouse_buttons_daily` 行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpMouseButtonDailyRow {
    /// 日期
    pub day: String,
    /// WhatPulse 按钮码（无公开文档，保留原码，§9.3）
    pub button_code: i64,
    /// 显示名
    pub label: String,
    /// 当日次数
    pub count: u64,
}

/// `wp_mouse_scroll_daily` 行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpMouseScrollDailyRow {
    /// 日期
    pub day: String,
    /// WhatPulse 滚轮方向码（推断语义，§9.3）
    pub direction_code: i64,
    /// 显示名
    pub label: String,
    /// 当日次数
    pub count: u64,
}

/// WhatPulse 概览（§4.7 WpOverview）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpOverviewData {
    /// 逐日活跃度（按键 + 鼠标点击之和；组合键不重复计入）
    pub days: Vec<DayCount>,
    /// 范围内按键总数
    pub keys_total: u64,
    /// 范围内组合键总数
    pub combos_total: u64,
    /// 范围内应用使用秒数总和（§4.7 未定语义的裁决：应用卡展示总时长）
    pub apps_total: u64,
    /// 范围内鼠标点击总数
    pub mouse_clicks_total: u64,
}

/// WhatPulse 按键 Top-N（范围内按 qt_key 聚合）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpTopKeyRow {
    /// Qt 键码
    pub qt_key: i64,
    /// 显示名
    pub label: String,
    /// 范围内总次数
    pub total: u64,
}

/// WhatPulse 组合键 Top-N。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpTopComboRow {
    /// 组合键原文
    pub combo: String,
    /// 解析后的友好格式
    pub label: String,
    /// 范围内总次数
    pub total: u64,
}

/// WhatPulse 应用 Top-N（按 path 聚合，按秒降序）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpAppAggRow {
    /// 源 path 原样
    pub path: String,
    /// 应用显示名
    pub name: String,
    /// 前台秒数合计
    pub seconds: u64,
    /// 按键数合计
    pub keys: u64,
    /// 点击数合计
    pub clicks: u64,
}

/// WhatPulse 鼠标按钮 Top-N。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpMouseButtonAggRow {
    /// WhatPulse 按钮码
    pub button_code: i64,
    /// 显示名
    pub label: String,
    /// 范围内总次数
    pub total: u64,
}

/// WhatPulse 滚轮方向 Top-N。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WpMouseScrollAggRow {
    /// WhatPulse 滚轮方向码
    pub direction_code: i64,
    /// 显示名
    pub label: String,
    /// 范围内总次数
    pub total: u64,
}

// ---------------------------------------------------------------------------
// 内部映射助手
// ---------------------------------------------------------------------------

/// 从行中读 `devices.kind` 文本并解析为 `DeviceKind`（契约外值折算成 rusqlite 转换错误）。
fn kind_col(r: &Row<'_>, idx: usize) -> std::result::Result<DeviceKind, rusqlite::Error> {
    let s: String = r.get(idx)?;
    kind_from_text(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
    })
}

/// 单标量查询（COALESCE 后恒有行）。
fn scalar_u64(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<u64> {
    let v: i64 = conn.query_row(sql, p, |r| r.get(0))?;
    Ok(i64_to_count(v))
}

// ---------------------------------------------------------------------------
// 自有数据查询（§4.5 逐字对齐）
// ---------------------------------------------------------------------------

/// 设备列表（含 lifetime total：LEFT JOIN SUM，从未输入的设备 total=0；按 id 升序）。
pub fn devices(conn: &Connection) -> Result<Vec<DeviceRow>> {
    const SQL: &str = "SELECT v.id, v.kind, v.vid, v.pid, v.name, v.nickname, v.first_seen, v.last_seen, \
         COALESCE(SUM(i.count), 0) \
         FROM devices v LEFT JOIN input_daily i ON i.device_id = v.id \
         GROUP BY v.id ORDER BY v.id";
    let mut stmt = conn.prepare(SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(DeviceRow {
            id: r.get(0)?,
            kind: kind_col(r, 1)?,
            vid: i64_to_u16(r.get(2)?),
            pid: i64_to_u16(r.get(3)?),
            name: r.get(4)?,
            nickname: r.get(5)?,
            first_seen: r.get(6)?,
            last_seen: r.get(7)?,
            total: i64_to_count(r.get(8)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn day_count_row(r: &Row<'_>) -> std::result::Result<DayCount, rusqlite::Error> {
    Ok(DayCount { day: r.get(0)?, total: i64_to_count(r.get(1)?) })
}

/// 逐日总量（可选按设备种类过滤；kind 消歧靠 devices.kind 连表，§4.1）。
pub fn daily_totals(
    conn: &Connection,
    from: &str,
    to: &str,
    kind: Option<DeviceKind>,
) -> Result<Vec<DayCount>> {
    match kind {
        None => {
            let mut stmt = conn.prepare(
                "SELECT day, COALESCE(SUM(count), 0) FROM input_daily \
                 WHERE day BETWEEN ?1 AND ?2 GROUP BY day ORDER BY day",
            )?;
            let rows = stmt.query_map(params![from, to], day_count_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        }
        Some(k) => {
            let mut stmt = conn.prepare(
                "SELECT d.day, COALESCE(SUM(d.count), 0) FROM input_daily d \
                 JOIN devices v ON v.id = d.device_id \
                 WHERE d.day BETWEEN ?1 AND ?2 AND v.kind = ?3 GROUP BY d.day ORDER BY d.day",
            )?;
            let rows = stmt.query_map(params![from, to, kind_to_text(k)], day_count_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        }
    }
}

/// 某日各设备总量 `(device_id, total)`（仪表盘近实时刷新；按 device_id 升序）。
pub fn today_by_device(conn: &Connection, day: &str) -> Result<Vec<(i64, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT device_id, COALESCE(SUM(count), 0) FROM input_daily \
         WHERE day = ?1 GROUP BY device_id ORDER BY device_id",
    )?;
    let rows = stmt.query_map([day], |r| {
        Ok((r.get::<_, i64>(0)?, i64_to_count(r.get::<_, i64>(1)?)))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 某设备在 [from, to] 的鼠标移动距离（英寸；WhatPulse 同口径）。
pub fn mouse_distance_total(conn: &Connection, device_id: i64, from: &str, to: &str) -> Result<f64> {
    let v: f64 = conn.query_row(
        "SELECT COALESCE(SUM(distance_inches), 0) FROM mouse_move_daily \
         WHERE device_id = ?1 AND day BETWEEN ?2 AND ?3",
        params![device_id, from, to],
        |r| r.get(0),
    )?;
    Ok(v)
}

/// 范围内鼠标移动逐日距离 `(day, distance_inches)`（按 day 升序；汇总所有鼠标设备）。
pub fn mouse_distance_daily(conn: &Connection, from: &str, to: &str) -> Result<Vec<(String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT day, COALESCE(SUM(distance_inches), 0) FROM mouse_move_daily \
         WHERE day BETWEEN ?1 AND ?2 GROUP BY day ORDER BY day",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 仪表盘一次组合查询（§4.7 Overview）：
/// - `days`：范围内逐日总量（全部种类）；
/// - `today`：**`to` 日**按 devices.kind 拆分为 keys/clicks/gamepad（GUI 传 `to=今日` 即今日拆分）；
/// - `devices`：设备列表（lifetime total）。
pub fn overview(conn: &Connection, from: &str, to: &str) -> Result<OverviewData> {
    let days = daily_totals(conn, from, to, None)?;
    let mut today = TodaySplit::default();
    {
        let mut stmt = conn.prepare(
            "SELECT v.kind, COALESCE(SUM(d.count), 0) FROM input_daily d \
             JOIN devices v ON v.id = d.device_id WHERE d.day = ?1 GROUP BY v.kind",
        )?;
        let rows = stmt.query_map([to], |r| {
            Ok((r.get::<_, String>(0)?, i64_to_count(r.get::<_, i64>(1)?)))
        })?;
        for row in rows {
            let (kind, total) = row?;
            match kind_from_text(&kind)? {
                DeviceKind::Keyboard => today.keys = total,
                DeviceKind::Mouse => today.clicks = total,
                DeviceKind::Gamepad => today.gamepad = total,
            }
        }
    }
    let mut devices_out = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT v.id, v.kind, COALESCE(v.nickname, v.name), COALESCE(SUM(i.count), 0) FROM devices v \
             LEFT JOIN input_daily i ON i.device_id = v.id GROUP BY v.id ORDER BY v.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(OverviewDeviceRow {
                id: r.get(0)?,
                kind: kind_col(r, 1)?,
                name: r.get(2)?,
                total: i64_to_count(r.get(3)?),
            })
        })?;
        for row in rows {
            devices_out.push(row?);
        }
    }
    Ok(OverviewData { days, today, devices: devices_out })
}

/// 键盘逐日明细（指定设备，`[from, to]` 闭区间；按 day, code 升序）。
pub fn key_daily(
    conn: &Connection,
    device_id: i64,
    from: &str,
    to: &str,
) -> Result<Vec<KeyDailyRow>> {
    let mut stmt = conn.prepare(
        "SELECT day, code, count FROM input_daily \
         WHERE device_id = ?1 AND day BETWEEN ?2 AND ?3 ORDER BY day, code",
    )?;
    let rows = stmt.query_map(params![device_id, from, to], |r| {
        Ok(KeyDailyRow {
            day: r.get(0)?,
            code: i64_to_u16(r.get(1)?),
            count: i64_to_count(r.get(2)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 键盘 Top-N（指定设备，范围内聚合，count 降序；`label` 由 GUI keylabel 填充，此处为空串）。
pub fn top_keys(
    conn: &Connection,
    device_id: i64,
    from: &str,
    to: &str,
    limit: u32,
) -> Result<Vec<TopKeyRow>> {
    let mut stmt = conn.prepare(
        "SELECT code, COALESCE(SUM(count), 0) FROM input_daily \
         WHERE device_id = ?1 AND day BETWEEN ?2 AND ?3 \
         GROUP BY code ORDER BY SUM(count) DESC, code ASC LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![device_id, from, to, i64::from(limit)], |r| {
        Ok(TopKeyRow {
            code: i64_to_u16(r.get(0)?),
            total: i64_to_count(r.get(1)?),
            label: String::new(),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 应用范围内聚合（按 exe 求和，前台秒数降序，§4.5"按秒降序"）。
pub fn apps(conn: &Connection, from: &str, to: &str, limit: u32) -> Result<Vec<AppRow>> {
    let mut stmt = conn.prepare(
        "SELECT exe, COALESCE(SUM(foreground_secs), 0), COALESCE(SUM(key_count), 0), \
         COALESCE(SUM(click_count), 0) FROM app_daily \
         WHERE day BETWEEN ?1 AND ?2 \
         GROUP BY exe ORDER BY SUM(foreground_secs) DESC, exe ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, i64::from(limit)], |r| {
        Ok(AppRow {
            exe: r.get(0)?,
            seconds: i64_to_count(r.get(1)?),
            keys: i64_to_count(r.get(2)?),
            clicks: i64_to_count(r.get(3)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 应用逐日明细（按 day, exe 升序；CSV/JSON 导出用，§4.10）。
pub fn app_daily_rows(conn: &Connection, from: &str, to: &str) -> Result<Vec<AppDayRow>> {
    let mut stmt = conn.prepare(
        "SELECT day, exe, foreground_secs, key_count, click_count FROM app_daily \
         WHERE day BETWEEN ?1 AND ?2 ORDER BY day, exe",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(AppDayRow {
            day: r.get(0)?,
            exe: r.get(1)?,
            seconds: i64_to_count(r.get(2)?),
            keys: i64_to_count(r.get(3)?),
            clicks: i64_to_count(r.get(4)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 组合键范围内聚合（按 (mods, code) 求和，count 降序）。
pub fn combos(conn: &Connection, from: &str, to: &str, limit: u32) -> Result<Vec<ComboRow>> {
    let mut stmt = conn.prepare(
        "SELECT mods, code, COALESCE(SUM(count), 0) FROM combo_daily \
         WHERE day BETWEEN ?1 AND ?2 \
         GROUP BY mods, code ORDER BY SUM(count) DESC, mods ASC, code ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, i64::from(limit)], |r| {
        Ok(ComboRow {
            mods: i64_to_u8(r.get(0)?),
            code: i64_to_u16(r.get(1)?),
            total: i64_to_count(r.get(2)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 组合键逐日明细（按 day, mods, code 升序；导出用）。
pub fn combo_daily_rows(conn: &Connection, from: &str, to: &str) -> Result<Vec<ComboDayRow>> {
    let mut stmt = conn.prepare(
        "SELECT day, mods, code, count FROM combo_daily \
         WHERE day BETWEEN ?1 AND ?2 ORDER BY day, mods, code",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(ComboDayRow {
            day: r.get(0)?,
            mods: i64_to_u8(r.get(1)?),
            code: i64_to_u16(r.get(2)?),
            count: i64_to_count(r.get(3)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// WhatPulse 查询（§4.5 wp_ 系列；全部只读 wp_* 镜像表）
// ---------------------------------------------------------------------------

/// WhatPulse 导入元数据；从未导入返回 `None`（GUI 显示引导态，§5.1）。
pub fn wp_meta(conn: &Connection) -> Result<Option<WpMetaRow>> {
    const SQL: &str = "SELECT imported_at, source_path, source_size, date_min, date_max, note \
         FROM wp_import_meta WHERE id = 1";
    let row = conn
        .query_row(SQL, [], |r| {
            Ok(WpMetaRow {
                imported_at: r.get(0)?,
                source_path: r.get(1)?,
                source_size: r.get(2)?,
                date_min: r.get(3)?,
                date_max: r.get(4)?,
                note: r.get(5)?,
            })
        })
        .optional()?;
    Ok(row)
}

/// WhatPulse 概览（§4.7 WpOverview）：
/// - `days`：逐日活跃度 = wp_key_daily.count + wp_mouse_daily.clicks（按 day 并集聚合）；
/// - `keys_total` / `combos_total` / `mouse_clicks_total`：各表范围内计数和；
/// - `apps_total`：范围内应用前台秒数总和。
pub fn wp_overview(conn: &Connection, from: &str, to: &str) -> Result<WpOverviewData> {
    let days = {
        let mut stmt = conn.prepare(
            "SELECT day, SUM(total) FROM (\
               SELECT day, SUM(count) AS total FROM wp_key_daily WHERE day BETWEEN ?1 AND ?2 GROUP BY day \
               UNION ALL \
               SELECT day, SUM(clicks) FROM wp_mouse_daily WHERE day BETWEEN ?1 AND ?2 GROUP BY day\
             ) GROUP BY day ORDER BY day",
        )?;
        let rows = stmt.query_map(params![from, to], day_count_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let keys_total = scalar_u64(
        conn,
        "SELECT COALESCE(SUM(count), 0) FROM wp_key_daily WHERE day BETWEEN ?1 AND ?2",
        params![from, to],
    )?;
    let combos_total = scalar_u64(
        conn,
        "SELECT COALESCE(SUM(count), 0) FROM wp_combo_daily WHERE day BETWEEN ?1 AND ?2",
        params![from, to],
    )?;
    let apps_total = scalar_u64(
        conn,
        "SELECT COALESCE(SUM(seconds), 0) FROM wp_app_daily WHERE day BETWEEN ?1 AND ?2",
        params![from, to],
    )?;
    let mouse_clicks_total = scalar_u64(
        conn,
        "SELECT COALESCE(SUM(clicks), 0) FROM wp_mouse_daily WHERE day BETWEEN ?1 AND ?2",
        params![from, to],
    )?;
    Ok(WpOverviewData { days, keys_total, combos_total, apps_total, mouse_clicks_total })
}

/// WhatPulse 按键 Top-N（范围内按 (qt_key, label) 聚合，count 降序）。
pub fn wp_top_keys(
    conn: &Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> Result<Vec<WpTopKeyRow>> {
    let mut stmt = conn.prepare(
        "SELECT qt_key, label, COALESCE(SUM(count), 0) FROM wp_key_daily \
         WHERE day BETWEEN ?1 AND ?2 \
         GROUP BY qt_key, label ORDER BY SUM(count) DESC, qt_key ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, i64::from(limit)], |r| {
        Ok(WpTopKeyRow { qt_key: r.get(0)?, label: r.get(1)?, total: i64_to_count(r.get(2)?) })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// WhatPulse 按键逐日明细（按 day, qt_key 升序）。
pub fn wp_key_daily_rows(conn: &Connection, from: &str, to: &str) -> Result<Vec<WpKeyDailyRow>> {
    let mut stmt = conn.prepare(
        "SELECT day, qt_key, label, count FROM wp_key_daily \
         WHERE day BETWEEN ?1 AND ?2 ORDER BY day, qt_key",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(WpKeyDailyRow {
            day: r.get(0)?,
            qt_key: r.get(1)?,
            label: r.get(2)?,
            count: i64_to_count(r.get(3)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// WhatPulse 组合键 Top-N（按 (combo, label) 聚合，count 降序）。
pub fn wp_top_combos(
    conn: &Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> Result<Vec<WpTopComboRow>> {
    let mut stmt = conn.prepare(
        "SELECT combo, label, COALESCE(SUM(count), 0) FROM wp_combo_daily \
         WHERE day BETWEEN ?1 AND ?2 \
         GROUP BY combo, label ORDER BY SUM(count) DESC, combo ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, i64::from(limit)], |r| {
        Ok(WpTopComboRow { combo: r.get(0)?, label: r.get(1)?, total: i64_to_count(r.get(2)?) })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// WhatPulse 组合键逐日明细（按 day, combo 升序）。
pub fn wp_combo_daily_rows(conn: &Connection, from: &str, to: &str) -> Result<Vec<WpComboDailyRow>> {
    let mut stmt = conn.prepare(
        "SELECT day, combo, label, count FROM wp_combo_daily \
         WHERE day BETWEEN ?1 AND ?2 ORDER BY day, combo",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(WpComboDailyRow {
            day: r.get(0)?,
            combo: r.get(1)?,
            label: r.get(2)?,
            count: i64_to_count(r.get(3)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// WhatPulse 应用 Top-N（按 (path, name) 聚合，秒数降序）。
pub fn wp_apps(conn: &Connection, from: &str, to: &str, limit: u32) -> Result<Vec<WpAppAggRow>> {
    let mut stmt = conn.prepare(
        "SELECT path, name, COALESCE(SUM(seconds), 0), COALESCE(SUM(keys), 0), \
         COALESCE(SUM(clicks), 0) FROM wp_app_daily \
         WHERE day BETWEEN ?1 AND ?2 \
         GROUP BY path, name ORDER BY SUM(seconds) DESC, path ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, i64::from(limit)], |r| {
        Ok(WpAppAggRow {
            path: r.get(0)?,
            name: r.get(1)?,
            seconds: i64_to_count(r.get(2)?),
            keys: i64_to_count(r.get(3)?),
            clicks: i64_to_count(r.get(4)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// WhatPulse 应用逐日明细（按 day, path 升序；导出用）。
pub fn wp_app_daily_rows(conn: &Connection, from: &str, to: &str) -> Result<Vec<WpAppDailyRow>> {
    let mut stmt = conn.prepare(
        "SELECT day, path, name, seconds, keys, clicks FROM wp_app_daily \
         WHERE day BETWEEN ?1 AND ?2 ORDER BY day, path",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(WpAppDailyRow {
            day: r.get(0)?,
            path: r.get(1)?,
            name: r.get(2)?,
            seconds: i64_to_count(r.get(3)?),
            keys: i64_to_count(r.get(4)?),
            clicks: i64_to_count(r.get(5)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// WhatPulse 鼠标逐日行（点击数 + 移动英寸；§4.7 WpMouseRow 逐日展示）。
pub fn wp_mouse(conn: &Connection, from: &str, to: &str) -> Result<Vec<WpMouseDailyRow>> {
    let mut stmt = conn.prepare(
        "SELECT day, clicks, distance_inches FROM wp_mouse_daily \
         WHERE day BETWEEN ?1 AND ?2 ORDER BY day",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(WpMouseDailyRow {
            day: r.get(0)?,
            clicks: i64_to_count(r.get(1)?),
            distance_inches: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// WhatPulse 鼠标按钮 Top-N（按 (button_code, label) 聚合，count 降序）。
pub fn wp_mouse_buttons(
    conn: &Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> Result<Vec<WpMouseButtonAggRow>> {
    let mut stmt = conn.prepare(
        "SELECT button_code, label, COALESCE(SUM(count), 0) FROM wp_mouse_buttons_daily \
         WHERE day BETWEEN ?1 AND ?2 \
         GROUP BY button_code, label ORDER BY SUM(count) DESC, button_code ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, i64::from(limit)], |r| {
        Ok(WpMouseButtonAggRow {
            button_code: r.get(0)?,
            label: r.get(1)?,
            total: i64_to_count(r.get(2)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// WhatPulse 滚轮方向 Top-N（按 (direction_code, label) 聚合，count 降序）。
pub fn wp_mouse_scrolls(
    conn: &Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> Result<Vec<WpMouseScrollAggRow>> {
    let mut stmt = conn.prepare(
        "SELECT direction_code, label, COALESCE(SUM(count), 0) FROM wp_mouse_scroll_daily \
         WHERE day BETWEEN ?1 AND ?2 \
         GROUP BY direction_code, label ORDER BY SUM(count) DESC, direction_code ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, i64::from(limit)], |r| {
        Ok(WpMouseScrollAggRow {
            direction_code: r.get(0)?,
            label: r.get(1)?,
            total: i64_to_count(r.get(2)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// 单测（PLAN §8-S4：reader 各查询返回 §4.5 形状；临时库）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDb;
    use crate::writer::{FlushBatch, Writer};
    use clrecoder_core::codes::mods;
    use clrecoder_core::event::DeviceKey;

    const DAY1: &str = "2026-09-27";
    const DAY2: &str = "2026-09-28";
    const RANGE_ALL: (&str, &str) = ("2026-09-01", "2026-09-30");

    /// 造数：3 设备（kb/mouse/pad，id 依插入序为 1/2/3）× 2 天 × input/combo/app。
    /// day1: kb 10+5=15, mouse 4 → 日合计 19；day2: kb 7, mouse 3, pad 2 → 日合计 12。
    fn seed(tag: &str) -> (TempDb, Writer) {
        let db = TempDb::new(tag);
        let w = Writer::open(db.as_ref()).unwrap();
        let kb = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0x04D9,
                pid: 0x0169,
                name: "测试键盘".into(),
            })
            .unwrap();
        let ms = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0x1532,
                pid: 0x0045,
                name: "测试鼠标".into(),
            })
            .unwrap();
        let gp = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 0,
                pid: 0,
                name: "XInput 手柄".into(),
            })
            .unwrap();
        assert_eq!((kb, ms, gp), (1, 2, 3), "依赖自增 id 顺序");
        w.flush(&FlushBatch {
            input: vec![
                (kb, DAY1.into(), 0x1E, 10),
                (kb, DAY1.into(), 0x1F, 5),
                (kb, DAY2.into(), 0x1E, 7),
                (ms, DAY1.into(), 1, 4),
                (ms, DAY2.into(), 6, 3),
                (gp, DAY2.into(), 1, 2),
            ],
            combos: vec![
                (DAY1.into(), mods::CTRL, 0x2E, 2),
                (DAY1.into(), mods::CTRL | mods::SHIFT, 0x2F, 1),
                (DAY2.into(), mods::ALT, 0x30, 4),
            ],
            apps: vec![
                (DAY1.into(), "code.exe".into(), 120, 15, 4),
                (DAY1.into(), "browser.exe".into(), 60, 0, 10),
                (DAY2.into(), "code.exe".into(), 30, 7, 1),
            ],
            ..Default::default()
        })
        .unwrap();
        (db, w)
    }

    /// §8-S4：devices() 返回 §4.5 形状（含 lifetime total、kind 解析、u16 vid/pid）。
    #[test]
    fn devices_returns_lifetime_totals() {
        let (_db, w) = seed("reader-devices");
        let rows = devices(&w.lock_conn()).unwrap();
        assert_eq!(rows.len(), 3);
        let kb = &rows[0];
        assert_eq!(
            (kb.id, kb.kind, kb.vid, kb.pid, kb.name.as_str(), kb.total),
            (1, DeviceKind::Keyboard, 0x04D9, 0x0169, "测试键盘", 22u64)
        );
        assert!(!kb.first_seen.is_empty() && !kb.last_seen.is_empty());
        assert_eq!((rows[1].kind, rows[1].total), (DeviceKind::Mouse, 7));
        assert_eq!((rows[2].kind, rows[2].total), (DeviceKind::Gamepad, 2));
    }

    /// daily_totals：全量与按 kind 过滤（kind 消歧靠 devices.kind）。
    #[test]
    fn daily_totals_filters_by_kind() {
        let (_db, w) = seed("reader-daily");
        let conn = w.lock_conn();
        let all = daily_totals(&conn, RANGE_ALL.0, RANGE_ALL.1, None).unwrap();
        assert_eq!(
            all.iter().map(|d| (d.day.as_str(), d.total)).collect::<Vec<_>>(),
            vec![(DAY1, 19), (DAY2, 12)]
        );
        let kb = daily_totals(&conn, RANGE_ALL.0, RANGE_ALL.1, Some(DeviceKind::Keyboard)).unwrap();
        assert_eq!(
            kb.iter().map(|d| (d.day.as_str(), d.total)).collect::<Vec<_>>(),
            vec![(DAY1, 15), (DAY2, 7)]
        );
        let ms = daily_totals(&conn, RANGE_ALL.0, RANGE_ALL.1, Some(DeviceKind::Mouse)).unwrap();
        assert_eq!(
            ms.iter().map(|d| (d.day.as_str(), d.total)).collect::<Vec<_>>(),
            vec![(DAY1, 4), (DAY2, 3)]
        );
        let gp = daily_totals(&conn, RANGE_ALL.0, RANGE_ALL.1, Some(DeviceKind::Gamepad)).unwrap();
        assert_eq!(gp.len(), 1);
        assert_eq!((gp[0].day.as_str(), gp[0].total), (DAY2, 2));
        // 窄范围裁剪
        let only1 = daily_totals(&conn, DAY1, DAY1, None).unwrap();
        assert_eq!(only1.len(), 1);
        assert_eq!(only1[0].total, 19);
        // 范围外为空
        assert!(daily_totals(&conn, "2020-01-01", "2020-01-31", None).unwrap().is_empty());
    }

    /// today_by_device：当日各设备合计，按 device_id 升序。
    #[test]
    fn today_by_device_shape() {
        let (_db, w) = seed("reader-today");
        let conn = w.lock_conn();
        let rows = today_by_device(&conn, DAY2).unwrap();
        assert_eq!(rows, vec![(1, 7), (2, 3), (3, 2)]);
        assert!(today_by_device(&conn, "2020-01-01").unwrap().is_empty());
    }

    /// key_daily / top_keys：§4.5 形状与排序（TopKeyRow.label 为空串，GUI keylabel 补）。
    #[test]
    fn key_daily_and_top_keys() {
        let (_db, w) = seed("reader-keys");
        let conn = w.lock_conn();
        let daily = key_daily(&conn, 1, RANGE_ALL.0, RANGE_ALL.1).unwrap();
        assert_eq!(
            daily.iter().map(|r| (r.day.as_str(), r.code, r.count)).collect::<Vec<_>>(),
            vec![(DAY1, 0x1E, 10), (DAY1, 0x1F, 5), (DAY2, 0x1E, 7)]
        );
        // 其他设备与范围外
        assert!(key_daily(&conn, 2, RANGE_ALL.0, RANGE_ALL.1).unwrap().len() == 2);
        assert!(key_daily(&conn, 1, "2020-01-01", "2020-01-31").unwrap().is_empty());

        let top1 = top_keys(&conn, 1, RANGE_ALL.0, RANGE_ALL.1, 1).unwrap();
        assert_eq!(top1.len(), 1);
        assert_eq!((top1[0].code, top1[0].total, top1[0].label.as_str()), (0x1E, 17, ""));
        let top2 = top_keys(&conn, 1, RANGE_ALL.0, RANGE_ALL.1, 2).unwrap();
        assert_eq!(
            top2.iter().map(|r| (r.code, r.total)).collect::<Vec<_>>(),
            vec![(0x1E, 17), (0x1F, 5)]
        );
    }

    /// apps / app_daily_rows：聚合按秒降序；逐日按 day, exe 升序。
    #[test]
    fn apps_and_app_daily_rows() {
        let (_db, w) = seed("reader-apps");
        let conn = w.lock_conn();
        let agg = apps(&conn, RANGE_ALL.0, RANGE_ALL.1, 10).unwrap();
        assert_eq!(agg.len(), 2);
        assert_eq!(
            (agg[0].exe.as_str(), agg[0].seconds, agg[0].keys, agg[0].clicks),
            ("code.exe", 150, 22, 5)
        );
        assert_eq!(
            (agg[1].exe.as_str(), agg[1].seconds, agg[1].keys, agg[1].clicks),
            ("browser.exe", 60, 0, 10)
        );
        let top1 = apps(&conn, RANGE_ALL.0, RANGE_ALL.1, 1).unwrap();
        assert_eq!(top1.len(), 1);
        assert_eq!(top1[0].exe, "code.exe");

        let daily = app_daily_rows(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap();
        assert_eq!(
            daily
                .iter()
                .map(|r| (r.day.as_str(), r.exe.as_str(), r.seconds, r.keys, r.clicks))
                .collect::<Vec<_>>(),
            vec![
                (DAY1, "browser.exe", 60, 0, 10),
                (DAY1, "code.exe", 120, 15, 4),
                (DAY2, "code.exe", 30, 7, 1),
            ]
        );
    }

    /// combos / combo_daily_rows：聚合 count 降序与逐日明细。
    #[test]
    fn combos_and_combo_daily_rows() {
        let (_db, w) = seed("reader-combos");
        let conn = w.lock_conn();
        let agg = combos(&conn, RANGE_ALL.0, RANGE_ALL.1, 10).unwrap();
        // count 降序：ALT+0x30(4) > CTRL+0x2E(2) > CTRL|SHIFT+0x2F(1)
        assert_eq!(
            agg.iter().map(|r| (r.mods, r.code, r.total)).collect::<Vec<_>>(),
            vec![(mods::ALT, 0x30, 4), (mods::CTRL, 0x2E, 2), (mods::CTRL | mods::SHIFT, 0x2F, 1)]
        );
        let top2 = combos(&conn, RANGE_ALL.0, RANGE_ALL.1, 2).unwrap();
        assert_eq!(top2.len(), 2);
        assert_eq!(top2[0].total, 4);

        let daily = combo_daily_rows(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap();
        assert_eq!(
            daily.iter().map(|r| (r.day.as_str(), r.mods, r.code, r.count)).collect::<Vec<_>>(),
            vec![
                (DAY1, mods::CTRL, 0x2E, 2),
                (DAY1, mods::CTRL | mods::SHIFT, 0x2F, 1),
                (DAY2, mods::ALT, 0x30, 4),
            ]
        );
    }

    /// overview：days 全种类合计；today = `to` 日按 kind 拆分；devices lifetime total。
    #[test]
    fn overview_returns_plan_shape() {
        let (_db, w) = seed("reader-overview");
        let conn = w.lock_conn();
        let ov = overview(&conn, DAY1, DAY2).unwrap();
        assert_eq!(
            ov.days.iter().map(|d| (d.day.as_str(), d.total)).collect::<Vec<_>>(),
            vec![(DAY1, 19), (DAY2, 12)]
        );
        // to=DAY2 的拆分
        assert_eq!(
            (ov.today.keys, ov.today.clicks, ov.today.gamepad),
            (7, 3, 2)
        );
        assert_eq!(ov.devices.len(), 3);
        assert_eq!((ov.devices[0].kind, ov.devices[0].total), (DeviceKind::Keyboard, 22));
        assert_eq!((ov.devices[1].kind, ov.devices[1].total), (DeviceKind::Mouse, 7));
        assert_eq!((ov.devices[2].kind, ov.devices[2].total), (DeviceKind::Gamepad, 2));

        // to=DAY1：拆分随 to 变化
        let ov1 = overview(&conn, DAY1, DAY1).unwrap();
        assert_eq!((ov1.today.keys, ov1.today.clicks, ov1.today.gamepad), (15, 4, 0));
    }

    /// §8-S4：wp_* 全家桶往返——meta/overview/top/daily/apps/mouse/buttons/scrolls 形状。
    #[test]
    fn wp_readers_return_plan_shapes() {
        let (_db, w) = seed("reader-wp");
        // 覆盖式导入（不影响自有表）
        w.rebuild_wp_tables(&crate::writer::WpImportBatch {
            meta: WpMetaRow {
                imported_at: "2026-09-28T12:00:00+08:00".into(),
                source_path: r"C:\wp\whatpulse.db".into(),
                source_size: Some(4096),
                date_min: Some(DAY1.into()),
                date_max: Some(DAY2.into()),
                note: "测试".into(),
            },
            keys: vec![
                WpKeyDailyRow { day: DAY1.into(), qt_key: 0x41, label: "A".into(), count: 30 },
                WpKeyDailyRow { day: DAY1.into(), qt_key: 0x42, label: "B".into(), count: 20 },
                WpKeyDailyRow { day: DAY2.into(), qt_key: 0x41, label: "A".into(), count: 10 },
            ],
            combos: vec![
                WpComboDailyRow {
                    day: DAY1.into(),
                    combo: "control,67".into(),
                    label: "Ctrl+C".into(),
                    count: 9,
                },
                WpComboDailyRow {
                    day: DAY2.into(),
                    combo: "shift,65".into(),
                    label: "Shift+A".into(),
                    count: 3,
                },
            ],
            apps: vec![
                WpAppDailyRow {
                    day: DAY1.into(),
                    path: "c:/dev/editor.exe".into(),
                    name: "Editor".into(),
                    seconds: 1000,
                    keys: 40,
                    clicks: 5,
                },
                WpAppDailyRow {
                    day: DAY1.into(),
                    path: "c:/web/browser.exe".into(),
                    name: "Browser".into(),
                    seconds: 2000,
                    keys: 10,
                    clicks: 25,
                },
                WpAppDailyRow {
                    day: DAY2.into(),
                    path: "c:/dev/editor.exe".into(),
                    name: "Editor".into(),
                    seconds: 500,
                    keys: 20,
                    clicks: 2,
                },
            ],
            mouse: vec![
                WpMouseDailyRow { day: DAY1.into(), clicks: 100, distance_inches: 10.5 },
                WpMouseDailyRow { day: DAY2.into(), clicks: 50, distance_inches: 4.25 },
            ],
            mouse_buttons: vec![
                WpMouseButtonDailyRow {
                    day: DAY1.into(),
                    button_code: 0,
                    label: "左键".into(),
                    count: 70,
                },
                WpMouseButtonDailyRow {
                    day: DAY1.into(),
                    button_code: 2,
                    label: "右键".into(),
                    count: 30,
                },
            ],
            mouse_scrolls: vec![
                WpMouseScrollDailyRow {
                    day: DAY1.into(),
                    direction_code: 1,
                    label: "向上".into(),
                    count: 12,
                },
            ],
        })
        .unwrap();

        let conn = w.lock_conn();
        // meta
        let meta = wp_meta(&conn).unwrap().expect("导入后必须有 meta");
        assert_eq!(meta.source_path, r"C:\wp\whatpulse.db");
        assert_eq!(meta.source_size, Some(4096));
        assert_eq!(meta.date_min.as_deref(), Some(DAY1));

        // overview
        let ov = wp_overview(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap();
        assert_eq!(
            ov.days.iter().map(|d| (d.day.as_str(), d.total)).collect::<Vec<_>>(),
            vec![(DAY1, 150), (DAY2, 60)], // keys + clicks，无重复计入
            "DAY1=50+100, DAY2=10+50"
        );
        assert_eq!(ov.keys_total, 60);
        assert_eq!(ov.combos_total, 12);
        assert_eq!(ov.apps_total, 3500, "seconds 总和");
        assert_eq!(ov.mouse_clicks_total, 150);

        // top keys / key daily
        let top = wp_top_keys(&conn, RANGE_ALL.0, RANGE_ALL.1, 2).unwrap();
        assert_eq!(
            top.iter().map(|r| (r.qt_key, r.label.as_str(), r.total)).collect::<Vec<_>>(),
            vec![(0x41, "A", 40), (0x42, "B", 20)]
        );
        let daily = wp_key_daily_rows(&conn, DAY2, DAY2).unwrap();
        assert_eq!(
            daily.iter().map(|r| (r.day.as_str(), r.qt_key, r.count)).collect::<Vec<_>>(),
            vec![(DAY2, 0x41, 10)]
        );

        // top combos / combo daily
        let ctop = wp_top_combos(&conn, RANGE_ALL.0, RANGE_ALL.1, 1).unwrap();
        assert_eq!(ctop.len(), 1);
        assert_eq!(
            (ctop[0].combo.as_str(), ctop[0].label.as_str(), ctop[0].total),
            ("control,67", "Ctrl+C", 9)
        );
        let cdaily = wp_combo_daily_rows(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap();
        assert_eq!(cdaily.len(), 2);

        // apps 聚合（秒降序）与逐日
        let apps = wp_apps(&conn, RANGE_ALL.0, RANGE_ALL.1, 10).unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(
            (apps[0].path.as_str(), apps[0].seconds, apps[0].keys, apps[0].clicks),
            ("c:/web/browser.exe", 2000, 10, 25)
        );
        assert_eq!((apps[1].seconds, apps[1].keys, apps[1].clicks), (1500, 60, 7));
        let adaily = wp_app_daily_rows(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap();
        assert_eq!(adaily.len(), 3);

        // mouse 逐日 + buttons/scrolls Top-N
        let mouse = wp_mouse(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap();
        assert_eq!(
            mouse.iter().map(|r| (r.day.as_str(), r.clicks, r.distance_inches)).collect::<Vec<_>>(),
            vec![(DAY1, 100, 10.5), (DAY2, 50, 4.25)]
        );
        let buttons = wp_mouse_buttons(&conn, RANGE_ALL.0, RANGE_ALL.1, 5).unwrap();
        assert_eq!(
            buttons
                .iter()
                .map(|r| (r.button_code, r.label.as_str(), r.total))
                .collect::<Vec<_>>(),
            vec![(0, "左键", 70), (2, "右键", 30)]
        );
        let scrolls = wp_mouse_scrolls(&conn, RANGE_ALL.0, RANGE_ALL.1, 5).unwrap();
        assert_eq!(
            scrolls
                .iter()
                .map(|r| (r.direction_code, r.label.as_str(), r.total))
                .collect::<Vec<_>>(),
            vec![(1, "向上", 12)]
        );
    }

    /// 未导入时 wp_meta 为 None；范围过滤为空；空库查询全部返回空集合（不报错）。
    #[test]
    fn wp_meta_none_when_absent_and_empty_queries_are_empty() {
        let (_db, w) = seed("reader-wp-empty");
        let conn = w.lock_conn();
        assert!(wp_meta(&conn).unwrap().is_none());
        let ov = wp_overview(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap();
        assert_eq!(
            (ov.days.len(), ov.keys_total, ov.combos_total, ov.apps_total, ov.mouse_clicks_total),
            (0, 0, 0, 0, 0)
        );
        assert!(wp_top_keys(&conn, RANGE_ALL.0, RANGE_ALL.1, 10).unwrap().is_empty());
        assert!(wp_key_daily_rows(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap().is_empty());
        assert!(wp_top_combos(&conn, RANGE_ALL.0, RANGE_ALL.1, 10).unwrap().is_empty());
        assert!(wp_combo_daily_rows(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap().is_empty());
        assert!(wp_apps(&conn, RANGE_ALL.0, RANGE_ALL.1, 10).unwrap().is_empty());
        assert!(wp_app_daily_rows(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap().is_empty());
        assert!(wp_mouse(&conn, RANGE_ALL.0, RANGE_ALL.1).unwrap().is_empty());
        assert!(wp_mouse_buttons(&conn, RANGE_ALL.0, RANGE_ALL.1, 10).unwrap().is_empty());
        assert!(wp_mouse_scrolls(&conn, RANGE_ALL.0, RANGE_ALL.1, 10).unwrap().is_empty());
        // 自有侧空范围
        assert!(devices(&conn).unwrap().len() == 3);
        assert!(key_daily(&conn, 1, RANGE_ALL.0, RANGE_ALL.1).unwrap().len() == 3);
        assert!(combos(&conn, "2020-01-01", "2020-01-31", 10).unwrap().is_empty());
        assert!(top_keys(&conn, 1, "2020-01-01", "2020-01-31", 10).unwrap().is_empty());
        assert!(apps(&conn, "2020-01-01", "2020-01-31", 10).unwrap().is_empty());
        assert!(app_daily_rows(&conn, "2020-01-01", "2020-01-31").unwrap().is_empty());
        assert!(combo_daily_rows(&conn, "2020-01-01", "2020-01-31").unwrap().is_empty());
    }
}
