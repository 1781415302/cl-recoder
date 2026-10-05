//! mouse_motion —— 鼠标运动查询与手动 DPI 配置（motion-dpi §4.5；SQL 全部在 store::motion）。
//!
//! 与既有查询命令（§4.7 swallow 策略）不同：§4.5 锁定"所有新增查询返回 Result，不使用
//! swallow 把 SQL 失败伪装成没有运动"——store/SQL 真实错误原样透传为 `Err(String)`；
//! 无 schema（v2 及更早库）由 store 判定：查询返回 `availability=needs_upgrade`、
//! 合法空数据返回 `ready`。原命令容错策略不全局更改（[`super::swallow`] 保持原样）。
//!
//! - 查询走 `AppState::with_ro`（spawn_blocking，错误不折叠）；
//! - `set_mouse_dpi` 是**有限 rw 配置入口**：`db::open_rw` + `store::motion::set_manual_dpi`，
//!   不用 `Writer::open` 迁移、不改 Settings JSON、不自行迁库；
//! - 校验在后端（§4.5）：正 ID、规范日期（`day::valid_range`）、DPI 范围、来源 physical
//!   与归属 kind；auto 有效（在线且未过期）时拒绝改 manual（自动值只读）；`null` 清除
//!   手动后备；
//! - `source_key`（rawpath/ContainerID）只入库匹配，任何 DTO 均不携带、不暴露前端。

use serde::Serialize;

use crate::db;
use crate::state::AppState;
use clrecoder_core::codes::DeviceKind;
use clrecoder_core::day;
use clrecoder_core::motion::{DpiOrigin, DpiProbeStatus};
use clrecoder_store as store;
use clrecoder_store::motion::{self, MouseSourceRow};

/// 手动 DPI 合同上限（schema CHECK `BETWEEN 1 AND 100000`，§6.1；与
/// `store::motion::MANUAL_DPI_MAX` 同值——该常量 pub(crate) 于 store，不跨 crate 引用）。
const MANUAL_DPI_MAX: u32 = 100_000;

// ---------------------------------------------------------------------------
// DTO（§4.5 逐字对齐 TS；camelCase 由 serde 保证，测试钉死线形状）
// ---------------------------------------------------------------------------

/// 鼠标运动来源行 DTO（§4.5 `MouseSourceRow`；字段与 store 行逐字对应）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseSourceRowDto {
    /// 来源 id（`mouse_motion_sources.id`）
    pub id: i64,
    /// 型号设备 id（型号按钮 TopKeys 等仍查它）
    pub device_id: i64,
    /// 型号显示名
    pub name: String,
    /// 型号昵称（可空）
    pub nickname: Option<String>,
    /// 是否物理来源（虚拟/未知桶为 false，禁配置 DPI）
    pub physical: bool,
    /// 当前是否在线（collector 发布且观测 ≤5 秒）
    pub connected: bool,
    /// 手动配置 DPI（持久，可离线显示）
    pub manual_dpi: Option<u32>,
    /// 当前有效的自动 DPI（要求在线且未过期，否则 null）
    pub auto_dpi: Option<u32>,
    /// 自动 DPI 失效时刻（RFC3339；仅与 auto_dpi 同真）
    pub auto_valid_until: Option<String>,
    /// 换算里程所用有效 DPI（auto 有效 > manual > null）
    pub effective_dpi: Option<u32>,
    /// effective_dpi 的取值来源
    pub dpi_origin: DpiOrigin,
    /// DPI 探测状态
    pub probe_status: DpiProbeStatus,
}

impl From<MouseSourceRow> for MouseSourceRowDto {
    fn from(r: MouseSourceRow) -> Self {
        Self {
            id: r.id,
            device_id: r.device_id,
            name: r.name,
            nickname: r.nickname,
            physical: r.physical,
            connected: r.connected,
            manual_dpi: r.manual_dpi,
            auto_dpi: r.auto_dpi,
            auto_valid_until: r.auto_valid_until,
            effective_dpi: r.effective_dpi,
            dpi_origin: r.dpi_origin,
            probe_status: r.probe_status,
        }
    }
}

/// 来源列表 DTO（§4.5 `MouseSources`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseSourcesDto {
    /// schema 可用性（旧 schema → needs_upgrade + 空 sources，引导"启动/更新采集器后可用"）
    pub availability: motion::MotionAvailability,
    /// 来源行（按来源 id 升序）
    pub sources: Vec<MouseSourceRowDto>,
}

/// 鼠标运动单日汇总 DTO（§4.5 `MouseMotionDay`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseMotionDayDto {
    /// 日期 `YYYY-MM-DD`
    pub day: String,
    /// 当日 raw counts（全部 DPI 桶求和，含 unknown）
    pub raw_counts: f64,
    /// 当日已配置部分折算米数（无任何已配置移动时 null——不用 0 米冒充）
    pub meters: Option<f64>,
    /// 当日未配置（dpi=0）counts
    pub unconfigured_counts: f64,
}

/// 鼠标运动区间汇总 DTO（§4.5 `MouseMotionSummary`；days 随 summary 返回，展开表复用）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseMotionSummaryDto {
    /// schema 可用性
    pub availability: motion::MotionAvailability,
    /// 来源 id
    pub source_id: i64,
    /// 区间 raw counts
    pub raw_counts: f64,
    /// 区间已配置部分折算米数（无任何已配置移动时 null）
    pub meters: Option<f64>,
    /// 区间未配置（dpi=0）counts
    pub unconfigured_counts: f64,
    /// 已配置 counts / 总 counts（total=0 时 null）
    pub coverage: Option<f64>,
    /// 逐日明细（按日升序；只含有运动桶的日）
    pub days: Vec<MouseMotionDayDto>,
}

