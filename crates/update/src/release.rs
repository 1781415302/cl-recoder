//! GitHub Release JSON 解析与资产选择（PLAN §4.3）。

use serde::Deserialize;

use crate::status::UpdateBlockReason;
use crate::version::{parse_version, Version};
use crate::{
    installer_asset_name, DOWNLOAD_PREFIX, MAX_INSTALLER_BYTES, NOTES_MAX_CHARS,
    RELEASE_PAGE_PREFIX,
};

/// 资产选择失败原因（与 [`UpdateBlockReason`] 共用变体，不含 portable）。
pub type BlockReason = UpdateBlockReason;

/// Release JSON 解析失败（draft / prerelease / 坏 tag / JSON 损坏）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    InvalidJson(String),
    Draft,
    Prerelease,
    BadTagName(String),
    MissingField(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(e) => write!(f, "Release JSON 无效: {e}"),
            Self::Draft => write!(f, "最新 Release 仍是 draft"),
            Self::Prerelease => write!(f, "最新 Release 是 prerelease"),
            Self::BadTagName(t) => write!(f, "无法解析 tag_name: {t}"),
            Self::MissingField(name) => write!(f, "缺少字段: {name}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// 选中的安装包资产。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetInfo {
    pub name: String,
    pub size: u64,
    pub sha256_hex: String,
    pub download_url: String,
}

/// 解析后的最新 Release。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    pub published_at: Option<String>,
    /// 已按字符边界截断到 [`NOTES_MAX_CHARS`]。
    pub notes: String,
    /// = `RELEASE_PAGE_PREFIX` + tag（不取 JSON 的 html_url）。
    pub page_url: String,
    pub asset: Result<AssetInfo, BlockReason>,
}

#[derive(Debug, Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    published_at: Option<String>,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    name: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

