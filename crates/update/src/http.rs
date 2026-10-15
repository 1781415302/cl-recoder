//! curl 参数构造与退出码映射（PLAN §4.6；本 crate 只产出参数向量，不跑进程）。

use crate::status::UpdateErrorCode;
use crate::{LATEST_API_URL, MAX_API_BYTES};

/// curl 退出码映射结果（错误码 + 中文提示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurlMappedError {
    pub code: UpdateErrorCode,
    pub message: String,
}

/// API URL 白名单：必须**恰好等于** [`LATEST_API_URL`]。
#[must_use]
pub fn is_allowed_api_url(url: &str) -> bool {
    url == LATEST_API_URL
}

/// 下载 URL 白名单（与 Release 解析同一套字符规则）。
#[must_use]
pub fn is_allowed_download_url(url: &str) -> bool {
    crate::release::download_url_allowed(url)
}

fn common_curl_prefix() -> Vec<String> {
    // 每个开关独立参数，禁止拼成 shell 字符串（无元字符拼接）
    vec![
        "--silent".into(),
        "--show-error".into(),
        "--fail".into(),
        "--location".into(),
        "--proto".into(),
        "=https".into(),
        "--proto-redir".into(),
        "=https".into(),
        "--connect-timeout".into(),
        "10".into(),
    ]
}

fn user_agent(app_version: &str) -> String {
    format!("CL-Recoder/{app_version}")
}

/// 构造拉取 latest Release JSON 的 curl 参数（不含 `curl.exe` 本身）。
///
/// `url` 必须通过 [`is_allowed_api_url`]。JSON 写到 **stdout**（不带 `-o`），
/// 由胶水层捕获，避免落在用户可写目录。
pub fn build_api_curl_args(
    url: &str,
    app_version: &str,
) -> Result<Vec<String>, &'static str> {
    if !is_allowed_api_url(url) {
        return Err("API URL 不在白名单");
    }
    let mut args = common_curl_prefix();
    args.extend([
        "--max-time".into(),
        "20".into(),
        "--max-filesize".into(),
        MAX_API_BYTES.to_string(),
        "-A".into(),
        user_agent(app_version),
        "-H".into(),
        "Accept: application/vnd.github+json".into(),
        url.into(),
    ]);
    Ok(args)
}

/// 构造下载安装包的 curl 参数（不含 `curl.exe` 本身）。
///
/// `max_filesize` 应为资产 `size + 1`（PLAN §4.6）；`url` 必须通过白名单。
pub fn build_download_curl_args(
    url: &str,
    output_path: &str,
    max_filesize: u64,
    app_version: &str,
) -> Result<Vec<String>, &'static str> {
    if !is_allowed_download_url(url) {
        return Err("下载 URL 不在白名单");
    }
    let mut args = common_curl_prefix();
    args.extend([
        "--max-time".into(),
        "600".into(),
        "--max-filesize".into(),
        max_filesize.to_string(),
        "-A".into(),
        user_agent(app_version),
        "-o".into(),
        output_path.into(),
        url.into(),
    ]);
    Ok(args)
}

