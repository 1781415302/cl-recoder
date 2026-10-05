//! motion —— schema 3 运动四表的注册/状态/查询/导出 SQL（motion-dpi §4.4/§6.1）。
//!
//! 职责边界：
//! - 来源注册与状态心跳（`mouse_motion_sources`）：元数据写入，幂等可重试、不涉 counts；
//! - 运动增量查询与口径结算（`mouse_motion_daily` / `gamepad_motion_daily` / `gamepad_heat_daily`）：
//!   meters/coverage/625 格组装等全部在此按 §4.4 逐字口径结算——meters=`Σ(counts/dpi×0.0254)`
//!   仅 dpi>0、rawCounts 全桶求和、unconfiguredCounts 仅 dpi=0、coverage=已配置/总量（total=0→null）；
//! - 旧 `mouse_move_daily` 的 legacy 读数（×80 只还原旧算法已投递的原始累计量，不换算米、
//!   不归给任何物理来源，§6.2）。
//!
//! 增量写入本体在 [`crate::writer`]（与 input/apps/combo 同一个 flush 事务，任一部分失败
//! 全回滚）；本模块另提供写入行类型与入库校验助手供其复用。
//!
//! schema 兼容（§4.5/§6.3）：v2 及更早库上 [`motion_schema_ready`]=false，查询函数据此返回
//! `NeedsUpgrade`/合法空结果——missing table 是**预期可判定**状态，不算 swallow；真实 SQL
//! 错误照常报错。`source_key`（本机设备路径）只入库匹配，不出现在任何对外行/日志/导出。

use std::collections::BTreeMap;

use chrono::{DateTime, Local, SecondsFormat, Utc};
use clrecoder_core::day;
use clrecoder_core::motion::{
    DpiOrigin, DpiProbeStatus, MouseSourceDescriptor, MouseSourceState, StickBinDelta, StickSide,
};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{i64_to_count, Result, StoreError};

/// 来源 `connected` 证据的新鲜度窗口（§6.1：读取须 lastSeen≤5 秒，退出/重启不依赖遗留标记）。
const CONNECTED_FRESH_WINDOW_US: i64 = 5_000_000;

/// 手动 DPI 合同上限（schema CHECK `BETWEEN 1 AND 100000`，§6.1 逐字）。
pub(crate) const MANUAL_DPI_MAX: u32 = 100_000;

/// 自动 DPI 合同上限（schema CHECK `BETWEEN 1 AND 57343`，§6.1 逐字）。
pub(crate) const AUTO_DPI_MAX: u32 = 57_343;

/// 摇杆热力网格边长（25×25=625 格，row-major；§4.5/§6.1）。
pub const GRID_SIZE: u8 = 25;

/// 每格号总数（[`GRID_SIZE`]²）。
pub const STICK_BIN_COUNT: usize = (GRID_SIZE as usize) * (GRID_SIZE as usize);

/// 旧 `mouse_move_daily` → 原始 counts 的固定折算（旧算法固定按 80 counts/英寸 投影，
/// §6.2"旧数值×80 只能还原已投递的原始累计量"——不换算米、不区分 DPI）。
const LEGACY_COUNTS_PER_INCH: f64 = 80.0;

/// 米换算常数（1 英寸 = 0.0254 m，§4.4）。
const METERS_PER_INCH: f64 = 0.0254;

// ---------------------------------------------------------------------------
// 写入行类型（§4.4 逐字对齐；由 Writer::flush 与旧统计同事务落库）
// ---------------------------------------------------------------------------

/// 鼠标运动增量写入行（§4.4）：按（来源, 日, DPI 桶）累计的 raw counts。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseMotionWrite {
    /// `mouse_motion_sources.id`（aggregator 注册来源后绑定；FK，未知 id 整批回滚）
    pub source_id: i64,
    /// 归属日 `YYYY-MM-DD`（本地时区）
    pub day: String,
    /// 打桶时的有效 DPI（unknown 编码 0）
    pub dpi: u32,
    /// DPI 取值来源（与 dpi 配对：dpi=0 ↔ Unknown，schema CHECK 逐字）
    pub origin: DpiOrigin,
    /// 本桶 raw counts 增量（须为有限非负数）
    pub counts: f64,
}

/// 手柄摇杆运动增量写入行（§4.4）：motion 行与热度 bins 同批同事务写入，
/// 不能在 flush 外单独提交 heat bins 或 distance。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StickMotionWrite {
    /// `devices.id`（型号行；FK）
    pub device_id: i64,
    /// 归属日 `YYYY-MM-DD`（本地时区）
    pub day: String,
    /// 摇杆侧
    pub side: StickSide,
    /// 本批活动微秒（tracker 不变量：恒等于 `Σbins.dwell_us`）
    pub active_us: u64,
    /// 本批路程增量（单位 R）
    pub travel_r: f64,
    /// 本批各格停留增量（空 bin 由 writer 跳过不落行，§6.1"空 bin 不写行"）
    pub bins: Vec<StickBinDelta>,
}

/// 来源手动 DPI 配置行（§4.4）：DPI worker ≤500ms 批读的载体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseConfigRow {
    /// 来源 key（接口路径；仅进程内匹配用，不出现在日志/UI/导出）
    pub source_key: String,
    /// 手动配置 DPI（未配置为 None）
    pub manual_dpi: Option<u32>,
}

// ---------------------------------------------------------------------------
// 对外行类型（§4.4 固定形状；内存类型不靠 SQL 字段猜测，camelCase DTO 在 src-tauri 侧）
// ---------------------------------------------------------------------------

/// 运动查询可用性（§4.4）：由 DTO adapter 映射 ready/needs_upgrade。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionAvailability {
    /// schema 3 就绪，数据合法（可为空）
    Ready,
    /// 库仍是旧 schema，运动查询不可用（GUI 引导"启动/更新采集器后可用"）
    NeedsUpgrade,
}

/// 鼠标运动来源行（§4.4）：展示口径在读取侧结算（connected 需新鲜度、auto 需未过期）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseSourceRow {
    /// 来源 id（`mouse_motion_sources.id`）
    pub id: i64,
    /// 型号设备 id（`devices.id`；按钮 TopKeys 等型号视图仍查它）
    pub device_id: i64,
    /// 型号显示名（devices.name）
    pub name: String,
    /// 型号昵称（devices.nickname）
    pub nickname: Option<String>,
    /// 是否物理来源（虚拟/未知桶为 false，禁自动/手动 DPI 换算）
    pub physical: bool,
    /// 当前是否在线（collector 发布的 connected 且 last_seen≤5 秒，§6.1）
    pub connected: bool,
    /// 手动配置 DPI（持久，可离线显示）
    pub manual_dpi: Option<u32>,
    /// 当前有效的自动 DPI（要求在线且未过期，否则 None）
    pub auto_dpi: Option<u32>,
    /// 自动 DPI 失效时刻（RFC3339；仅与 auto_dpi 同真，§4.5）
    pub auto_valid_until: Option<String>,
    /// 换算里程所用有效 DPI：auto 有效优先，其次 manual，否则 None
    pub effective_dpi: Option<u32>,
    /// effective_dpi 的取值来源（None 时为 Unknown）
    pub dpi_origin: DpiOrigin,
    /// DPI 探测状态
    pub probe_status: DpiProbeStatus,
}

/// 鼠标运动单日汇总（§4.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseMotionDay {
    /// 日期 `YYYY-MM-DD`
    pub day: String,
    /// 当日 raw counts（全部 DPI 桶求和，含 unknown）
    pub raw_counts: f64,
    /// 当日已配置部分折算米数（无任何已配置移动时 None——不用 0 米冒充"全部距离"）
    pub meters: Option<f64>,
    /// 当日未配置（dpi=0）counts
    pub unconfigured_counts: f64,
}

/// 鼠标运动区间汇总（§4.4）：`[from, to]` 闭区间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseMotionSummary {
    /// schema 可用性
    pub availability: MotionAvailability,
    /// 来源 id
    pub source_id: i64,
    /// 区间 raw counts（全部 DPI 桶求和）
    pub raw_counts: f64,
    /// 区间已配置部分折算米数（无任何已配置移动时 None）
    pub meters: Option<f64>,
    /// 区间未配置（dpi=0）counts
    pub unconfigured_counts: f64,
    /// 已配置 counts / 总 counts（total=0 时 None）
    pub coverage: Option<f64>,
    /// 逐日明细（按日升序；只含有运动桶的日）
    pub days: Vec<MouseMotionDay>,
}

/// 旧算法鼠标移动读数（§4.4）：单独来自旧表×80，只说明旧算法原始量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyMouseSummary {
    /// 型号设备 id
    pub device_id: i64,
    /// 旧算法原始累计量（`mouse_move_daily.distance_inches`×80；schema1 无表则 0）
    pub raw_counts: f64,
}

/// 单侧摇杆运动汇总（§4.4）：停留恰 625 格（秒），无数据全 0。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StickMotionSummary {
    /// 摇杆侧
    pub side: StickSide,
    /// 区间活动秒数（active_us/1e6）
    pub active_seconds: f64,
    /// 区间累计路程（R）
    pub travel_r: f64,
    /// 停留热力（恰 625 格、row-major、单位秒；空格为 0）
    pub dwell_seconds: Vec<f64>,
}

