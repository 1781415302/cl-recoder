//! export —— CSV / JSON 导出（PLAN §4.10/§5.5 + motion-dpi §6.3）。
//!
//! - CSV：UTF-8 **带 BOM**（Excel 中文兼容）；每视图一文件，`{device}` = 设备 id（数字）；
//!   列 = §4.7 同名 TS 类型字段；WhatPulse 侧 `wp_keys_/wp_combos_/wp_apps_/wp_mouse_/
//!   wp_mouse_buttons_/wp_mouse_scrolls_`。motion-dpi §6.3 起 scope=own 追加五个运动
//!   视图：`mouse_sources`（无范围后缀，同 devices.csv 惯例）与 `mouse_motion_/
//!   gamepad_motion_/gamepad_heat_/legacy_mouse_motion_`（带 `{from}_{to}` 后缀），
//!   列顺序逐字对应 §6.3 行类型；scope=wp 只出既有 WP 文件，不重新解释 WP 英寸。
//!   运动视图的 null 单元格写空字符串（不写字面 null）。
//! - JSON：单文件全量，顶层键逐字按 §4.10（`schema_version`/`generated_at`/`range`/
//!   `devices`/`input_daily`/`combos`/`apps`/`motion`/`whatpulse`——文件格式键名为
//!   plan 原文），数组元素 = §4.7 同名 TS 类型（camelCase DTO）；scope=own 省略
//!   whatpulse 节点。文件格式 v2（correctness-v2 §4.7）：`input_daily` 每行带设备外键
//!   `deviceId`（由外层设备循环注入，不从 code/label 猜测）。文件格式 v3（motion-dpi
//!   §6.3）：v2 语义保留并新增 `motion` 节点（§6.3 `MotionExportRows`，camelCase；
//!   legacy 行带常量 `quality="legacy_uncalibrated"`，source_key/path 不导出）；
//!   scope=wp 时 motion 仍表达自有数据；`schema_version=3` 只是导出文件格式版本，
//!   与 SQLite schema_migrations 无关；v1/v2 旧文件保留原样，无反向导入。
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
use clrecoder_core::motion::{DpiOrigin, StickSide};
use clrecoder_store::motion;
use clrecoder_store::reader;

use super::wp;

