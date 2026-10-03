//! get_key_daily / get_top_keys —— 键盘逐日与 Top-N（PLAN §4.7；SQL 在 store::reader）。
//!
//! §4.5："label 由 GUI keylabel 补"——reader 返回的行不带 label，本模块用
//! [`crate::keylabel::code_label`] 填充（键盘走布局相关键名，鼠标/手柄走静态展示名，
//! 因此同一内部函数也服务仪表盘设备明细）。

use serde::Serialize;

use crate::keylabel;
use crate::state::AppState;
use clrecoder_store::reader::{self, KeyDailyRow, TopKeyRow};
use clrecoder_store as store;

/// 键盘逐日行 DTO（§4.7 `KeyDailyRowLabeled`，camelCase）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyDailyRowLabeled {
    /// 日期
    pub day: String,
    /// 归一化 scancode（或鼠标/手柄 code）
    pub code: u16,
    /// 当日次数
    pub count: u64,
    /// 显示名（GUI keylabel 填充）
    pub label: String,
}

/// 键盘 Top-N 行 DTO（§4.7 `TopKeyRow`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopKeyRowDto {
    /// 归一化 scancode
    pub code: u16,
    /// 范围内总次数
    pub total: u64,
    /// 显示名
    pub label: String,
}

impl KeyDailyRowLabeled {
    fn labeled(r: KeyDailyRow, kind: clrecoder_core::codes::DeviceKind) -> Self {
        Self { day: r.day, code: r.code, count: r.count, label: keylabel::code_label(kind, r.code) }
    }
}

impl TopKeyRowDto {
    fn labeled(r: TopKeyRow, kind: clrecoder_core::codes::DeviceKind) -> Self {
        Self { code: r.code, total: r.total, label: keylabel::code_label(kind, r.code) }
    }
}

/// 设备 id → 种类（label 分派需要；§4.1 消歧靠 devices.kind）。
/// usability-runtime-v3 §4.6：走 store 主键直查（`device_kind_by_id`，不做全历史 SUM），
/// `None` 复用原 `UnknownDeviceKind` 错误。
pub(crate) fn device_kind(
    conn: &rusqlite::Connection,
    device_id: i64,
) -> store::Result<clrecoder_core::codes::DeviceKind> {
    reader::device_kind_by_id(conn, device_id)?.ok_or_else(|| {
        clrecoder_store::StoreError::UnknownDeviceKind(format!("device_id={device_id} 不存在"))
    })
}

/// 内部查询：逐日明细 + label 填充（测试直连）。
pub(crate) fn query_key_daily(
    conn: &rusqlite::Connection,
    device_id: i64,
    from: &str,
    to: &str,
) -> store::Result<Vec<KeyDailyRowLabeled>> {
    let kind = device_kind(conn, device_id)?;
    Ok(reader::key_daily(conn, device_id, from, to)?
        .into_iter()
        .map(|r| KeyDailyRowLabeled::labeled(r, kind))
        .collect())
}

/// 内部查询：Top-N + label 填充（测试直连）。
pub(crate) fn query_top_keys(
    conn: &rusqlite::Connection,
    device_id: i64,
    from: &str,
    to: &str,
    limit: u32,
) -> store::Result<Vec<TopKeyRowDto>> {
    let kind = device_kind(conn, device_id)?;
    Ok(reader::top_keys(conn, device_id, from, to, limit)?
        .into_iter()
        .map(|r| TopKeyRowDto::labeled(r, kind))
        .collect())
}

