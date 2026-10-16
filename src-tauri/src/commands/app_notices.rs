//! app_notices —— GUI 顶部/设置页的只读提示（hardening-v4 §4.5(b) schema 过新横幅、
//! §4.2.1 旧安装迁移提示；PLAN §4.7 更新已完成横幅）。
//!
//! 设计取舍：不并入 500ms 轮询的 `collector_status`（该命令只走管道/进程证据，不持有
//! AppState、不触 SQLite），而以独立的低频命令返回，前端以 30s 间隔 + 活动门控查询。
//! 本命令只读：不修改任何文件、注册表或计划任务。

use clrecoder_update::UpdateApplied;
use serde::Serialize;

use crate::state::AppState;

use super::update::UpdateRuntime;

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
    /// 启动时检测到的「更新已完成」（None = 无）
    pub update_applied: Option<UpdateApplied>,
}

/// 计算提示（纯组装，便于单测）。
pub(crate) fn build_notices(
    schema: Option<(i64, i64)>,
    update_applied: Option<UpdateApplied>,
) -> AppNoticesDto {
    AppNoticesDto {
        schema_too_new: schema.map(|(found, supported)| SchemaTooNewDto { found, supported }),
        update_applied,
    }
}

/// 只读提示（hardening-v4 + PLAN §4.7）：前端无参数。
#[tauri::command]
pub async fn get_app_notices(
    state: tauri::State<'_, AppState>,
    update_rt: tauri::State<'_, UpdateRuntime>,
) -> Result<AppNoticesDto, String> {
    let st = state.inner().clone();
    let applied = update_rt.applied_snapshot();
    tauri::async_runtime::spawn_blocking(move || build_notices(st.refresh_schema_status(), applied))
        .await
        .map_err(|e| format!("提示查询任务失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_update::UpdateAppliedOutcome;

    #[test]
    fn notices_dto_shape_is_camel_case() {
        let v = serde_json::to_value(build_notices(Some((99, 3)), None)).unwrap();
        assert_eq!(v["schemaTooNew"]["found"], 99);
        assert_eq!(v["schemaTooNew"]["supported"], 3);
        assert!(v["updateApplied"].is_null());
        let v = serde_json::to_value(build_notices(None, None)).unwrap();
        assert!(v["schemaTooNew"].is_null());
        assert!(v["updateApplied"].is_null());
    }

    #[test]
    fn notices_include_update_applied_camel_case() {
        let applied = UpdateApplied {
            from_version: "0.4.0".into(),
            to_version: "0.5.0".into(),
            collector_was_running: true,
            collector_restored: false,
            outcome: UpdateAppliedOutcome::Applied,
        };
        let v = serde_json::to_value(build_notices(None, Some(applied))).unwrap();
        assert_eq!(v["updateApplied"]["fromVersion"], "0.4.0");
        assert_eq!(v["updateApplied"]["toVersion"], "0.5.0");
        assert_eq!(v["updateApplied"]["collectorWasRunning"], true);
        assert_eq!(v["updateApplied"]["collectorRestored"], false);
        assert_eq!(v["updateApplied"]["outcome"], "applied");
        assert!(v.get("update_applied").is_none());
    }
}