/// 手柄摇杆运动汇总（§4.4）：左右两路独立结算。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GamepadMotionSummary {
    /// schema 可用性
    pub availability: MotionAvailability,
    /// 型号设备 id
    pub device_id: i64,
    /// 热力网格边长（25）
    pub grid_size: u8,
    /// 左摇杆
    pub left: StickMotionSummary,
    /// 右摇杆
    pub right: StickMotionSummary,
}

// ---------------------------------------------------------------------------
// 导出行类型（§4.4/§6.3；quality="legacy_uncalibrated" 常量由 GUI/导出层添加）
// ---------------------------------------------------------------------------

/// 导出：鼠标来源行（`mice`，只列区间内有运动桶的来源；source_key 不导出）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportMouseSourceRow {
    /// 来源 id
    pub source_id: i64,
    /// 型号设备 id
    pub device_id: i64,
    /// 型号显示名
    pub name: String,
    /// 手动配置 DPI
    pub manual_dpi: Option<u32>,
}

/// 导出：鼠标运动逐桶行（§6.3：dpi=0（unknown）时 dpi=None、meters=None）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportMouseMotionRow {
    /// 来源 id
    pub source_id: i64,
    /// 日期
    pub day: String,
    /// 桶 DPI（unknown 桶为 None）
    pub dpi: Option<u32>,
    /// DPI 取值来源
    pub dpi_origin: DpiOrigin,
    /// 该桶 counts
    pub counts: f64,
    /// 该桶折算米数（仅 dpi>0；unknown 桶 None）
    pub meters: Option<f64>,
}

/// 导出：手柄摇杆逐日运动行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportGamepadMotionRow {
    /// 型号设备 id
    pub device_id: i64,
    /// 日期
    pub day: String,
    /// 摇杆侧
    pub stick: StickSide,
    /// 当日活动微秒
    pub active_us: u64,
    /// 当日累计路程（R）
    pub travel_r: f64,
}

/// 导出：手柄停留热力行（空格不导出，UI 自行按 625 组装）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportGamepadHeatRow {
    /// 型号设备 id
    pub device_id: i64,
    /// 日期
    pub day: String,
    /// 摇杆侧
    pub stick: StickSide,
    /// 热力格号（0..=624，row-major）
    pub bin: u16,
    /// 该格停留微秒
    pub dwell_us: u64,
}

/// 导出：旧算法鼠标移动逐日行（原始量；米数/倍率不回写）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportLegacyMouseRow {
    /// 型号设备 id
    pub device_id: i64,
    /// 日期
    pub day: String,
    /// 旧算法原始累计量（distance_inches×80）
    pub raw_counts: f64,
}

/// 导出：motion 节点全部行集合（§6.3；from/to 过滤全部 daily，mice 只列 motion 引用的来源）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MotionExportRows {
    /// 鼠标来源行
    pub mice: Vec<ExportMouseSourceRow>,
    /// 鼠标运动逐桶行
    pub mouse_daily: Vec<ExportMouseMotionRow>,
    /// 手柄摇杆逐日运动行
    pub gamepad_daily: Vec<ExportGamepadMotionRow>,
    /// 手柄停留热力行
    pub gamepad_heat: Vec<ExportGamepadHeatRow>,
    /// 旧算法鼠标移动逐日行
    pub legacy_mouse_daily: Vec<ExportLegacyMouseRow>,
}

// ---------------------------------------------------------------------------
// 文本映射（DDL CHECK 值与 §4.1 serde 小写逐字一致，由测试锚定不漂移）
// ---------------------------------------------------------------------------

/// `DpiOrigin` → `mouse_motion_daily.dpi_origin` 列文本（与 serde snake_case 逐字一致）。
pub(crate) fn dpi_origin_to_text(o: DpiOrigin) -> &'static str {
    match o {
        DpiOrigin::Auto => "auto",
        DpiOrigin::Manual => "manual",
        DpiOrigin::Unknown => "unknown",
    }
}

/// `mouse_motion_daily.dpi_origin` 列文本 → `DpiOrigin`；契约外值报错（库损坏防御）。
pub(crate) fn dpi_origin_from_text(s: &str) -> Result<DpiOrigin> {
    match s {
        "auto" => Ok(DpiOrigin::Auto),
        "manual" => Ok(DpiOrigin::Manual),
        "unknown" => Ok(DpiOrigin::Unknown),
        other => Err(StoreError::InvalidMotionField(format!("未知 dpi_origin: {other}"))),
    }
}

/// `DpiProbeStatus` → `mouse_motion_sources.probe_status` 列文本。
pub(crate) fn probe_status_to_text(s: DpiProbeStatus) -> &'static str {
    match s {
        DpiProbeStatus::Pending => "pending",
        DpiProbeStatus::Available => "available",
        DpiProbeStatus::Unsupported => "unsupported",
        DpiProbeStatus::Ambiguous => "ambiguous",
        DpiProbeStatus::Unavailable => "unavailable",
        DpiProbeStatus::Disconnected => "disconnected",
    }
}

/// `probe_status` 列文本 → `DpiProbeStatus`；契约外值报错（库损坏防御）。
pub(crate) fn probe_status_from_text(s: &str) -> Result<DpiProbeStatus> {
    match s {
        "pending" => Ok(DpiProbeStatus::Pending),
        "available" => Ok(DpiProbeStatus::Available),
        "unsupported" => Ok(DpiProbeStatus::Unsupported),
        "ambiguous" => Ok(DpiProbeStatus::Ambiguous),
        "unavailable" => Ok(DpiProbeStatus::Unavailable),
        "disconnected" => Ok(DpiProbeStatus::Disconnected),
        other => Err(StoreError::InvalidMotionField(format!("未知 probe_status: {other}"))),
    }
}

/// `StickSide` → `stick` 列文本（与 serde snake_case 逐字一致）。
pub(crate) fn stick_side_to_text(s: StickSide) -> &'static str {
    match s {
        StickSide::Left => "left",
        StickSide::Right => "right",
    }
}

/// `stick` 列文本 → `StickSide`；契约外值报错（库损坏防御）。
pub(crate) fn stick_side_from_text(s: &str) -> Result<StickSide> {
    match s {
        "left" => Ok(StickSide::Left),
        "right" => Ok(StickSide::Right),
        other => Err(StoreError::InvalidMotionField(format!("未知摇杆侧: {other}"))),
    }
}

/// REAL 运动计量值（counts/travel_r）入库校验：只接受有限非负数
/// （§6.1"不只依赖 REAL CHECK 拒绝 NaN"——Rust 先行校验，失败整批回滚）。
pub(crate) fn validate_finite_nonneg(v: f64) -> Result<()> {
    if v.is_finite() && v >= 0.0 {
        Ok(())
    } else {
        Err(StoreError::InvalidMotionCount(v))
    }
}

/// [`MouseMotionWrite`] 入库前校验：counts 有限非负 + dpi×origin 配对逐字合同
/// （dpi=0 ↔ unknown；dpi>0 禁 unknown 且 ≤100000——不依赖 CHECK 兜底）。
pub(crate) fn validate_mouse_motion_write(w: &MouseMotionWrite) -> Result<()> {
    validate_finite_nonneg(w.counts)?;
    if w.dpi == 0 {
        if w.origin != DpiOrigin::Unknown {
            return Err(StoreError::InvalidMotionField(
                "dpi=0（unknown 桶）时 dpi_origin 必须为 unknown".to_string(),
            ));
        }
    } else {
        if w.origin == DpiOrigin::Unknown {
            return Err(StoreError::InvalidMotionField(format!(
                "dpi={} >0 时 dpi_origin 不得为 unknown",
                w.dpi
            )));
        }
        if w.dpi > MANUAL_DPI_MAX {
            return Err(StoreError::InvalidMotionField(format!(
                "dpi={} 超出 1..={MANUAL_DPI_MAX}",
                w.dpi
            )));
        }
    }
    Ok(())
}

/// 列 INTEGER（CHECK 保证 1..）→ DPI；越界/非正（损坏行）按未配置处理（防御，绝不 crash）。
fn dpi_from_i64(v: i64) -> Option<u32> {
    u32::try_from(v).ok().filter(|d| *d > 0)
}

/// UTC unix µs → 本地 RFC3339（与 `day::now_local_rfc3339` 同形）；时钟异常兜底为当前时刻。
fn rfc3339_from_unix_us(unix_us: i64) -> String {
    DateTime::<Utc>::from_timestamp_micros(unix_us)
        .map(|t| t.with_timezone(&Local).to_rfc3339_opts(SecondsFormat::Secs, false))
        .unwrap_or_else(day::now_local_rfc3339)
}