/// 将 curl 退出码映射为 [`UpdateErrorCode`] + 中文文案。
///
/// - 28 → 超时（`network`）
/// - 22 → `http_status`（提示限流）
/// - 63 → `size_mismatch`
/// - 其余非 0 → `network`
///
/// `curl.exe` 不存在由胶水层直接返回 `curl_missing`，不经过本函数。
#[must_use]
pub fn map_curl_exit(exit_code: i32) -> Option<CurlMappedError> {
    if exit_code == 0 {
        return None;
    }
    let (code, message) = match exit_code {
        28 => (
            UpdateErrorCode::Network,
            "连接超时，请检查网络后重试。".to_string(),
        ),
        22 => (
            UpdateErrorCode::HttpStatus,
            "GitHub 限流，请稍后重试。".to_string(),
        ),
        63 => (
            UpdateErrorCode::SizeMismatch,
            "下载文件超出大小限制。".to_string(),
        ),
        _ => (
            UpdateErrorCode::Network,
            format!("网络错误（curl 退出码 {exit_code}），请稍后重试或前往发布页手动下载。"),
        ),
    };
    Some(CurlMappedError { code, message })
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::DOWNLOAD_PREFIX;

    #[test]
    fn api_args_contain_required_flags_no_shell_meta() {
        let args = build_api_curl_args(LATEST_API_URL, "0.4.0").unwrap();
        assert!(args.contains(&"--silent".into()));
        assert!(args.contains(&"--show-error".into()));
        assert!(args.contains(&"--fail".into()));
        assert!(args.contains(&"--location".into()));
        assert!(args.contains(&"--proto".into()));
        assert!(args.contains(&"=https".into()));
        assert!(args.contains(&"--proto-redir".into()));
        assert!(args.contains(&"--connect-timeout".into()));
        assert!(args.contains(&"10".into()));
        assert!(args.contains(&"--max-time".into()));
        assert!(args.contains(&"20".into()));
        assert!(args.contains(&"--max-filesize".into()));
        assert!(args.contains(&MAX_API_BYTES.to_string()));
        assert!(args.contains(&"-A".into()));
        assert!(args.contains(&"CL-Recoder/0.4.0".into()));
        assert!(args.contains(&"-H".into()));
        assert!(args.contains(&"Accept: application/vnd.github+json".into()));
        assert!(!args.iter().any(|s| s == "-o"), "API curl must not write to a file: {args:?}");
        assert_eq!(args.last().map(String::as_str), Some(LATEST_API_URL));

        // 无 shell 元字符拼接：每个元素不应含未转义的 `|`、`;`、`$()` 等拼接点
        for a in &args {
            assert!(!a.contains("&&"), "{a}");
            assert!(!a.contains("||"), "{a}");
            assert!(!a.contains("$("), "{a}");
            assert!(!a.contains('`'), "{a}");
        }
        // `--proto =https` 是两个独立 argv，不是单个 `"--proto =https"`
        let proto_idx = args.iter().position(|s| s == "--proto").unwrap();
        assert_eq!(args[proto_idx + 1], "=https");
    }

    #[test]
    fn download_args_and_whitelist() {
        let url = format!(
            "{DOWNLOAD_PREFIX}v0.5.0/CL-Recoder-0.5.0-windows-x64-setup.exe"
        );
        let args = build_download_curl_args(&url, r"C:\tmp\a.part", 1001, "0.4.0").unwrap();
        assert!(args.contains(&"600".into()));
        assert!(args.contains(&"1001".into()));
        assert!(args.contains(&"CL-Recoder/0.4.0".into()));
        assert!(!args.iter().any(|s| s.contains("Accept:")));
        assert_eq!(args.last().map(String::as_str), Some(url.as_str()));

        assert!(build_api_curl_args(
            "https://evil.example/latest",
            "0.4.0"
        )
        .is_err());
        assert!(build_download_curl_args(
            "https://evil.example/a.exe",
            "o",
            1,
            "0.4.0"
        )
        .is_err());
        assert!(build_download_curl_args(
            &format!("{DOWNLOAD_PREFIX}v0.5.0/../evil.exe"),
            "o",
            1,
            "0.4.0"
        )
        .is_err());
    }

    #[test]
    fn map_curl_exit_codes() {
        assert!(map_curl_exit(0).is_none());
        let e = map_curl_exit(28).unwrap();
        assert_eq!(e.code, UpdateErrorCode::Network);
        assert!(e.message.contains("超时"));
        let e = map_curl_exit(22).unwrap();
        assert_eq!(e.code, UpdateErrorCode::HttpStatus);
        assert!(e.message.contains("限流"));
        let e = map_curl_exit(63).unwrap();
        assert_eq!(e.code, UpdateErrorCode::SizeMismatch);
        let e = map_curl_exit(7).unwrap();
        assert_eq!(e.code, UpdateErrorCode::Network);
    }
}
