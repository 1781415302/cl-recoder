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
pub(crate) fn device_kind(
    conn: &rusqlite::Connection,
    device_id: i64,
) -> store::Result<clrecoder_core::codes::DeviceKind> {
    let rows = reader::devices(conn)?;
    rows.into_iter()
        .find(|d| d.id == device_id)
        .map(|d| d.kind)
        .ok_or_else(|| clrecoder_store::StoreError::UnknownDeviceKind(format!("device_id={device_id} 不存在")))
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
}
