//! get_overview —— 仪表盘组合查询（PLAN §4.7；SQL 全部在 store::reader::overview）。

use serde::Serialize;

use crate::state::AppState;
use clrecoder_core::codes::DeviceKind;
use clrecoder_store::reader::{self, OverviewData};
use clrecoder_store as store;

/// 仪表盘 DTO（§4.7 `Overview`，camelCase）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewDto {
    /// 范围内逐日总量
    pub days: Vec<DayDto>,
    /// `to` 日按设备种类拆分（前端当"今日"）
    pub today: TodayDto,
    /// 设备列表（lifetime total）
    pub devices: Vec<OverviewDeviceDto>,
}

/// 逐日总量（§4.7 `{ day: string; total: number }`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayDto {
    /// 日期 YYYY-MM-DD
    pub day: String,
    /// 当日总量
    pub total: u64,
}

/// 今日拆分（§4.7 `{ keys, clicks, gamepad }`）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodayDto {
    /// 键盘按键合计
    pub keys: u64,
    /// 鼠标点击（含滚轮）合计
    pub clicks: u64,
    /// 手柄按键合计
    pub gamepad: u64,
}

/// 仪表盘设备行（§4.7 `Overview.devices`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewDeviceDto {
    /// 设备 id
    pub id: i64,
    /// 设备种类（serde 小写）
    pub kind: DeviceKind,
    /// 显示名
    pub name: String,
    /// lifetime 总计数
    pub total: u64,
}

impl From<OverviewData> for OverviewDto {
    fn from(d: OverviewData) -> Self {
        Self {
            days: d
                .days
                .into_iter()
                .map(|x| DayDto { day: x.day, total: x.total })
                .collect(),
            today: TodayDto {
                keys: d.today.keys,
                clicks: d.today.clicks,
                gamepad: d.today.gamepad,
            },
            devices: d
                .devices
                .into_iter()
                .map(|x| OverviewDeviceDto {
                    id: x.id,
                    kind: x.kind,
                    name: x.name,
                    total: x.total,
                })
                .collect(),
        }
    }
}

/// 内部查询（测试直连；command 只包 spawn_blocking + 空数据容错）。
pub(crate) fn query_overview(conn: &rusqlite::Connection, from: &str, to: &str) -> store::Result<OverviewDto> {
    Ok(reader::overview(conn, from, to)?.into())
}

/// 仪表盘一次组合查询（§4.7 `get_overview(from, to) -> Overview`）。
///
/// 失败 → 空数据（§5.1 引导态 / §5.3 容错）。
#[tauri::command]
pub async fn get_overview(
    from: String,
    to: String,
    state: tauri::State<'_, AppState>,
) -> Result<OverviewDto, String> {
    super::blocking_query(&state, move |c| query_overview(c, &from, &to)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testutil::seeded_db;
    use crate::state::testutil::TempFile;
    use clrecoder_core::codes::DeviceKind;
    use clrecoder_core::event::DeviceKey;
    use clrecoder_store::writer::{FlushBatch, Writer};

    /// reader::overview → DTO 映射（§4.7 形状 + camelCase 序列化逐字断言）。
    #[test]
    fn overview_maps_store_shape_and_camel_case() {
        let (_f, _conn) = seeded_db("overview-unused");
        let f = TempFile::new("overview", "db");
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
                (kb, "2026-09-28".into(), 0x1E, 5),
                (ms, "2026-09-28".into(), 1, 3),
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

        let dto = query_overview(&conn, "2026-09-01", "2026-09-28").unwrap();
        assert_eq!(dto.days.len(), 1);
        assert_eq!((dto.days[0].day.as_str(), dto.days[0].total), ("2026-09-28", 8));
        assert_eq!((dto.today.keys, dto.today.clicks, dto.today.gamepad), (5, 3, 0));
        assert_eq!(dto.devices.len(), 2);
        assert_eq!(dto.devices[0].kind, DeviceKind::Keyboard);

        // camelCase 逐字对齐（§4.7 Overview）
        let js = serde_json::to_string(&dto).unwrap();
        assert!(js.contains(r#""days":[{"day":"2026-09-28","total":8}]"#), "{js}");
        assert!(js.contains(r#""today":{"keys":5,"clicks":3,"gamepad":0}"#), "{js}");
        // devices 是 lifetime total（键盘 5、鼠标 3），与 days 的 8（合计）不同口径
        assert!(js.contains(r#""devices":[{"id":1,"kind":"keyboard","name":"测试键盘","total":5},"#), "{js}");
        assert!(js.contains(r#"{"id":2,"kind":"mouse","name":"测试鼠标","total":3}]"#), "{js}");
    }
}
