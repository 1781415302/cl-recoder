//! clrecoder-update —— 应用内更新**纯逻辑** crate（PLAN §2 / §4.1–4.4）。
//!
//! 职责边界：版本比较、GitHub Release JSON 解析与资产选择、URL 白名单、
//! curl 参数构造、sha256 校验、待完成更新标记序列化、前端 DTO/状态枚举。
//!
//! **禁止**依赖 `windows` / `tauri` / `rusqlite`，禁止 `cfg(windows)`——
//! 必须在 Linux 上完整跑通 `cargo test` / `clippy -D warnings`。
//! Windows 胶水（跑 curl、句柄锁、停采集器、UAC）在 `src-tauri` 的 S6。

mod http;
mod marker;
mod release;
mod status;
mod verify;
mod version;

pub use http::{
    build_api_curl_args, build_download_curl_args, is_allowed_api_url, is_allowed_download_url,
    map_curl_exit, CurlMappedError,
};
pub use marker::{parse_marker, serialize_marker, PendingMarker};
pub use release::{parse_latest_release, AssetInfo, ParseError, ReleaseInfo};
pub use status::{
    build_update_check, decide_marker_outcome, MarkerOutcomeDecision, UpdateApplied,
    UpdateAppliedOutcome, UpdateBlockReason, UpdateCheck, UpdateErrorCode, UpdatePhase,
    UpdateStatus,
};
pub use verify::{sha256_hex, verify_installer, VerifyError};
pub use version::{parse_version, Version};

/// 仓库 `owner/name`。
pub const REPO: &str = "1781415302/cl-recoder";

/// GitHub Releases latest API（唯一允许的检查 URL）。
pub const LATEST_API_URL: &str =
    "https://api.github.com/repos/1781415302/cl-recoder/releases/latest";

/// 安装包下载 URL 前缀（资产 URL 必须以此开头）。
pub const DOWNLOAD_PREFIX: &str =
    "https://github.com/1781415302/cl-recoder/releases/download/";

/// 发布页 URL 前缀（`page_url` 自行拼接，不取 JSON 的 `html_url`）。
pub const RELEASE_PAGE_PREFIX: &str =
    "https://github.com/1781415302/cl-recoder/releases/tag/";

/// 安装包大小上限（200 MiB）。
pub const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;

/// Release API 响应大小上限（1 MiB）。
pub const MAX_API_BYTES: u64 = 1024 * 1024;

/// 更新说明最大字符数（按 Unicode 标量值截断）。
pub const NOTES_MAX_CHARS: usize = 4000;

/// 安装包资产名：`CL-Recoder-{major}.{minor}.{patch}-windows-x64-setup.exe`。
#[must_use]
pub fn installer_asset_name(version: &Version) -> String {
    format!(
        "CL-Recoder-{}-windows-x64-setup.exe",
        version.to_string_no_v()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_asset_name_matches_convention() {
        let v = parse_version("0.2.0").unwrap();
        assert_eq!(
            installer_asset_name(&v),
            "CL-Recoder-0.2.0-windows-x64-setup.exe"
        );
    }

    #[test]
    fn constants_are_stable() {
        assert_eq!(REPO, "1781415302/cl-recoder");
        assert!(LATEST_API_URL.contains(REPO));
        assert!(DOWNLOAD_PREFIX.starts_with("https://github.com/"));
        assert!(RELEASE_PAGE_PREFIX.starts_with("https://github.com/"));
        assert_eq!(MAX_INSTALLER_BYTES, 200 * 1024 * 1024);
        assert_eq!(MAX_API_BYTES, 1024 * 1024);
        assert_eq!(NOTES_MAX_CHARS, 4000);
    }
}