/// CSV UTF-8 BOM（§4.10：Excel 中文兼容）。
pub const CSV_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// legacy 鼠标逐日行的 quality 常量（motion-dpi §6.3：旧算法记录，未校准——
/// 只说明旧算法原始量，不归给物理来源、不套当前 DPI 换算米）。
pub const LEGACY_MOUSE_QUALITY: &str = "legacy_uncalibrated";

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

            // motion-dpi §6.3：五个运动视图（数据经 store::motion::export_motion_rows；
            // 列顺序逐字对应 §6.3 行类型 camelCase 字段。mouse_sources 无范围后缀，
            // 同 devices.csv 惯例；null 单元格写空字符串；BOM/转义复用 write_csv_file）
            let motion_rows = motion::export_motion_rows(conn, from, to)?;

            // mouse_sources.csv：只列区间内有运动桶的来源（source_key 不导出）
            let rows = motion_rows
                .mice
                .iter()
                .map(|m| {
                    vec![
                        m.source_id.to_string(),
                        m.device_id.to_string(),
                        m.name.clone(),
                        m.manual_dpi.map_or_else(String::new, |d| d.to_string()),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join("mouse_sources.csv"),
                &["sourceId", "deviceId", "name", "manualDpi"],
                rows,
            )?);

            // mouse_motion_{from}_{to}.csv：逐桶行（unknown 桶 dpi/meters 为空单元格）
            let rows = motion_rows
                .mouse_daily
                .iter()
                .map(|r| {
                    vec![
                        r.source_id.to_string(),
                        r.day.clone(),
                        r.dpi.map_or_else(String::new, |d| d.to_string()),
                        serde_json::to_string(&r.dpi_origin).unwrap().trim_matches('"').to_string(),
                        r.counts.to_string(),
                        r.meters.map_or_else(String::new, |m| m.to_string()),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("mouse_motion_{suffix}.csv")),
                &["sourceId", "day", "dpi", "dpiOrigin", "counts", "meters"],
                rows,
            )?);

            // gamepad_motion_{from}_{to}.csv：摇杆逐日运动行
            let rows = motion_rows
                .gamepad_daily
                .iter()
                .map(|r| {
                    vec![
                        r.device_id.to_string(),
                        r.day.clone(),
                        serde_json::to_string(&r.stick).unwrap().trim_matches('"').to_string(),
                        r.active_us.to_string(),
                        r.travel_r.to_string(),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("gamepad_motion_{suffix}.csv")),
                &["deviceId", "day", "stick", "activeUs", "travelR"],
                rows,
            )?);

            // gamepad_heat_{from}_{to}.csv：停留热力行（空格本就不落行，导出即稀疏行）
            let rows = motion_rows
                .gamepad_heat
                .iter()
                .map(|r| {
                    vec![
                        r.device_id.to_string(),
                        r.day.clone(),
                        serde_json::to_string(&r.stick).unwrap().trim_matches('"').to_string(),
                        r.bin.to_string(),
                        r.dwell_us.to_string(),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("gamepad_heat_{suffix}.csv")),
                &["deviceId", "day", "stick", "bin", "dwellUs"],
                rows,
            )?);

            // legacy_mouse_motion_{from}_{to}.csv：旧算法逐日行（原始量 + quality 常量，
            // 不换算米、不归给物理来源）
            let rows = motion_rows
                .legacy_mouse_daily
                .iter()
                .map(|r| {
                    vec![
                        r.device_id.to_string(),
                        r.day.clone(),
                        r.raw_counts.to_string(),
                        LEGACY_MOUSE_QUALITY.to_string(),
                    ]
                })
                .collect();
            files.push(write_csv_file(
                &dir.join(format!("legacy_mouse_motion_{suffix}.csv")),
                &["deviceId", "day", "rawCounts", "quality"],
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

// ---------------------------------------------------------------------------
// JSON 导出 motion 节点 DTO（motion-dpi §6.3 `MotionExportRows`，camelCase）。
// store 侧导出行类型（store::motion::Export*Row）为 snake_case 内存类型；线上形状按
// §6.3 TS 逐字 camelCase，故做 DTO adapter（与 GUI 查询命令同惯例）。legacy 行的
// `quality` 常量在此层添加（§6.3），source_key/path 不出现在任何导出行。
// ---------------------------------------------------------------------------

/// JSON 导出：鼠标来源行（`mice`，只列区间内有运动桶的来源）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportMouseSourceRowDto {
    /// 来源 id（`mouse_motion_sources.id`）
    source_id: i64,
    /// 型号设备 id（须存在于根 devices[].id）
    device_id: i64,
    /// 型号显示名
    name: String,
    /// 手动配置 DPI
    manual_dpi: Option<u32>,
}

impl From<motion::ExportMouseSourceRow> for ExportMouseSourceRowDto {
    fn from(r: motion::ExportMouseSourceRow) -> Self {
        Self {
            source_id: r.source_id,
            device_id: r.device_id,
            name: r.name,
            manual_dpi: r.manual_dpi,
        }
    }
}

/// JSON 导出：鼠标运动逐桶行（unknown 桶 dpi=None、meters=None）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportMouseMotionRowDto {
    /// 来源 id
    source_id: i64,
    /// 日期
    day: String,
    /// 桶 DPI（unknown 桶为 null）
    dpi: Option<u32>,
    /// DPI 取值来源
    dpi_origin: DpiOrigin,
    /// 该桶 counts
    counts: f64,
    /// 该桶折算米数（仅 dpi>0）
    meters: Option<f64>,
}

impl From<motion::ExportMouseMotionRow> for ExportMouseMotionRowDto {
    fn from(r: motion::ExportMouseMotionRow) -> Self {
        Self {
            source_id: r.source_id,
            day: r.day,
            dpi: r.dpi,
            dpi_origin: r.dpi_origin,
            counts: r.counts,
            meters: r.meters,
        }
    }
}

/// JSON 导出：手柄摇杆逐日运动行。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportGamepadMotionRowDto {
    /// 型号设备 id
    device_id: i64,
    /// 日期
    day: String,
    /// 摇杆侧
    stick: StickSide,
    /// 当日活动微秒
    active_us: u64,
    /// 当日累计路程（R）
    travel_r: f64,
}

impl From<motion::ExportGamepadMotionRow> for ExportGamepadMotionRowDto {
    fn from(r: motion::ExportGamepadMotionRow) -> Self {
        Self {
            device_id: r.device_id,
            day: r.day,
            stick: r.stick,
            active_us: r.active_us,
            travel_r: r.travel_r,
        }
    }
}

/// JSON 导出：手柄停留热力行（空格本就不落行，导出即稀疏行）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportGamepadHeatRowDto {
    /// 型号设备 id
    device_id: i64,
    /// 日期
    day: String,
    /// 摇杆侧
    stick: StickSide,
    /// 热力格号（0..=624，row-major）
    bin: u16,
    /// 该格停留微秒
    dwell_us: u64,
}

impl From<motion::ExportGamepadHeatRow> for ExportGamepadHeatRowDto {
    fn from(r: motion::ExportGamepadHeatRow) -> Self {
        Self {
            device_id: r.device_id,
            day: r.day,
            stick: r.stick,
            bin: r.bin,
            dwell_us: r.dwell_us,
        }
    }
}

