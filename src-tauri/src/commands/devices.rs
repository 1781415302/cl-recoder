//! get_devices / set_device_nickname —— 设备列表与昵称（PLAN §4.7；SQL 在 store）。

use serde::Serialize;

use crate::state::AppState;
use clrecoder_core::codes::DeviceKind;
use clrecoder_store::reader::{self, DeviceRow};
use clrecoder_store as store;

/// 设备行 DTO（§4.7 `DeviceRow`，camelCase：firstSeen/lastSeen/nickname）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRowDto {
    /// 设备 id
    pub id: i64,
    /// 设备种类（serde 小写）
    pub kind: DeviceKind,
    /// Vendor ID（未知为 0）
    pub vid: u16,
    /// Product ID（未知为 0）
    pub pid: u16,
    /// 显示名
    pub name: String,
    /// 用户自定义昵称（可空）
    pub nickname: Option<String>,
    /// 首次见到（RFC3339）
    pub first_seen: String,
    /// 最近见到（RFC3339）
    pub last_seen: String,
    /// lifetime 总计数
    pub total: u64,
}

impl From<DeviceRow> for DeviceRowDto {
    fn from(r: DeviceRow) -> Self {
        Self {
            id: r.id,
            kind: r.kind,
            vid: r.vid,
            pid: r.pid,
            name: r.name,
            nickname: r.nickname,
            first_seen: r.first_seen,
            last_seen: r.last_seen,
            total: r.total,
        }
    }
}

/// 内部查询（测试直连）。
pub(crate) fn query_devices(conn: &rusqlite::Connection) -> store::Result<Vec<DeviceRowDto>> {
    Ok(reader::devices(conn)?.into_iter().map(DeviceRowDto::from).collect())
}

/// 设备列表（含 lifetime total；§4.7 `get_devices() -> Vec<DeviceRow>`）。
/// 失败 → 空数据（§5.1/§5.3）。
#[tauri::command]
pub async fn get_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceRowDto>, String> {
    super::blocking_query(&state, query_devices).await
}

/// 设置设备昵称（空串/None 清除）。写走临时 rw 连接（与 WhatPulse 导入同路径）。
#[tauri::command]
pub async fn set_device_nickname(
    id: i64,
    nickname: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = crate::db::stats_db_path();
        let w = store::writer::Writer::open(&path).map_err(|e| format!("打开统计库失败: {e}"))?;
        w.set_device_nickname(id, nickname.as_deref())
            .map_err(|e| format!("保存昵称失败: {e}"))
    })
    .await
    .map_err(|e| format!("昵称任务失败: {e}"))?
}

/// 鼠标移动距离摘要（英寸→米在 GUI 层换算，与 WhatPulse 一致）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseDistanceDto {
    /// 范围内总距离（英寸）
    pub total_inches: f64,
    /// 逐日距离
    pub days: Vec<MouseDistanceDayDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseDistanceDayDto {
    pub day: String,
    pub distance_inches: f64,
}

/// 鼠标移动距离（本软件统计，非 WhatPulse 导入）。
#[tauri::command]
pub async fn get_mouse_distance(
    state: tauri::State<'_, AppState>,
    from: String,
    to: String,
) -> Result<MouseDistanceDto, String> {
    super::blocking_query(&state, move |conn| {
        let days = reader::mouse_distance_daily(conn, &from, &to)?;
        let total_inches = days.iter().map(|(_, d)| *d).sum();
        Ok(MouseDistanceDto {
            total_inches,
            days: days
                .into_iter()
                .map(|(day, distance_inches)| MouseDistanceDayDto { day, distance_inches })
                .collect(),
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_core::event::DeviceKey;
    use clrecoder_store::writer::{FlushBatch, Writer};

    /// 设备 DTO 映射 + camelCase 逐字（§4.7 DeviceRow）。
    #[test]
    fn devices_map_and_camel_case() {
        let f = TempFile::new("devices", "db");
        let w = Writer::open(f.as_ref()).unwrap();
        let kb = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0x04D9,
                pid: 0x0169,
                name: "测试键盘".into(),
            })
            .unwrap();
        w.flush(&FlushBatch { input: vec![(kb, "2026-09-28".into(), 0x1E, 2)], ..Default::default() })
            .unwrap();
        drop(w);
        let conn = rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();

        let rows = query_devices(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        let d = &rows[0];
        assert_eq!((d.id, d.kind, d.vid, d.pid, d.name.as_str(), d.total),
            (1, DeviceKind::Keyboard, 0x04D9, 0x0169, "测试键盘", 2u64));
        assert!(d.nickname.is_none());
        assert!(!d.first_seen.is_empty() && !d.last_seen.is_empty());

        let js = serde_json::to_string(d).unwrap();
        assert!(js.contains(r#""firstSeen":"#) && js.contains(r#""lastSeen":"#), "{js}");
        assert!(js.contains(r#""kind":"keyboard""#), "{js}");
        assert!(!js.contains("first_seen"), "DTO 必须是 camelCase: {js}");
    }

    /// MouseDistanceDto 序列化逐字钉死（DEVPLAN §3.7/§4.6/§4.7）：
    /// `"totalInches"`/`"distanceInches"` 在场，`total_inches`/`distance_inches` 缺席。
    /// 禁止反向拆 Rust 的 `rename_all="camelCase"`；前端已按 camelCase 对齐。
    #[test]
    fn mouse_distance_dto_serializes_camel_case() {
        let dto = MouseDistanceDto {
            total_inches: 12.5,
            days: vec![MouseDistanceDayDto {
                day: "2026-09-28".into(),
                distance_inches: 3.25,
            }],
        };
        let js = serde_json::to_string(&dto).unwrap();
        assert!(js.contains(r#""totalInches":"#), "{js}");
        assert!(js.contains(r#""distanceInches":"#), "{js}");
        assert!(!js.contains("total_inches"), "不得序列化 snake_case: {js}");
        assert!(!js.contains("distance_inches"), "不得序列化 snake_case: {js}");
    }
}
