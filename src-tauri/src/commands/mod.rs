//! commands —— 全部 Tauri commands（PLAN §4.7 契约）。
//!
//! 统一模式（§4.7 锁定）：**全部 `async fn` + `tauri::async_runtime::spawn_blocking` 包阻塞查询**；
//! state 注入 [`crate::state::AppState`]；对前端的 DTO 一律 `#[serde(rename_all = "camelCase")]`
//!（serde 默认按 Rust 字段名原样输出，DTO 必须显式加该属性，§4.7 注）。
//!
//! 错误语义（§5.1/§5.3）：查询类命令在 stats.db 缺失/短暂 BUSY/任何查询失败时
//! **返回空数据**（[`swallow`]）+ 前端引导态/React Query 容错重试，绝不弹错；
//! 设置/导入/导出/采集器控制类命令返回真实 `Result::Err(String)`。

pub mod apps;
pub mod collector_ctl;
pub mod combos;
pub mod devices;
pub mod export;
pub mod import;
pub mod keys;
pub mod overview;
pub mod settings;
pub mod wp;

use rusqlite::Connection;

use crate::state::AppState;
use clrecoder_store as store;

/// 查询容错（§5.3："GUI 查询失败返回空 + 前端容错重试"）：
/// store 错误记日志后折叠为 `T::default()`（空 Vec / None / 空结构体）。
pub(crate) fn swallow<T: Default>(r: store::Result<T>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            crate::gui_log!("WARN: GUI 查询失败，按 §5.3 返回空数据: {e}");
            T::default()
        }
    }
}

/// 统一的阻塞查询执行器：clone 状态 → `spawn_blocking` → 错误折叠为空数据。
pub(crate) async fn blocking_query<T, F>(state: &AppState, f: F) -> Result<T, String>
where
    T: Default + Send + 'static,
    F: FnOnce(&Connection) -> store::Result<T> + Send + 'static,
{
    let st = state.clone();
    tauri::async_runtime::spawn_blocking(move || swallow(st.with_ro(f)))
        .await
        .map_err(|e| format!("后台查询任务失败: {e}"))
}

/// 汇总注册全部 commands（main.rs 的 `invoke_handler` 调用）。
pub fn handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        // 查询类（§4.7 前 6 条）
        overview::get_overview,
        devices::get_devices,
        devices::get_mouse_distance,
        keys::get_key_daily,
        keys::get_top_keys,
        apps::get_apps,
        combos::get_combos,
        // WhatPulse 只读查询（§4.7 wp 系列）
        wp::get_wp_meta,
        wp::get_wp_overview,
        wp::get_wp_keys,
        wp::get_wp_combos,
        wp::get_wp_apps,
        wp::get_wp_mouse,
        wp::get_wp_mouse_buttons,
        wp::get_wp_mouse_scrolls,
        // 导入 / 导出
        import::import_whatpulse,
        export::export_data,
        // 采集器控制
        collector_ctl::collector_status,
        collector_ctl::set_collector_paused,
        collector_ctl::collector_autostart_enable,
        collector_ctl::collector_autostart_disable,
        collector_ctl::collector_start_now,
        // 设置
        settings::get_settings,
        settings::set_settings,
        devices::set_device_nickname,
    ]
}

#[cfg(test)]
pub(crate) mod testutil {
    //! 临时库构造工具：commands 单测复用（真实走 store::Writer 造数 + ro 连接）。

    use crate::state::testutil::TempFile;
    use rusqlite::Connection;

    /// 建一个已迁移的空库并返回（路径 + ro 连接）。
    pub(crate) fn seeded_db(tag: &str) -> (TempFile, Connection) {
        let f = TempFile::new(tag, "db");
        let w = clrecoder_store::writer::Writer::open(f.as_ref()).unwrap();
        drop(w);
        let conn = Connection::open_with_flags(
            f.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        (f, conn)
    }
}