/// 键盘逐日明细（§4.7 `get_key_daily(device_id, from, to) -> Vec<KeyDailyRowLabeled>`）。
/// 设备不存在/查询失败 → 空数据（§5.1/§5.3）。
#[tauri::command]
pub async fn get_key_daily(
    device_id: i64,
    from: String,
    to: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<KeyDailyRowLabeled>, String> {
    super::blocking_query(&state, move |c| query_key_daily(c, device_id, &from, &to)).await
}

/// 键盘 Top-N（§4.7 `get_top_keys(device_id, from, to, limit) -> Vec<TopKeyRow>`，count 降序）。
#[tauri::command]
pub async fn get_top_keys(
    device_id: i64,
    from: String,
    to: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<TopKeyRowDto>, String> {
    super::blocking_query(&state, move |c| query_top_keys(c, device_id, &from, &to, limit)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_core::codes::DeviceKind;
    use clrecoder_core::event::DeviceKey;
    use clrecoder_store::writer::{FlushBatch, Writer};

    /// reader 查询 + label 填充：键盘设备走 key_label（QWERTY 下 0x1E→"A"），
    /// 鼠标设备走静态展示名——同一 command 服务不同 kind 的明细。
    #[test]
    fn key_daily_and_top_keys_fill_labels() {
        let f = TempFile::new("keys", "db");
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
                (kb, "2026-09-27".into(), 0x2C, 4),
                (ms, "2026-09-28".into(), 1, 7),
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

        let daily = query_key_daily(&conn, kb, "2026-09-01", "2026-09-30").unwrap();
        assert_eq!(daily.len(), 2);
        assert_eq!((daily[0].day.as_str(), daily[0].code, daily[0].count), ("2026-09-27", 0x2C, 4));
        assert_eq!(daily[0].label, keylabel::key_label(0x2C));
        assert_eq!(daily[1].label, "A", "QWERTY 下 0x1E 应为 A");

        let top = query_top_keys(&conn, kb, "2026-09-01", "2026-09-30", 1).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!((top[0].code, top[0].total), (0x1E, 10u64));
        assert_eq!(top[0].label, "A");

        // 鼠标设备同接口：label 走静态表
        let ms_top = query_top_keys(&conn, ms, "2026-09-01", "2026-09-30", 5).unwrap();
        assert_eq!(ms_top[0].label, "左键");

        // 不存在的设备 → 报错（上层折叠为空数据）
        assert!(query_top_keys(&conn, 999, "2026-09-01", "2026-09-30", 5).is_err());

        // camelCase 逐字（§4.7 KeyDailyRowLabeled）
        let js = serde_json::to_string(&daily[0]).unwrap();
        assert_eq!(
            js,
            format!(
                r#"{{"day":"2026-09-27","code":44,"count":4,"label":"{}"}}"#,
                keylabel::key_label(0x2C)
            )
        );
    }

    /// §4.6（usability-runtime-v3）：kind 读取走 store 主键直查；`None`
    /// 复用原 `UnknownDeviceKind` 错误（含 device_id 的中文文案不变）。
    #[test]
    fn usability_v3_device_kind_by_id_none_maps_to_unknown_kind_error() {
        let f = TempFile::new("keys-kind", "db");
        let w = Writer::open(f.as_ref()).unwrap();
        let ms = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 1,
                pid: 2,
                name: "测试鼠标".into(),
            })
            .unwrap();
        drop(w);
        let conn = rusqlite::Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();

        assert_eq!(device_kind(&conn, ms).unwrap(), DeviceKind::Mouse);
        let err = device_kind(&conn, 999).unwrap_err();
        match err {
            clrecoder_store::StoreError::UnknownDeviceKind(msg) => {
                assert_eq!(msg, "device_id=999 不存在", "{msg}");
            }
            other => panic!("应复用 UnknownDeviceKind 错误，实际: {other}"),
        }
    }

    /// U2（usability-runtime-v3 §4.6）：临时 DB 里手柄 code 3..=8 按验收点指定 count
    /// （31/42/53/64/75/86）写入，查询层 label 走统一后的 17 码物理标签表——
    /// 3=Y（北）、4=X（西）、5=LB（左肩）、6=LT（左扳机）、7=RB（右肩）、8=RT（右扳机）——
    /// count 原样返回（存储码与历史 count 不动，仅显示纠正）；
    /// 同码鼠标设备按 `devices.kind` 消歧（§4.1 禁止按 code 值域判断种类）：
    /// label 走鼠标静态表、count 与手柄行互不混淆。
    #[test]
    fn usability_v3_gamepad_u2_labels_counts_intact_mouse_disambiguated() {
        let f = TempFile::new("keys-gp", "db");
        let w = Writer::open(f.as_ref()).unwrap();
        let gp = w
            .get_or_create_device(&DeviceKey {
                kind: DeviceKind::Gamepad,
                vid: 0,
                pid: 0,
                name: "Xbox Controller".into(),
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
                // 手柄：验收点指定值（3=31/4=42/5=53/6=64/7=75/8=86）
                (gp, "2026-09-28".into(), 3, 31),
                (gp, "2026-09-28".into(), 4, 42),
                (gp, "2026-09-28".into(), 5, 53),
                (gp, "2026-09-28".into(), 6, 64),
                (gp, "2026-09-28".into(), 7, 75),
                (gp, "2026-09-28".into(), 8, 86),
                // 鼠标同码（3=中键、4=侧键X1、5=侧键X2），count 与手柄行互异
                (ms, "2026-09-28".into(), 3, 7),
                (ms, "2026-09-28".into(), 4, 8),
                (ms, "2026-09-28".into(), 5, 9),
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

        let expect: [(u16, u64, &str); 6] = [
            (3, 31, "Y（北）"),
            (4, 42, "X（西）"),
            (5, 53, "LB（左肩）"),
            (6, 64, "LT（左扳机）"),
            (7, 75, "RB（右肩）"),
            (8, 86, "RT（右扳机）"),
        ];

        // 逐日明细：6 行，label/count 逐码断言（count 原样）
        let daily = query_key_daily(&conn, gp, "2026-09-01", "2026-09-30").unwrap();
        assert_eq!(daily.len(), 6);
        for (code, count, label) in expect {
            let row = daily
                .iter()
                .find(|r| r.code == code)
                .unwrap_or_else(|| panic!("手柄逐日缺 code {code} 行"));
            assert_eq!((row.count, row.label.as_str()), (count, label), "code {code}");
        }

        // Top-N：label 与逐日一致、total 原样（单日各行，total 即当日 count），count 降序
        let top = query_top_keys(&conn, gp, "2026-09-01", "2026-09-30", 10).unwrap();
        assert_eq!(top.len(), 6);
        assert_eq!((top[0].code, top[0].total, top[0].label.as_str()), (8, 86, "RT（右扳机）"));
        for (code, count, label) in expect {
            let row = top
                .iter()
                .find(|r| r.code == code)
                .unwrap_or_else(|| panic!("Top-N 缺 code {code} 行"));
            assert_eq!((row.total, row.label.as_str()), (count, label), "code {code}");
        }

        // 鼠标同码按 kind 消歧：label 走鼠标静态表、count 原样，不含任何手柄标签
        let ms_daily = query_key_daily(&conn, ms, "2026-09-01", "2026-09-30").unwrap();
        assert_eq!(ms_daily.len(), 3);
        let ms_expect: [(u16, u64, &str); 3] =
            [(3, 7, "中键"), (4, 8, "侧键X1（后退）"), (5, 9, "侧键X2（前进）")];
        for (code, count, label) in ms_expect {
            let row = ms_daily
                .iter()
                .find(|r| r.code == code)
                .unwrap_or_else(|| panic!("鼠标逐日缺 code {code} 行"));
            assert_eq!((row.count, row.label.as_str()), (count, label), "鼠标 code {code}");
        }
        assert!(
            ms_daily.iter().all(|r| r.label != "Y（北）" && r.label != "X（西）"),
            "鼠标结果混入手柄标签: {:?}",
            ms_daily.iter().map(|r| r.label.as_str()).collect::<Vec<_>>()
        );
    }
}
