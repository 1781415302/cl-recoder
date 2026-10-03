//! wp —— WhatPulse 只读查询 commands（PLAN §4.7 wp 系列；SQL 全部在 store::reader wp_* 函数）。
//!
//! 数据来源是导入镜像表 wp_*（§4.8 整体重建），本模块**只读**；从未导入 → 引导态空值。
//! 语义决策（§4.7 注释与 TS 形状共同约束）：
//! - `WpKeyRow/WpComboRow/WpAppRow` 的 TS 形状都带 `day` 字段，命令注释又要求"范围内
//!   Top-N、count/秒 降序、limit 截断"——落地为：取**逐日明细行**（导入时已按 day+维度聚合），
//!   按主指标降序排序后截断 limit。day 语义保全、Top-N 语义保全、JSON 导出复用同一行形状。
//! - `WpMouseButtonRow/WpMouseScrollRow` 无 day 字段 → 用 reader 的按码聚合函数。
//! - usability-runtime-v3 §4.6：`WpKeyRow` 带 `qtKey`、`WpAppRow` 带 `path`（均来自既有
//!   reader 行，camelCase 输出）——`day:qtKey`/`day:path` 即来源主键身份（schema 上
//!   wp_key_daily PK=(day,qt_key)、wp_app_daily PK=(day,path)），同名不同码/路径不冲突。

use serde::Serialize;

use crate::state::AppState;
use clrecoder_store::reader::{self, WpMetaRow, WpOverviewData};
use clrecoder_store as store;

/// WhatPulse 导入元数据 DTO（§4.7 `WpMeta`，camelCase）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WpMetaDto {
    /// 导入时刻（RFC3339）
    pub imported_at: String,
    /// 源库路径
    pub source_path: String,
    /// 源库大小（字节；未知为 null）
    pub source_size: Option<i64>,
    /// 源数据最早日期
    pub date_min: Option<String>,
    /// 源数据最晚日期
    pub date_max: Option<String>,
    /// 备注/警告补充
    pub note: String,
}

impl From<WpMetaRow> for WpMetaDto {
    fn from(m: WpMetaRow) -> Self {
        Self {
            imported_at: m.imported_at,
            source_path: m.source_path,
            source_size: m.source_size,
            date_min: m.date_min,
            date_max: m.date_max,
            note: m.note,
        }
    }
}

/// WhatPulse 概览 DTO（§4.7 `WpOverview`，camelCase）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WpOverviewDto {
    /// 逐日活跃度（按键 + 鼠标点击）
    pub days: Vec<super::overview::DayDto>,
    /// 范围内按键总数
    pub keys_total: u64,
    /// 范围内组合键总数
    pub combos_total: u64,
    /// 范围内应用使用秒数总和
    pub apps_total: u64,
    /// 范围内鼠标点击总数
    pub mouse_clicks_total: u64,
}

impl From<WpOverviewData> for WpOverviewDto {
    fn from(d: WpOverviewData) -> Self {
        Self {
            days: d
                .days
                .into_iter()
                .map(|x| super::overview::DayDto { day: x.day, total: x.total })
                .collect(),
            keys_total: d.keys_total,
            combos_total: d.combos_total,
            apps_total: d.apps_total,
            mouse_clicks_total: d.mouse_clicks_total,
        }
    }
}

/// WhatPulse 按键行 DTO（§4.7 `WpKeyRow`；usability-runtime-v3 §4.6 增加 qtKey 稳定身份）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WpKeyRow {
    /// 日期
    pub day: String,
    /// Qt 键码（usability-runtime-v3 §4.6：与 day 组成稳定身份 `day:qtKey`，
    /// 同名不同 Qt 码不冲突；来自既有 reader 行）
    pub qt_key: i64,
    /// 显示名（导入时由 qt_key_label 固化，不从码猜测）
    pub label: String,
    /// 当日次数
    pub count: u64,
}

/// WhatPulse 组合键行 DTO（§4.7 `WpComboRow`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WpComboRow {
    /// 日期
    pub day: String,
    /// 组合键原文（如 "shift,87"）
    pub combo: String,
    /// 解析后的友好格式（如 "Shift+W"）
    pub label: String,
    /// 当日次数
    pub count: u64,
}

/// WhatPulse 应用行 DTO（§4.7 `WpAppRow`；usability-runtime-v3 §4.6 增加 path 稳定身份）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WpAppRow {
    /// 日期
    pub day: String,
    /// 源 path 原样（usability-runtime-v3 §4.6：与 day 组成稳定身份 `day:path`，
    /// 同名不同路径不冲突；来自既有 reader 行）
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

