//! export —— CSV / JSON 导出（PLAN §4.10/§5.5）。
//!
//! - CSV：UTF-8 **带 BOM**（Excel 中文兼容）；每视图一文件，`{device}` = 设备 id（数字）；
//!   列 = §4.7 同名 TS 类型字段；WhatPulse 侧 `wp_keys_/wp_combos_/wp_apps_/wp_mouse_/
//!   wp_mouse_buttons_/wp_mouse_scrolls_`。
//! - JSON：单文件全量，顶层键逐字按 §4.10（`schema_version`/`generated_at`/`range`/
//!   `devices`/`input_daily`/`combos`/`apps`/`whatpulse`——文件格式键名为 plan 原文），
//!   数组元素 = §4.7 同名 TS 类型（camelCase DTO）；scope=own 省略 whatpulse 节点。
//!   文件格式 v2（correctness-v2 §4.7）：`input_daily` 每行带设备外键 `deviceId`
//!   （由外层设备循环注入，不从 code/label 猜测）；`schema_version=2` 只是导出文件
//!   格式版本，与 SQLite schema_migrations 无关；v1 旧文件保留原样，无反向导入。
//!
//! `path` 参数约定（前端对接，S12 按此传参）：`format="csv"` 时 `path` 为**目录**
//! （带扩展名时取其父目录，兼容保存框回传文件名）；`format="json"` 时 `path` 为**文件**。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::keylabel;
use crate::state::AppState;
use clrecoder_core::codes::DeviceKind;
use clrecoder_core::day;
use clrecoder_store::reader;

use super::wp;

/// CSV UTF-8 BOM（§4.10：Excel 中文兼容）。
pub const CSV_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// 导出错误（§9.2 executor 自主：不引入新依赖，手写 Display）。
#[derive(Debug)]
pub enum ExportError {
    /// 查询失败
    Store(clrecoder_store::StoreError),
    /// 文件写入失败
    Io(std::io::Error),
    /// JSON 序列化失败
    Json(serde_json::Error),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "查询失败: {e}"),
            Self::Io(e) => write!(f, "写入失败: {e}"),
            Self::Json(e) => write!(f, "JSON 序列化失败: {e}"),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<clrecoder_store::StoreError> for ExportError {
    fn from(e: clrecoder_store::StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for ExportError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// 导出范围（§4.7 `scope`："own"|"wp"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 自有数据
    Own,
    /// WhatPulse 导入镜像
    Wp,
}

impl Scope {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "own" => Some(Self::Own),
            "wp" => Some(Self::Wp),
            _ => None,
        }
    }
}

/// 导出报告（§4.7 `ExportReport`，camelCase）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    /// 是否成功
    pub ok: bool,
    /// 产出的文件绝对路径
    pub files: Vec<String>,
    /// 写出的数据行总数（CSV 数据行 / JSON 数组元素合计）
    pub rows: u64,
}

/// CSV 字段转义：含逗号/引号/换行时加引号并把内部引号翻倍（RFC 4180）。
pub(crate) fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// 写一个 CSV 文件：BOM + 表头行 + 数据行；返回 (路径, 数据行数)。
fn write_csv_file(
    path: &Path,
    header: &[&str],
    rows: Vec<Vec<String>>,
) -> Result<(PathBuf, u64), ExportError> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    out.write_all(CSV_BOM)?;
    let mut buf = String::new();
    buf.push_str(&header.iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(","));
    buf.push('\n');
    for row in &rows {
        buf.push_str(&row.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        buf.push('\n');
    }
    out.write_all(buf.as_bytes())?;
    out.flush()?;
    let n = rows.len() as u64;
    Ok((path.to_path_buf(), n))
}

