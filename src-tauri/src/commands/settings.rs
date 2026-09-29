//! settings —— 设置读写（PLAN §4.7/§6）。
//!
//! - 文件形状（§6，snake_case）：`{ "gui_autostart": bool, "wp_db_path": string|null, "first_run_done": bool }`，
//!   持久化在 `%LOCALAPPDATA%\ClRecoder\settings.json`，损坏/缺失 → 默认值重建；
//! - DTO 形状（§4.7，camelCase）：`Settings` / `SettingsPatch`——**patch 语义**：
//!   字段缺省 = 不改动；`wpDbPath: null` = 清除覆盖回默认探测路径
//!   （`#[serde(default, deserialize_with = double_option)]` 区分"缺省"与"显式 null"）；
//! - `gui_autostart` 由 tauri-plugin-autostart 落实（HKCU Run，§4.7）：先做插件副作用，
//!   成功后才落盘，保证设置文件与真实自启状态一致。

use serde::{Deserialize, Serialize};

use crate::db;
use crate::state::{AppState, Settings};

/// 设置 DTO（§4.7 `Settings`，camelCase）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    /// GUI 开机自启
    pub gui_autostart: bool,
    /// WhatPulse 库路径覆盖（null = 用默认探测路径）
    pub wp_db_path: Option<String>,
    /// 首启引导是否完成
    pub first_run_done: bool,
}

impl From<Settings> for SettingsDto {
    fn from(s: Settings) -> Self {
        Self {
            gui_autostart: s.gui_autostart,
            wp_db_path: s.wp_db_path,
            first_run_done: s.first_run_done,
        }
    }
}

/// 设置补丁（§4.7 `SettingsPatch`，camelCase）。
///
/// serde 双层 Option：外层 None = 字段缺省（不改动）；`Some(None)` = 显式 `null`（清除）；
/// `Some(Some(v))` = 设置为 v。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    /// 缺省 = 不改
    pub gui_autostart: Option<bool>,
    /// 缺省 = 不改；null = 清除覆盖；string = 设置路径
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub wp_db_path: Option<Option<String>>,
}

/// serde 助手：把 JSON `null` 反序列化为 `Some(None)`（区别于字段缺省的 `None`）。
fn deserialize_double_option<'de, D>(de: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// 内部应用补丁（测试直连；`apply_autostart` 为 tauri-plugin-autostart 副作用钩子，
/// `path` 为落盘目标——command 传全局 settings 路径，单测传临时路径避免污染真实设置）。
pub(crate) fn apply_patch(
    current: &Settings,
    patch: &SettingsPatch,
    apply_autostart: impl FnOnce(Option<bool>) -> Result<(), String>,
    path: &std::path::Path,
) -> Result<Settings, String> {
    let mut next = current.clone();
    if let Some(gui) = patch.gui_autostart {
        next.gui_autostart = gui;
    }
    if let Some(wp) = &patch.wp_db_path {
        next.wp_db_path = wp.clone();
    }
    // 先落实自启副作用（§4.7：gui_autostart 用 tauri-plugin-autostart 落实），
    // 失败则不落盘，保持设置与真实状态一致。
    apply_autostart(patch.gui_autostart)?;
    next.save_to(path)?;
    Ok(next)
}

/// 读取设置（§4.7 `get_settings() -> Settings`；内存镜像，启动时已加载）。
#[tauri::command]
pub async fn get_settings(state: tauri::State<'_, AppState>) -> Result<SettingsDto, String> {
    let st = state.inner().clone();
    let snapshot = st.settings.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    Ok(SettingsDto::from(snapshot))
}