impl From<motion::MouseMotionSummary> for MouseMotionSummaryDto {
    fn from(s: motion::MouseMotionSummary) -> Self {
        Self {
            availability: s.availability,
            source_id: s.source_id,
            raw_counts: s.raw_counts,
            meters: s.meters,
            unconfigured_counts: s.unconfigured_counts,
            coverage: s.coverage,
            days: s
                .days
                .into_iter()
                .map(|d| MouseMotionDayDto {
                    day: d.day,
                    raw_counts: d.raw_counts,
                    meters: d.meters,
                    unconfigured_counts: d.unconfigured_counts,
                })
                .collect(),
        }
    }
}

/// 旧算法鼠标移动读数 DTO（§4.5 `LegacyMouseSummary`；`quality` 常量由 GUI 层添加）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyMouseSummaryDto {
    /// 型号设备 id
    pub device_id: i64,
    /// 旧算法原始累计量（`mouse_move_daily.distance_inches`×80）
    pub raw_counts: f64,
    /// 固定质量标记：旧数据未校准（不换算米、不归物理来源）
    pub quality: &'static str,
}

impl From<motion::LegacyMouseSummary> for LegacyMouseSummaryDto {
    fn from(s: motion::LegacyMouseSummary) -> Self {
        Self {
            device_id: s.device_id,
            raw_counts: s.raw_counts,
            quality: "legacy_uncalibrated",
        }
    }
}

// ---------------------------------------------------------------------------
// 共用执行器与校验（gamepad_motion 复用；与 super::blocking_query 的 swallow 语义不同）
// ---------------------------------------------------------------------------

/// 新增运动查询的阻塞执行器（§4.5）：spawn_blocking 包 `with_ro`，三层错误分开透传——
/// JoinError / store（SQL 真实错误，原样文案，不 swallow）/ 内层校验 String。
pub(crate) async fn blocking_motion_query<T, F>(state: &AppState, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&rusqlite::Connection) -> Result<Result<T, String>, store::StoreError>
        + Send
        + 'static,
{
    let st = state.clone();
    match tauri::async_runtime::spawn_blocking(move || st.with_ro(f)).await {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("运动查询失败: {e}")),
        Err(e) => Err(format!("后台查询任务失败: {e}")),
    }
}

/// 正整数 id 校验（§4.5：正 ID 在后端校验）。
pub(crate) fn validate_positive_id(id: i64, what: &str) -> Result<(), String> {
    if id > 0 {
        Ok(())
    } else {
        Err(format!("{what}必须为正整数: {id}"))
    }
}

/// 日期范围校验（§4.5：日期在后端校验——必须为规范 `YYYY-MM-DD` 且 from≤to）。
pub(crate) fn validate_range(from: &str, to: &str) -> Result<(), String> {
    if day::valid_range(from, to) {
        Ok(())
    } else {
        Err(format!("日期范围无效（须为规范 YYYY-MM-DD 且 from≤to）: {from} ~ {to}"))
    }
}

/// 手动 DPI 范围校验（§4.5：DPI 范围在后端校验；None=清除后备，合法）。
fn validate_manual_dpi(dpi: Option<u32>) -> Result<(), String> {
    match dpi {
        None => Ok(()),
        Some(d) if d > 0 && d <= MANUAL_DPI_MAX => Ok(()),
        Some(d) => Err(format!("手动 DPI 超出合同范围（1..={MANUAL_DPI_MAX}）: {d}")),
    }
}

/// 按 id 取来源行（读取/配置共用；展示口径由 `list_mouse_sources` 结算）。
fn mouse_source_row_of(
    conn: &rusqlite::Connection,
    source_id: i64,
    now_unix_us: i64,
) -> Result<MouseSourceRow, String> {
    motion::list_mouse_sources(conn, now_unix_us)
        .map_err(|e| format!("运动查询失败: {e}"))?
        .into_iter()
        .find(|r| r.id == source_id)
        .ok_or_else(|| format!("未知鼠标运动来源 id: {source_id}"))
}

/// 归属 kind 校验（§4.5，读取路径）：来源必须归属 mouse 型号设备。
/// physical/auto 只读约束仅属于 set_mouse_dpi 的配置守卫——虚拟桶与 auto 有效的
/// 来源都必须能查询，读取不做任何限制。
fn ensure_mouse_source_kind(
    conn: &rusqlite::Connection,
    source_id: i64,
    now_unix_us: i64,
) -> Result<(), String> {
    let row = mouse_source_row_of(conn, source_id, now_unix_us)?;
    let kind = store::reader::device_kind_by_id(conn, row.device_id)
        .map_err(|e| format!("运动查询失败: {e}"))?
        .ok_or_else(|| format!("来源 {source_id} 归属设备 id={} 不存在", row.device_id))?;
    if kind != DeviceKind::Mouse {
        return Err(format!("来源 {source_id} 归属设备种类不是鼠标"));
    }
    Ok(())
}