/// CSV 导出（§4.10）：`dir` 为目标目录；返回 (文件, 数据行数) 列表。
pub fn export_csv(
    conn: &rusqlite::Connection,
    scope: Scope,
    from: &str,
    to: &str,
    dir: &Path,
) -> Result<Vec<(PathBuf, u64)>, ExportError> {
    std::fs::create_dir_all(dir)?;
    let suffix = format!("{from}_{to}");
    let mut files = Vec::new();

    match scope {
        Scope::Own => {
            // devices.csv（无范围后缀，§4.10）
            let devices = super::devices::query_devices(conn)?;
            let rows = devices
                .iter()
                .map(|d| {
                    vec![
                        d.id.to_string(),
                        serde_json::to_string(&d.kind).unwrap().trim_matches('"').to_string(),
                        d.vid.to_string(),
                        d.pid.to_string(),
                        d.name.clone(),
                        d.first_seen.clone(),
                        d.last_seen.clone(),
                        d.total.to_string(),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join("devices.csv"),
                &["id", "kind", "vid", "pid", "name", "firstSeen", "lastSeen", "total"],
                rows,
            )?);

            // keys_{device}_{from}_{to}.csv：每个键盘设备一个文件（§4.10）
            for d in &devices {
                if d.kind != DeviceKind::Keyboard {
                    continue;
                }
                let daily = reader::key_daily(conn, d.id, from, to)?;
                let rows = daily
                    .iter()
                    .map(|r| {
                        vec![
                            r.day.clone(),
                            keylabel::code_label(DeviceKind::Keyboard, r.code),
                            r.code.to_string(),
                            r.count.to_string(),
                        ]
                    })
                    .collect();
                files.push(write_csv_file(
                    &dir.join(format!("keys_{}_{}.csv", d.id, suffix)),
                    &["date", "key", "code", "count"],
                    rows,
                )?);
            }

            // apps_{from}_{to}.csv（逐日明细，name = basename 显示名）
            let apps = reader::app_daily_rows(conn, from, to)?;
            let rows = apps
                .iter()
                .map(|r| {
                    vec![
                        r.day.clone(),
                        r.exe.clone(),
                        r.exe.clone(),
                        r.seconds.to_string(),
                        r.keys.to_string(),
                        r.clicks.to_string(),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("apps_{suffix}.csv")),
                &["date", "exe", "name", "seconds", "keys", "clicks"],
                rows,
            )?);

            // combos_{from}_{to}.csv（逐日明细 + label）
            let combos = reader::combo_daily_rows(conn, from, to)?;
            let rows = combos
                .iter()
                .map(|r| {
                    vec![
                        r.day.clone(),
                        r.mods.to_string(),
                        r.code.to_string(),
                        keylabel::combo_label(r.mods, r.code),
                        r.count.to_string(),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("combos_{suffix}.csv")),
                &["date", "mods", "code", "label", "count"],
                rows,
            )?);
        }
        Scope::Wp => {
            let keys = reader::wp_key_daily_rows(conn, from, to)?;
            let rows = keys
                .iter()
                .map(|r| vec![r.day.clone(), r.label.clone(), r.count.to_string()])
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("wp_keys_{suffix}.csv")),
                &["day", "label", "count"],
                rows,
            )?);

            let combos = reader::wp_combo_daily_rows(conn, from, to)?;
            let rows = combos
                .iter()
                .map(|r| {
                    vec![r.day.clone(), r.combo.clone(), r.label.clone(), r.count.to_string()]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("wp_combos_{suffix}.csv")),
                &["day", "combo", "label", "count"],
                rows,
            )?);

            let apps = reader::wp_app_daily_rows(conn, from, to)?;
            let rows = apps
                .iter()
                .map(|r| {
                    vec![
                        r.day.clone(),
                        r.name.clone(),
                        r.seconds.to_string(),
                        r.keys.to_string(),
                        r.clicks.to_string(),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("wp_apps_{suffix}.csv")),
                &["day", "name", "seconds", "keys", "clicks"],
                rows,
            )?);

            let mouse = reader::wp_mouse(conn, from, to)?;
            let rows = mouse
                .iter()
                .map(|r| {
                    vec![
                        r.day.clone(),
                        r.clicks.to_string(),
                        format!("{}", wp::inches_to_meters(r.distance_inches)),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("wp_mouse_{suffix}.csv")),
                &["day", "clicks", "distanceMeters"],
                rows,
            )?);

            let buttons = reader::wp_mouse_buttons(conn, from, to, u32::MAX)?;
            let rows = buttons
                .iter()
                .map(|r| vec![r.label.clone(), r.total.to_string()])
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("wp_mouse_buttons_{suffix}.csv")),
                &["label", "total"],
                rows,
            )?);

            let scrolls = reader::wp_mouse_scrolls(conn, from, to, u32::MAX)?;
            let rows = scrolls
                .iter()
                .map(|r| vec![r.label.clone(), r.total.to_string()])
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("wp_mouse_scrolls_{suffix}.csv")),
                &["label", "total"],
                rows,
            )?);
        }
    }
    Ok(files)
}