/// JSON 导出：旧算法鼠标移动逐日行（原始量；米数/倍率不回写，quality 常量在此层添加）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportLegacyMouseRowDto {
    /// 型号设备 id
    device_id: i64,
    /// 日期
    day: String,
    /// 旧算法原始累计量（distance_inches×80）
    raw_counts: f64,
    /// 固定常量（§6.3：`legacy_uncalibrated`）
    quality: &'static str,
}

impl From<motion::ExportLegacyMouseRow> for ExportLegacyMouseRowDto {
    fn from(r: motion::ExportLegacyMouseRow) -> Self {
        Self { device_id: r.device_id, day: r.day, raw_counts: r.raw_counts, quality: LEGACY_MOUSE_QUALITY }
    }
}

/// JSON 导出 motion 节点（§6.3 `MotionExportRows`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportMotionNodeDto {
    /// 鼠标来源行
    mice: Vec<ExportMouseSourceRowDto>,
    /// 鼠标运动逐桶行
    mouse_daily: Vec<ExportMouseMotionRowDto>,
    /// 手柄摇杆逐日运动行
    gamepad_daily: Vec<ExportGamepadMotionRowDto>,
    /// 手柄停留热力行
    gamepad_heat: Vec<ExportGamepadHeatRowDto>,
    /// 旧算法鼠标移动逐日行
    legacy_mouse_daily: Vec<ExportLegacyMouseRowDto>,
}

impl ExportMotionNodeDto {
    /// rows 口径（§6.3"rows 包括新增各数组元素"）：motion 节点各数组元素合计。
    fn rows(&self) -> u64 {
        self.mice.len() as u64
            + self.mouse_daily.len() as u64
            + self.gamepad_daily.len() as u64
            + self.gamepad_heat.len() as u64
            + self.legacy_mouse_daily.len() as u64
    }
}

/// 查询 motion 导出行并转 DTO（数据经 store::motion::export_motion_rows 获取：
/// from/to 过滤全部 daily、mice 只列 motion 引用的来源、旧 schema 新运动数组全空
/// 或仅 legacy 可读部分——不报导出成功却遗漏可读数据）。
fn export_motion_node(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
) -> Result<ExportMotionNodeDto, ExportError> {
    let rows = motion::export_motion_rows(conn, from, to)?;
    Ok(ExportMotionNodeDto {
        mice: rows.mice.into_iter().map(ExportMouseSourceRowDto::from).collect(),
        mouse_daily: rows.mouse_daily.into_iter().map(ExportMouseMotionRowDto::from).collect(),
        gamepad_daily: rows.gamepad_daily.into_iter().map(ExportGamepadMotionRowDto::from).collect(),
        gamepad_heat: rows.gamepad_heat.into_iter().map(ExportGamepadHeatRowDto::from).collect(),
        legacy_mouse_daily: rows
            .legacy_mouse_daily
            .into_iter()
            .map(ExportLegacyMouseRowDto::from)
            .collect(),
    })
}

