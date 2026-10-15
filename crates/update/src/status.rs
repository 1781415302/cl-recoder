//! 更新 DTO 与状态枚举（PLAN §4.4）。
//!
//! 全部 `serde(rename_all = "camelCase")`；枚举值为 `snake_case`。

use serde::{Deserialize, Serialize};

use crate::release::ReleaseInfo;
use crate::version::Version;

/// 阻断自动安装的原因（与前端 `UpdateBlockReason` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateBlockReason {
    PortableLayout,
    AssetMissing,
    DigestMissing,
    AssetTooLarge,
}

/// 检查更新结果（`update_check` 返回值）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    /// 当前版本，如 `"0.4.0"`（无 v 前缀）。
    pub current_version: String,
    /// 远端最新版本，如 `"0.5.0"`（无 v 前缀）。
    pub latest_version: String,
    /// `latest > current`（严格更高）。
    pub has_update: bool,
    /// 发布页 URL（自行拼接）。
    pub release_url: String,
    /// 发布时间（RFC3339），可能为空。
    pub published_at: Option<String>,
    /// 更新说明纯文本，≤ [`crate::NOTES_MAX_CHARS`] 字符。
    pub notes: String,
    /// 安装包字节数；资产不可用时为 `None`。
    pub asset_size: Option<u64>,
    /// `has_update && 无阻断`。
    pub installable: bool,
    /// 阻断原因；无阻断为 `None`。
    pub block_reason: Option<UpdateBlockReason>,
}

/// 安装流水线阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Idle,
    Downloading,
    Verifying,
    StoppingCollector,
    BackingUp,
    Launching,
    Launched,
    Failed,
}

/// 安装失败错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateErrorCode {
    Busy,
    NotInstalledLayout,
    VersionChanged,
    CurlMissing,
    Network,
    HttpStatus,
    SizeMismatch,
    DigestMismatch,
    Disk,
    CollectorStopFailed,
    BackupFailed,
    UacCancelled,
    LaunchFailed,
}

/// 安装进度/状态（`update_status` 返回值）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub phase: UpdatePhase,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error_code: Option<UpdateErrorCode>,
    /// 中文，可直接展示。
    pub message: Option<String>,
    /// `failed` 且更新前采集器在运行时：回滚是否成功。
    pub collector_restored: Option<bool>,
}

impl Default for UpdateStatus {
    fn default() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            received_bytes: 0,
            total_bytes: None,
            error_code: None,
            message: None,
            collector_restored: None,
        }
    }
}

/// 启动时「更新已完成」提示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateApplied {
    pub from_version: String,
    pub to_version: String,
    pub collector_was_running: bool,
    pub collector_restored: bool,
}