/// 来源配置守卫（§4.5，仅 set_mouse_dpi）：schema 就绪、来源存在、physical、
/// 归属 kind=mouse、auto 未生效（auto 展示有效 = 在线且未过期）。通过返回该行。
fn authorized_source_row(
    conn: &rusqlite::Connection,
    source_id: i64,
    now_unix_us: i64,
) -> Result<MouseSourceRow, String> {
    if !motion::motion_schema_ready(conn).map_err(|e| format!("运动查询失败: {e}"))? {
        return Err("运动配置表未就绪，请启动/更新采集器后再试".to_string());
    }
    let row = mouse_source_row_of(conn, source_id, now_unix_us)?;
    if !row.physical {
        return Err(format!("来源 {source_id} 为虚拟/未知桶，不支持配置 DPI"));
    }
    let kind = store::reader::device_kind_by_id(conn, row.device_id)
        .map_err(|e| format!("运动查询失败: {e}"))?
        .ok_or_else(|| format!("来源 {source_id} 归属设备 id={} 不存在", row.device_id))?;
    if kind != DeviceKind::Mouse {
        return Err(format!(
            "来源 {source_id} 归属设备种类不是鼠标，不能配置鼠标 DPI"
        ));
    }
    if row.dpi_origin == DpiOrigin::Auto {
        return Err(format!(
            "来源 {source_id} 自动 DPI 有效（{}），手动配置只读；请等待自动值失效后再修改",
            row.auto_dpi.map(|d| d.to_string()).unwrap_or_default()
        ));
    }
    Ok(row)
}

// ---------------------------------------------------------------------------
// 内部查询（测试直连；now_unix_us 注入避免测试依赖真实时钟）
// ---------------------------------------------------------------------------

/// 来源列表内部查询：schema 可用性 + 全部来源行（connected/auto 展示口径在 store 结算）。
pub(crate) fn query_mouse_sources(
    conn: &rusqlite::Connection,
    now_unix_us: i64,
) -> store::Result<MouseSourcesDto> {
    let availability = if motion::motion_schema_ready(conn)? {
        motion::MotionAvailability::Ready
    } else {
        motion::MotionAvailability::NeedsUpgrade
    };
    let sources = motion::list_mouse_sources(conn, now_unix_us)?;
    Ok(MouseSourcesDto {
        availability,
        sources: sources.into_iter().map(MouseSourceRowDto::from).collect(),
    })
}

/// 单来源运动汇总内部查询（未知 sourceId 由 store 报错，不静默切全部鼠标）。
pub(crate) fn query_mouse_motion(
    conn: &rusqlite::Connection,
    source_id: i64,
    from: &str,
    to: &str,
    now_unix_us: i64,
) -> Result<MouseMotionSummaryDto, String> {
    let summary =
        motion::mouse_motion(conn, source_id, from, to).map_err(|e| format!("运动查询失败: {e}"))?;
    // 归属 kind 校验（§4.5）仅对 ready 数据：旧 schema 返回 NeedsUpgrade（无来源行可查）；
    // 读取路径不含 physical/auto 限制（那些只属于配置守卫）
    if summary.availability == motion::MotionAvailability::Ready {
        ensure_mouse_source_kind(conn, source_id, now_unix_us)?;
    }
    Ok(MouseMotionSummaryDto::from(summary))
}

/// 旧算法鼠标移动内部查询（§4.5：支持 schema2 按 deviceId 查询；schema1 无旧表合法 0；
/// 归属 kind 必须是鼠标型号；SQL 真实错误原样失败）。
pub(crate) fn query_mouse_legacy(
    conn: &rusqlite::Connection,
    device_id: i64,
    from: &str,
    to: &str,
) -> Result<LegacyMouseSummaryDto, String> {
    let kind = store::reader::device_kind_by_id(conn, device_id)
        .map_err(|e| format!("运动查询失败: {e}"))?
        .ok_or_else(|| format!("未知设备 id: {device_id}"))?;
    if kind != DeviceKind::Mouse {
        return Err(format!("设备 {device_id} 不是鼠标，无旧版移动数据"));
    }
    let s =
        motion::mouse_legacy(conn, device_id, from, to).map_err(|e| format!("运动查询失败: {e}"))?;
    Ok(LegacyMouseSummaryDto::from(s))
}

/// `set_mouse_dpi` 的同步实现（rw 连接；测试直连注入路径）：
/// 校验通过后写 store 配置函数，写成功后回读该行返回（前端刷新 sources 展示）。
fn set_manual_dpi_at(
    path: &std::path::Path,
    source_id: i64,
    dpi: Option<u32>,
) -> Result<MouseSourceRowDto, String> {
    if !path.is_file() {
        return Err("统计库不存在，请先启动采集器后再配置 DPI".to_string());
    }
    let conn = db::open_rw(path).map_err(|e| format!("打开统计库失败: {e}"))?;
    let now_unix_us = chrono::Utc::now().timestamp_micros();
    authorized_source_row(&conn, source_id, now_unix_us)?;
    motion::set_manual_dpi(&conn, source_id, dpi)
        .map_err(|e| format!("保存手动 DPI 失败: {e}"))?;
    let updated = motion::list_mouse_sources(&conn, now_unix_us)
        .map_err(|e| format!("运动查询失败: {e}"))?
        .into_iter()
        .find(|r| r.id == source_id)
        .ok_or_else(|| format!("保存后回读失败，未知鼠标运动来源 id: {source_id}"))?;
    Ok(MouseSourceRowDto::from(updated))
}

// ---------------------------------------------------------------------------
// Tauri commands（§4.5 签名逐字；查询不 swallow，配置走 open_rw）
// ---------------------------------------------------------------------------

/// 来源列表（§4.5 `get_mouse_sources(state) -> Result<MouseSourcesDto, String>`）。
/// 旧 schema → `needs_upgrade` + 空 sources（引导态，不算错误）；SQL 真实错误原样返回。
#[tauri::command]
pub async fn get_mouse_sources(
    state: tauri::State<'_, AppState>,
) -> Result<MouseSourcesDto, String> {
    blocking_motion_query(&state, |conn| {
        query_mouse_sources(conn, chrono::Utc::now().timestamp_micros()).map(Ok)
    })
    .await
}