/// JSON 导出（§4.10 单文件全量；v3 文件格式：v2 语义保留 + motion 节点，motion-dpi §6.3）。
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
    // motion 节点：自有数据（scope=wp 时同样只表达自有数据，motion-dpi §6.3）
    let motion_node = export_motion_node(conn, from, to)?;

    let mut root = serde_json::json!({
        // 导出文件格式版本 v3（motion-dpi §6.3；非 SQLite schema_migrations 版本）
        "schema_version": 3,
        "generated_at": day::now_local_rfc3339(),
        "range": { "from": from, "to": to },
        "devices": devices,
        "input_daily": input_daily,
        "combos": combos,
        "apps": apps,
        "motion": motion_node,
    });
    let mut rows = devices.len() as u64
        + input_daily.len() as u64
        + combos.len() as u64
        + apps.len() as u64
        + motion_node.rows();

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
    use clrecoder_core::motion::{MouseSourceDescriptor, StickBinDelta};
    use clrecoder_store::motion::{MouseMotionWrite, StickMotionWrite};
    use clrecoder_store::writer::{FlushBatch, WpImportBatch, Writer};
    use clrecoder_store::reader::{WpKeyDailyRow, WpMetaRow, WpMouseDailyRow};

    const FROM: &str = "2026-01-01";
    const TO: &str = "2026-12-31";

    /// motion fixture 固定日（DAY/DAY2 两日各有运动桶，供范围过滤验证）。
    const DAY: &str = "2026-09-28";
    const DAY2: &str = "2026-09-29";

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
        // §4.10 顶层键逐字（schema_version=3 为导出文件格式版本，非 SQLite 迁移版本）
        assert_eq!(v["schema_version"], 3);
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

        assert_eq!(
            v["schema_version"],
            3,
            "JSON 文件格式版本 v3（motion-dpi §6.3 起，非 SQLite 迁移版本）"
        );

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
        assert_eq!(v["schema_version"], 3);
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

        // own 根字段齐全且为 v3 形状（motion 节点为自有数据，见 motion_dpi_ 用例）
        assert_eq!(v["schema_version"], 3);
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

    /// 只读连接（与 seed 系列打开方式一致）。
    fn ro_conn(f: &TempFile) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    /// motion 导出 fixture（motion-dpi §6.3）：v3 库 + 同型号鼠标两来源（A 有桶/B 仅注册
    /// 无桶，验证 mice 只列 motion 引用的来源）+ 手柄摇杆两日数据 + 旧算法行 + WP 镜像
    /// （原英寸单位）。返回 (文件, ro 连接, 鼠标型号 id, 来源A id, 手柄 id)。
    fn seed_motion(tag: &str) -> (TempFile, rusqlite::Connection, i64, i64, i64) {
        let f = TempFile::new(tag, "db");
        let w = Writer::open(f.as_ref()).unwrap();
        // 鼠标型号名带逗号 → CSV 引号转义验证（来源注册与型号行共用同一 DeviceKey）
        let model = DeviceKey {
            kind: DeviceKind::Mouse,
            vid: 0x1532,
            pid: 0x0045,
            name: "测试,鼠标".into(),
        };
        let ms = w.get_or_create_device(&model).unwrap();
        let gp = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 3,
                pid: 4,
                name: "测试手柄".into(),
            })
            .unwrap();
        let a = w
            .register_mouse_source(&MouseSourceDescriptor {
                source_key: r"\\?\hid#vid_1532&pid_0045&mi_00".into(),
                model: model.clone(),
                interface_path: None,
                physical: true,
            })
            .unwrap();
        w.register_mouse_source(&MouseSourceDescriptor {
            source_key: r"\\?\hid#vid_1532&pid_0045&mi_01".into(), // 来源 B：区间内无桶
            model: model.clone(),
            interface_path: None,
            physical: true,
        })
        .unwrap();
        w.flush(&FlushBatch {
            mouse_motion: vec![
                MouseMotionWrite {
                    source_id: a,
                    day: DAY.into(),
                    dpi: 800,
                    origin: DpiOrigin::Manual,
                    counts: 800.0,
                },
                MouseMotionWrite {
                    source_id: a,
                    day: DAY.into(),
                    dpi: 0,
                    origin: DpiOrigin::Unknown,
                    counts: 400.0,
                },
                MouseMotionWrite {
                    source_id: a,
                    day: DAY2.into(),
                    dpi: 800,
                    origin: DpiOrigin::Manual,
                    counts: 100.0,
                },
            ],
            stick_motion: vec![
                StickMotionWrite {
                    device_id: gp,
                    day: DAY.into(),
                    side: StickSide::Left,
                    active_us: 1_500_000,
                    travel_r: 0.707,
                    bins: vec![
                        StickBinDelta { bin: 312, dwell_us: 1_000_000 },
                        StickBinDelta { bin: 313, dwell_us: 500_000 },
                    ],
                },
                StickMotionWrite {
                    device_id: gp,
                    day: DAY.into(),
                    side: StickSide::Right,
                    active_us: 250_000,
                    travel_r: 0.0,
                    bins: vec![StickBinDelta { bin: 324, dwell_us: 250_000 }],
                },
                StickMotionWrite {
                    device_id: gp,
                    day: DAY2.into(),
                    side: StickSide::Left,
                    active_us: 100_000,
                    travel_r: 0.1,
                    bins: vec![StickBinDelta { bin: 312, dwell_us: 100_000 }],
                },
            ],
            mouse_move: vec![(ms, DAY.into(), 1.0)], // 旧算法：1 英寸 → ×80 原始量
            ..Default::default()
        })
        .unwrap();
        w.rebuild_wp_tables(&WpImportBatch {
            meta: WpMetaRow {
                imported_at: "2026-09-28T12:00:00+08:00".into(),
                source_path: r"C:\wp\whatpulse.db".into(),
                source_size: Some(1024),
                date_min: Some(DAY.into()),
                date_max: Some(DAY.into()),
                note: String::new(),
            },
            mouse: vec![WpMouseDailyRow { day: DAY.into(), clicks: 10, distance_inches: 2.0 }],
            ..Default::default()
        })
        .unwrap();
        drop(w);
        // 来源 A 的手动 DPI（配置写入与 S6 GUI 同一 store API；manualDpi 列与运动桶独立）
        {
            let rw = rusqlite::Connection::open(f.as_ref()).unwrap();
            motion::set_manual_dpi(&rw, a, Some(800)).unwrap();
        }
        let conn = ro_conn(&f);
        (f, conn, ms, a, gp)
    }

    /// 验收点（motion-dpi §8-S9）：JSON v3 —— motion 节点在场、camelCase 逐字（无
    /// snake_case 泄漏）、mice 只列 motion 引用的来源、mouseDaily.sourceId 与 mice/
    /// deviceId 与根 devices 外键齐全、unknown 桶 dpi/meters=null、legacy 行带 quality
    /// 常量、source_key/path 不导出、rows 含 motion 各数组元素。
    #[test]
    fn motion_dpi_json_v3_motion_node_camel_case_foreign_keys_and_rows() {
        let (_f, conn, ms, a, _gp) = seed_motion("export-motion-json");
        let out = TempFile::new("export-motion-json", "json");

        let (_, rows) = export_json(&conn, Scope::Own, FROM, TO, out.as_ref()).unwrap();
        let text = std::fs::read_to_string(out.as_ref()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).expect("JSON 必须可解析");

        assert_eq!(v["schema_version"], 3, "v3 文件格式（motion-dpi §6.3）");
        let motion = v.get("motion").expect("v3 导出必须有 motion 节点");

        // mice：只列有运动桶的来源（B 注册但无桶 → 不在场）
        let mice = motion["mice"].as_array().unwrap();
        assert_eq!(mice.len(), 1, "{mice:?}");
        assert_eq!(mice[0]["sourceId"], a);
        assert_eq!(mice[0]["deviceId"], ms);
        assert_eq!(mice[0]["name"], "测试,鼠标");
        assert_eq!(mice[0]["manualDpi"], 800);

        // mouseDaily：3 桶；unknown 桶 dpi/meters=null；manual 桶按桶内 DPI 折算米
        let daily = motion["mouseDaily"].as_array().unwrap();
        assert_eq!(daily.len(), 3, "{daily:?}");
        let manual = daily.iter().find(|r| r["dpi"] == 800).unwrap();
        assert_eq!(manual["sourceId"], a);
        assert_eq!(manual["day"], DAY);
        assert_eq!(manual["dpiOrigin"], "manual");
        assert_eq!(manual["counts"], 800.0);
        assert_eq!(manual["meters"], 0.0254, "800/800×0.0254");
        let unknown = daily.iter().find(|r| r["dpi"].is_null()).unwrap();
        assert_eq!(unknown["day"], DAY);
        assert_eq!(unknown["dpiOrigin"], "unknown");
        assert_eq!(unknown["counts"], 400.0);
        assert!(unknown["meters"].is_null(), "unknown 桶 meters=null: {unknown}");
        // 完整 source 外键：每条 mouseDaily.sourceId 都在 mice 中
        let mice_ids: Vec<i64> = mice.iter().map(|m| m["sourceId"].as_i64().unwrap()).collect();
        assert!(
            daily.iter().all(|r| mice_ids.contains(&r["sourceId"].as_i64().unwrap())),
            "{daily:?}"
        );

        // gamepadDaily/gamepadHeat：deviceId 必须在根 devices 中
        let device_ids: Vec<i64> = v["devices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_i64().unwrap())
            .collect();
        let gdaily = motion["gamepadDaily"].as_array().unwrap();
        assert_eq!(gdaily.len(), 3, "{gdaily:?}");
        assert!(gdaily.iter().all(|r| device_ids.contains(&r["deviceId"].as_i64().unwrap())));
        let left = gdaily.iter().find(|r| r["stick"] == "left" && r["day"] == DAY).unwrap();
        assert_eq!(left["activeUs"], 1_500_000);
        assert_eq!(left["travelR"], 0.707);
        let heat = motion["gamepadHeat"].as_array().unwrap();
        assert_eq!(heat.len(), 4, "{heat:?}"); // DAY 左 2 格 + DAY 右 1 格 + DAY2 左 1 格
        assert!(heat.iter().all(|r| device_ids.contains(&r["deviceId"].as_i64().unwrap())));
        let h312 = heat.iter().find(|r| r["bin"] == 312).unwrap();
        assert_eq!(h312["stick"], "left");
        assert_eq!(h312["dwellUs"], 1_000_000);

        // legacyMouseDaily：旧算法原始量（1.0 英寸×80）+ quality 常量
        let legacy = motion["legacyMouseDaily"].as_array().unwrap();
        assert_eq!(legacy.len(), 1, "{legacy:?}");
        assert_eq!(legacy[0]["deviceId"], ms);
        assert_eq!(legacy[0]["day"], DAY);
        assert_eq!(legacy[0]["rawCounts"], 80.0);
        assert_eq!(legacy[0]["quality"], LEGACY_MOUSE_QUALITY);

        // camelCase 逐字（文件全文）：新键在场、snake_case 键缺席、source_key/path 不导出
        for key in [
            "\"sourceId\"", "\"deviceId\"", "\"manualDpi\"", "\"dpiOrigin\"", "\"activeUs\"",
            "\"travelR\"", "\"dwellUs\"", "\"rawCounts\"", "\"mouseDaily\"", "\"gamepadDaily\"",
            "\"gamepadHeat\"", "\"legacyMouseDaily\"",
        ] {
            assert!(text.contains(key), "缺 {key}");
        }
        for bad in [
            "\"source_id\"", "\"device_id\"", "\"manual_dpi\"", "\"active_us\"", "\"travel_r\"",
            "\"dwell_us\"", "\"raw_counts\"", "source_key", "interface_path",
        ] {
            assert!(!text.contains(bad), "不得出现 {bad}");
        }

        // rows = own 四数组 + motion 五数组元素精确合计
        let arr_len = |x: &serde_json::Value| x.as_array().map_or(0, |a| a.len()) as u64;
        let expect = arr_len(&v["devices"])
            + arr_len(&v["input_daily"])
            + arr_len(&v["combos"])
            + arr_len(&v["apps"])
            + mice.len() as u64
            + daily.len() as u64
            + gdaily.len() as u64
            + heat.len() as u64
            + legacy.len() as u64;
        assert_eq!(rows, expect, "rows 须含 motion 各数组元素");
    }

    /// 验收点（motion-dpi §8-S9）：from/to 过滤全部 daily 数组；空范围五数组全空但节点
    /// 在场；旧 schema（删运动四表）新 motion 全空、legacy 可读部分照常导出（不报导出
    /// 成功却遗漏可读数据），rows 口径仍精确。
    #[test]
    fn motion_dpi_json_v3_motion_range_filter_empty_and_old_schema() {
        let (f, conn, _ms, _a, _gp) = seed_motion("export-motion-range");

        // 子范围 [DAY2, DAY2]：mice 仍列 A（DAY2 有桶）、legacy（在 DAY）被过滤
        let d2 = TempFile::new("export-motion-range-d2", "json");
        export_json(&conn, Scope::Own, DAY2, DAY2, d2.as_ref()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d2.as_ref()).unwrap()).unwrap();
        let motion = &v["motion"];
        assert_eq!(motion["mice"].as_array().unwrap().len(), 1);
        let daily = motion["mouseDaily"].as_array().unwrap();
        assert_eq!(daily.len(), 1, "{daily:?}");
        assert_eq!(daily[0]["day"], DAY2);
        assert_eq!(daily[0]["counts"], 100.0);
        assert!(motion["gamepadDaily"].as_array().unwrap().iter().all(|r| r["day"] == DAY2));
        assert!(motion["gamepadHeat"].as_array().unwrap().iter().all(|r| r["day"] == DAY2));
        assert!(motion["legacyMouseDaily"].as_array().unwrap().is_empty(), "legacy 行在 DAY");

        // 空范围：五数组全空但 motion 节点在场，rows = devices 数
        let empty = TempFile::new("export-motion-range-empty", "json");
        let (_, rows_empty) =
            export_json(&conn, Scope::Own, "2026-10-01", "2026-10-02", empty.as_ref()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(empty.as_ref()).unwrap()).unwrap();
        let motion = &v["motion"];
        for k in ["mice", "mouseDaily", "gamepadDaily", "gamepadHeat", "legacyMouseDaily"] {
            assert!(motion[k].as_array().is_some_and(|a| a.is_empty()), "{k} 须为空数组");
        }
        assert_eq!(rows_empty, 2, "空范围 rows = devices 数");

        // 旧 schema：删运动四表 → 新 motion 全空，legacy 可读部分照常导出
        drop(conn);
        {
            let rw = rusqlite::Connection::open(f.as_ref()).unwrap();
            rw.execute_batch(
                "DROP TABLE IF EXISTS gamepad_heat_daily;
                 DROP TABLE IF EXISTS gamepad_motion_daily;
                 DROP TABLE IF EXISTS mouse_motion_daily;
                 DROP TABLE IF EXISTS mouse_motion_sources;",
            )
            .unwrap();
        }
        let conn = ro_conn(&f);
        let old = TempFile::new("export-motion-range-old", "json");
        let (_, rows_old) = export_json(&conn, Scope::Own, FROM, TO, old.as_ref()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(old.as_ref()).unwrap()).unwrap();
        let motion = &v["motion"];
        for k in ["mice", "mouseDaily", "gamepadDaily", "gamepadHeat"] {
            assert!(motion[k].as_array().is_some_and(|a| a.is_empty()), "旧 schema {k} 须为空");
        }
        let legacy = motion["legacyMouseDaily"].as_array().unwrap();
        assert_eq!(legacy.len(), 1, "旧 schema 不得遗漏可读的 legacy 数据");
        assert_eq!(legacy[0]["rawCounts"], 80.0);
        assert_eq!(legacy[0]["quality"], LEGACY_MOUSE_QUALITY);
        assert_eq!(rows_old, 2 + 1, "旧 schema rows = devices + legacy 行数");
    }

    /// 验收点（motion-dpi §8-S9）：CSV scope=own 追加五个运动文件——文件名/表头逐字、
    /// BOM、null 空单元格、逗号名字引号转义、quality 常量列、空范围 0 数据行、
    /// rows 含新文件数据行。
    #[test]
    fn motion_dpi_csv_own_appends_motion_files_bom_headers_escaping() {
        let (_f, conn, ms, a, gp) = seed_motion("export-motion-csv");
        let dir = TempFile::new("export-motion-csv-dir", "dir");
        std::fs::create_dir_all(&dir).unwrap();

        let files = export_csv(&conn, Scope::Own, FROM, TO, dir.as_ref()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        for expect in [
            "mouse_sources.csv".to_string(),
            format!("mouse_motion_{FROM}_{TO}.csv"),
            format!("gamepad_motion_{FROM}_{TO}.csv"),
            format!("gamepad_heat_{FROM}_{TO}.csv"),
            format!("legacy_mouse_motion_{FROM}_{TO}.csv"),
        ] {
            assert!(names.contains(&expect), "{names:?}");
        }

        // BOM 逐字节 + 表头逐字（§6.3 行类型字段序）
        let read_csv = |dir: &TempFile, name: &str| -> String {
            let bytes = std::fs::read(dir.as_ref().join(name)).unwrap();
            assert_eq!(&bytes[..3], CSV_BOM, "{name} 必须以 UTF-8 BOM 开头");
            String::from_utf8(bytes[3..].to_vec()).unwrap()
        };
        let src = read_csv(&dir, "mouse_sources.csv");
        assert!(src.starts_with("sourceId,deviceId,name,manualDpi\n"), "{src}");
        // 逗号名字被引号包裹（复用 RFC 4180 转义）
        assert!(src.contains(&format!("{a},{ms},\"测试,鼠标\",800\n")), "{src}");

        let mm = read_csv(&dir, &format!("mouse_motion_{FROM}_{TO}.csv"));
        assert!(mm.starts_with("sourceId,day,dpi,dpiOrigin,counts,meters\n"), "{mm}");
        assert!(mm.contains(&format!("{a},{DAY},800,manual,800,0.0254\n")), "{mm}");
        assert!(mm.contains(&format!("{a},{DAY},,unknown,400,\n")), "{mm}"); // unknown → 空单元格
        assert!(mm.contains(&format!("{a},{DAY2},800,manual,100,0.003175\n")), "{mm}");

        let gm = read_csv(&dir, &format!("gamepad_motion_{FROM}_{TO}.csv"));
        assert!(gm.starts_with("deviceId,day,stick,activeUs,travelR\n"), "{gm}");
        assert!(gm.contains(&format!("{gp},{DAY},left,1500000,0.707\n")), "{gm}");
        assert!(gm.contains(&format!("{gp},{DAY},right,250000,0\n")), "{gm}");

        let gh = read_csv(&dir, &format!("gamepad_heat_{FROM}_{TO}.csv"));
        assert!(gh.starts_with("deviceId,day,stick,bin,dwellUs\n"), "{gh}");
        assert!(gh.contains(&format!("{gp},{DAY},left,312,1000000\n")), "{gh}");
        assert!(gh.contains(&format!("{gp},{DAY},right,324,250000\n")), "{gh}");

        let lm = read_csv(&dir, &format!("legacy_mouse_motion_{FROM}_{TO}.csv"));
        assert!(lm.starts_with("deviceId,day,rawCounts,quality\n"), "{lm}");
        assert!(lm.contains(&format!("{ms},{DAY},80,{LEGACY_MOUSE_QUALITY}\n")), "{lm}");

        // rows = 各 CSV 数据行合计（devices 2 + mouse_sources 1 + mouse_motion 3 +
        // gamepad_motion 3 + gamepad_heat 4 + legacy 1；无键盘/apps/combos 数据行）
        let total: u64 = files.iter().map(|(_, n)| *n).sum();
        assert_eq!(total, 14, "rows 须含新运动文件的数据行");
        assert_eq!(files.len(), 8, "8 个文件（无键盘 → 无 keys_ 视图）: {names:?}");

        // 空范围：五个运动文件仍在场但 0 数据行（BOM + 表头）
        let dir2 = TempFile::new("export-motion-csv-empty", "dir");
        std::fs::create_dir_all(&dir2).unwrap();
        let files2 = export_csv(&conn, Scope::Own, "2026-10-01", "2026-10-02", dir2.as_ref()).unwrap();
        let names2: Vec<String> = files2
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        for expect in [
            "mouse_sources.csv".to_string(),
            "mouse_motion_2026-10-01_2026-10-02.csv".to_string(),
            "gamepad_motion_2026-10-01_2026-10-02.csv".to_string(),
            "gamepad_heat_2026-10-01_2026-10-02.csv".to_string(),
            "legacy_mouse_motion_2026-10-01_2026-10-02.csv".to_string(),
        ] {
            assert!(names2.contains(&expect), "{names2:?}");
        }
        let src2 = read_csv(&dir2, "mouse_sources.csv");
        assert_eq!(src2, "sourceId,deviceId,name,manualDpi\n", "空范围只余表头");
    }

    /// 验收点（motion-dpi §8-S9）：CSV scope=wp 只出既有 WP 文件（不新增运动文件、
    /// 不重新解释 WP 英寸）；JSON scope=wp 保留自有根数据 + motion 仍表达自有数据 +
    /// 追加 WhatPulse；rows 含 motion 与 whatpulse 各数组元素。
    #[test]
    fn motion_dpi_wp_scope_csv_no_motion_files_and_json_motion_is_own_data() {
        let (_f, conn, _ms, a, _gp) = seed_motion("export-motion-wp");

        // CSV scope=wp：只有六个既有 wp_ 视图，无任何运动文件
        let dir = TempFile::new("export-motion-wp-dir", "dir");
        std::fs::create_dir_all(&dir).unwrap();
        let files = export_csv(&conn, Scope::Wp, FROM, TO, dir.as_ref()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| n.starts_with("wp_")), "{names:?}");
        assert_eq!(files.len(), 6, "wp scope 不得追加运动文件: {names:?}");
        // WP 英寸原样换算（wp_mouse_ 仍是旧口径，不套来源 DPI）
        let bytes = std::fs::read(dir.as_ref().join(format!("wp_mouse_{FROM}_{TO}.csv"))).unwrap();
        let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
        assert!(
            text.contains(&format!("{DAY},10,{}", wp::inches_to_meters(2.0))),
            "{text}"
        );

        // JSON scope=wp：motion 节点与 own 导出逐字一致（自有数据）
        let own = TempFile::new("export-motion-wp-own", "json");
        export_json(&conn, Scope::Own, FROM, TO, own.as_ref()).unwrap();
        let v_own: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(own.as_ref()).unwrap()).unwrap();
        let out = TempFile::new("export-motion-wp-json", "json");
        let (_, rows) = export_json(&conn, Scope::Wp, FROM, TO, out.as_ref()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.as_ref()).unwrap()).unwrap();
        assert_eq!(v["motion"], v_own["motion"], "wp 导出的 motion 仍表达自有数据");

        // whatpulse 追加且原单位（clicks 10、英寸→米沿用旧换算）
        let wp_node = v.get("whatpulse").expect("wp 导出必须有 whatpulse 节点");
        assert_eq!(wp_node["mouse"][0]["clicks"], 10);

        // rows = own 四数组 + motion 五数组 + whatpulse 六数组元素精确合计（meta 不计）
        let motion = &v["motion"];
        let arr_len = |x: &serde_json::Value| x.as_array().map_or(0, |a| a.len()) as u64;
        let expect = arr_len(&v["devices"])
            + arr_len(&v["input_daily"])
            + arr_len(&v["combos"])
            + arr_len(&v["apps"])
            + arr_len(&motion["mice"])
            + arr_len(&motion["mouseDaily"])
            + arr_len(&motion["gamepadDaily"])
            + arr_len(&motion["gamepadHeat"])
            + arr_len(&motion["legacyMouseDaily"])
            + arr_len(&wp_node["keys"])
            + arr_len(&wp_node["combos"])
            + arr_len(&wp_node["apps"])
            + arr_len(&wp_node["mouse"])
            + arr_len(&wp_node["buttons"])
            + arr_len(&wp_node["scrolls"]);
        assert_eq!(rows, expect, "wp 导出 rows 须含 motion 与 whatpulse 各数组元素");
        assert!(mice_row_present(motion, a), "motion.mice 仍列来源 A");
    }

    /// motion.mice 是否列出指定来源（wp 用例的独立断言助手）。
    fn mice_row_present(motion: &serde_json::Value, source_id: i64) -> bool {
        motion["mice"]
            .as_array()
            .is_some_and(|mice| mice.iter().any(|m| m["sourceId"].as_i64() == Some(source_id)))
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
