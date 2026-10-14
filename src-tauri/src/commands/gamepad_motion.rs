//! gamepad_motion —— 手柄摇杆/扳机运动查询（motion-dpi §4.5 + input-live-trigger §4.5；SQL 全部在 store::motion）。
//!
//! 与鼠标运动同一条容错线（§4.5）：不 swallow——SQL 真实错误原样透传；旧 schema 由
//! store 判定返回 `availability=needs_upgrade` + 恰 625 全零格（§4.5"未知 schema 在
//! 组件旁提供'启动/更新采集器后可用'"）。查询按**型号 device_id**（sourceId 是鼠标
//! 独立物理来源，手柄摇杆归型号统计），归属 kind 必须是 gamepad（§4.5 后端校验）。
//! 扳机子汇总独立 readiness：v3 库摇杆 ready、扳机 needs_upgrade；缺扳机表不得隐藏摇杆。

use serde::Serialize;

use crate::state::AppState;
use clrecoder_core::codes::DeviceKind;
use clrecoder_core::motion::{StickSide, TriggerSide};
use clrecoder_store as store;
use clrecoder_store::motion::{
    self, GamepadMotionSummary, GamepadTriggerSummary, StickMotionSummary, TriggerHeatSummary,
};

use super::mouse_motion::{blocking_motion_query, validate_positive_id, validate_range};

/// 单侧摇杆运动汇总 DTO（§4.5 `StickMotionSummary`；dwellSeconds 恰 625、row-major）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StickMotionSummaryDto {
    /// 摇杆侧
    pub side: StickSide,
    /// 区间活动秒数
    pub active_seconds: f64,
    /// 区间累计路程（R）
    pub travel_r: f64,
    /// 停留热力（恰 625 格、单位秒；空格为 0）
    pub dwell_seconds: Vec<f64>,
}

impl From<StickMotionSummary> for StickMotionSummaryDto {
    fn from(s: StickMotionSummary) -> Self {
        Self {
            side: s.side,
            active_seconds: s.active_seconds,
            travel_r: s.travel_r,
            dwell_seconds: s.dwell_seconds,
        }
    }
}

/// 单侧扳机热力汇总 DTO（§4.5 `TriggerHeatSummary`；dwellSeconds 恰 101）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerHeatSummaryDto {
    /// 扳机侧
    pub side: TriggerSide,
    /// 区间活动秒数（Σdwell_us/1e6）
    pub active_seconds: f64,
    /// 停留热力（恰 101 格、单位秒；空格为 0）
    pub dwell_seconds: Vec<f64>,
}

impl From<TriggerHeatSummary> for TriggerHeatSummaryDto {
    fn from(s: TriggerHeatSummary) -> Self {
        Self {
            side: s.side,
            active_seconds: s.active_seconds,
            dwell_seconds: s.dwell_seconds,
        }
    }
}

/// 手柄双扳机热力汇总 DTO（§4.5 `GamepadTriggerSummary`；binCount 恒 101）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamepadTriggerSummaryDto {
    /// 扳机表可用性（独立于摇杆四表门）
    pub availability: motion::MotionAvailability,
    /// 热力格数（101）
    pub bin_count: u8,
    /// 左扳机
    pub left: TriggerHeatSummaryDto,
    /// 右扳机
    pub right: TriggerHeatSummaryDto,
}

impl From<GamepadTriggerSummary> for GamepadTriggerSummaryDto {
    fn from(g: GamepadTriggerSummary) -> Self {
        Self {
            availability: g.availability,
            bin_count: g.bin_count,
            left: TriggerHeatSummaryDto::from(g.left),
            right: TriggerHeatSummaryDto::from(g.right),
        }
    }
}

/// 手柄摇杆运动汇总 DTO（§4.5 `GamepadMotionSummary`；gridSize 恒 25；追加 triggers）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamepadMotionSummaryDto {
    /// schema 可用性（摇杆四表门）
    pub availability: motion::MotionAvailability,
    /// 型号设备 id
    pub device_id: i64,
    /// 热力网格边长（25）
    pub grid_size: u8,
    /// 左摇杆
    pub left: StickMotionSummaryDto,
    /// 右摇杆
    pub right: StickMotionSummaryDto,
    /// 扳机热力子汇总（独立 readiness）
    pub triggers: GamepadTriggerSummaryDto,
}

