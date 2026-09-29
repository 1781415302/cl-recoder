//! get_apps —— 应用聚合（PLAN §4.7；SQL 在 store::reader::apps，按前台秒数降序）。

use serde::Serialize;

use crate::state::AppState;
use clrecoder_store::reader::{self, AppRow};
use clrecoder_store as store;

/// 应用行 DTO（§4.7 `AppRowLabeled`，camelCase）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRowLabeled {
    /// 前台 exe 小写 basename（存储键）
    pub exe: String,
    /// 显示名（basename；自有数据只有 exe 名，WhatPulse 侧才有友好名）
    pub name: String,
    /// 前台秒数合计
    pub seconds: u64,
    /// 按键数合计
    pub keys: u64,
    /// 点击数合计
    pub clicks: u64,
}

impl From<AppRow> for AppRowLabeled {
    fn from(r: AppRow) -> Self {
        // §4.7：name = 显示名(basename)。自有库只有 exe basename，显示名即其本身。
        Self { name: r.exe.clone(), exe: r.exe, seconds: r.seconds, keys: r.keys, clicks: r.clicks }
    }
}

/// 内部查询（测试直连）。
pub(crate) fn query_apps(
    conn: &rusqlite::Connection,
    from: &str,
    to: &str,
    limit: u32,
) -> store::Result<Vec<AppRowLabeled>> {
    Ok(reader::apps(conn, from, to, limit)?.into_iter().map(AppRowLabeled::from).collect())
}

/// 应用范围内聚合（§4.7 `get_apps(from, to, limit) -> Vec<AppRowLabeled>`，秒数降序）。
/// 失败 → 空数据（§5.1/§5.3）。
#[tauri::command]
pub async fn get_apps(
    from: String,
    to: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<AppRowLabeled>, String> {
    super::blocking_query(&state, move |c| query_apps(c, &from, &to, limit)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;
    use clrecoder_store::writer::{FlushBatch, Writer};

    /// reader::apps → DTO（秒降序 + name=basename 显示名）。
    #[test]
    fn apps_map_ordered_by_seconds() {
        let f = TempFile::new("apps", "db");
        let w = Writer::open(f.as_ref()).unwrap();
        w.flush(&FlushBatch {
            apps: vec![
                ("2026-09-28".into(), "code.exe".into(), 120, 15, 4),
                ("2026-09-28".into(), "browser.exe".into(), 60, 0, 10),
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

        let rows = query_apps(&conn, "2026-09-01", "2026-09-30", 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].exe.as_str(), rows[0].name.as_str(), rows[0].seconds, rows[0].keys, rows[0].clicks),
            ("code.exe", "code.exe", 120, 15, 4),
            "按秒降序"
        );
        assert_eq!(rows[1].exe, "browser.exe");

        // camelCase（本 DTO 字段本就无下划线——但 rename_all 仍须在场以防漂移）
        let js = serde_json::to_string(&rows[0]).unwrap();
        assert_eq!(
            js,
            r#"{"exe":"code.exe","name":"code.exe","seconds":120,"keys":15,"clicks":4}"#
        );
    }
}