/// JSON 导出 input_daily 行（correctness-v2 §4.7 导出专用 DTO，camelCase）：
/// 在 §4.7 `KeyDailyRowLabeled` 基础上增加设备外键；GUI 查询 DTO（keys.rs）不变。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportInputDailyRow {
    /// 所属设备 id（外层设备循环注入，不从 code/label 猜测；须存在于根 devices[].id）
    device_id: i64,
    /// 日期
    day: String,
    /// 归一化 scancode（或鼠标/手柄 code）
    code: u16,
    /// 当日次数
    count: u64,
    /// 显示名（GUI keylabel 填充）
    label: String,
}

/// JSON 导出（§4.10 单文件全量；v2 文件格式：input_daily 每行带设备外键 deviceId）。
/// 返回 (文件路径, 数组元素总数)。
pub fn export_json(
    conn: &rusqlite::Connection,
    scope: Scope,
    from: &str,
    to: &str,
    file: &Path,
) -> Result<(PathBuf, u64), ExportError> {
    let devices = super::devices::query_devices(conn)?;
    // input_daily：全设备逐日 + 设备外键（correctness-v2 §4.7：deviceId 由外层设备
    // 循环注入；设备顺序 = query_devices 的 id 顺序，每设备维持 reader 的 day/code 排序）
    let mut input_daily: Vec<ExportInputDailyRow> = Vec::new();
    for d in &devices {
        input_daily.extend(
            super::keys::query_key_daily(conn, d.id, from, to)?.into_iter().map(|r| {
                ExportInputDailyRow {
                    device_id: d.id,
                    day: r.day,
                    code: r.code,
                    count: r.count,
                    label: r.label,
                }
            }),
        );
    }
    let combos = super::combos::query_combos(conn, from, to, u32::MAX)?;
    let apps = super::apps::query_apps(conn, from, to, u32::MAX)?;

    let mut root = serde_json::json!({
        // 导出文件格式版本 v2（非 SQLite schema_migrations 版本）
        "schema_version": 2,
        "generated_at": day::now_local_rfc3339(),
        "range": { "from": from, "to": to },
        "devices": devices,
        "input_daily": input_daily,
        "combos": combos,
        "apps": apps,
    });
    let mut rows = devices.len() as u64
        + input_daily.len() as u64
        + combos.len() as u64
        + apps.len() as u64;

    if scope == Scope::Wp {
        let meta = super::wp::query_wp_meta(conn)?;
        let keys = super::wp::query_wp_keys(conn, from, to, u32::MAX)?;
        let combos = super::wp::query_wp_combos(conn, from, to, u32::MAX)?;
        let apps = super::wp::query_wp_apps(conn, from, to, u32::MAX)?;
        let mouse = super::wp::query_wp_mouse(conn, from, to)?;
        let buttons = super::wp::query_wp_mouse_buttons(conn, from, to, u32::MAX)?;
        let scrolls = super::wp::query_wp_mouse_scrolls(conn, from, to, u32::MAX)?;
        rows += keys.len() as u64
            + combos.len() as u64
            + apps.len() as u64
            + mouse.len() as u64
            + buttons.len() as u64
            + scrolls.len() as u64;
        let whatpulse = serde_json::json!({
            "meta": meta,
            "keys": keys,
            "combos": combos,
            "apps": apps,
            "mouse": mouse,
            "buttons": buttons,
            "scrolls": scrolls,
        });
        root.as_object_mut()
            .expect("json! 顶层必为 object")
            .insert("whatpulse".into(), whatpulse);
    }

    if let Some(parent) = file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let text = serde_json::to_string_pretty(&root)?;
    std::fs::write(file, text).map_err(ExportError::from)?;
    Ok((file.to_path_buf(), rows))
}

/// `path` → CSV 目标目录（带扩展名时取父目录，兼容保存框回传文件名）。
fn csv_dir(path: &Path) -> PathBuf {
    if path.extension().is_some() {
        path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    } else {
        path.to_path_buf()
    }
}