impl From<GamepadMotionSummary> for GamepadMotionSummaryDto {
    fn from(g: GamepadMotionSummary) -> Self {
        Self {
            availability: g.availability,
            device_id: g.device_id,
            grid_size: g.grid_size,
            left: StickMotionSummaryDto::from(g.left),
            right: StickMotionSummaryDto::from(g.right),
            triggers: GamepadTriggerSummaryDto::from(g.triggers),
        }
    }
}

/// 归属 kind 校验（§4.5）：deviceId 必须存在且为手柄型号（不静默把别的设备当手柄）。
fn ensure_gamepad_device(conn: &rusqlite::Connection, device_id: i64) -> Result<(), String> {
    let kind = store::reader::device_kind_by_id(conn, device_id)
        .map_err(|e| format!("运动查询失败: {e}"))?
        .ok_or_else(|| format!("未知设备 id: {device_id}"))?;
    if kind != DeviceKind::Gamepad {
        return Err(format!("设备 {device_id} 不是手柄，无摇杆运动数据"));
    }
    Ok(())
}

/// 内部查询（测试直连）：kind 校验 + store 区间汇总（左右独立、625 格组装在 store）。
pub(crate) fn query_gamepad_motion(
    conn: &rusqlite::Connection,
    device_id: i64,
    from: &str,
    to: &str,
) -> Result<GamepadMotionSummaryDto, String> {
    ensure_gamepad_device(conn, device_id)?;
    let g =
        motion::gamepad_motion(conn, device_id, from, to).map_err(|e| format!("运动查询失败: {e}"))?;
    Ok(GamepadMotionSummaryDto::from(g))
}