/// 修改设置（§4.7 `set_settings(patch: SettingsPatch) -> Settings`）。
/// `gui_autostart` 变化时同步 HKCU Run（tauri-plugin-autostart）；成功后落盘并刷新内存镜像。
#[tauri::command]
pub async fn set_settings(
    patch: SettingsPatch,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<SettingsDto, String> {
    let st = state.inner().clone();
    let current = st.settings.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    // 自启副作用：仅当补丁携带 gui_autostart 时执行（tauri-plugin-autostart，HKCU Run）
    let plugin_app = app.clone();
    let settings_file = db::settings_path();
    let next = apply_patch(&current, &patch, move |gui| match gui {
        None => Ok(()),
        Some(true) => {
            use tauri_plugin_autostart::ManagerExt;
            plugin_app
                .autolaunch()
                .enable()
                .map_err(|e| format!("启用 GUI 自启失败: {e}"))
        }
        Some(false) => {
            use tauri_plugin_autostart::ManagerExt;
            plugin_app
                .autolaunch()
                .disable()
                .map_err(|e| format!("停用 GUI 自启失败: {e}"))
        }
    }, &settings_file)?;
    *st.settings.write().unwrap_or_else(std::sync::PoisonError::into_inner) = next.clone();
    Ok(SettingsDto::from(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;

    /// SettingsPatch 反序列化：缺省 / 显式 null / 字符串三分支（§4.7 TS 形状）。
    #[test]
    fn patch_distinguishes_missing_null_and_value() {
        let p: SettingsPatch = serde_json::from_str("{}").unwrap();
        assert!(p.gui_autostart.is_none());
        assert!(p.wp_db_path.is_none(), "缺省 = 不改动");

        let p: SettingsPatch = serde_json::from_str(r#"{"wpDbPath":null}"#).unwrap();
        assert!(p.gui_autostart.is_none());
        assert_eq!(p.wp_db_path, Some(None), "显式 null = 清除覆盖");

        let p: SettingsPatch =
            serde_json::from_str(r#"{"guiAutostart":true,"wpDbPath":"C:/wp/whatpulse.db"}"#).unwrap();
        assert_eq!(p.gui_autostart, Some(true));
        assert_eq!(p.wp_db_path, Some(Some("C:/wp/whatpulse.db".into())));
    }

    /// apply_patch：只改动携带的字段；gui_autostart 副作用按补丁执行；落盘到指定路径。
    #[test]
    fn apply_patch_semantics_and_persistence() {
        let current = Settings {
            gui_autostart: false,
            wp_db_path: Some(r"C:\old\wp.db".into()),
            first_run_done: true,
        };
        // 落盘目标一律是临时路径——绝不碰真实 %LOCALAPPDATA%\ClRecoder\settings.json
        let file = TempFile::new("settings-patch", "json");

        // 1) 空 patch：不改动、不触发自启副作用
        let seen = std::cell::Cell::new(0u8);
        let next = apply_patch(&current, &SettingsPatch::default(), |g| {
            seen.set(seen.get() + 1);
            assert!(g.is_none());
            Ok(())
        }, file.as_ref())
        .unwrap();
        assert_eq!(next, current);
        assert_eq!(seen.get(), 1, "apply_patch 恰调用一次副作用钩子");
        assert_eq!(Settings::load_from(file.as_ref()), current, "空 patch 落盘 = 原值");

        // 2) gui_autostart=true + wpDbPath=null：副作用收到 Some(true)，路径被清除
        let patch = SettingsPatch {
            gui_autostart: Some(true),
            wp_db_path: Some(None),
        };
        let seen2 = std::cell::Cell::new(None::<bool>);
        let next = apply_patch(&current, &patch, |g| {
            seen2.set(g);
            Ok(())
        }, file.as_ref())
        .unwrap();
        assert!(next.gui_autostart);
        assert_eq!(next.wp_db_path, None);
        assert_eq!(seen2.get(), Some(true), "副作用须收到补丁里的 gui_autostart");
        assert_eq!(Settings::load_from(file.as_ref()), next, "落盘内容与返回值一致");

        // 3) 副作用失败 → 整体失败（不落盘语义）
        let r = apply_patch(&current, &patch, |_| Err("UAC 失败".into()), file.as_ref());
        assert!(r.is_err());
        assert_eq!(Settings::load_from(file.as_ref()), next, "失败不得覆盖已落盘值");
    }

    /// DTO camelCase 逐字（§4.7 Settings）。
    #[test]
    fn dto_camel_case_wire_shape() {
        let dto = SettingsDto {
            gui_autostart: true,
            wp_db_path: Some("x".into()),
            first_run_done: false,
        };
        let js = serde_json::to_string(&dto).unwrap();
        assert_eq!(
            js,
            r#"{"guiAutostart":true,"wpDbPath":"x","firstRunDone":false}"#
        );
        let dto2 = SettingsDto { wp_db_path: None, ..dto };
        assert!(
            serde_json::to_string(&dto2).unwrap().contains(r#""wpDbPath":null"#),
            "null 覆盖必须序列化为 null（TS: string | null）"
        );
    }

    /// 设置文件 round-trip 已在 state.rs 覆盖（snake_case §6 形状）；这里补文件路径存在性。
    #[test]
    fn settings_file_path_shape() {
        let f = TempFile::new("settings-path", "json");
        let s = Settings { gui_autostart: true, wp_db_path: None, first_run_done: true };
        s.save_to(&f).unwrap();
        assert_eq!(Settings::load_from(&f), s);
    }
}