/// 单来源运动汇总（§4.5 `get_mouse_motion(state, source_id, from, to)`）；
/// 未知 sourceId 返回错误，不静默切全部鼠标；纯空范围 raw=0、meters=null、coverage=null。
#[tauri::command]
pub async fn get_mouse_motion(
    state: tauri::State<'_, AppState>,
    source_id: i64,
    from: String,
    to: String,
) -> Result<MouseMotionSummaryDto, String> {
    validate_positive_id(source_id, "来源 id")?;
    validate_range(&from, &to)?;
    blocking_motion_query(&state, move |conn| {
        Ok(query_mouse_motion(
            conn,
            source_id,
            &from,
            &to,
            chrono::Utc::now().timestamp_micros(),
        ))
    })
    .await
}

/// 旧算法鼠标移动读数（§4.5 `get_mouse_legacy(state, device_id, from, to)`）；
/// 按型号 deviceId 查询（与按钮 TopKeys 同一型号 id），quality 恒为 legacy_uncalibrated。
#[tauri::command]
pub async fn get_mouse_legacy(
    state: tauri::State<'_, AppState>,
    device_id: i64,
    from: String,
    to: String,
) -> Result<LegacyMouseSummaryDto, String> {
    validate_positive_id(device_id, "设备 id")?;
    validate_range(&from, &to)?;
    blocking_motion_query(&state, move |conn| {
        Ok(query_mouse_legacy(conn, device_id, &from, &to))
    })
    .await
}