/// last_seen（RFC3339）是否在 `now_unix_us` 前 `window_us` 内；无法解析不作在线证据（防御）。
fn last_seen_fresh_within(last_seen: &str, now_unix_us: i64, window_us: i64) -> bool {
    match DateTime::parse_from_rfc3339(last_seen) {
        Ok(t) => now_unix_us - t.timestamp_micros() <= window_us,
        Err(_) => false,
    }
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// 来源注册 / 状态心跳 / 手动 DPI（元数据写入：幂等可重试，不涉 counts）
// ---------------------------------------------------------------------------

/// schema 3 四张运动表是否齐备（§4.5：无 schema → NeedsUpgrade，不把失败伪装成没有运动）。
pub fn motion_schema_ready(conn: &Connection) -> Result<bool> {
    for name in
        ["mouse_motion_sources", "mouse_motion_daily", "gamepad_motion_daily", "gamepad_heat_daily"]
    {
        if !table_exists(conn, name)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 注册鼠标运动来源（§4.4）：按 `source_key` 幂等——已存在则刷新 model/物理性/last_seen
/// 并返回既有 id（不新增行、不触碰 manual_dpi/连接状态；连接代际只在内存，进程重启首次
/// 注册即自然重置）。新路径 = 新来源行，不按型号继承旧 manual（§6.1）。
///
/// `device_id` 由调用方经 `Writer` 既有设备缓存解析（"model id 复用 Writer 缓存"）；
/// `observed_at` 为本次连接观察时刻（RFC3339）。
pub fn register_mouse_source(
    conn: &Connection,
    descriptor: &MouseSourceDescriptor,
    device_id: i64,
    observed_at: &str,
) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM mouse_motion_sources WHERE source_key = ?1",
            params![descriptor.source_key],
            |r| r.get(0),
        )
        .optional()?;
    let physical = i64::from(descriptor.physical);
    match existing {
        Some(id) => {
            // 幂等重注册：元数据刷新到最新连接（model 行重解析/物理性原语义），配置与状态不动
            conn.execute(
                "UPDATE mouse_motion_sources
                 SET device_id = ?1, physical = ?2, last_seen = ?3 WHERE id = ?4",
                params![device_id, physical, observed_at, id],
            )?;
            Ok(id)
        }
        None => {
            conn.execute(
                "INSERT INTO mouse_motion_sources(source_key, device_id, physical, first_seen, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![descriptor.source_key, device_id, physical, observed_at],
            )?;
            Ok(conn.last_insert_rowid())
        }
    }
}

/// 应用来源状态快照（§4.4 心跳，约每 2 秒一次）：写 connected/probe_status/auto_dpi/
/// auto_valid_until/last_seen（由 `state.stamp` 换算 RFC3339）。
///
/// 同来源当前连接代际的判定（旧代 disconnect 不得把新连接标离线、重启首次注册重置代际）
/// 依赖连接代际内存态，由 aggregator 侧在调用前完成——连接 ID 只在内存、不入库（§6.1）。
/// 未知的 `id` 报错（§4.5"未知 sourceId 返回错误"）。
pub fn update_mouse_source_state(
    conn: &Connection,
    id: i64,
    state: &MouseSourceState,
) -> Result<()> {
    if let Some(dpi) = state.auto_dpi {
        if dpi == 0 || dpi > AUTO_DPI_MAX {
            return Err(StoreError::InvalidMotionField(format!(
                "auto DPI {dpi} 超出 1..={AUTO_DPI_MAX}"
            )));
        }
    }
    let n = conn.execute(
        "UPDATE mouse_motion_sources
         SET connected = ?1, probe_status = ?2, auto_dpi = ?3,
             auto_valid_until_unix_us = ?4, last_seen = ?5
         WHERE id = ?6",
        params![
            i64::from(state.connected),
            probe_status_to_text(state.probe_status),
            state.auto_dpi,
            state.auto_valid_until_unix_us,
            rfc3339_from_unix_us(state.stamp.unix_us),
            id,
        ],
    )?;
    if n == 0 {
        return Err(StoreError::UnknownMouseSource(id));
    }
    Ok(())
}

/// 批量读取来源手动 DPI 配置（§4.4：DPI worker 批读；未注册的 key 不在返回中，
/// 已注册未配置的返回 `manual_dpi=None`）。
pub fn read_manual_dpi(conn: &Connection, keys: &[String]) -> Result<Vec<MouseConfigRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; keys.len()].join(",");
    let sql = format!(
        "SELECT source_key, manual_dpi FROM mouse_motion_sources WHERE source_key IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(keys))?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(MouseConfigRow {
            source_key: r.get(0)?,
            // CHECK 保证 1..=100000；越界（损坏行）按未配置处理
            manual_dpi: r.get::<_, Option<i64>>(1)?.and_then(dpi_from_i64),
        });
    }
    Ok(out)
}

/// 设置/清空来源手动 DPI（§4.4：手动配置不是增量、幂等可重试、不回溯既有 counts）。
/// auto 有效时拒绝改 manual 的校验在 GUI 后端（§4.5），本函数为无条件配置写入。
pub fn set_manual_dpi(conn: &Connection, source_id: i64, dpi: Option<u32>) -> Result<()> {
    if let Some(d) = dpi {
        if d == 0 || d > MANUAL_DPI_MAX {
            return Err(StoreError::InvalidMotionField(format!(
                "manual DPI {d} 超出 1..={MANUAL_DPI_MAX}"
            )));
        }
    }
    let n = conn.execute(
        "UPDATE mouse_motion_sources SET manual_dpi = ?1 WHERE id = ?2",
        params![dpi, source_id],
    )?;
    if n == 0 {
        return Err(StoreError::UnknownMouseSource(source_id));
    }
    Ok(())
}

