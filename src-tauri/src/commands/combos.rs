//! get_combos —— 组合键聚合（PLAN §4.7；SQL 在 store::reader::combos，label = "Ctrl+Shift+T" 形状）。

use serde::Serialize;

use crate::keylabel;
use crate::state::AppState;
use clrecoder_store::reader::{self, ComboRow};
use clrecoder_store as store;

/// 组合键行 DTO（§4.7 `ComboRowLabeled`，camelCase）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboRowLabeled {
    /// 修饰键位掩码（core::codes::mods 位或）
    pub mods: u8,
    /// 非修饰键 scancode
    pub code: u16,
    /// 范围内总次数
    pub total: u64,
    /// 显示名（如 "Ctrl+Shift+T"）
    pub label: String,
}

impl From<ComboRow> for ComboRowLabeled {
    fn from(r: ComboRow) -> Self {
        Self { label: keylabel::combo_label(r.mods, r.code), mods: r.mods, code: r.code, total: r.total }
    }
}

/// 内部查询（测试直连；导出 combos 视图也复用）。
pub(crate) fn query_combos(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> store::Result<Vec<ComboRowLabeled>> {
    Ok(reader::combos(conn, from, to, limit)?.into_iter().map(ComboRowLabeled::from).collect())
}

/// 组合键聚合（§4.7 `get_combos(from, to, limit) -> Vec<ComboRowLabeled>`，count 降序）。
/// 失败 → 空数据（§5.1/§5.3）。
#[tauri::command]
pub async fn get_combos(
    from: String,
    to: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ComboRowLabeled>, String> {
    super::blocking_query(&state, move |c| query_combos(c, &from, &to, limit)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_core::codes::mods;
    use clrecoder_store::writer::{FlushBatch, Writer};

    /// reader::combos → DTO：count 降序 + label（QWERTY：0x2E→"C" 类，此处直接对 keylabel 一致性断言）。
    #[test]
    fn combos_map_with_labels() {
        let f = TempFile::new("combos", "db");
        let w = Writer::open(f.as_ref()).unwrap();
        w.flush(&FlushBatch {
            combos: vec![
                ("2026-09-28".into(), mods::CTRL, 0x2E, 2),
                ("2026-09-28".into(), mods::CTRL | mods::SHIFT, 0x2F, 5),
                ("2026-09-27".into(), mods::ALT, 0x30, 1),
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

        let rows = query_combos(&conn, "2026-09-01", "2026-09-30", 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].total, 5, "count 降序");
        assert_eq!(
            rows[0].label,
            format!("Ctrl+Shift+{}", crate::keylabel::key_label(0x2F)),
            "label 与 keylabel 一致"
        );
        assert!(rows[0].label.starts_with("Ctrl+Shift+"));
        assert!(rows[2].label.starts_with("Alt+"));

        // camelCase
        let js = serde_json::to_string(&rows[1]).unwrap();
        assert!(js.contains(r#""mods":1"#) && js.contains(r#""total":"#), "{js}");
        assert!(!js.contains("label_label"), "{js}");
    }
}