/// WhatPulse 鼠标行 DTO（§4.7 `WpMouseRow`；米 = 源英寸 × 0.0254，GUI 层换算）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WpMouseRow {
    /// 日期
    pub day: String,
    /// 当日点击数
    pub clicks: u64,
    /// 当日移动距离（米）
    pub distance_meters: f64,
}

/// WhatPulse 鼠标按钮行 DTO（§4.7 `WpMouseButtonRow`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WpMouseButtonRow {
    /// 显示名
    pub label: String,
    /// 范围内总次数
    pub total: u64,
}

/// WhatPulse 滚轮方向行 DTO（§4.7 `WpMouseScrollRow`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WpMouseScrollRow {
    /// 显示名
    pub label: String,
    /// 范围内总次数
    pub total: u64,
}

/// 英寸 → 米（§4.7：换算在 GUI reader/DTO 层做）。
#[must_use]
pub fn inches_to_meters(inches: f64) -> f64 {
    inches * 0.0254
}

/// 内部查询（测试直连）。
pub(crate) fn query_wp_meta(conn: &rusqlite::Connection) -> store::Result<Option<WpMetaDto>> {
    Ok(reader::wp_meta(conn)?.map(WpMetaDto::from))
}

/// 内部查询（测试直连）。
pub(crate) fn query_wp_overview(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
) -> store::Result<WpOverviewDto> {
    Ok(reader::wp_overview(conn, from, to)?.into())
}

/// 内部查询（测试直连）：逐日按键明细按 count 降序截断 limit。
pub(crate) fn query_wp_keys(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> store::Result<Vec<WpKeyRow>> {
    let mut rows: Vec<WpKeyRow> = reader::wp_key_daily_rows(conn, from, to)?
        .into_iter()
        .map(|r| WpKeyRow { day: r.day, qt_key: r.qt_key, label: r.label, count: r.count })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    rows.truncate(limit as usize);
    Ok(rows)
}

/// 内部查询（测试直连）：逐日组合键明细按 count 降序截断 limit。
pub(crate) fn query_wp_combos(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> store::Result<Vec<WpComboRow>> {
    let mut rows: Vec<WpComboRow> = reader::wp_combo_daily_rows(conn, from, to)?
        .into_iter()
        .map(|r| WpComboRow { day: r.day, combo: r.combo, label: r.label, count: r.count })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.combo.cmp(&b.combo)));
    rows.truncate(limit as usize);
    Ok(rows)
}