/// 列出全部鼠标运动来源（§4.4）：按来源 id 升序（UI 的"来源序号"）。
///
/// 展示口径（§4.5/§6.1）：`connected` = collector 发布的连接证据 **且** last_seen≤5 秒；
/// 自动 DPI 仅在在线且未过期时展示，effective = auto（有效）> manual > None；
/// `auto_valid_until` 仅与 `auto_dpi` 同时给出。schema 未就绪返回空列表
/// （可用性由 [`motion_schema_ready`] 单独查询）。
pub fn list_mouse_sources(conn: &Connection, now_unix_us: i64) -> Result<Vec<MouseSourceRow>> {
    if !motion_schema_ready(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT s.id, s.device_id, d.name, d.nickname, s.physical, s.connected,
                s.manual_dpi, s.auto_dpi, s.auto_valid_until_unix_us, s.probe_status, s.last_seen
         FROM mouse_motion_sources AS s JOIN devices AS d ON d.id = s.device_id
         ORDER BY s.id",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let connected_flag = r.get::<_, i64>(5)? != 0;
        let last_seen: String = r.get(10)?;
        let connected = connected_flag
            && last_seen_fresh_within(&last_seen, now_unix_us, CONNECTED_FRESH_WINDOW_US);
        let manual_dpi = r.get::<_, Option<i64>>(6)?.and_then(dpi_from_i64);
        let auto_stored = r.get::<_, Option<i64>>(7)?.and_then(dpi_from_i64);
        let auto_until_us: Option<i64> = r.get(8)?;
        let probe_status = probe_status_from_text(&r.get::<_, String>(9)?)?;
        // 自动值只在有效期与当前连接证据成立时使用（§6.1）
        let auto_valid = connected
            && auto_until_us.is_some_and(|until| until > now_unix_us)
            && auto_stored.is_some();
        let (effective_dpi, dpi_origin) = if auto_valid {
            (auto_stored, DpiOrigin::Auto)
        } else if let Some(m) = manual_dpi {
            (Some(m), DpiOrigin::Manual)
        } else {
            (None, DpiOrigin::Unknown)
        };
        out.push(MouseSourceRow {
            id: r.get(0)?,
            device_id: r.get(1)?,
            name: r.get(2)?,
            nickname: r.get(3)?,
            physical: r.get::<_, i64>(4)? != 0,
            connected,
            manual_dpi,
            auto_dpi: if auto_valid { auto_stored } else { None },
            auto_valid_until: if auto_valid {
                auto_until_us.map(rfc3339_from_unix_us)
            } else {
                None
            },
            effective_dpi,
            dpi_origin,
            probe_status,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 运动查询（§4.4 口径：meters=Σ(counts/dpi×0.0254) 仅 dpi>0；coverage=已配置/总量）
// ---------------------------------------------------------------------------

/// 单日聚合中间量（`mouse_motion` 的按日结算，BTreeMap 保证逐日升序）。
#[derive(Debug, Default, Clone, Copy)]
struct DayAccum {
    /// 全部桶 counts 合计
    raw: f64,
    /// 已配置（dpi>0）桶 counts 合计
    configured: f64,
    /// 已配置部分折算米数
    meters: f64,
    /// 未配置（dpi=0）桶 counts 合计
    unconfigured: f64,
}

/// 鼠标单来源运动区间汇总（§4.4）。`[from, to]` 为闭区间；未知 `source_id` 报错；
/// schema 未就绪返回 `NeedsUpgrade` 全零汇总（§4.5）。
///
/// 米数口径：meters=`Σ(counts/dpi×0.0254)` 仅 dpi>0——**读取时按桶内固化 DPI 折算**，
/// 之后修改 manual/auto 都不回溯既有桶（§4.4"配置不回算"）；未配置 counts 不当 0 米。
pub fn mouse_motion(
    conn: &Connection,
    source_id: i64,
    from: &str,
    to: &str,
) -> Result<MouseMotionSummary> {
    if !motion_schema_ready(conn)? {
        return Ok(MouseMotionSummary {
            availability: MotionAvailability::NeedsUpgrade,
            source_id,
            raw_counts: 0.0,
            meters: None,
            unconfigured_counts: 0.0,
            coverage: None,
            days: Vec::new(),
        });
    }
    let known: Option<i64> = conn
        .query_row(
            "SELECT id FROM mouse_motion_sources WHERE id = ?1",
            [source_id],
            |r| r.get(0),
        )
        .optional()?;
    if known.is_none() {
        return Err(StoreError::UnknownMouseSource(source_id));
    }

    let mut stmt = conn.prepare(
        "SELECT day, dpi, counts FROM mouse_motion_daily
         WHERE source_id = ?1 AND day >= ?2 AND day <= ?3
         ORDER BY day",
    )?;
    let mut rows = stmt.query(params![source_id, from, to])?;
    let mut days: BTreeMap<String, DayAccum> = BTreeMap::new();
    while let Some(r) = rows.next()? {
        let day: String = r.get(0)?;
        let dpi: i64 = r.get(1)?;
        let counts: f64 = r.get(2)?;
        let acc = days.entry(day).or_default();
        acc.raw += counts;
        if dpi > 0 {
            acc.configured += counts;
            acc.meters += counts / dpi as f64 * METERS_PER_INCH;
        } else {
            acc.unconfigured += counts;
        }
    }

    let mut raw_total = 0.0;
    let mut configured_total = 0.0;
    let mut unconfigured_total = 0.0;
    let mut meters_total = 0.0;
    let mut day_rows = Vec::with_capacity(days.len());
    for (day, acc) in days {
        day_rows.push(MouseMotionDay {
            day,
            raw_counts: acc.raw,
            // 有已配置移动才给米（可为 0 值语义不存在：dpi>0 且 counts>0 必有距离），
            // 没有任何已配置移动时 None——不用 0 米冒充"全部距离"（§4.5）
            meters: if acc.configured > 0.0 { Some(acc.meters) } else { None },
            unconfigured_counts: acc.unconfigured,
        });
        raw_total += acc.raw;
        configured_total += acc.configured;
        unconfigured_total += acc.unconfigured;
        meters_total += acc.meters;
    }
    Ok(MouseMotionSummary {
        availability: MotionAvailability::Ready,
        source_id,
        raw_counts: raw_total,
        meters: if configured_total > 0.0 { Some(meters_total) } else { None },
        unconfigured_counts: unconfigured_total,
        coverage: if raw_total > 0.0 {
            Some(configured_total / raw_total)
        } else {
            None
        },
        days: day_rows,
    })
}

/// 旧算法鼠标移动原始累计量（§4.4/§6.2）：`mouse_move_daily` 英寸×80。
/// 只说明旧算法已投递的原始量——不可换算米、不归给物理来源；schema1 无旧表则合法 0。
pub fn legacy_mouse_counts(
    conn: &Connection,
    device_id: i64,
    from: &str,
    to: &str,
) -> Result<f64> {
    if !table_exists(conn, "mouse_move_daily")? {
        return Ok(0.0);
    }
    let inches: Option<f64> = conn.query_row(
        "SELECT SUM(distance_inches) FROM mouse_move_daily
         WHERE device_id = ?1 AND day >= ?2 AND day <= ?3",
        params![device_id, from, to],
        |r| r.get(0),
    )?;
    Ok(inches.unwrap_or(0.0) * LEGACY_COUNTS_PER_INCH)
}

/// 旧算法鼠标移动汇总行（§4.4）：[`legacy_mouse_counts`] 的行包装。
pub fn mouse_legacy(
    conn: &Connection,
    device_id: i64,
    from: &str,
    to: &str,
) -> Result<LegacyMouseSummary> {
    Ok(LegacyMouseSummary {
        device_id,
        raw_counts: legacy_mouse_counts(conn, device_id, from, to)?,
    })
}

/// 空侧汇总（schema 未就绪 / 无数据的合法空形状：恰 625 个 0）。
fn empty_stick_summary(side: StickSide) -> StickMotionSummary {
    StickMotionSummary {
        side,
        active_seconds: 0.0,
        travel_r: 0.0,
        dwell_seconds: vec![0.0; STICK_BIN_COUNT],
    }
}

/// 摇杆侧 → 汇总数组下标（左 0 / 右 1）。
fn stick_index(side: StickSide) -> usize {
    match side {
        StickSide::Left => 0,
        StickSide::Right => 1,
    }
}

/// 手柄摇杆运动区间汇总（§4.4）：左右两路独立，停留组装恰 625 格（空格 0）。
/// `[from, to]` 闭区间；schema 未就绪返回 `NeedsUpgrade` 全零（§4.5）。
pub fn gamepad_motion(
    conn: &Connection,
    device_id: i64,
    from: &str,
    to: &str,
) -> Result<GamepadMotionSummary> {
    let mut active_us = [0u64; 2];
    let mut travel = [0.0f64; 2];
    let mut dwell = [[0.0f64; STICK_BIN_COUNT]; 2];
    if !motion_schema_ready(conn)? {
        return Ok(GamepadMotionSummary {
            availability: MotionAvailability::NeedsUpgrade,
            device_id,
            grid_size: GRID_SIZE,
            left: empty_stick_summary(StickSide::Left),
            right: empty_stick_summary(StickSide::Right),
        });
    }

    {
        let mut stmt = conn.prepare(
            "SELECT stick, active_us, travel_r FROM gamepad_motion_daily
             WHERE device_id = ?1 AND day >= ?2 AND day <= ?3",
        )?;
        let mut rows = stmt.query(params![device_id, from, to])?;
        while let Some(r) = rows.next()? {
            let side = stick_side_from_text(&r.get::<_, String>(0)?)?;
            let i = stick_index(side);
            active_us[i] = active_us[i].saturating_add(i64_to_count(r.get::<_, i64>(1)?));
            travel[i] += r.get::<_, f64>(2)?;
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT stick, bin, dwell_us FROM gamepad_heat_daily
             WHERE device_id = ?1 AND day >= ?2 AND day <= ?3",
        )?;
        let mut rows = stmt.query(params![device_id, from, to])?;
        while let Some(r) = rows.next()? {
            let side = stick_side_from_text(&r.get::<_, String>(0)?)?;
            let i = stick_index(side);
            let bin: i64 = r.get(1)?;
            let dwell_us = i64_to_count(r.get::<_, i64>(2)?);
            // CHECK 保证 0..=624；越界（损坏行）防御性跳过，绝不 crash、不污染其余格
            if (0..STICK_BIN_COUNT as i64).contains(&bin) {
                dwell[i][bin as usize] += dwell_us as f64 / 1_000_000.0;
            }
        }
    }

    let side_summary = |side: StickSide| StickMotionSummary {
        side,
        active_seconds: active_us[stick_index(side)] as f64 / 1_000_000.0,
        travel_r: travel[stick_index(side)],
        dwell_seconds: dwell[stick_index(side)].to_vec(),
    };
    Ok(GamepadMotionSummary {
        availability: MotionAvailability::Ready,
        device_id,
        grid_size: GRID_SIZE,
        left: side_summary(StickSide::Left),
        right: side_summary(StickSide::Right),
    })
}

// ---------------------------------------------------------------------------
// 导出（§6.3：from/to 过滤全部 daily；mice 只列 motion 引用的来源；source_key 不导出）
// ---------------------------------------------------------------------------

/// 导出 motion 节点全部行（§6.3）。schema 3 未就绪时新运动数组全空；旧
/// `mouse_move_daily`（schema≥2）可读部分照常导出——"不报导出成功却遗漏可读数据"。
pub fn export_motion_rows(conn: &Connection, from: &str, to: &str) -> Result<MotionExportRows> {
    let mut out = MotionExportRows::default();
    if motion_schema_ready(conn)? {
        // mice：只列 [from,to] 内有运动桶的来源（§6.3"mice 只列 motion 引用的来源"）
        {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT s.id, s.device_id, d.name, s.manual_dpi
                 FROM mouse_motion_daily AS m
                 JOIN mouse_motion_sources AS s ON s.id = m.source_id
                 JOIN devices AS d ON d.id = s.device_id
                 WHERE m.day >= ?1 AND m.day <= ?2
                 ORDER BY s.id",
            )?;
            let mut rows = stmt.query(params![from, to])?;
            while let Some(r) = rows.next()? {
                out.mice.push(ExportMouseSourceRow {
                    source_id: r.get(0)?,
                    device_id: r.get(1)?,
                    name: r.get(2)?,
                    manual_dpi: r.get::<_, Option<i64>>(3)?.and_then(dpi_from_i64),
                });
            }
        }
        // 鼠标逐桶行：unknown 桶 dpi=None、meters=None
        {
            let mut stmt = conn.prepare(
                "SELECT source_id, day, dpi, dpi_origin, counts FROM mouse_motion_daily
                 WHERE day >= ?1 AND day <= ?2
                 ORDER BY source_id, day, dpi",
            )?;
            let mut rows = stmt.query(params![from, to])?;
            while let Some(r) = rows.next()? {
                let source_id: i64 = r.get(0)?;
                let day: String = r.get(1)?;
                let dpi: i64 = r.get(2)?;
                let dpi_origin = dpi_origin_from_text(&r.get::<_, String>(3)?)?;
                let counts: f64 = r.get(4)?;
                out.mouse_daily.push(ExportMouseMotionRow {
                    source_id,
                    day,
                    dpi: if dpi > 0 { dpi_from_i64(dpi) } else { None },
                    dpi_origin,
                    counts,
                    meters: if dpi > 0 {
                        Some(counts / dpi as f64 * METERS_PER_INCH)
                    } else {
                        None
                    },
                });
            }
        }
        // 手柄逐日运动行
        {
            let mut stmt = conn.prepare(
                "SELECT device_id, day, stick, active_us, travel_r FROM gamepad_motion_daily
                 WHERE day >= ?1 AND day <= ?2
                 ORDER BY device_id, day, stick",
            )?;
            let mut rows = stmt.query(params![from, to])?;
            while let Some(r) = rows.next()? {
                out.gamepad_daily.push(ExportGamepadMotionRow {
                    device_id: r.get(0)?,
                    day: r.get(1)?,
                    stick: stick_side_from_text(&r.get::<_, String>(2)?)?,
                    active_us: i64_to_count(r.get::<_, i64>(3)?),
                    travel_r: r.get(4)?,
                });
            }
        }
        // 手柄停留热力行（空格本就不落行，导出即稀疏行）
        {
            let mut stmt = conn.prepare(
                "SELECT device_id, day, stick, bin, dwell_us FROM gamepad_heat_daily
                 WHERE day >= ?1 AND day <= ?2
                 ORDER BY device_id, day, stick, bin",
            )?;
            let mut rows = stmt.query(params![from, to])?;
            while let Some(r) = rows.next()? {
                out.gamepad_heat.push(ExportGamepadHeatRow {
                    device_id: r.get(0)?,
                    day: r.get(1)?,
                    stick: stick_side_from_text(&r.get::<_, String>(2)?)?,
                    bin: r.get::<_, i64>(3)?.try_into().unwrap_or(0),
                    dwell_us: i64_to_count(r.get::<_, i64>(4)?),
                });
            }
        }
    }
    // legacy：schema≥2 即可读（v3 未迁移时仍导出，§6.3"仅 legacy 可读部分"）
    if table_exists(conn, "mouse_move_daily")? {
        let mut stmt = conn.prepare(
            "SELECT device_id, day, distance_inches FROM mouse_move_daily
             WHERE day >= ?1 AND day <= ?2
             ORDER BY device_id, day",
        )?;
        let mut rows = stmt.query(params![from, to])?;
        while let Some(r) = rows.next()? {
            out.legacy_mouse_daily.push(ExportLegacyMouseRow {
                device_id: r.get(0)?,
                day: r.get(1)?,
                raw_counts: r.get::<_, f64>(2)? * LEGACY_COUNTS_PER_INCH,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{SCHEMA_V1_SQL, SCHEMA_V2_SQL};
    use crate::testutil::TempDb;
    use chrono::TimeZone;
    use clrecoder_core::codes::DeviceKind;
    use clrecoder_core::event::DeviceKey;

    /// 测试用固定日。
    const DAY: &str = "2026-09-28";
    const DAY2: &str = "2026-09-29";

    /// 固定观察时刻（本地 2026-09-28 12:00:00）及其 unix µs。
    fn fixed_now_us() -> i64 {
        Local.with_ymd_and_hms(2026, 9, 28, 12, 0, 0).unwrap().timestamp_micros()
    }

    /// 固定时刻的本地 RFC3339（与 Writer::register 落库形状一致）。
    fn rfc_at(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> String {
        Local.with_ymd_and_hms(y, m, d, h, mi, s).unwrap().to_rfc3339_opts(SecondsFormat::Secs, false)
    }

    /// 建内存态 v3 库（临时文件 + migrate）。
    fn open_v3(tag: &str) -> (TempDb, Connection) {
        let db = TempDb::new(tag);
        let conn = Connection::open(db.as_ref()).unwrap();
        crate::schema::migrate(&conn).unwrap();
        (db, conn)
    }

    /// 手工搭出 v2 库（schema≥2、无运动四表；可写 legacy 行）。
    fn open_v2(tag: &str) -> (TempDb, Connection) {
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

    /// 物理鼠标来源描述（模型名/路径可区分）。
    fn descriptor(key: &str, model_name: &str) -> MouseSourceDescriptor {
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

    /// 虚拟来源固定桶（physical=false）。
    fn virtual_descriptor() -> MouseSourceDescriptor {
        MouseSourceDescriptor {
            source_key: "virtual:unknown".to_string(),
            model: DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0,
                pid: 0,
                name: "未知/虚拟设备".to_string(),
            },
            interface_path: None,
            physical: false,
        }
    }

    /// 状态快照构造（stamp 取固定时刻 + 偏移秒）。
    fn state(
        desc: &MouseSourceDescriptor,
        connection: u64,
        connected: bool,
        stamp_offset_s: i64,
        probe: DpiProbeStatus,
        auto_dpi: Option<u32>,
        auto_until_s: Option<i64>,
    ) -> MouseSourceState {
        MouseSourceState {
            descriptor: desc.clone(),
            connection: clrecoder_core::motion::MotionConnectionId(connection),
            connected,
            stamp: clrecoder_core::motion::MotionStamp {
                mono_us: 0,
                unix_us: fixed_now_us() + stamp_offset_s * 1_000_000,
            },
            probe_status: probe,
            auto_dpi,
            auto_valid_until_unix_us: auto_until_s.map(|s| fixed_now_us() + s * 1_000_000),
        }
    }

    /// 文本映射必须与 §4.1 serde 小写序列化逐字一致（DDL CHECK 值与 serde 契约不漂移）。
    #[test]
    fn motion_dpi_motion_text_mappings_match_serde() {
        for (o, text) in [
            (DpiOrigin::Auto, "auto"),
            (DpiOrigin::Manual, "manual"),
            (DpiOrigin::Unknown, "unknown"),
        ] {
            assert_eq!(dpi_origin_to_text(o), text);
            assert_eq!(serde_json::to_string(&o).unwrap(), format!(r#""{text}""#));
            assert_eq!(dpi_origin_from_text(text).unwrap(), o);
        }
        for (s, text) in [
            (DpiProbeStatus::Pending, "pending"),
            (DpiProbeStatus::Available, "available"),
            (DpiProbeStatus::Unsupported, "unsupported"),
            (DpiProbeStatus::Ambiguous, "ambiguous"),
            (DpiProbeStatus::Unavailable, "unavailable"),
            (DpiProbeStatus::Disconnected, "disconnected"),
        ] {
            assert_eq!(probe_status_to_text(s), text);
            assert_eq!(serde_json::to_string(&s).unwrap(), format!(r#""{text}""#));
            assert_eq!(probe_status_from_text(text).unwrap(), s);
        }
        for (s, text) in [(StickSide::Left, "left"), (StickSide::Right, "right")] {
            assert_eq!(stick_side_to_text(s), text);
            assert_eq!(serde_json::to_string(&s).unwrap(), format!(r#""{text}""#));
            assert_eq!(stick_side_from_text(text).unwrap(), s);
        }
        // 契约外值拒绝
        assert!(dpi_origin_from_text("Auto").is_err());
        assert!(probe_status_from_text("ok").is_err());
        assert!(stick_side_from_text("center").is_err());
        // MotionAvailability serde 形状（DTO adapter 映射 ready/needs_upgrade 的对侧锚点）
        assert_eq!(serde_json::to_string(&MotionAvailability::Ready).unwrap(), r#""ready""#);
        assert_eq!(
            serde_json::to_string(&MotionAvailability::NeedsUpgrade).unwrap(),
            r#""needs_upgrade""#
        );
        // 计量校验：只接受有限非负
        assert!(validate_finite_nonneg(0.0).is_ok());
        assert!(validate_finite_nonneg(1.5).is_ok());
        assert!(validate_finite_nonneg(f64::NAN).is_err());
        assert!(validate_finite_nonneg(f64::INFINITY).is_err());
        assert!(validate_finite_nonneg(-0.1).is_err());
    }

    /// 写入行校验：dpi×origin 配对逐字合同 + DPI 上限（§6.1 CHECK 的 Rust 先行镜像）。
    #[test]
    fn motion_dpi_validate_mouse_motion_write_pairs() {
        let ok = |dpi: u32, origin: DpiOrigin| {
            validate_mouse_motion_write(&MouseMotionWrite {
                source_id: 1,
                day: DAY.to_string(),
                dpi,
                origin,
                counts: 1.0,
            })
        };
        assert!(ok(0, DpiOrigin::Unknown).is_ok());
        assert!(ok(800, DpiOrigin::Manual).is_ok());
        assert!(ok(800, DpiOrigin::Auto).is_ok());
        assert!(ok(0, DpiOrigin::Manual).is_err(), "dpi=0 只允许 unknown");
        assert!(ok(800, DpiOrigin::Unknown).is_err(), "dpi>0 禁 unknown");
        assert!(ok(MANUAL_DPI_MAX + 1, DpiOrigin::Manual).is_err(), "DPI 超上限");
        // NaN counts 拒绝
        assert!(validate_mouse_motion_write(&MouseMotionWrite {
            source_id: 1,
            day: DAY.to_string(),
            dpi: 800,
            origin: DpiOrigin::Manual,
            counts: f64::NAN,
        })
        .is_err());
    }

    /// 注册幂等：同 source_key 返回同 id、不新增行、manual/状态不被重置；
    /// 新路径 = 新来源（不按型号继承旧 manual，§6.1）；虚拟桶 physical=false。
    #[test]
    fn motion_dpi_register_mouse_source_is_idempotent_by_source_key() {
        let (_db, conn) = open_v3("motion-register");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 0x1532, 0x0045, 'M', 't', 't')",
            [],
        )
        .unwrap();
        let dev_id: i64 =
            conn.query_row("SELECT id FROM devices WHERE name='M'", [], |r| r.get(0)).unwrap();

        let d1 = descriptor(r"\\?\hid#vid_1532&pid_0045&mi_00", "M");
        let id1 = register_mouse_source(&conn, &d1, dev_id, "2026-09-28T10:00:00+08:00").unwrap();
        // 已配置 manual 后重注册：配置与既有行保留
        set_manual_dpi(&conn, id1, Some(800)).unwrap();
        let id_again =
            register_mouse_source(&conn, &d1, dev_id, "2026-09-28T11:00:00+08:00").unwrap();
        assert_eq!(id1, id_again, "同 source_key 幂等");
        let (rows, manual, last_seen): (i64, Option<i64>, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(manual_dpi), MAX(last_seen) FROM mouse_motion_sources",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(rows, 1, "重注册不新增行");
        assert_eq!(manual, Some(800), "重注册不清 manual");
        assert_eq!(last_seen, "2026-09-28T11:00:00+08:00", "重注册刷新 last_seen");

        // 新路径（换端口）：新来源、manual 不继承
        let d2 = descriptor(r"\\?\hid#vid_1532&pid_0045&mi_01", "M");
        let id2 = register_mouse_source(&conn, &d2, dev_id, "2026-09-28T11:00:00+08:00").unwrap();
        assert_ne!(id1, id2);
        let manual2: Option<i64> = conn
            .query_row("SELECT manual_dpi FROM mouse_motion_sources WHERE id=?1", [id2], |r| r.get(0))
            .unwrap();
        assert_eq!(manual2, None, "新来源不套旧 manual");

        // 虚拟桶：physical=false
        let idv = register_mouse_source(&conn, &virtual_descriptor(), dev_id, "t").unwrap();
        let phys: i64 = conn
            .query_row("SELECT physical FROM mouse_motion_sources WHERE id=?1", [idv], |r| r.get(0))
            .unwrap();
        assert_eq!(phys, 0);
    }

    /// 状态心跳：connected/probe/auto/last_seen 落库；未知 id 报错；auto DPI 越界拒绝。
    #[test]
    fn motion_dpi_update_mouse_source_state_applies_and_rejects() {
        let (_db, conn) = open_v3("motion-state");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 1, 2, 'M', 't', 't')",
            [],
        )
        .unwrap();
        let d = descriptor("k1", "M");
        let id = register_mouse_source(&conn, &d, 1, "2026-09-28T10:00:00+08:00").unwrap();

        let st = state(&d, 7, true, 0, DpiProbeStatus::Available, Some(800), Some(4));
        update_mouse_source_state(&conn, id, &st).unwrap();
        let (connected, probe, auto, until, last_seen): (i64, String, Option<i64>, Option<i64>, String) = conn
            .query_row(
                "SELECT connected, probe_status, auto_dpi, auto_valid_until_unix_us, last_seen
                 FROM mouse_motion_sources WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(connected, 1);
        assert_eq!(probe, "available");
        assert_eq!(auto, Some(800));
        assert_eq!(until, Some(fixed_now_us() + 4_000_000));
        assert_eq!(last_seen, rfc_at(2026, 9, 28, 12, 0, 0));

        // 断连快照（Disconnected + connected=false）
        let down = state(&d, 7, false, 1, DpiProbeStatus::Disconnected, None, None);
        update_mouse_source_state(&conn, id, &down).unwrap();
        let (connected, probe): (i64, String) = conn
            .query_row(
                "SELECT connected, probe_status FROM mouse_motion_sources WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((connected, probe.as_str()), (0, "disconnected"));

        // 未知 id 报错；auto DPI 越界拒绝
        let ghost = state(&d, 7, true, 0, DpiProbeStatus::Pending, None, None);
        assert!(matches!(
            update_mouse_source_state(&conn, 999, &ghost),
            Err(StoreError::UnknownMouseSource(999))
        ));
        assert!(update_mouse_source_state(
            &conn,
            id,
            &state(&d, 7, true, 0, DpiProbeStatus::Available, Some(0), None)
        )
        .is_err());
        assert!(update_mouse_source_state(
            &conn,
            id,
            &state(&d, 7, true, 0, DpiProbeStatus::Available, Some(AUTO_DPI_MAX + 1), None)
        )
        .is_err());
    }

    /// 手动 DPI 配置：set/read 往返、None 清除、越界与未知 id 拒绝；未注册 key 不在批读返回中。
    #[test]
    fn motion_dpi_manual_dpi_roundtrip_and_guards() {
        let (_db, conn) = open_v3("motion-manual");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 1, 2, 'M', 't', 't')",
            [],
        )
        .unwrap();
        let k1 = "k1".to_string();
        let k2 = "k2".to_string();
        let id1 = register_mouse_source(&conn, &descriptor(&k1, "M"), 1, "t").unwrap();
        let id2 = register_mouse_source(&conn, &descriptor(&k2, "M"), 1, "t").unwrap();

        assert!(read_manual_dpi(&conn, &[]).unwrap().is_empty());
        set_manual_dpi(&conn, id1, Some(1600)).unwrap();
        set_manual_dpi(&conn, id2, None).unwrap(); // 未配置也返回行

        let rows = read_manual_dpi(&conn, &[k1.clone(), k2.clone(), "ghost".to_string()]).unwrap();
        assert_eq!(rows.len(), 2, "未注册 key 不在返回中");
        assert_eq!(rows[0], MouseConfigRow { source_key: k1.clone(), manual_dpi: Some(1600) });
        assert_eq!(rows[1], MouseConfigRow { source_key: k2, manual_dpi: None });

        // None 清除已配置值
        set_manual_dpi(&conn, id1, None).unwrap();
        let rows = read_manual_dpi(&conn, std::slice::from_ref(&k1)).unwrap();
        assert_eq!(rows[0].manual_dpi, None);

        // 越界与未知 id
        assert!(set_manual_dpi(&conn, id1, Some(0)).is_err());
        assert!(set_manual_dpi(&conn, id1, Some(MANUAL_DPI_MAX + 1)).is_err());
        assert!(matches!(
            set_manual_dpi(&conn, 999, Some(800)),
            Err(StoreError::UnknownMouseSource(999))
        ));
    }

    /// 来源行展示口径（§4.5/§6.1）：connected 需 last_seen≤5 秒；auto 需在线且未过期；
    /// effective = auto > manual > None；manual 可离线显示。
    #[test]
    fn motion_dpi_list_mouse_sources_freshness_and_effective_dpi() {
        let (_db, conn) = open_v3("motion-list");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 1, 2, 'M', 't', 't')",
            [],
        )
        .unwrap();
        let d = descriptor("k1", "M");
        let id = register_mouse_source(&conn, &d, 1, &rfc_at(2026, 9, 28, 11, 0, 0)).unwrap();
        // 手动 800 先落库（manual 持久、可离线显示）
        set_manual_dpi(&conn, id, Some(800)).unwrap();

        // 在线 + auto 有效（until = now+4s）：auto 展示且 effective=auto
        let st = state(&d, 7, true, 0, DpiProbeStatus::Available, Some(1600), Some(4));
        update_mouse_source_state(&conn, id, &st).unwrap();

        let now = fixed_now_us();
        let rows = list_mouse_sources(&conn, now).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            (r.id, r.device_id, r.name.as_str(), r.nickname.as_deref(), r.physical, r.connected),
            (id, 1, "M", None, true, true)
        );
        assert_eq!((r.manual_dpi, r.auto_dpi), (Some(800), Some(1600)));
        assert!(r.auto_valid_until.is_some(), "auto 有效时给出失效时刻");
        assert_eq!((r.effective_dpi, r.dpi_origin), (Some(1600), DpiOrigin::Auto));
        assert_eq!(r.probe_status, DpiProbeStatus::Available);

        // 6 秒后（未再心跳）：connected 变 false，auto 证据失效 → 只显示 manual
        let rows = list_mouse_sources(&conn, now + 6_000_000).unwrap();
        let r = &rows[0];
        assert!(!r.connected, "last_seen 超 5 秒不得算在线");
        assert_eq!(r.auto_dpi, None, "不在线则自动值不展示");
        assert_eq!(r.auto_valid_until, None);
        assert_eq!((r.effective_dpi, r.dpi_origin), (Some(800), DpiOrigin::Manual));

        // 恢复心跳但 auto 已过期（until 仍在过去）：auto 不展示，effective 回落 manual
        let late = state(&d, 7, true, 100, DpiProbeStatus::Available, Some(1600), Some(4));
        update_mouse_source_state(&conn, id, &late).unwrap();
        let rows = list_mouse_sources(&conn, fixed_now_us() + 100_000_000).unwrap();
        let r = &rows[0];
        assert!(r.connected);
        assert_eq!(r.auto_dpi, None, "过期自动值不展示");
        assert_eq!((r.effective_dpi, r.dpi_origin), (Some(800), DpiOrigin::Manual));

        // 无 manual 无 auto：effective=None / origin=Unknown
        let d2 = descriptor("k2", "M");
        register_mouse_source(&conn, &d2, 1, &rfc_at(2026, 9, 28, 11, 0, 0)).unwrap();
        let rows = list_mouse_sources(&conn, fixed_now_us()).unwrap();
        let r = rows.iter().find(|r| r.id != id).unwrap();
        assert_eq!((r.effective_dpi, r.dpi_origin), (None, DpiOrigin::Unknown));
    }

    /// §4.4 精确示例：800 桶 800 + 1600 桶 1600 + unknown 桶 400 → raw 2800、
    /// meters 0.0508、unconfigured 400、coverage≈0.857143；改 manual=3200 后
    /// 汇总**保持原样**（米按桶内固化 DPI 折算，配置不回算）。
    #[test]
    fn motion_dpi_mouse_motion_example_800_1600_unknown_and_no_recalc() {
        let (_db, conn) = open_v3("motion-example");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 1, 2, 'M', 't', 't')",
            [],
        )
        .unwrap();
        let id = register_mouse_source(&conn, &descriptor("k1", "M"), 1, "t").unwrap();
        for (dpi, origin, counts) in
            [(800u32, DpiOrigin::Manual, 800.0), (1600, DpiOrigin::Manual, 1600.0), (0, DpiOrigin::Unknown, 400.0)]
        {
            conn.execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, DAY, dpi, dpi_origin_to_text(origin), counts],
            )
            .unwrap();
        }

        let s = mouse_motion(&conn, id, DAY, DAY).unwrap();
        assert_eq!(s.availability, MotionAvailability::Ready);
        assert_eq!((s.raw_counts, s.unconfigured_counts), (2800.0, 400.0));
        assert_eq!(s.meters, Some(0.0508), "800/800×0.0254 + 1600/1600×0.0254");
        assert!(
            (s.coverage.unwrap() - 2400.0 / 2800.0).abs() < 1e-12,
            "coverage=2400/2800: {:?}",
            s.coverage
        );
        assert_eq!(s.days.len(), 1);
        let d = &s.days[0];
        assert_eq!((d.day.as_str(), d.raw_counts, d.unconfigured_counts), (DAY, 2800.0, 400.0));
        assert_eq!(d.meters, Some(0.0508));

        // 配置不回算：manual 改 3200 后以上结果保持原样
        set_manual_dpi(&conn, id, Some(3200)).unwrap();
        let s2 = mouse_motion(&conn, id, DAY, DAY).unwrap();
        assert_eq!(s2, s, "改 manual 不得回溯既有桶");
    }

    /// 同型号双来源不串设置（§4.4 示例）：两只同型号分别 1600 counts @800DPI 与
    /// 1600 counts @1600DPI → 0.0508m 与 0.0254m，汇总互不影响。
    #[test]
    fn motion_dpi_mouse_motion_two_sources_same_model_do_not_cross() {
        let (_db, conn) = open_v3("motion-two-sources");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 0x1532, 0x0045, 'M', 't', 't')",
            [],
        )
        .unwrap();
        let a = register_mouse_source(&conn, &descriptor("path-a", "M"), 1, "t").unwrap();
        let b = register_mouse_source(&conn, &descriptor("path-b", "M"), 1, "t").unwrap();
        assert_ne!(a, b);
        conn.execute(
            "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
             VALUES (?1, ?2, 800, 'manual', 1600.0)",
            params![a, DAY],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
             VALUES (?1, ?2, 1600, 'manual', 1600.0)",
            params![b, DAY],
        )
        .unwrap();

        let sa = mouse_motion(&conn, a, DAY, DAY).unwrap();
        let sb = mouse_motion(&conn, b, DAY, DAY).unwrap();
        assert_eq!((sa.raw_counts, sb.raw_counts), (1600.0, 1600.0));
        assert_eq!(sa.meters, Some(0.0508), "A：1600/800×0.0254");
        assert_eq!(sb.meters, Some(0.0254), "B：1600/1600×0.0254");
        assert_eq!((sa.coverage, sb.coverage), (Some(1.0), Some(1.0)));
    }

    /// 日过滤与纯空范围：闭区间取子集；空范围 raw=0、meters=None、coverage=None、days 空。
    #[test]
    fn motion_dpi_mouse_motion_day_filter_and_empty_range() {
        let (_db, conn) = open_v3("motion-day-filter");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 1, 2, 'M', 't', 't')",
            [],
        )
        .unwrap();
        let id = register_mouse_source(&conn, &descriptor("k1", "M"), 1, "t").unwrap();
        for (day, counts) in [(DAY, 800.0), (DAY2, 1600.0)] {
            conn.execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (?1, ?2, 800, 'manual', ?3)",
                params![id, day, counts],
            )
            .unwrap();
        }
        // 单日闭区间
        let s = mouse_motion(&conn, id, DAY, DAY).unwrap();
        assert_eq!((s.raw_counts, s.days.len()), (800.0, 1));
        // 跨日闭区间
        let s = mouse_motion(&conn, id, DAY, DAY2).unwrap();
        assert_eq!((s.raw_counts, s.days.len()), (2400.0, 2));
        assert_eq!(s.days.iter().map(|d| d.day.as_str()).collect::<Vec<_>>(), [DAY, DAY2]);
        // 逆序字典序之外的日期：完全无命中
        let s = mouse_motion(&conn, id, "2026-10-01", "2026-10-02").unwrap();
        assert_eq!(
            (s.raw_counts, s.meters, s.coverage, s.unconfigured_counts, s.days.len()),
            (0.0, None, None, 0.0, 0)
        );
        // 全 unknown 的日：raw 有值、meters=None、coverage=Some(0)
        conn.execute(
            "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
             VALUES (?1, ?2, 0, 'unknown', 500.0)",
            params![id, DAY2],
        )
        .unwrap();
        let s = mouse_motion(&conn, id, DAY2, DAY2).unwrap();
        assert_eq!((s.raw_counts, s.unconfigured_counts), (2100.0, 500.0));
        assert_eq!(s.meters, Some(1600.0 / 800.0 * 0.0254), "只有已配置部分算米");
        assert!((s.coverage.unwrap() - 1600.0 / 2100.0).abs() < 1e-12);
        let d = &s.days[0];
        assert_eq!(d.meters, Some(0.0508));
    }

    /// 未知 sourceId 报错（§4.5"不静默切全部鼠标"）。
    #[test]
    fn motion_dpi_mouse_motion_unknown_source_errors() {
        let (_db, conn) = open_v3("motion-unknown-source");
        assert!(matches!(
            mouse_motion(&conn, 42, DAY, DAY),
            Err(StoreError::UnknownMouseSource(42))
        ));
    }

    /// 手柄汇总：左右独立、625 格组装（空格 0）、日过滤、active_seconds/travel_r 口径。
    #[test]
    fn motion_dpi_gamepad_motion_grid_and_day_filter() {
        let (_db, conn) = open_v3("motion-gamepad");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('gamepad', 3, 4, '手柄', 't', 't')",
            [],
        )
        .unwrap();
        // 左摇杆 DAY：active 1.5s、两格停留、travel 0.707
        conn.execute(
            "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
             VALUES (1, ?1, 'left', 1_500_000, 0.707)",
            params![DAY],
        )
        .unwrap();
        for (bin, us) in [(312i64, 1_000_000i64), (313, 500_000)] {
            conn.execute(
                "INSERT INTO gamepad_heat_daily(device_id, day, stick, bin, dwell_us)
                 VALUES (1, ?1, 'left', ?2, ?3)",
                params![DAY, bin, us],
            )
            .unwrap();
        }
        // 右摇杆 DAY：active 0.25s、一格停留
        conn.execute(
            "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
             VALUES (1, ?1, 'right', 250_000, 0.0)",
            params![DAY],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gamepad_heat_daily(device_id, day, stick, bin, dwell_us)
             VALUES (1, ?1, 'right', 324, 250_000)",
            params![DAY],
        )
        .unwrap();
        // 左摇杆 DAY2：仅次日数据（日过滤用）
        conn.execute(
            "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
             VALUES (1, ?1, 'left', 100_000, 0.1)",
            params![DAY2],
        )
        .unwrap();

        let g = gamepad_motion(&conn, 1, DAY, DAY).unwrap();
        assert_eq!(g.availability, MotionAvailability::Ready);
        assert_eq!((g.device_id, g.grid_size), (1, 25));
        assert_eq!((g.left.active_seconds, g.left.travel_r), (1.5, 0.707));
        assert_eq!(g.left.dwell_seconds.len(), 625, "恰 625 格");
        assert_eq!(g.left.dwell_seconds[312], 1.0);
        assert_eq!(g.left.dwell_seconds[313], 0.5);
        assert_eq!(g.left.dwell_seconds[311] + g.left.dwell_seconds[314], 0.0, "空格为 0");
        assert_eq!((g.right.active_seconds, g.right.travel_r), (0.25, 0.0));
        assert_eq!(g.right.dwell_seconds[324], 0.25);
        assert_eq!(g.right.dwell_seconds[312], 0.0, "左右独立不串格");
        // 日过滤：只取 DAY2 → 仅左摇杆 0.1s
        let g2 = gamepad_motion(&conn, 1, DAY2, DAY2).unwrap();
        assert_eq!((g2.left.active_seconds, g2.left.travel_r), (0.1, 0.1));
        assert_eq!(g2.left.dwell_seconds.iter().sum::<f64>(), 0.0, "次日无热度行全 0");
        assert_eq!(g2.right.active_seconds, 0.0);
        // 无数据设备：合法空（全 0、625 格）
        let g3 = gamepad_motion(&conn, 999, DAY, DAY).unwrap();
        assert_eq!(g3.availability, MotionAvailability::Ready);
        assert_eq!(g3.left.dwell_seconds.len(), 625);
        assert_eq!(g3.left.active_seconds + g3.right.active_seconds, 0.0);
    }

    /// legacy：旧表英寸×80 只还原原始量；schema1 无旧表合法 0；mouse_legacy 行包装。
    #[test]
    fn motion_dpi_legacy_mouse_counts_times_80() {
        let (_db, conn) = open_v3("motion-legacy");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 1, 2, 'M', 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mouse_move_daily(device_id, day, distance_inches) VALUES (1, ?1, 1.5)",
            params![DAY],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mouse_move_daily(device_id, day, distance_inches) VALUES (1, ?1, 0.25)",
            params![DAY2],
        )
        .unwrap();
        assert_eq!(legacy_mouse_counts(&conn, 1, DAY, DAY).unwrap(), 120.0, "1.5×80");
        assert_eq!(legacy_mouse_counts(&conn, 1, DAY, DAY2).unwrap(), 140.0);
        assert_eq!(legacy_mouse_counts(&conn, 1, "2026-10-01", "2026-10-02").unwrap(), 0.0);
        let row = mouse_legacy(&conn, 1, DAY, DAY).unwrap();
        assert_eq!(row, LegacyMouseSummary { device_id: 1, raw_counts: 120.0 });

        // schema1（无 mouse_move_daily）：合法 0
        let db1 = TempDb::new("motion-legacy-v1");
        let conn1 = Connection::open(db1.as_ref()).unwrap();
        conn1.execute_batch(SCHEMA_V1_SQL).unwrap();
        assert_eq!(legacy_mouse_counts(&conn1, 1, DAY, DAY).unwrap(), 0.0);
    }

    /// 未知 schema 查询（§4.5/§6.3）：v2 库上 motion_schema_ready=false，新运动查询返回
    /// NeedsUpgrade/合法空，legacy（v2 有旧表）照常可读。
    #[test]
    fn motion_dpi_old_schema_queries_degrade_legally() {
        let (_db, conn) = open_v2("motion-old-schema");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 1, 2, 'M', 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mouse_move_daily(device_id, day, distance_inches) VALUES (1, ?1, 2.0)",
            params![DAY],
        )
        .unwrap();

        assert!(!motion_schema_ready(&conn).unwrap());
        assert!(list_mouse_sources(&conn, fixed_now_us()).unwrap().is_empty());

        let s = mouse_motion(&conn, 1, DAY, DAY).unwrap();
        assert_eq!(s.availability, MotionAvailability::NeedsUpgrade);
        assert_eq!(
            (s.raw_counts, s.meters, s.unconfigured_counts, s.coverage, s.days.len()),
            (0.0, None, 0.0, None, 0)
        );

        let g = gamepad_motion(&conn, 1, DAY, DAY).unwrap();
        assert_eq!(g.availability, MotionAvailability::NeedsUpgrade);
        assert_eq!(g.left.dwell_seconds.len(), 625);
        assert_eq!(g.left.dwell_seconds.iter().sum::<f64>(), 0.0);

        // legacy 在旧 schema 仍可读（×80）
        assert_eq!(legacy_mouse_counts(&conn, 1, DAY, DAY).unwrap(), 160.0);

        // 导出：新 motion 全空 + legacy 可读部分照常（§6.3）
        let ex = export_motion_rows(&conn, DAY, DAY).unwrap();
        assert!(ex.mice.is_empty() && ex.mouse_daily.is_empty());
        assert!(ex.gamepad_daily.is_empty() && ex.gamepad_heat.is_empty());
        assert_eq!(
            ex.legacy_mouse_daily,
            vec![ExportLegacyMouseRow { device_id: 1, day: DAY.to_string(), raw_counts: 160.0 }]
        );

        // 升级到 v3 后 schema 就绪（同一连接续跑迁移链）；注册来源后查询为合法空 Ready
        crate::schema::migrate(&conn).unwrap();
        assert!(motion_schema_ready(&conn).unwrap());
        let id = register_mouse_source(&conn, &descriptor("k1", "M"), 1, "t").unwrap();
        let s = mouse_motion(&conn, id, DAY, DAY).unwrap();
        assert_eq!(s.availability, MotionAvailability::Ready);
        assert_eq!(s.raw_counts, 0.0);
        // 升级不回写旧行：legacy 读数在升级后保持不变
        assert_eq!(legacy_mouse_counts(&conn, 1, DAY, DAY).unwrap(), 160.0);
    }

    /// §6.3 导出形状：mice 只列区间内有运动桶的来源、逐桶 meters、日过滤、
    /// gamepad 两表行、legacy×80；source_key 不出现在任何导出行（逐字段断言）。
    #[test]
    fn motion_dpi_export_motion_rows_shape_and_range() {
        let (_db, conn) = open_v3("motion-export");
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('mouse', 1, 2, '鼠标A', 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO devices(kind, vid, pid, name, first_seen, last_seen)
             VALUES ('gamepad', 3, 4, '手柄', 't', 't')",
            [],
        )
        .unwrap();
        let a = register_mouse_source(&conn, &descriptor("path-a", "鼠标A"), 1, "t").unwrap();
        // 来源 B：注册了但区间内无运动桶 → 不进 mice
        register_mouse_source(&conn, &descriptor("path-b", "鼠标A"), 1, "t").unwrap();
        set_manual_dpi(&conn, a, Some(800)).unwrap();
        for (day, dpi, origin, counts) in [
            (DAY, 800i64, "manual", 800.0f64),
            (DAY, 0, "unknown", 400.0),
            (DAY2, 800, "manual", 100.0),
        ] {
            conn.execute(
                "INSERT INTO mouse_motion_daily(source_id, day, dpi, dpi_origin, counts)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![a, day, dpi, origin, counts],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO gamepad_motion_daily(device_id, day, stick, active_us, travel_r)
             VALUES (2, ?1, 'right', 2_500_000, 1.25)",
            params![DAY],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gamepad_heat_daily(device_id, day, stick, bin, dwell_us)
             VALUES (2, ?1, 'right', 624, 2_500_000)",
            params![DAY],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mouse_move_daily(device_id, day, distance_inches) VALUES (1, ?1, 1.0)",
            params![DAY],
        )
        .unwrap();

        let ex = export_motion_rows(&conn, DAY, DAY).unwrap();
        // mice：仅 A（B 无 motion 桶）；source_key 不在行中（字段逐一断言）
        assert_eq!(
            ex.mice,
            vec![ExportMouseSourceRow {
                source_id: a,
                device_id: 1,
                name: "鼠标A".to_string(),
                manual_dpi: Some(800),
            }]
        );
        // mouse_daily：DAY 两桶；unknown 桶 dpi/meters=None
        assert_eq!(ex.mouse_daily.len(), 2);
        let manual_row = ex.mouse_daily.iter().find(|r| r.dpi == Some(800)).unwrap();
        assert_eq!(
            (manual_row.source_id, manual_row.day.as_str(), manual_row.counts, manual_row.meters),
            (a, DAY, 800.0, Some(0.0254))
        );
        let unknown_row = ex.mouse_daily.iter().find(|r| r.dpi.is_none()).unwrap();
        assert_eq!(
            (unknown_row.dpi_origin, unknown_row.counts, unknown_row.meters),
            (DpiOrigin::Unknown, 400.0, None)
        );
        // gamepad 两表
        assert_eq!(
            ex.gamepad_daily,
            vec![ExportGamepadMotionRow {
                device_id: 2,
                day: DAY.to_string(),
                stick: StickSide::Right,
                active_us: 2_500_000,
                travel_r: 1.25,
            }]
        );
        assert_eq!(
            ex.gamepad_heat,
            vec![ExportGamepadHeatRow {
                device_id: 2,
                day: DAY.to_string(),
                stick: StickSide::Right,
                bin: 624,
                dwell_us: 2_500_000,
            }]
        );
        // legacy×80
        assert_eq!(
            ex.legacy_mouse_daily,
            vec![ExportLegacyMouseRow { device_id: 1, day: DAY.to_string(), raw_counts: 80.0 }]
        );

        // 日过滤：[DAY2, DAY2] → 只有 A 的第三桶，mice 仍列 A
        let ex2 = export_motion_rows(&conn, DAY2, DAY2).unwrap();
        assert_eq!(ex2.mice.len(), 1);
        assert_eq!(ex2.mouse_daily.len(), 1);
        assert_eq!(ex2.mouse_daily[0].counts, 100.0);
        assert!(ex2.gamepad_daily.is_empty() && ex2.gamepad_heat.is_empty());
        assert!(ex2.legacy_mouse_daily.is_empty());
    }
}