/// 手柄摇杆运动汇总（§4.5 `get_gamepad_motion(state, device_id, from, to)`）；
/// 旧 schema → needs_upgrade 全零；SQL 真实错误原样返回；无摇杆数据的合法空为 ready 全零。
/// DTO 追加 triggers；继续 positive-id、日期、gamepad kind 校验。
#[tauri::command]
pub async fn get_gamepad_motion(
    state: tauri::State<'_, AppState>,
    device_id: i64,
    from: String,
    to: String,
) -> Result<GamepadMotionSummaryDto, String> {
    validate_positive_id(device_id, "设备 id")?;
    validate_range(&from, &to)?;
    blocking_motion_query(&state, move |conn| {
        Ok(query_gamepad_motion(conn, device_id, &from, &to))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_core::event::DeviceKey;
    use clrecoder_core::motion::{StickBinDelta, TriggerBinDelta};
    use clrecoder_store::motion::{StickMotionWrite, TriggerHeatWrite};
    use clrecoder_store::writer::{FlushBatch, Writer};

    /// 固定测试日。
    const DAY: &str = "2026-09-28";
    const DAY2: &str = "2026-09-29";

    /// 建临时 v4 库并造一只手柄型号 + 一只鼠标型号，返回 (文件, 手柄 id, 鼠标 id)。
    fn seeded(tag: &str) -> (TempFile, i64, i64) {
        let f = TempFile::new(tag, "db");
        let w = Writer::open(f.as_ref()).unwrap();
        let gp = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 0,
                pid: 0,
                name: "XInput 手柄".into(),
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
        // DAY：左摇杆 1.5s/0.707R/两格热度；右摇杆 0.25s/一格；DAY2 仅左摇杆 0.1s
        // 扳机：DAY 左 1.0s 两格、右 0.5s 一格
        w.flush_with_triggers(
            &FlushBatch {
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
                ..Default::default()
            },
            &[
                TriggerHeatWrite {
                    device_id: gp,
                    day: DAY.into(),
                    side: TriggerSide::Left,
                    bins: vec![
                        TriggerBinDelta { bin: 40, dwell_us: 800_000 },
                        TriggerBinDelta { bin: 41, dwell_us: 200_000 },
                    ],
                },
                TriggerHeatWrite {
                    device_id: gp,
                    day: DAY.into(),
                    side: TriggerSide::Right,
                    bins: vec![TriggerBinDelta { bin: 60, dwell_us: 500_000 }],
                },
            ],
        )
        .unwrap();
        drop(w);
        (f, gp, ms)
    }

    /// 只读连接。
    fn ro_conn(f: &TempFile) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap()
    }

    /// 汇总与 DTO（§4.5）：左右独立、恰 625 格 row-major、gridSize=25、triggers 101 格、
    /// camelCase 逐字、日过滤、无摇杆数据的设备合法空（ready 全零）。
    #[test]
    fn motion_dpi_gamepad_motion_dto_grid_and_camel_case() {
        let (f, gp, _ms) = seeded("gamepad-motion");
        let conn = ro_conn(&f);

        let g = query_gamepad_motion(&conn, gp, DAY, DAY).unwrap();
        assert_eq!(g.availability, motion::MotionAvailability::Ready);
        assert_eq!((g.device_id, g.grid_size), (gp, 25));
        assert_eq!((g.left.active_seconds, g.left.travel_r), (1.5, 0.707));
        assert_eq!(g.left.side, StickSide::Left);
        assert_eq!(g.left.dwell_seconds.len(), 625, "恰 625 格");
        assert_eq!((g.left.dwell_seconds[312], g.left.dwell_seconds[313]), (1.0, 0.5));
        assert_eq!(g.left.dwell_seconds[311] + g.left.dwell_seconds[314], 0.0, "空格为 0");
        assert_eq!((g.right.active_seconds, g.right.travel_r), (0.25, 0.0));
        assert_eq!(g.right.dwell_seconds[324], 0.25);
        assert_eq!(g.right.dwell_seconds[312], 0.0, "左右独立不串格");

        // 扳机子汇总（v4 ready、恰 101 格、Σdwell）
        assert_eq!(g.triggers.availability, motion::MotionAvailability::Ready);
        assert_eq!(g.triggers.bin_count, 101);
        assert_eq!(g.triggers.left.dwell_seconds.len(), 101);
        assert_eq!(g.triggers.right.dwell_seconds.len(), 101);
        assert_eq!(g.triggers.left.side, TriggerSide::Left);
        assert_eq!((g.triggers.left.active_seconds, g.triggers.left.dwell_seconds[40]), (1.0, 0.8));
        assert_eq!(g.triggers.left.dwell_seconds[41], 0.2);
        assert_eq!((g.triggers.right.active_seconds, g.triggers.right.dwell_seconds[60]), (0.5, 0.5));
        assert_eq!(g.triggers.left.dwell_seconds[60], 0.0, "左右扳机独立不串格");

        // 日过滤：只取 DAY2 → 仅左摇杆 0.1s；扳机无 DAY2 行 → ready 零格（恰 101）
        let g2 = query_gamepad_motion(&conn, gp, DAY2, DAY2).unwrap();
        assert_eq!((g2.left.active_seconds, g2.left.travel_r), (0.1, 0.1));
        assert_eq!(g2.left.dwell_seconds[312], 0.1);
        assert_eq!(g2.right.active_seconds, 0.0);
        assert_eq!(g2.triggers.availability, motion::MotionAvailability::Ready);
        assert_eq!(g2.triggers.bin_count, 101);
        assert_eq!(g2.triggers.left.dwell_seconds.len(), 101);
        assert_eq!(g2.triggers.left.active_seconds + g2.triggers.right.active_seconds, 0.0);

        // camelCase 线形状（§4.5 GamepadMotionSummary/StickMotionSummary/triggers）
        let js = serde_json::to_string(&g).unwrap();
        for key in [
            "\"deviceId\"",
            "\"gridSize\":25",
            "\"activeSeconds\"",
            "\"travelR\"",
            "\"dwellSeconds\"",
            "\"side\":\"left\"",
            "\"side\":\"right\"",
            "\"triggers\"",
            "\"binCount\":101",
        ] {
            assert!(js.contains(key), "缺 {key}: {js}");
        }
        assert!(js.contains(r#""availability":"ready""#), "{js}");
        assert!(
            !js.contains("device_id")
                && !js.contains("active_seconds")
                && !js.contains("travel_r")
                && !js.contains("bin_count"),
            "snake_case 不得出现: {js}"
        );

        // 无摇杆/扳机数据的其他手柄设备：合法空（ready、625/101 全零、不报错）
        let f2 = TempFile::new("gamepad-motion-empty", "db");
        let w = Writer::open(f2.as_ref()).unwrap();
        let gp2 = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 5,
                pid: 6,
                name: "另一只手柄".into(),
            })
            .unwrap();
        drop(w);
        let conn2 = ro_conn(&f2);
        let g3 = query_gamepad_motion(&conn2, gp2, DAY, DAY).unwrap();
        assert_eq!(g3.availability, motion::MotionAvailability::Ready);
        assert_eq!(g3.left.dwell_seconds.len(), 625);
        assert_eq!(
            g3.left.dwell_seconds.iter().sum::<f64>() + g3.right.dwell_seconds.iter().sum::<f64>(),
            0.0
        );
        assert_eq!(g3.left.active_seconds + g3.right.active_seconds, 0.0);
        assert_eq!(g3.triggers.availability, motion::MotionAvailability::Ready);
        assert_eq!(g3.triggers.bin_count, 101);
        assert_eq!(g3.triggers.left.dwell_seconds.len(), 101);
        assert_eq!(
            g3.triggers.left.dwell_seconds.iter().sum::<f64>()
                + g3.triggers.right.dwell_seconds.iter().sum::<f64>(),
            0.0
        );
    }

    /// kind/存在性校验与旧 schema（§4.5）：非手柄型号与未知设备报错（不静默）；
    /// 仅删扳机表 → 摇杆 ready、扳机 needs_upgrade + 101 零格；删运动四表 → 摇杆 needs_upgrade。
    #[test]
    fn motion_dpi_gamepad_motion_rejects_non_gamepad_and_needs_upgrade() {
        let (f, gp, ms) = seeded("gamepad-motion-guards");

        // 归属 kind 校验：鼠标型号拒绝、未知设备拒绝
        {
            let conn = ro_conn(&f);
            let err = query_gamepad_motion(&conn, ms, DAY, DAY).unwrap_err();
            assert!(err.contains("不是手柄"), "{err}");
            let err = query_gamepad_motion(&conn, 999, DAY, DAY).unwrap_err();
            assert!(err.contains("未知设备 id: 999"), "{err}");
        }

        // v3 模拟：只删扳机表（真实旧结构）→ 摇杆 ready、扳机 needs_upgrade
        {
            let conn = rusqlite::Connection::open(f.as_ref()).unwrap();
            conn.execute_batch(
                "DROP TABLE IF EXISTS gamepad_trigger_heat_daily;
                 DELETE FROM schema_migrations WHERE version = 4;",
            )
            .unwrap();
        }
        {
            let conn = ro_conn(&f);
            let g = query_gamepad_motion(&conn, gp, DAY, DAY).unwrap();
            assert_eq!(g.availability, motion::MotionAvailability::Ready, "缺扳机表不得隐藏摇杆");
            assert_eq!(g.left.active_seconds, 1.5);
            assert_eq!(g.triggers.availability, motion::MotionAvailability::NeedsUpgrade);
            assert_eq!(g.triggers.bin_count, 101);
            assert_eq!(g.triggers.left.dwell_seconds.len(), 101);
            assert_eq!(
                g.triggers.left.dwell_seconds.iter().sum::<f64>()
                    + g.triggers.right.dwell_seconds.iter().sum::<f64>(),
                0.0
            );
            let js = serde_json::to_string(&g.triggers).unwrap();
            assert!(js.contains(r#""availability":"needs_upgrade""#), "{js}");
        }

        // 更旧：删运动四表 → 摇杆 needs_upgrade 全零（引导态，不算错误）
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
        let g = query_gamepad_motion(&conn, gp, DAY, DAY).unwrap();
        assert_eq!(g.availability, motion::MotionAvailability::NeedsUpgrade);
        assert_eq!(g.grid_size, 25);
        assert_eq!(g.left.dwell_seconds.len(), 625);
        assert_eq!(
            g.left.dwell_seconds.iter().sum::<f64>() + g.right.dwell_seconds.iter().sum::<f64>(),
            0.0
        );
        assert!(js_needs_upgrade(&g), "线形状必须是 needs_upgrade");
        assert_eq!(g.triggers.availability, motion::MotionAvailability::NeedsUpgrade);
        assert_eq!(g.triggers.bin_count, 101);
    }

    /// 序列化 availability 断言助手（与 store serde snake_case 对齐）。
    fn js_needs_upgrade(g: &GamepadMotionSummaryDto) -> bool {
        serde_json::to_string(g)
            .map(|js| js.contains(r#""availability":"needs_upgrade""#))
            .unwrap_or(false)
    }
}