/// 由已解析的 Release + 当前版本 + 布局标志组装 [`UpdateCheck`]。
///
/// - `is_portable_layout == true` → `block_reason = portable_layout`（覆盖资产阻断展示）。
/// - `installable = has_update && block_reason.is_none()`。
#[must_use]
pub fn build_update_check(
    current: Version,
    release: &ReleaseInfo,
    is_portable_layout: bool,
) -> UpdateCheck {
    let has_update = release.version > current;
    let (asset_size, asset_block) = match &release.asset {
        Ok(a) => (Some(a.size), None),
        Err(b) => (None, Some(*b)),
    };
    let block_reason = if is_portable_layout {
        Some(UpdateBlockReason::PortableLayout)
    } else {
        asset_block
    };
    // 无更新时不展示阻断（「已是最新」路径）
    let block_reason = if has_update { block_reason } else { None };
    let installable = has_update && block_reason.is_none();
    UpdateCheck {
        current_version: current.to_string_no_v(),
        latest_version: release.version.to_string_no_v(),
        has_update,
        release_url: release.page_url.clone(),
        published_at: release.published_at.clone(),
        notes: release.notes.clone(),
        asset_size,
        installable,
        block_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_latest_release;
    use crate::parse_version;

    fn sample_ok_json() -> String {
        r#"{
          "tag_name": "v0.5.0",
          "draft": false,
          "prerelease": false,
          "published_at": "2026-10-01T00:00:00Z",
          "body": "notes",
          "assets": [{
            "name": "CL-Recoder-0.5.0-windows-x64-setup.exe",
            "size": 1000,
            "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "browser_download_url": "https://github.com/1781415302/cl-recoder/releases/download/v0.5.0/CL-Recoder-0.5.0-windows-x64-setup.exe"
          }]
        }"#
        .to_string()
    }

    #[test]
    fn dto_json_keys_are_camel_case_enums_snake_case() {
        let check = UpdateCheck {
            current_version: "0.4.0".into(),
            latest_version: "0.5.0".into(),
            has_update: true,
            release_url: "https://example".into(),
            published_at: None,
            notes: "n".into(),
            asset_size: Some(42),
            installable: true,
            block_reason: Some(UpdateBlockReason::AssetMissing),
        };
        let v = serde_json::to_value(&check).unwrap();
        assert!(v.get("currentVersion").is_some());
        assert!(v.get("latestVersion").is_some());
        assert!(v.get("hasUpdate").is_some());
        assert!(v.get("releaseUrl").is_some());
        assert!(v.get("publishedAt").is_some());
        assert!(v.get("assetSize").is_some());
        assert!(v.get("blockReason").is_some());
        assert_eq!(v["blockReason"], "asset_missing");
        assert!(v.get("current_version").is_none());

        let status = UpdateStatus {
            phase: UpdatePhase::StoppingCollector,
            received_bytes: 1,
            total_bytes: Some(2),
            error_code: Some(UpdateErrorCode::CurlMissing),
            message: Some("x".into()),
            collector_restored: Some(false),
        };
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(v["phase"], "stopping_collector");
        assert_eq!(v["errorCode"], "curl_missing");
        assert_eq!(v["receivedBytes"], 1);
        assert_eq!(v["totalBytes"], 2);
        assert_eq!(v["collectorRestored"], false);

        let applied = UpdateApplied {
            from_version: "0.4.0".into(),
            to_version: "0.5.0".into(),
            collector_was_running: true,
            collector_restored: false,
        };
        let v = serde_json::to_value(&applied).unwrap();
        assert_eq!(v["fromVersion"], "0.4.0");
        assert_eq!(v["collectorWasRunning"], true);
        assert_eq!(v["collectorRestored"], false);

        // 全部枚举值 round-trip
        for reason in [
            UpdateBlockReason::PortableLayout,
            UpdateBlockReason::AssetMissing,
            UpdateBlockReason::DigestMissing,
            UpdateBlockReason::AssetTooLarge,
        ] {
            let s = serde_json::to_string(&reason).unwrap();
            let back: UpdateBlockReason = serde_json::from_str(&s).unwrap();
            assert_eq!(back, reason);
        }
        for phase in [
            UpdatePhase::Idle,
            UpdatePhase::Downloading,
            UpdatePhase::Verifying,
            UpdatePhase::StoppingCollector,
            UpdatePhase::BackingUp,
            UpdatePhase::Launching,
            UpdatePhase::Launched,
            UpdatePhase::Failed,
        ] {
            let s = serde_json::to_string(&phase).unwrap();
            let back: UpdatePhase = serde_json::from_str(&s).unwrap();
            assert_eq!(back, phase);
        }
    }

    #[test]
    fn build_update_check_installable_and_portable() {
        let release = parse_latest_release(&sample_ok_json()).unwrap();
        let current = parse_version("0.4.0").unwrap();
        let ok = build_update_check(current, &release, false);
        assert!(ok.has_update);
        assert!(ok.installable);
        assert!(ok.block_reason.is_none());
        assert_eq!(ok.latest_version, "0.5.0");
        assert_eq!(ok.current_version, "0.4.0");

        let portable = build_update_check(current, &release, true);
        assert!(portable.has_update);
        assert!(!portable.installable);
        assert_eq!(
            portable.block_reason,
            Some(UpdateBlockReason::PortableLayout)
        );

        let same = build_update_check(parse_version("0.5.0").unwrap(), &release, false);
        assert!(!same.has_update);
        assert!(!same.installable);
        assert!(same.block_reason.is_none());
    }
}