/// 设置/清空来源手动 DPI（§4.5 `set_mouse_dpi(source_id, dpi) -> Result<MouseSourceRowDto, String>`，
/// 无 state）：经 `db::open_rw` 调 store 配置函数，不用 `Writer::open` 迁移。
/// auto 有效时拒绝改 manual；`None` 清除手动后备；写成功才返回更新后的行。
#[tauri::command]
pub async fn set_mouse_dpi(source_id: i64, dpi: Option<u32>) -> Result<MouseSourceRowDto, String> {
    validate_positive_id(source_id, "来源 id")?;
    validate_manual_dpi(dpi)?;
    tauri::async_runtime::spawn_blocking(move || {
        set_manual_dpi_at(&db::stats_db_path(), source_id, dpi)
    })
    .await
    .map_err(|e| format!("配置任务失败: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_core::event::DeviceKey;
    use clrecoder_core::motion::{MotionConnectionId, MotionStamp, MouseSourceState};
    use clrecoder_store::writer::{FlushBatch, Writer};
    use clrecoder_store::motion::{MouseMotionWrite, StickMotionWrite};

    /// 固定测试日。
    const DAY: &str = "2026-09-28";
    const DAY2: &str = "2026-09-29";

    /// 固定观察时刻（本地 2026-09-28 12:00:00）的 unix µs——查询注入该值，不依赖真实时钟。
    fn fixed_now_us() -> i64 {
        use chrono::TimeZone;
        chrono::Local
            .with_ymd_and_hms(2026, 9, 28, 12, 0, 0)
            .unwrap()
            .timestamp_micros()
    }

    /// 建临时 v3 库（Writer 迁移 + Drop 释放句柄）。
    fn seeded_v3(tag: &str) -> (TempFile, Writer) {
        let f = TempFile::new(tag, "db");
        let w = Writer::open(f.as_ref()).unwrap();
        (f, w)
    }

    /// G102 型号身份（两只同型号来源共用）。
    fn g102_key() -> DeviceKey {
        DeviceKey {
            kind: DeviceKind::Mouse,
            vid: 0x046D,
            pid: 0xC092,
            name: "Logitech G102 游戏鼠标".into(),
        }
    }

    /// 物理来源描述（source_key 可区分两只同型号）。
    fn source_desc(key: &str, model: DeviceKey, physical: bool) -> clrecoder_core::motion::MouseSourceDescriptor {
        clrecoder_core::motion::MouseSourceDescriptor {
            source_key: key.to_string(),
            model,
            interface_path: None,
            physical,
        }
    }

    /// 心跳快照（stamp 取固定时刻 + 偏移秒）。
    fn heartbeat(
        desc: &clrecoder_core::motion::MouseSourceDescriptor,
        connected: bool,
        offset_s: i64,
        probe: DpiProbeStatus,
        auto_dpi: Option<u32>,
        auto_until_s: Option<i64>,
    ) -> MouseSourceState {
        MouseSourceState {
            descriptor: desc.clone(),
            connection: MotionConnectionId(7),
            connected,
            stamp: MotionStamp { mono_us: 0, unix_us: fixed_now_us() + offset_s * 1_000_000 },
            probe_status: probe,
            auto_dpi,
            auto_valid_until_unix_us: auto_until_s.map(|s| fixed_now_us() + s * 1_000_000),
        }
    }

    /// 心跳快照（真实时钟 stamp/until——`set_manual_dpi_at` 内部用 `Utc::now()` 观察，
    /// "auto 有效拒绝改 manual" 的守卫必须以同钟基准才可测）。
    fn heartbeat_live(
        desc: &clrecoder_core::motion::MouseSourceDescriptor,
        connected: bool,
        probe: DpiProbeStatus,
        auto_dpi: Option<u32>,
        auto_until_s: Option<i64>,
    ) -> MouseSourceState {
        let now = chrono::Utc::now().timestamp_micros();
        MouseSourceState {
            descriptor: desc.clone(),
            connection: MotionConnectionId(7),
            connected,
            stamp: MotionStamp { mono_us: 0, unix_us: now },
            probe_status: probe,
            auto_dpi,
            auto_valid_until_unix_us: auto_until_s.map(|s| now + s * 1_000_000),
        }
    }

    /// 只读连接（与 commands::testutil::seeded_db 同形，但由本模块造数后打开）。
    fn ro_conn(f: &TempFile) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap()
    }

    /// DTO camelCase 线形状逐字钉死（§4.5 MouseSources/MouseSourceRow）：
    /// deviceId/manualDpi/autoDpi/autoValidUntil/effectiveDpi/dpiOrigin/probeStatus 在场，
    /// snake_case 缺席，source_key（rawpath）不出现。
    #[test]
    fn motion_dpi_mouse_sources_dto_camel_case_and_freshness() {
        let (f, w) = seeded_v3("mouse-sources");
        let desc_a = source_desc(r"\\?\hid#vid_046d&pid_c092&mi_00", g102_key(), true);
        let id_a = w.register_mouse_source(&desc_a).unwrap();
        // 同型号第二只（不同接口路径）→ 独立来源
        let desc_b = source_desc(r"\\?\hid#vid_046d&pid_c092&mi_01", g102_key(), true);
        let id_b = w.register_mouse_source(&desc_b).unwrap();
        assert_ne!(id_a, id_b);
        // 虚拟桶：physical=false（归属"未知/虚拟设备"型号行）
        let id_v = w
            .register_mouse_source(&source_desc(
                "virtual:unknown",
                DeviceKey {
                    kind: DeviceKind::Mouse,
                    vid: 0,
                    pid: 0,
                    name: "未知/虚拟设备".into(),
                },
                false,
            ))
            .unwrap();
        drop(w);
        // manual 先落库（经被测命令路径；Writer 释放后写，避免并发写句柄）
        assert!(set_manual_dpi_at(f.as_ref(), id_a, Some(800)).is_ok());
        assert!(set_manual_dpi_at(f.as_ref(), id_b, Some(1200)).is_ok());
        // 心跳：A auto 1600 有效（在线、未过期）；B 探测失败；虚拟桶离线
        {
            let w = Writer::open(f.as_ref()).unwrap();
            w.update_mouse_source_state(
                id_a,
                &heartbeat(&desc_a, true, 0, DpiProbeStatus::Available, Some(1600), Some(30)),
            )
            .unwrap();
            w.update_mouse_source_state(
                id_b,
                &heartbeat(&desc_b, true, 0, DpiProbeStatus::Unavailable, None, None),
            )
            .unwrap();
            w.update_mouse_source_state(
                id_v,
                &heartbeat(&desc_a, false, 0, DpiProbeStatus::Disconnected, None, None),
            )
            .unwrap();
        }

        let conn = ro_conn(&f);
        let dto = query_mouse_sources(&conn, fixed_now_us()).unwrap();
        assert_eq!(dto.availability, motion::MotionAvailability::Ready);
        assert_eq!(dto.sources.len(), 3);

        let a = dto.sources.iter().find(|r| r.id == id_a).unwrap();
        assert_eq!(
            (a.device_id, a.physical, a.connected),
            (1, true, true),
            "同型号两来源都归属 device 1"
        );
        assert_eq!((a.manual_dpi, a.auto_dpi), (Some(800), Some(1600)));
        assert!(a.auto_valid_until.is_some(), "auto 有效时给出失效时刻");
        assert_eq!((a.effective_dpi, a.dpi_origin), (Some(1600), DpiOrigin::Auto));
        assert_eq!(a.probe_status, DpiProbeStatus::Available);

        let b = dto.sources.iter().find(|r| r.id == id_b).unwrap();
        assert!(b.connected);
        assert_eq!(b.auto_dpi, None, "无自动值不展示");
        assert_eq!((b.effective_dpi, b.dpi_origin), (Some(1200), DpiOrigin::Manual));
        assert_eq!(b.probe_status, DpiProbeStatus::Unavailable);

        let v = dto.sources.iter().find(|r| r.id == id_v).unwrap();
        assert_eq!((v.physical, v.connected), (false, false));

        // camelCase 线形状逐字（任取一行序列化）
        let js = serde_json::to_string(&dto.sources[0]).unwrap();
        for key in
            ["\"deviceId\"", "\"manualDpi\"", "\"autoDpi\"", "\"autoValidUntil\"", "\"effectiveDpi\"", "\"dpiOrigin\"", "\"probeStatus\""]
        {
            assert!(js.contains(key), "缺 {key}: {js}");
        }
        assert!(!js.contains("device_id") && !js.contains("manual_dpi"), "snake_case 不得出现: {js}");
        assert!(
            !js.contains("hid#") && !js.contains("source_key") && !js.contains("sourceKey"),
            "rawpath/source_key 不得暴露前端: {js}"
        );

        // 心跳过期（>5s）：connected 变 false、auto 证据失效 → effective 回落 manual（§6.1）
        let dto = query_mouse_sources(&conn, fixed_now_us() + 6_000_000).unwrap();
        let a = dto.sources.iter().find(|r| r.id == id_a).unwrap();
        assert!(!a.connected, "观测超 5 秒不得算在线");
        assert_eq!(a.auto_dpi, None, "不在线则自动值不展示");
        assert_eq!((a.effective_dpi, a.dpi_origin), (Some(800), DpiOrigin::Manual));
    }

    /// 单来源运动汇总（§4.4 口径 + §4.5 DTO）：多桶 meters、partial coverage、
    /// 逐日 days 随 summary 返回、纯空范围全空、未知 sourceId 报错、camelCase 逐字。
    #[test]
    fn motion_dpi_mouse_motion_summary_days_empty_and_unknown() {
        let (f, w) = seeded_v3("mouse-motion");
        let desc = source_desc("path-a", g102_key(), true);
        let id = w.register_mouse_source(&desc).unwrap();
        // DAY：1600 桶 1600 counts + unknown 桶 400 counts（partial coverage）
        // DAY2：仅 unknown 桶 500 counts（meters=None 的日）
        w.flush(&FlushBatch {
            mouse_motion: vec![
                MouseMotionWrite {
                    source_id: id,
                    day: DAY.into(),
                    dpi: 1600,
                    origin: DpiOrigin::Auto,
                    counts: 1600.0,
                },
                MouseMotionWrite {
                    source_id: id,
                    day: DAY.into(),
                    dpi: 0,
                    origin: DpiOrigin::Unknown,
                    counts: 400.0,
                },
                MouseMotionWrite {
                    source_id: id,
                    day: DAY2.into(),
                    dpi: 0,
                    origin: DpiOrigin::Unknown,
                    counts: 500.0,
                },
            ],
            ..Default::default()
        })
        .unwrap();
        drop(w);

        let conn = ro_conn(&f);
        let s = query_mouse_motion(&conn, id, DAY, DAY2, fixed_now_us()).unwrap();
        assert_eq!(s.availability, motion::MotionAvailability::Ready);
        assert_eq!(s.source_id, id);
        assert_eq!((s.raw_counts, s.unconfigured_counts), (2500.0, 900.0));
        assert_eq!(s.meters, Some(1600.0 / 1600.0 * 0.0254), "只有已配置部分算米");
        assert!((s.coverage.unwrap() - 1600.0 / 2500.0).abs() < 1e-12);
        assert_eq!(s.days.len(), 2, "days 随 summary 返回（展开表复用，不额外查询）");
        assert_eq!(s.days[0].day, DAY);
        assert_eq!(s.days[0].meters, Some(0.0254));
        assert_eq!(s.days[1].meters, None, "全 unknown 的日不用 0 米冒充");

        // 纯空范围：raw=0、meters=null、coverage=null、days 空（§4.5）
        let s = query_mouse_motion(&conn, id, "2026-10-01", "2026-10-02", fixed_now_us()).unwrap();
        assert_eq!(
            (s.raw_counts, s.meters, s.coverage, s.unconfigured_counts, s.days.len()),
            (0.0, None, None, 0.0, 0)
        );

        // 未知 sourceId 报错，不静默切全部鼠标（§4.5）
        let err = query_mouse_motion(&conn, 999, DAY, DAY, fixed_now_us()).unwrap_err();
        assert!(err.contains("未知鼠标运动来源 id: 999"), "{err}");

        // camelCase 线形状逐字（§4.5 MouseMotionSummary/MouseMotionDay）
        let s = query_mouse_motion(&conn, id, DAY, DAY, fixed_now_us()).unwrap();
        let js = serde_json::to_string(&s).unwrap();
        for key in ["\"sourceId\"", "\"rawCounts\"", "\"unconfiguredCounts\"", "\"coverage\""] {
            assert!(js.contains(key), "缺 {key}: {js}");
        }
        assert!(js.contains(r#""availability":"ready""#), "{js}");
        assert!(!js.contains("needs_upgrade"), "ready 场景不得出现 needs_upgrade: {js}");
        assert!(!js.contains("source_id") && !js.contains("raw_counts"), "snake_case 不得出现: {js}");
    }

    /// 旧 schema（§4.5）：v3 库移除运动四表模拟 v2 库——sources 返回 needs_upgrade+空、
    /// motion 返回 needs_upgrade 全零（不算错误）、legacy（旧表仍在）照常可读。
    #[test]
    fn motion_dpi_old_schema_degrades_needs_upgrade_legacy_readable() {
        let (f, w) = seeded_v3("mouse-old-schema");
        let desc = source_desc("path-a", g102_key(), true);
        let id = w.register_mouse_source(&desc).unwrap();
        w.flush(&FlushBatch {
            mouse_motion: vec![MouseMotionWrite {
                source_id: id,
                day: DAY.into(),
                dpi: 800,
                origin: DpiOrigin::Manual,
                counts: 800.0,
            }],
            mouse_move: vec![(1, DAY.to_string(), 1.5)],
            ..Default::default()
        })
        .unwrap();
        drop(w);
        // 模拟旧 schema：删运动四表（§4.5 无 schema 判定按表存在性）
        {
            let conn = rusqlite::Connection::open(f.as_ref()).unwrap();
            conn.execute_batch(
                "DROP TABLE IF EXISTS gamepad_heat_daily;
                 DROP TABLE IF EXISTS gamepad_motion_daily;
                 DROP TABLE IF EXISTS mouse_motion_daily;
                 DROP TABLE IF EXISTS mouse_motion_sources;",
            )
            .unwrap();
        }
        let conn = ro_conn(&f);

        let dto = query_mouse_sources(&conn, fixed_now_us()).unwrap();
        assert_eq!(dto.availability, motion::MotionAvailability::NeedsUpgrade);
        assert!(dto.sources.is_empty(), "旧 schema 无来源行（引导态）");

        let s = query_mouse_motion(&conn, id, DAY, DAY, fixed_now_us()).unwrap();
        assert_eq!(s.availability, motion::MotionAvailability::NeedsUpgrade);
        assert_eq!(
            (s.raw_counts, s.meters, s.coverage, s.unconfigured_counts, s.days.len()),
            (0.0, None, None, 0.0, 0)
        );

        // legacy 在旧 schema 仍可读（×80）——旧按钮/历史不从界面消失的数据侧依据
        let legacy = query_mouse_legacy(&conn, 1, DAY, DAY).unwrap();
        assert_eq!(legacy.raw_counts, 120.0);
        assert_eq!(legacy.quality, "legacy_uncalibrated");
    }

    /// set_mouse_dpi 守卫链（§4.5）：DPI 范围、未知来源、虚拟桶、归属 kind、
    /// auto 有效只读、auto 失效后可编辑原 manual、None 清除、写后回读、落库持久。
    #[test]
    fn motion_dpi_set_mouse_dpi_guards_and_roundtrip() {
        let (f, w) = seeded_v3("mouse-set-dpi");
        let desc_a = source_desc("path-a", g102_key(), true);
        let id_a = w.register_mouse_source(&desc_a).unwrap();
        let desc_b = source_desc("path-b", g102_key(), true);
        let id_b = w.register_mouse_source(&desc_b).unwrap();
        // 虚拟桶（禁配置）
        let id_v = w
            .register_mouse_source(&source_desc(
                "virtual:unknown",
                DeviceKey { kind: DeviceKind::Mouse, vid: 0, pid: 0, name: "未知/虚拟设备".into() },
                false,
            ))
            .unwrap();
        // 归属 kind 守卫：来源挂在键盘型号上（防御采集侧异常归并）
        let id_kb = w
            .register_mouse_source(&source_desc(
                "path-kb",
                DeviceKey { kind: DeviceKind::Keyboard, vid: 1, pid: 2, name: "键盘".into() },
                true,
            ))
            .unwrap();
        // B：auto 有效（在线、未过期，真实时钟基准）→ 只读
        w.update_mouse_source_state(
            id_b,
            &heartbeat_live(&desc_b, true, DpiProbeStatus::Available, Some(1600), Some(30)),
        )
        .unwrap();
        drop(w);
        let path = f.to_path_buf();

        // DPI 范围校验（命令层前置）
        assert!(validate_manual_dpi(Some(0)).is_err(), "0 非法（清除用 null）");
        assert!(validate_manual_dpi(Some(MANUAL_DPI_MAX + 1)).is_err());
        assert!(validate_manual_dpi(None).is_ok());
        assert!(validate_manual_dpi(Some(800)).is_ok());

        // 未知来源
        let err = set_manual_dpi_at(&path, 999, Some(800)).unwrap_err();
        assert!(err.contains("未知鼠标运动来源 id: 999"), "{err}");
        // 虚拟桶禁配置
        let err = set_manual_dpi_at(&path, id_v, Some(800)).unwrap_err();
        assert!(err.contains("不支持配置 DPI"), "{err}");
        // 归属 kind 非鼠标
        let err = set_manual_dpi_at(&path, id_kb, Some(800)).unwrap_err();
        assert!(err.contains("不是鼠标"), "{err}");
        // auto 有效 → 拒绝改 manual（自动值只读）
        let err = set_manual_dpi_at(&path, id_b, Some(800)).unwrap_err();
        assert!(err.contains("自动 DPI 有效") && err.contains("只读"), "{err}");

        // A：写成功 → 回读行生效
        let row = set_manual_dpi_at(&path, id_a, Some(3200)).unwrap();
        assert_eq!((row.id, row.manual_dpi, row.effective_dpi, row.dpi_origin),
            (id_a, Some(3200), Some(3200), DpiOrigin::Manual));
        // auto 失效后 B 可编辑原 manual（§4.5"Unavailable 后可编辑"）：探测降级为 Unavailable
        {
            let w = Writer::open(&path).unwrap();
            w.update_mouse_source_state(
                id_b,
                &heartbeat(&desc_b, true, 0, DpiProbeStatus::Unavailable, None, None),
            )
            .unwrap();
        }
        let row = set_manual_dpi_at(&path, id_b, Some(900)).unwrap();
        assert_eq!((row.manual_dpi, row.effective_dpi), (Some(900), Some(900)));
        // None 清除后备
        let row = set_manual_dpi_at(&path, id_a, None).unwrap();
        assert_eq!((row.manual_dpi, row.effective_dpi, row.dpi_origin),
            (None, None, DpiOrigin::Unknown), "清除后无后备");

        // 落库持久（重开连接读 store 批读接口）
        {
            let conn = ro_conn(&f);
            let rows = clrecoder_store::motion::read_manual_dpi(
                &conn,
                &[desc_a.source_key.clone(), desc_b.source_key.clone()],
            )
            .unwrap();
            assert_eq!(rows[0].manual_dpi, None, "A 已清除");
            assert_eq!(rows[1].manual_dpi, Some(900), "B 持久化");
        }

        // 库不存在：报错且不建文件（不自行迁库）
        let missing = crate::state::testutil::temp_path("mouse-set-dpi-missing", "db");
        crate::state::testutil::remove_all(&missing);
        let err = set_manual_dpi_at(&missing, id_a, Some(800)).unwrap_err();
        assert!(err.contains("统计库不存在"), "{err}");
        assert!(!missing.is_file(), "set 失败不得凭空建库");
    }

    /// get_mouse_legacy（§4.5）：×80 口径、quality 常量、camelCase、kind/存在性校验、
    /// schema1（无旧表）合法 0、真实 SQL 失败原样返回。
    #[test]
    fn motion_dpi_mouse_legacy_quality_kind_and_guards() {
        let (f, w) = seeded_v3("mouse-legacy");
        let ms = w.get_or_create_device(&g102_key()).unwrap();
        let kb = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0x04D9,
                pid: 0x0169,
                name: "键盘".into(),
            })
            .unwrap();
        w.flush(&FlushBatch {
            mouse_move: vec![(ms, DAY.to_string(), 1.5), (ms, DAY2.to_string(), 0.25)],
            ..Default::default()
        })
        .unwrap();
        drop(w);
        let conn = ro_conn(&f);

        let row = query_mouse_legacy(&conn, ms, DAY, DAY).unwrap();
        assert_eq!(row.raw_counts, 120.0, "1.5×80 只还原旧算法原始量");
        assert_eq!(row.device_id, ms);
        assert_eq!(row.quality, "legacy_uncalibrated");
        let js = serde_json::to_string(&row).unwrap();
        assert_eq!(
            js,
            r#"{"deviceId":1,"rawCounts":120.0,"quality":"legacy_uncalibrated"}"#,
            "{js}"
        );

        // 空范围合法 0
        let row = query_mouse_legacy(&conn, ms, "2026-10-01", "2026-10-02").unwrap();
        assert_eq!(row.raw_counts, 0.0);

        // 归属 kind 校验：键盘型号拒绝
        let err = query_mouse_legacy(&conn, kb, DAY, DAY).unwrap_err();
        assert!(err.contains("不是鼠标"), "{err}");
        // 不存在的设备拒绝
        let err = query_mouse_legacy(&conn, 999, DAY, DAY).unwrap_err();
        assert!(err.contains("未知设备 id: 999"), "{err}");

        // schema1（无旧表）合法 0：删掉 mouse_move_daily 后按 deviceId 查询仍成功
        drop(conn);
        {
            let c = rusqlite::Connection::open(f.as_ref()).unwrap();
            c.execute_batch("DROP TABLE IF EXISTS mouse_move_daily;").unwrap();
        }
        let conn = ro_conn(&f);
        let row = query_mouse_legacy(&conn, ms, DAY, DAY).unwrap();
        assert_eq!(row.raw_counts, 0.0, "无旧距离表 → 合法 0（§4.5）");
    }

    /// 参数校验（§4.5"正 ID、日期在后端校验"）：非正 id 与非法日期在进 store 前被拒。
    #[test]
    fn motion_dpi_validate_positive_id_and_range() {
        assert!(validate_positive_id(0, "来源 id").is_err());
        assert!(validate_positive_id(-1, "设备 id").is_err());
        assert!(validate_positive_id(1, "来源 id").is_ok());
        assert!(validate_range(DAY, DAY).is_ok());
        assert!(validate_range("2026-09-29", DAY).is_err(), "逆序拒绝");
        assert!(validate_range("2026-9-8", DAY).is_err(), "非规范格式拒绝");
        assert!(validate_range("垃圾", DAY).is_err());
    }

    /// 回归：auto 有效的来源**读取**必须正常（"自动值只读"守卫只属于 set_mouse_dpi，
    /// 读取路径不受 physical/auto 限制——否则在线自动 DPI 的来源永远查不出运动）。
    #[test]
    fn motion_dpi_mouse_motion_read_allowed_while_auto_effective() {
        let (f, w) = seeded_v3("mouse-read-auto");
        let desc = source_desc("path-a", g102_key(), true);
        let id = w.register_mouse_source(&desc).unwrap();
        w.flush(&FlushBatch {
            mouse_motion: vec![MouseMotionWrite {
                source_id: id,
                day: DAY.into(),
                dpi: 1600,
                origin: DpiOrigin::Auto,
                counts: 1600.0,
            }],
            ..Default::default()
        })
        .unwrap();
        drop(w);
        // 真实时钟基准的 auto 有效心跳（与 set/query 内部观察时刻同钟）
        {
            let w = Writer::open(f.as_ref()).unwrap();
            w.update_mouse_source_state(
                id,
                &heartbeat_live(&desc, true, DpiProbeStatus::Available, Some(1600), Some(30)),
            )
            .unwrap();
        }
        let conn = ro_conn(&f);
        let now = chrono::Utc::now().timestamp_micros();
        // 前置确认：展示口径 auto 生效
        let dto = query_mouse_sources(&conn, now).unwrap();
        let row = dto.sources.iter().find(|r| r.id == id).unwrap();
        assert_eq!((row.effective_dpi, row.dpi_origin), (Some(1600), DpiOrigin::Auto));
        // 读取成功（不受 auto 只读守卫影响）
        let s = query_mouse_motion(&conn, id, DAY, DAY, now).unwrap();
        assert_eq!(s.availability, motion::MotionAvailability::Ready);
        assert_eq!(s.raw_counts, 1600.0);
        assert_eq!(s.meters, Some(1600.0 / 1600.0 * 0.0254));
        // 对照：同一状态下 set 被拒（守卫仍在配置路径）
        let err = set_manual_dpi_at(f.as_ref(), id, Some(800)).unwrap_err();
        assert!(err.contains("自动 DPI 有效"), "{err}");
    }

    /// StickMotionWrite 的写入侧契约已在 store 测过；此处只保证 FlushBatch 运动字段
    /// 与查询链路（writer→store::motion→DTO）全通，防止 src-tauri 侧误接线。
    #[test]
    fn motion_dpi_flush_batch_motion_fields_reach_query() {
        let (f, w) = seeded_v3("mouse-flush-link");
        let gp = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 0,
                pid: 0,
                name: "XInput 手柄".into(),
            })
            .unwrap();
        w.flush(&FlushBatch {
            stick_motion: vec![StickMotionWrite {
                device_id: gp,
                day: DAY.into(),
                side: clrecoder_core::motion::StickSide::Left,
                active_us: 1_000_000,
                travel_r: 2.0,
                bins: vec![clrecoder_core::motion::StickBinDelta { bin: 312, dwell_us: 1_000_000 }],
            }],
            ..Default::default()
        })
        .unwrap();
        drop(w);
        let conn = ro_conn(&f);
        let g = clrecoder_store::motion::gamepad_motion(&conn, gp, DAY, DAY).unwrap();
        assert_eq!(g.availability, motion::MotionAvailability::Ready);
        assert_eq!((g.left.active_seconds, g.left.travel_r), (1.0, 2.0));
    }
}