/// 导出（§4.7 `export_data(format, scope, from, to, path) -> ExportReport`）。
///
/// 失败返回 `Err`（导出是用户显式动作，应报错而不是静默空文件）。
/// - `format`：`"csv"`（path=目录）| `"json"`（path=文件）；
/// - `scope`：`"own"` | `"wp"`；
/// - `from`/`to`：规范 `YYYY-MM-DD` 且 `from <= to`。
#[tauri::command]
pub async fn export_data(
    format: String,
    scope: String,
    from: String,
    to: String,
    path: String,
    state: tauri::State<'_, AppState>,
) -> Result<ExportReport, String> {
    let fmt = match format.as_str() {
        "csv" => Format::Csv,
        "json" => Format::Json,
        other => return Err(format!("未知导出格式: {other}（应为 csv|json）")),
    };
    let scp = Scope::parse(&scope)
        .ok_or_else(|| format!("未知导出范围: {scope}（应为 own|wp）"))?;
    if !day::valid_range(&from, &to) {
        return Err(format!("日期范围无效: from={from} to={to}（须为 YYYY-MM-DD 且 from<=to）"));
    }
    if path.trim().is_empty() {
        return Err("导出路径为空".to_string());
    }

    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // with_ro 的错误 → String；导出自身的 ExportError → String（导出错误要如实上报）
        match st.with_ro(|conn| Ok(run_export(conn, fmt, scp, &from, &to, &path))) {
            Ok(Ok(report)) => Ok(report),
            Ok(Err(msg)) => Err(msg),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| format!("导出任务失败: {e}"))?
}

/// 导出格式（模块内枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// CSV（多视图多文件）
    Csv,
    /// JSON（单文件全量）
    Json,
}