/// 按 Unicode 标量值截断到 `max_chars`（中文等多字节字符按「字符」计）。
fn truncate_notes(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// 下载 URL 白名单：必须以 [`DOWNLOAD_PREFIX`] 开头，且不含危险字符。
pub(crate) fn download_url_allowed(url: &str) -> bool {
    if !url.starts_with(DOWNLOAD_PREFIX) {
        return false;
    }
    // 不含 `..`、`@`、`?`、`#`、空白、控制字符
    if url.contains("..")
        || url.contains('@')
        || url.contains('?')
        || url.contains('#')
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    true
}

/// 解析 `digest`：`sha256:` + 64 位十六进制（不区分大小写）→ 小写 hex。
fn parse_digest(digest: &str) -> Option<String> {
    let rest = digest.strip_prefix("sha256:")?;
    if rest.len() != 64 {
        return None;
    }
    if !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(rest.to_ascii_lowercase())
}

fn select_asset(version: &Version, assets: &[RawAsset]) -> Result<AssetInfo, BlockReason> {
    let want = installer_asset_name(version);
    let matches: Vec<&RawAsset> = assets.iter().filter(|a| a.name == want).collect();
    if matches.len() != 1 {
        return Err(UpdateBlockReason::AssetMissing);
    }
    let a = matches[0];
    if a.size == 0 || a.size > MAX_INSTALLER_BYTES {
        return Err(UpdateBlockReason::AssetTooLarge);
    }
    let Some(digest) = a.digest.as_deref() else {
        return Err(UpdateBlockReason::DigestMissing);
    };
    let Some(sha256_hex) = parse_digest(digest) else {
        return Err(UpdateBlockReason::DigestMissing);
    };
    if !download_url_allowed(&a.browser_download_url) {
        return Err(UpdateBlockReason::AssetMissing);
    }
    Ok(AssetInfo {
        name: a.name.clone(),
        size: a.size,
        sha256_hex,
        download_url: a.browser_download_url.clone(),
    })
}

/// 解析 GitHub `/releases/latest`（或等价形状）的 JSON。
pub fn parse_latest_release(json: &str) -> Result<ReleaseInfo, ParseError> {
    let raw: RawRelease =
        serde_json::from_str(json).map_err(|e| ParseError::InvalidJson(e.to_string()))?;
    if raw.draft {
        return Err(ParseError::Draft);
    }
    if raw.prerelease {
        return Err(ParseError::Prerelease);
    }
    let version = parse_version(&raw.tag_name)
        .ok_or_else(|| ParseError::BadTagName(raw.tag_name.clone()))?;
    let notes = truncate_notes(raw.body.as_deref().unwrap_or(""), NOTES_MAX_CHARS);
    let page_url = format!("{RELEASE_PAGE_PREFIX}{}", raw.tag_name);
    let asset = select_asset(&version, &raw.assets);
    Ok(ReleaseInfo {
        version,
        tag: raw.tag_name,
        published_at: raw.published_at,
        notes,
        page_url,
        asset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_INSTALLER_BYTES;

    /// 参照 v0.2.0 真实形状裁剪的夹具（含 setup.exe 与 portable.zip）。
    fn fixture_v020() -> String {
        r#"{
  "tag_name": "v0.2.0",
  "draft": false,
  "prerelease": false,
  "published_at": "2026-10-08T11:34:45Z",
  "body": "CL Recoder 的 Windows x64 版本。\n\n## 本版内容\n\n- 全新浅色界面",
  "assets": [
    {
      "name": "CL-Recoder-0.2.0-windows-x64-portable.zip",
      "size": 6924572,
      "digest": "sha256:eafa82ad71acf92d07b7c44f62678f184365e20ec562c8ec0b47516669c11fe3",
      "browser_download_url": "https://github.com/1781415302/cl-recoder/releases/download/v0.2.0/CL-Recoder-0.2.0-windows-x64-portable.zip"
    },
    {
      "name": "CL-Recoder-0.2.0-windows-x64-setup.exe",
      "size": 4242729,
      "digest": "sha256:B5DA5C23117E8B4931938E9F30FB78C856267ACDD3EAD176A9C6C279F86CDC49",
      "browser_download_url": "https://github.com/1781415302/cl-recoder/releases/download/v0.2.0/CL-Recoder-0.2.0-windows-x64-setup.exe"
    }
  ]
}"#
        .to_string()
    }

    fn base_asset(name: &str, url: &str) -> String {
        format!(
            r#"{{
            "name": "{name}",
            "size": 1000,
            "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "browser_download_url": "{url}"
        }}"#
        )
    }

    fn wrap(tag: &str, draft: bool, pre: bool, body: &str, assets_json: &str) -> String {
        format!(
            r#"{{
          "tag_name": "{tag}",
          "draft": {draft},
          "prerelease": {pre},
          "published_at": "2026-10-01T00:00:00Z",
          "body": {body},
          "assets": [{assets_json}]
        }}"#,
            body = serde_json::to_string(body).unwrap(),
        )
    }

    #[test]
    fn fixture_v020_parses_successfully() {
        let info = parse_latest_release(&fixture_v020()).unwrap();
        assert_eq!(info.version, parse_version("0.2.0").unwrap());
        assert_eq!(info.tag, "v0.2.0");
        assert_eq!(
            info.page_url,
            "https://github.com/1781415302/cl-recoder/releases/tag/v0.2.0"
        );
        assert!(info.notes.contains("浅色界面"));
        let asset = info.asset.unwrap();
        assert_eq!(asset.name, "CL-Recoder-0.2.0-windows-x64-setup.exe");
        assert_eq!(asset.size, 4242729);
        // digest 存小写
        assert_eq!(
            asset.sha256_hex,
            "b5da5c23117e8b4931938e9f30fb78c856267acdd3ead176a9c6c279f86cdc49"
        );
        assert!(asset
            .download_url
            .starts_with("https://github.com/1781415302/cl-recoder/releases/download/"));
    }

    #[test]
    fn draft_and_prerelease_and_bad_tag() {
        let url = "https://github.com/1781415302/cl-recoder/releases/download/v0.5.0/CL-Recoder-0.5.0-windows-x64-setup.exe";
        let a = base_asset("CL-Recoder-0.5.0-windows-x64-setup.exe", url);
        assert_eq!(
            parse_latest_release(&wrap("v0.5.0", true, false, "x", &a)).unwrap_err(),
            ParseError::Draft
        );
        assert_eq!(
            parse_latest_release(&wrap("v0.5.0", false, true, "x", &a)).unwrap_err(),
            ParseError::Prerelease
        );
        match parse_latest_release(&wrap("not-a-version", false, false, "x", &a)) {
            Err(ParseError::BadTagName(_)) => {}
            other => panic!("expected BadTagName, got {other:?}"),
        }
        assert!(matches!(
            parse_latest_release("{not json"),
            Err(ParseError::InvalidJson(_))
        ));
    }

    #[test]
    fn asset_missing_duplicate_digest_size_url() {
        let good_url = "https://github.com/1781415302/cl-recoder/releases/download/v0.5.0/CL-Recoder-0.5.0-windows-x64-setup.exe";
        let want = "CL-Recoder-0.5.0-windows-x64-setup.exe";

        // 缺失
        let info = parse_latest_release(&wrap(
            "v0.5.0",
            false,
            false,
            "x",
            &base_asset("other.exe", good_url),
        ))
        .unwrap();
        assert_eq!(info.asset, Err(UpdateBlockReason::AssetMissing));

        // 重复
        let dup = format!(
            "{},{}",
            base_asset(want, good_url),
            base_asset(want, good_url)
        );
        let info = parse_latest_release(&wrap("v0.5.0", false, false, "x", &dup)).unwrap();
        assert_eq!(info.asset, Err(UpdateBlockReason::AssetMissing));

        // digest 缺失
        let no_digest = format!(
            r#"{{"name":"{want}","size":1000,"browser_download_url":"{good_url}"}}"#
        );
        let info = parse_latest_release(&wrap("v0.5.0", false, false, "x", &no_digest)).unwrap();
        assert_eq!(info.asset, Err(UpdateBlockReason::DigestMissing));

        // digest 长度错
        let bad_len = format!(
            r#"{{"name":"{want}","size":1000,"digest":"sha256:abcd","browser_download_url":"{good_url}"}}"#
        );
        let info = parse_latest_release(&wrap("v0.5.0", false, false, "x", &bad_len)).unwrap();
        assert_eq!(info.asset, Err(UpdateBlockReason::DigestMissing));

        // 超大
        let too_big = format!(
            r#"{{"name":"{want}","size":{},"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","browser_download_url":"{good_url}"}}"#,
            MAX_INSTALLER_BYTES + 1
        );
        let info = parse_latest_release(&wrap("v0.5.0", false, false, "x", &too_big)).unwrap();
        assert_eq!(info.asset, Err(UpdateBlockReason::AssetTooLarge));

        // size == 0
        let zero = format!(
            r#"{{"name":"{want}","size":0,"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","browser_download_url":"{good_url}"}}"#
        );
        let info = parse_latest_release(&wrap("v0.5.0", false, false, "x", &zero)).unwrap();
        assert_eq!(info.asset, Err(UpdateBlockReason::AssetTooLarge));

        // URL 白名单：`..` / `@` / `?`
        for bad in [
            "https://github.com/1781415302/cl-recoder/releases/download/v0.5.0/../evil.exe",
            "https://github.com/1781415302/cl-recoder/releases/download/v0.5.0/CL-Recoder-0.5.0-windows-x64-setup.exe?raw=1",
            "https://user@github.com/1781415302/cl-recoder/releases/download/v0.5.0/CL-Recoder-0.5.0-windows-x64-setup.exe",
            "https://evil.example/CL-Recoder-0.5.0-windows-x64-setup.exe",
        ] {
            // 注意：带 @ 的 URL 不以 DOWNLOAD_PREFIX 开头（前缀是 https://github.com/...）
            // 第三条会因 prefix 失败；构造时让 name 匹配但 URL 坏
            let asset = format!(
                r#"{{"name":"{want}","size":1000,"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","browser_download_url":"{bad}"}}"#
            );
            let info = parse_latest_release(&wrap("v0.5.0", false, false, "x", &asset)).unwrap();
            assert_eq!(
                info.asset,
                Err(UpdateBlockReason::AssetMissing),
                "url={bad}"
            );
        }
    }

    #[test]
    fn notes_truncated_on_char_boundary_with_chinese() {
        // 4000 个「测」+ 多余字符 → 恰好 4000
        let long = "测".repeat(NOTES_MAX_CHARS + 10);
        let url = "https://github.com/1781415302/cl-recoder/releases/download/v0.5.0/CL-Recoder-0.5.0-windows-x64-setup.exe";
        let a = base_asset("CL-Recoder-0.5.0-windows-x64-setup.exe", url);
        let info = parse_latest_release(&wrap("v0.5.0", false, false, &long, &a)).unwrap();
        assert_eq!(info.notes.chars().count(), NOTES_MAX_CHARS);
        assert!(info.notes.chars().all(|c| c == '测'));
        // 截断后仍是合法 UTF-8
        assert!(std::str::from_utf8(info.notes.as_bytes()).is_ok());
    }
}