/// 内部查询（测试直连）：逐日应用明细按秒降序截断 limit。
pub(crate) fn query_wp_apps(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> store::Result<Vec<WpAppRow>> {
    let mut rows: Vec<WpAppRow> = reader::wp_app_daily_rows(conn, from, to)?
        .into_iter()
        .map(|r| {
            WpAppRow {
                day: r.day,
                path: r.path,
                name: r.name,
                seconds: r.seconds,
                keys: r.keys,
                clicks: r.clicks,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.seconds.cmp(&a.seconds).then_with(|| a.name.cmp(&b.name)));
    rows.truncate(limit as usize);
    Ok(rows)
}

/// 内部查询（测试直连）：鼠标逐日（英寸 → 米）。
pub(crate) fn query_wp_mouse(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
) -> store::Result<Vec<WpMouseRow>> {
    Ok(reader::wp_mouse(conn, from, to)?
        .into_iter()
        .map(|r| WpMouseRow {
            day: r.day,
            clicks: r.clicks,
            distance_meters: inches_to_meters(r.distance_inches),
        })
        .collect())
}

/// 内部查询（测试直连）。
pub(crate) fn query_wp_mouse_buttons(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> store::Result<Vec<WpMouseButtonRow>> {
    Ok(reader::wp_mouse_buttons(conn, from, to, limit)?
        .into_iter()
        .map(|r| WpMouseButtonRow { label: r.label, total: r.total })
        .collect())
}

/// 内部查询（测试直连）。
pub(crate) fn query_wp_mouse_scrolls(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> store::Result<Vec<WpMouseScrollRow>> {
    Ok(reader::wp_mouse_scrolls(conn, from, to, limit)?
        .into_iter()
        .map(|r| WpMouseScrollRow { label: r.label, total: r.total })
        .collect())
}

/// 导入元数据（§4.7 `get_wp_meta() -> Option<WpMeta>`；未导入 → None 引导态）。
#[tauri::command]
pub async fn get_wp_meta(state: tauri::State<'_, AppState>) -> Result<Option<WpMetaDto>, String> {
    super::blocking_query(&state, query_wp_meta).await
}

/// WhatPulse 概览（§4.7 `get_wp_overview(from, to) -> WpOverview`）。
#[tauri::command]
pub async fn get_wp_overview(
    from: String,
    to: String,
    state: tauri::State<'_, AppState>,
) -> Result<WpOverviewDto, String> {
    super::blocking_query(&state, move |c| query_wp_overview(c, &from, &to)).await
}

/// WhatPulse 按键 Top-N（§4.7 `get_wp_keys(from, to, limit)`，count 降序）。
#[tauri::command]
pub async fn get_wp_keys(
    from: String,
    to: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<WpKeyRow>, String> {
    super::blocking_query(&state, move |c| query_wp_keys(c, &from, &to, limit)).await
}

/// WhatPulse 组合键 Top-N（§4.7 `get_wp_combos(from, to, limit)`，count 降序）。
#[tauri::command]
pub async fn get_wp_combos(
    from: String,
    to: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<WpComboRow>, String> {
    super::blocking_query(&state, move |c| query_wp_combos(c, &from, &to, limit)).await
}

/// WhatPulse 应用 Top-N（§4.7 `get_wp_apps(from, to, limit)`，秒降序）。
#[tauri::command]
pub async fn get_wp_apps(
    from: String,
    to: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<WpAppRow>, String> {
    super::blocking_query(&state, move |c| query_wp_apps(c, &from, &to, limit)).await
}

/// WhatPulse 鼠标逐日（§4.7 `get_wp_mouse(from, to)`，米 = 英寸 × 0.0254）。
#[tauri::command]
pub async fn get_wp_mouse(
    from: String,
    to: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<WpMouseRow>, String> {
    super::blocking_query(&state, move |c| query_wp_mouse(c, &from, &to)).await
}

/// WhatPulse 鼠标按钮 Top-N（§4.7 `get_wp_mouse_buttons(from, to, limit)`，按码聚合 count 降序）。
#[tauri::command]
pub async fn get_wp_mouse_buttons(
    from: String,
    to: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<WpMouseButtonRow>, String> {
    super::blocking_query(&state, move |c| query_wp_mouse_buttons(c, &from, &to, limit)).await
}

/// WhatPulse 滚轮方向 Top-N（§4.7 `get_wp_mouse_scrolls(from, to, limit)`）。
#[tauri::command]
pub async fn get_wp_mouse_scrolls(
    from: String,
    to: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<WpMouseScrollRow>, String> {
    super::blocking_query(&state, move |c| query_wp_mouse_scrolls(c, &from, &to, limit)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_store::writer::{WpImportBatch, Writer};
    use clrecoder_store::reader::{
        WpAppDailyRow, WpComboDailyRow, WpKeyDailyRow, WpMetaRow, WpMouseButtonDailyRow,
        WpMouseDailyRow, WpMouseScrollDailyRow,
    };

    /// 灌入 wp_* 镜像数据后走全部查询函数（reader 查询 + DTO 映射 + 排序/截断/米换算）。
    #[test]
    fn wp_queries_map_and_rank() {
        let f = TempFile::new("wp-queries", "db");
        let w = Writer::open(f.as_ref()).unwrap();
        w.rebuild_wp_tables(&WpImportBatch {
            meta: WpMetaRow {
                imported_at: "2026-09-28T12:00:00+08:00".into(),
                source_path: r"C:\wp\whatpulse.db".into(),
                source_size: Some(4096),
                date_min: Some("2026-09-27".into()),
                date_max: Some("2026-09-28".into()),
                note: "测试".into(),
            },
            keys: vec![
                WpKeyDailyRow { day: "2026-09-27".into(), qt_key: 0x41, label: "A".into(), count: 30 },
                WpKeyDailyRow { day: "2026-09-28".into(), qt_key: 0x42, label: "B".into(), count: 50 },
                WpKeyDailyRow { day: "2026-09-28".into(), qt_key: 0x43, label: "C".into(), count: 10 },
            ],
            combos: vec![
                WpComboDailyRow {
                    day: "2026-09-27".into(),
                    combo: "control,67".into(),
                    label: "Ctrl+C".into(),
                    count: 9,
                },
                WpComboDailyRow {
                    day: "2026-09-28".into(),
                    combo: "shift,65".into(),
                    label: "Shift+A".into(),
                    count: 3,
                },
            ],
            apps: vec![
                WpAppDailyRow {
                    day: "2026-09-27".into(),
                    path: "c:/dev/editor.exe".into(),
                    name: "Editor".into(),
                    seconds: 1000,
                    keys: 40,
                    clicks: 5,
                },
                WpAppDailyRow {
                    day: "2026-09-28".into(),
                    path: "c:/web/browser.exe".into(),
                    name: "Browser".into(),
                    seconds: 2000,
                    keys: 10,
                    clicks: 25,
                },
            ],
            mouse: vec![WpMouseDailyRow { day: "2026-09-27".into(), clicks: 100, distance_inches: 100.0 }],
            mouse_buttons: vec![WpMouseButtonDailyRow {
                day: "2026-09-27".into(),
                button_code: 0,
                label: "左键".into(),
                count: 70,
            }],
            mouse_scrolls: vec![WpMouseScrollDailyRow {
                day: "2026-09-27".into(),
                direction_code: 1,
                label: "向上".into(),
                count: 12,
            }],
        })
        .unwrap();
        drop(w);
        let conn = rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let (f0, t0) = ("2026-09-01", "2026-09-30");

        // meta
        let meta = query_wp_meta(&conn).unwrap().expect("已导入必须有 meta");
        assert_eq!(meta.source_path, r"C:\wp\whatpulse.db");
        let js = serde_json::to_string(&meta).unwrap();
        for k in [r#""importedAt":"#, r#""sourcePath":"#, r#""sourceSize":"#, r#""dateMin":"#, r#""dateMax":"#] {
            assert!(js.contains(k), "{js}");
        }

        // overview
        let ov = query_wp_overview(&conn, f0, t0).unwrap();
        assert_eq!(ov.keys_total, 90);
        assert_eq!(ov.combos_total, 12);
        assert_eq!(ov.apps_total, 3000);
        assert_eq!(ov.mouse_clicks_total, 100);
        let js = serde_json::to_string(&ov).unwrap();
        assert!(js.contains(r#""keysTotal":90"#) && js.contains(r#""mouseClicksTotal":100"#), "{js}");

        // keys Top-N：count 降序 + limit（qt_key 身份随行携带，§4.6）
        let keys = query_wp_keys(&conn, f0, t0, 2).unwrap();
        assert_eq!(
            keys.iter().map(|r| (r.label.as_str(), r.qt_key, r.count)).collect::<Vec<_>>(),
            vec![("B", 0x42, 50), ("A", 0x41, 30)]
        );

        // combos Top-N
        let combos = query_wp_combos(&conn, f0, t0, 1).unwrap();
        assert_eq!(combos[0].label, "Ctrl+C");
        assert_eq!(combos[0].count, 9);

        // apps Top-N：秒降序（path 身份随行携带，§4.6）
        let apps = query_wp_apps(&conn, f0, t0, 1).unwrap();
        assert_eq!(apps[0].name, "Browser");
        assert_eq!(apps[0].path, "c:/web/browser.exe");
        assert_eq!(apps[0].seconds, 2000);

        // mouse：英寸 → 米（×0.0254）
        let mouse = query_wp_mouse(&conn, f0, t0).unwrap();
        assert_eq!(mouse[0].clicks, 100);
        assert!((mouse[0].distance_meters - 2.54).abs() < 1e-9, "{:?}", mouse[0].distance_meters);

        // buttons / scrolls（无 day，按码聚合）
        let buttons = query_wp_mouse_buttons(&conn, f0, t0, 5).unwrap();
        assert_eq!(buttons[0].label, "左键");
        assert_eq!(buttons[0].total, 70);
        let scrolls = query_wp_mouse_scrolls(&conn, f0, t0, 5).unwrap();
        assert_eq!(scrolls[0].label, "向上");
    }

    /// §4.6（usability-runtime-v3）：WpKeyRow 携带 qt_key、WpAppRow 携带 path
    /// （均来自既有 reader 行，camelCase 输出，不从 label/name 猜测）；
    /// 同名不同 Qt 码 / 不同 path 不冲突——`day:qtKey`/`day:path` 唯一定位一行。
    #[test]
    fn usability_v3_wp_rows_carry_stable_identity() {
        let f = TempFile::new("wp-identity", "db");
        let w = Writer::open(f.as_ref()).unwrap();
        w.rebuild_wp_tables(&WpImportBatch {
            meta: WpMetaRow {
                imported_at: "2026-09-28T12:00:00+08:00".into(),
                source_path: r"C:\wp\whatpulse.db".into(),
                source_size: Some(1024),
                date_min: Some("2026-09-28".into()),
                date_max: Some("2026-09-28".into()),
                note: String::new(),
            },
            // 同日同名 "A" 但 Qt 码不同：0x41（字母）与 0x01000020（Qt_Shift）
            keys: vec![
                WpKeyDailyRow {
                    day: "2026-09-28".into(),
                    qt_key: 0x41,
                    label: "A".into(),
                    count: 30,
                },
                WpKeyDailyRow {
                    day: "2026-09-28".into(),
                    qt_key: 0x01000020,
                    label: "A".into(),
                    count: 5,
                },
            ],
            // 同日同名 "Editor" 但 path 不同
            apps: vec![
                WpAppDailyRow {
                    day: "2026-09-28".into(),
                    path: "c:/a/editor.exe".into(),
                    name: "Editor".into(),
                    seconds: 100,
                    keys: 4,
                    clicks: 1,
                },
                WpAppDailyRow {
                    day: "2026-09-28".into(),
                    path: "c:/b/editor.exe".into(),
                    name: "Editor".into(),
                    seconds: 60,
                    keys: 2,
                    clicks: 1,
                },
            ],
            ..Default::default()
        })
        .unwrap();
        drop(w);
        let conn = rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();

        let keys = query_wp_keys(&conn, "2026-09-01", "2026-09-30", 10).unwrap();
        assert_eq!(keys.len(), 2, "同名不同 Qt 码必须各自成行");
        assert_eq!(
            keys.iter().map(|r| (r.day.as_str(), r.qt_key, r.label.as_str(), r.count)).collect::<Vec<_>>(),
            vec![("2026-09-28", 0x41, "A", 30), ("2026-09-28", 0x01000020, "A", 5)]
        );
        let apps = query_wp_apps(&conn, "2026-09-01", "2026-09-30", 10).unwrap();
        assert_eq!(apps.len(), 2, "同名不同 path 必须各自成行");
        assert_eq!(
            apps.iter().map(|r| (r.day.as_str(), r.path.as_str(), r.name.as_str())).collect::<Vec<_>>(),
            vec![("2026-09-28", "c:/a/editor.exe", "Editor"), ("2026-09-28", "c:/b/editor.exe", "Editor")]
        );

        // camelCase 线形状：qtKey 在场且为码值、qt_key 缺席；path 在场
        let js = serde_json::to_string(&keys[0]).unwrap();
        assert!(js.contains(r#""qtKey":65"#) && !js.contains("qt_key"), "{js}");
        let japp = serde_json::to_string(&apps[0]).unwrap();
        assert!(japp.contains(r#""path":"c:/a/editor.exe""#), "{japp}");
    }

    /// 未导入：meta=None、全部查询为空（引导态）。
    #[test]
    fn wp_queries_empty_when_never_imported() {
        let (_f, conn) = crate::commands::testutil::seeded_db("wp-empty");
        assert!(query_wp_meta(&conn).unwrap().is_none());
        assert!(query_wp_keys(&conn, "2026-01-01", "2026-12-31", 10).unwrap().is_empty());
        assert!(query_wp_combos(&conn, "2026-01-01", "2026-12-31", 10).unwrap().is_empty());
        assert!(query_wp_apps(&conn, "2026-01-01", "2026-12-31", 10).unwrap().is_empty());
        assert!(query_wp_mouse(&conn, "2026-01-01", "2026-12-31").unwrap().is_empty());
        assert!(query_wp_mouse_buttons(&conn, "2026-01-01", "2026-12-31", 10).unwrap().is_empty());
        assert!(query_wp_mouse_scrolls(&conn, "2026-01-01", "2026-12-31", 10).unwrap().is_empty());
        let ov = query_wp_overview(&conn, "2026-01-01", "2026-12-31").unwrap();
        assert_eq!((ov.days.len(), ov.keys_total, ov.apps_total), (0, 0, 0));
    }

    /// 英寸 → 米换算锚点。
    #[test]
    fn inches_to_meters_constant() {
        assert!((inches_to_meters(0.0) - 0.0).abs() < 1e-12);
        assert!((inches_to_meters(100.0) - 2.54).abs() < 1e-9);
    }
}