/// 导出主体（command 与错误类型已在此收敛为 String 文案）。
fn run_export(
    conn: &rusqlite::Connection,
    fmt: Format,
    scp: Scope,
    from: &str,
    to: &str,
    path: &str,
) -> Result<ExportReport, String> {
    let target = PathBuf::from(path);
    let (files, rows) = match fmt {
        Format::Csv => {
            let dir = csv_dir(&target);
            let files = export_csv(conn, scp, from, to, &dir).map_err(|e| e.to_string())?;
            (
                files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
                files.iter().map(|(_, n)| *n).sum::<u64>(),
            )
        }
        Format::Json => {
            let (p, n) = export_json(conn, scp, from, to, &target).map_err(|e| e.to_string())?;
            (vec![p], n)
        }
    };
    Ok(ExportReport {
        ok: true,
        files: files.into_iter().map(|p| p.to_string_lossy().into_owned()).collect(),
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_core::codes::mods;
    use clrecoder_core::event::DeviceKey;
    use clrecoder_store::writer::{FlushBatch, WpImportBatch, Writer};
    use clrecoder_store::reader::{WpKeyDailyRow, WpMetaRow};

    const FROM: &str = "2026-01-01";
    const TO: &str = "2026-12-31";

    /// 造自有数据（含逗号 exe 验证 CSV 转义）+ wp 镜像。
    fn seed(tag: &str) -> (TempFile, rusqlite::Connection) {
        let f = TempFile::new(tag, "db");
        let w = Writer::open(f.as_ref()).unwrap();
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
                vid: 1,
                pid: 2,
                name: "测试鼠标".into(),
            })
            .unwrap();
        w.flush(&FlushBatch {
            input: vec![
                (kb, "2026-09-28".into(), 0x1E, 10),
                (kb, "2026-09-28".into(), 0x2C, 4),
                (ms, "2026-09-28".into(), 1, 7),
            ],
            combos: vec![("2026-09-28".into(), mods::CTRL, 0x2E, 3)],
            apps: vec![
                ("2026-09-28".into(), "code.exe".into(), 120, 14, 4),
                ("2026-09-28".into(), "my,app.exe".into(), 30, 0, 2), // 逗号 → CSV 引号转义
            ],
            ..Default::default()
        })
        .unwrap();
        w.rebuild_wp_tables(&WpImportBatch {
            meta: WpMetaRow {
                imported_at: "2026-09-28T12:00:00+08:00".into(),
                source_path: r"C:\wp\whatpulse.db".into(),
                source_size: Some(1024),
                date_min: Some("2026-09-28".into()),
                date_max: Some("2026-09-28".into()),
                note: String::new(),
            },
            keys: vec![WpKeyDailyRow {
                day: "2026-09-28".into(),
                qt_key: 0x41,
                label: "A".into(),
                count: 55,
            }],
            ..Default::default()
        })
        .unwrap();
        drop(w);
        let conn = rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        (f, conn)
    }

    /// 验收点（§8-S10）：CSV 带 BOM；每视图一文件；列名 = §4.7 TS 字段；转义正确。
    #[test]
    fn csv_export_writes_bom_and_views() {
        let (_f, conn) = seed("export-csv");
        let dir = TempFile::new("export-csv-dir", "dir");
        std::fs::create_dir_all(&dir).unwrap();

        let files = export_csv(&conn, Scope::Own, FROM, TO, dir.as_ref()).unwrap();
        let names: Vec<String> =
            files.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"devices.csv".to_string()), "{names:?}");
        assert!(names.contains(&format!("keys_1_{FROM}_{TO}.csv").to_string()), "{names:?}");
        assert!(names.contains(&format!("apps_{FROM}_{TO}.csv").to_string()), "{names:?}");
        assert!(names.contains(&format!("combos_{FROM}_{TO}.csv").to_string()), "{names:?}");
        // 鼠标设备不得产出 keys_ 视图（keys 只属于键盘设备）
        assert!(!names.iter().any(|n| n.starts_with("keys_2_")), "{names:?}");

        // BOM 逐字节 + 表头
        let keys_path = dir.as_ref().join(format!("keys_1_{FROM}_{TO}.csv"));
        let bytes = std::fs::read(&keys_path).unwrap();
        assert_eq!(&bytes[..3], CSV_BOM, "keys CSV 必须以 UTF-8 BOM 开头");
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        assert!(text.starts_with("date,key,code,count\n"), "{text}");
        assert!(text.contains("2026-09-28,A,30,10"), "{text}");

        // devices.csv 列 = §4.7 DeviceRow TS 字段
        let dev_text = std::fs::read_to_string(dir.as_ref().join("devices.csv")).unwrap();
        assert!(dev_text.contains("id,kind,vid,pid,name,firstSeen,lastSeen,total"), "{dev_text}");
        assert!(dev_text.contains("测试键盘"), "{dev_text}");

        // 逗号字段转义（apps CSV：exe "my,app.exe" 被引号包裹）
        let apps_text = std::fs::read_to_string(dir.as_ref().join(format!("apps_{FROM}_{TO}.csv"))).unwrap();
        assert!(apps_text.contains("\"my,app.exe\""), "{apps_text}");

        // combos CSV 列与 label（剥 BOM 后比对——文件本就必须以 BOM 开头）
        let combos_bytes = std::fs::read(dir.as_ref().join(format!("combos_{FROM}_{TO}.csv"))).unwrap();
        assert_eq!(&combos_bytes[..3], CSV_BOM, "combos CSV 同样必须带 BOM");
        let combos_text = String::from_utf8(combos_bytes[3..].to_vec()).unwrap();
        assert!(combos_text.starts_with("date,mods,code,label,count\n"), "{combos_text}");
        assert!(combos_text.contains(&format!("2026-09-28,1,46,Ctrl+{},3", keylabel::key_label(0x2E))), "{combos_text}");

        // wp scope：六视图 + BOM + 米换算
        let wp_dir = TempFile::new("export-wp-dir", "dir");
        std::fs::create_dir_all(&wp_dir).unwrap();
        let wp_files = export_csv(&conn, Scope::Wp, FROM, TO, wp_dir.as_ref()).unwrap();
        let wp_names: Vec<String> = wp_files
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        for expect in ["wp_keys_", "wp_combos_", "wp_apps_", "wp_mouse_", "wp_mouse_buttons_", "wp_mouse_scrolls_"] {
            assert!(wp_names.iter().any(|n| n.starts_with(expect)), "{wp_names:?}");
        }
        let wp_keys = std::fs::read(wp_dir.as_ref().join(format!("wp_keys_{FROM}_{TO}.csv"))).unwrap();
        assert_eq!(&wp_keys[..3], CSV_BOM, "wp CSV 同样必须带 BOM");
        let wp_keys_text = String::from_utf8(wp_keys[3..].to_vec()).unwrap();
        assert!(wp_keys_text.starts_with("day,label,count\n"), "{wp_keys_text}");
        assert!(wp_keys_text.contains("2026-09-28,A,55"), "{wp_keys_text}");
    }

    /// 验收点（§8-S10）：JSON 可解析；顶层键逐字按 §4.10；scope=own 无 whatpulse 节点。
    #[test]
    fn json_export_parses_and_matches_plan_shape() {
        let (_f, conn) = seed("export-json");
        let out = TempFile::new("export-json", "json");

        let (path, rows) = export_json(&conn, Scope::Own, FROM, TO, out.as_ref()).unwrap();
        assert_eq!(path, out.as_ref().to_path_buf());
        assert!(rows > 0);
        let text = std::fs::read_to_string(out.as_ref()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).expect("JSON 必须可解析");
        // §4.10 顶层键逐字（schema_version=2 为导出文件格式版本，非 SQLite 迁移版本）
        assert_eq!(v["schema_version"], 2);
        assert!(v["generated_at"].as_str().unwrap().len() == 25);
        assert_eq!(v["range"]["from"], FROM);
        assert_eq!(v["range"]["to"], TO);
        assert_eq!(v["devices"].as_array().unwrap().len(), 2);
        assert!(v["input_daily"].as_array().unwrap().len() >= 3);
        assert_eq!(v["combos"].as_array().unwrap().len(), 1);
        assert_eq!(v["apps"].as_array().unwrap().len(), 2);
        // 元素 = §4.7 camelCase TS 形状（v2：input_daily 每行带 deviceId 外键）
        assert!(v["devices"][0].get("firstSeen").is_some(), "{}", v["devices"][0]);
        assert!(v["input_daily"][0].get("deviceId").is_some(), "{}", v["input_daily"][0]);
        assert!(v["input_daily"][0].get("label").is_some(), "{}", v["input_daily"][0]);
        assert!(v["apps"][0].get("name").is_some(), "{}", v["apps"][0]);
        // scope=own 省略 whatpulse
        assert!(v.get("whatpulse").is_none(), "own 导出不得包含 whatpulse 节点");

        // wp 导出：whatpulse 节点齐全
        let out2 = TempFile::new("export-json-wp", "json");
        export_json(&conn, Scope::Wp, FROM, TO, out2.as_ref()).unwrap();
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out2.as_ref()).unwrap()).unwrap();
        let wp_node = v2.get("whatpulse").expect("wp 导出必须有 whatpulse 节点");
        for k in ["meta", "keys", "combos", "apps", "mouse", "buttons", "scrolls"] {
            assert!(wp_node.get(k).is_some(), "whatpulse 缺 {k}");
        }
        assert_eq!(wp_node["keys"][0]["label"], "A");
        assert!(wp_node["mouse"].as_array().is_some_and(|a| a.is_empty()), "fixture 无 wp 鼠标数据");
    }

    /// v2 外键 fixture（correctness-v2 §4.7）：两个键盘同 day 同 code 各自独立 + 鼠标 + 手柄 + wp 镜像。
    fn seed_v2(tag: &str) -> (TempFile, rusqlite::Connection) {
        let f = TempFile::new(tag, "db");
        let w = Writer::open(f.as_ref()).unwrap();
        let kb1 = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0x04D9,
                pid: 0x0169,
                name: "键盘A".into(),
            })
            .unwrap();
        let kb2 = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0x1234,
                pid: 0x5678,
                name: "键盘B".into(),
            })
            .unwrap();
        let ms = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 1,
                pid: 2,
                name: "测试鼠标".into(),
            })
            .unwrap();
        let gp = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 3,
                pid: 4,
                name: "测试手柄".into(),
            })
            .unwrap();
        w.flush(&FlushBatch {
            input: vec![
                (kb1, "2026-09-28".into(), 0x1E, 10), // 与 kb2 同 day 同 code → 独立两行
                (kb2, "2026-09-28".into(), 0x1E, 7),
                (kb1, "2026-09-27".into(), 0x2C, 4),
                (ms, "2026-09-28".into(), 1, 5),
                (gp, "2026-09-28".into(), 1, 3),
            ],
            combos: vec![("2026-09-28".into(), mods::CTRL, 0x2E, 2)],
            apps: vec![("2026-09-28".into(), "code.exe".into(), 60, 8, 2)],
            ..Default::default()
        })
        .unwrap();
        w.rebuild_wp_tables(&WpImportBatch {
            meta: WpMetaRow {
                imported_at: "2026-09-28T12:00:00+08:00".into(),
                source_path: r"C:\wp\whatpulse.db".into(),
                source_size: Some(1024),
                date_min: Some("2026-09-28".into()),
                date_max: Some("2026-09-28".into()),
                note: String::new(),
            },
            keys: vec![WpKeyDailyRow {
                day: "2026-09-28".into(),
                qt_key: 0x41,
                label: "A".into(),
                count: 55,
            }],
            ..Default::default()
        })
        .unwrap();
        drop(w);
        let conn = rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        (f, conn)
    }

    /// 验收点（correctness-v2 §8.2-S7）：v2 `input_daily` 每行带 `deviceId` 外键；
    /// 两键盘同 day/code 独立成行；鼠标/手柄 id 在场且均在根 devices 内；
    /// 每设备记录维持 day/code 排序；序列化逐字 camelCase（deviceId 在场、device_id 缺席）；
    /// rows 仍为各数组元素合计，不因新增字段变化。
    #[test]
    fn correctness_v2_json_input_daily_rows_carry_device_foreign_key() {
        let (_f, conn) = seed_v2("export-fk");
        let out = TempFile::new("export-fk", "json");

        let (path, rows) = export_json(&conn, Scope::Own, FROM, TO, out.as_ref()).unwrap();
        assert_eq!(path, out.as_ref().to_path_buf());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.as_ref()).unwrap()).unwrap();

        assert_eq!(v["schema_version"], 2, "JSON 文件格式版本 v2（非 SQLite 迁移版本）");

        // 根 devices 按现有查询 id 顺序
        let devices = v["devices"].as_array().unwrap();
        let ids: Vec<i64> = devices.iter().map(|d| d["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![1, 2, 3, 4], "{devices:?}");

        // 两键盘同 day/code（2026-09-28 code 30）各自独立成行，不合并、不串号
        let daily = v["input_daily"].as_array().unwrap();
        let row_of = |dev: i64| {
            daily
                .iter()
                .find(|r| r["deviceId"].as_i64() == Some(dev) && r["code"] == 30)
                .unwrap_or_else(|| panic!("deviceId={dev} 缺 2026-09-28 code=30 行: {daily:?}"))
        };
        let r1 = row_of(1);
        let r2 = row_of(2);
        assert_eq!(r1["day"], "2026-09-28");
        assert_eq!(r1["count"], 10);
        assert_eq!(r1["label"], "A", "QWERTY 下 0x1E 应为 A");
        assert_eq!(r2["day"], "2026-09-28");
        assert_eq!(r2["count"], 7);
        assert_eq!(r2["label"], "A");

        // 每条 deviceId 必须在根 devices 存在；鼠标/手柄 id 在场
        for r in daily {
            let dev = r["deviceId"].as_i64().expect("deviceId 必须存在且为数字");
            assert!(ids.contains(&dev), "deviceId={dev} 不在 devices {ids:?}: {r}");
        }
        assert!(daily.iter().any(|r| r["deviceId"] == 3), "鼠标行缺 deviceId: {daily:?}");
        assert!(daily.iter().any(|r| r["deviceId"] == 4), "手柄行缺 deviceId: {daily:?}");

        // 每设备记录维持 day/code 排序（外层循环分设备注入）
        for dev in &ids {
            let dev_rows: Vec<(String, u64)> = daily
                .iter()
                .filter(|r| r["deviceId"].as_i64() == Some(*dev))
                .map(|r| (r["day"].as_str().unwrap().to_string(), r["code"].as_u64().unwrap()))
                .collect();
            let mut sorted = dev_rows.clone();
            sorted.sort();
            assert_eq!(dev_rows, sorted, "deviceId={dev} 须按 day/code 有序");
        }

        // camelCase 逐字：deviceId 在场，snake_case device_id 缺席
        let js = serde_json::to_string(&daily[0]).unwrap();
        assert!(js.contains(r#""deviceId":"#), "{js}");
        assert!(!js.contains("device_id"), "不得序列化 snake_case: {js}");

        // rows = devices + input_daily + combos + apps 元素数精确合计（新增字段不影响 rows）
        let expect = devices.len() as u64
            + daily.len() as u64
            + v["combos"].as_array().unwrap().len() as u64
            + v["apps"].as_array().unwrap().len() as u64;
        assert_eq!(rows, expect, "rows 须等于各数组元素合计");
        assert_eq!(daily.len(), 5, "5 条输入记录（含同 day/code 两键盘独立行）: {daily:?}");
    }

    /// 验收点（S7）：日期过滤——无数据日期导出得空数组（devices 为全量仍在场），
    /// 有数据日期精确过滤；两种情形 rows 都等于元素合计。
    #[test]
    fn correctness_v2_json_date_filter_and_empty_arrays() {
        let (_f, conn) = seed_v2("export-filter");

        // 范围外（数据都在 2026-09-28）：三数组全空
        let miss = TempFile::new("export-filter-miss", "json");
        let (_, rows) =
            export_json(&conn, Scope::Own, "2026-09-29", "2026-09-30", miss.as_ref()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(miss.as_ref()).unwrap()).unwrap();
        assert_eq!(v["schema_version"], 2);
        assert!(v["input_daily"].as_array().unwrap().is_empty(), "范围外不得有输入行");
        assert!(v["combos"].as_array().unwrap().is_empty(), "范围外不得有组合行");
        assert!(v["apps"].as_array().unwrap().is_empty(), "范围外不得有应用行");
        assert_eq!(v["devices"].as_array().unwrap().len(), 4, "devices 为全量，不随范围过滤");
        assert_eq!(rows, 4, "空数组时 rows = devices 数");

        // 命中 2026-09-28：仅该日 4 条输入（09-27 的 kb1 0x2C 被过滤）
        let hit = TempFile::new("export-filter-hit", "json");
        let (_, rows_hit) =
            export_json(&conn, Scope::Own, "2026-09-28", "2026-09-28", hit.as_ref()).unwrap();
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(hit.as_ref()).unwrap()).unwrap();
        let daily = v2["input_daily"].as_array().unwrap();
        assert_eq!(daily.len(), 4, "仅 2026-09-28 的 4 条输入: {daily:?}");
        assert!(daily.iter().all(|r| r["day"] == "2026-09-28"), "{daily:?}");
        assert_eq!(v2["combos"].as_array().unwrap().len(), 1);
        assert_eq!(v2["apps"].as_array().unwrap().len(), 1);
        let expect = v2["devices"].as_array().unwrap().len() as u64
            + daily.len() as u64
            + v2["combos"].as_array().unwrap().len() as u64
            + v2["apps"].as_array().unwrap().len() as u64;
        assert_eq!(rows_hit, expect, "命中范围 rows 同样精确");
    }

    /// 验收点（S7）：scope=wp 仍导出 own 根字段（v2 + deviceId）并追加 whatpulse 节点，
    /// 不得改成 WP-only；rows = own 四数组 + whatpulse 六数组元素精确合计。
    #[test]
    fn correctness_v2_json_wp_scope_appends_whatpulse_and_rows_exact() {
        let (_f, conn) = seed_v2("export-wp2");
        let out = TempFile::new("export-wp2", "json");
        let (_, rows) = export_json(&conn, Scope::Wp, FROM, TO, out.as_ref()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.as_ref()).unwrap()).unwrap();

        // own 根字段齐全且为 v2 形状
        assert_eq!(v["schema_version"], 2);
        assert_eq!(v["devices"].as_array().unwrap().len(), 4);
        let daily = v["input_daily"].as_array().unwrap();
        assert!(daily.iter().all(|r| r["deviceId"].as_i64().is_some()), "{daily:?}");
        assert_eq!(v["combos"].as_array().unwrap().len(), 1);
        assert_eq!(v["apps"].as_array().unwrap().len(), 1);

        let wp = v.get("whatpulse").expect("wp 导出必须在 own 根字段外追加 whatpulse 节点");
        for k in ["meta", "keys", "combos", "apps", "mouse", "buttons", "scrolls"] {
            assert!(wp.get(k).is_some(), "whatpulse 缺 {k}");
        }
        assert_eq!(wp["keys"][0]["label"], "A");
        assert_eq!(wp["keys"][0]["count"], 55);

        // rows = own 四数组 + whatpulse 六数组元素合计（meta 为对象不计）
        let arr_len = |x: &serde_json::Value| x.as_array().map_or(0, |a| a.len()) as u64;
        let expect = v["devices"].as_array().unwrap().len() as u64
            + daily.len() as u64
            + arr_len(&v["combos"])
            + arr_len(&v["apps"])
            + arr_len(&wp["keys"])
            + arr_len(&wp["combos"])
            + arr_len(&wp["apps"])
            + arr_len(&wp["mouse"])
            + arr_len(&wp["buttons"])
            + arr_len(&wp["scrolls"]);
        assert_eq!(rows, expect, "wp scope rows 须等于各数组元素合计");
    }

    /// CSV 转义规则（RFC 4180）。
    #[test]
    fn csv_field_escaping() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
        assert_eq!(csv_field(""), "");
    }

    /// csv_dir 规则：带扩展名取父目录，纯目录原样。
    #[test]
    fn csv_dir_resolution() {
        assert_eq!(csv_dir(Path::new("C:/tmp/out.csv")), PathBuf::from("C:/tmp"));
        assert_eq!(csv_dir(Path::new("C:/tmp/exports")), PathBuf::from("C:/tmp/exports"));
    }
}
