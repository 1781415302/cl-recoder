//! app_notices —— GUI 顶部/设置页的只读提示（hardening-v4 §4.5(b) schema 过新横幅、
//! §4.2.1 旧安装迁移提示）。
//!
//! 设计取舍：不并入 500ms 轮询的 `collector_status`（该命令只走管道/进程证据，不持有
//! AppState、不触 SQLite），而以独立的低频命令返回，前端以 30s 间隔 + 活动门控查询。
//! 本命令只读：不修改任何文件、注册表或计划任务。

use serde::Serialize;

use crate::state::AppState;

/// schema 过新详情（camelCase：`{found, supported}`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaTooNewDto {
    /// 库中记录的 schema 版本
    pub found: i64,
    /// 本程序支持的 schema 版本
    pub supported: i64,
}

/// 只读提示汇总 DTO。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppNoticesDto {
    /// 统计库 schema 过新（None = 正常）
    pub schema_too_new: Option<SchemaTooNewDto>,
}

/// 计算提示（纯组装，便于单测）。
pub(crate) fn build_notices(schema: Option<(i64, i64)>) -> AppNoticesDto {
    AppNoticesDto {
        schema_too_new: schema.map(|(found, supported)| SchemaTooNewDto { found, supported }),
    }
}

/// 只读提示（hardening-v4）：前端无参数。
#[tauri::command]
pub async fn get_app_notices(state: tauri::State<'_, AppState>) -> Result<AppNoticesDto, String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || build_notices(st.refresh_schema_status()))
        .await
        .map_err(|e| format!("提示查询任务失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_dto_shape_is_camel_case() {
        let v = serde_json::to_value(build_notices(Some((99, 3)))).unwrap();
        assert_eq!(v["schemaTooNew"]["found"], 99);
        assert_eq!(v["schemaTooNew"]["supported"], 3);
        let v = serde_json::to_value(build_notices(None)).unwrap();
        assert!(v["schemaTooNew"].is_null());
    }
}
