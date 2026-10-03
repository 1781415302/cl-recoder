//! cl-recoder —— CL Recoder GUI 进程（Tauri v2，中完整性，HKCU Run 自启，PLAN §2.1）。
//!
//! S1 插件骨架 + S10 commands 接线 + **S12 集成联调**（PLAN §8-S12）：
//! - **单实例**：`tauri-plugin-single-instance` 第一个注册；二次启动聚焦已有主窗口；
//! - **托盘常驻**（§2.1 进程形态）：菜单 = 打开仪表盘 / 暂停-恢复 / 开机自启 / 退出；
//!   左键点击托盘图标直接显示主窗口，右键弹出菜单；
//! - **关闭到托盘**：主窗口 CloseRequested → 阻止关闭并隐藏（不退出进程）；
//! - **首启引导**：`first_run_done=false` 时自动弹出主窗口（默认无窗口启动）——
//!   首启时采集器必未运行（计划任务尚未安装），设置页/仪表盘给出"启用采集器"引导（§5.1）；
//!   弹窗后即标记完成，之后每次启动静默进托盘。
//!
//! 采集器故障隔离（§1）：退出/隐藏 GUI 绝不影响采集进程；托盘"暂停-恢复"只通过
//! 控制管道（§4.4）转发，管道不可达时打开主窗口给引导态而非报错。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// GUI 侧调试日志（§9.2 / §4.1-S1 适配）：stderr 部分沿用原诊断习惯——仅设置
/// `CLRECODER_DEBUG` 环境变量时输出（与 collector 的 debug_log! 同策略）；同时把消息
/// 转交 [`crate::commands::diagnostics::record_gui_message`] 做持久化适配（按既有
/// INFO/WARN/ERROR 前缀识别级别 + 固定 gui 事件码 + 60s/code 限频；诊断 sink 未初始化
/// 时静默丢弃）。`CLRECODER_DEBUG` 只影响 stderr 回显，不影响持久日志是否启用（§4.1）。
/// 宏定义于 crate root 且先于 mod 声明——全部子模块可见（`crate::gui_log!` 引用）。
#[macro_export]
macro_rules! gui_log {
    ($($arg:tt)*) => {{
        if std::env::var_os("CLRECODER_DEBUG").is_some() {
            eprintln!("[cl-recoder] {}", format_args!($($arg)*));
        }
        $crate::commands::diagnostics::record_gui_message(format_args!($($arg)*));
    }};
}

mod commands;
mod db;
mod keylabel;
mod state;
// S3（§4.4）：原生窗口 UI 活动快照——active 的唯一权威，前端经事件 + get_ui_activity 消费。
mod ui_activity;

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

/// 托盘图标 id（`tray_by_id` 可寻址）。
const TRAY_ID: &str = "clrecoder-tray";
/// 托盘菜单项 id：打开仪表盘。
const ID_OPEN: &str = "open-dashboard";
/// 托盘菜单项 id：暂停/恢复统计（文案随 collector 状态刷新）。
const ID_TOGGLE_PAUSE: &str = "toggle-pause";
/// 托盘菜单项 id：GUI 开机自启（复选；HKCU Run，无 UAC——采集器自启走设置页 UAC 流程）。
const ID_AUTOSTART: &str = "gui-autostart";
/// 托盘菜单项 id：退出 GUI（不影响采集器，§1 故障隔离）。
const ID_QUIT: &str = "quit";

/// 显示并聚焦主窗口（托盘"打开仪表盘"/托盘左键/单实例二次启动共用）。
fn show_main(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
    // S3（§4.4）：显式 show 之后发布——读取实际 visible/minimized 状态（统一发布入口）。
    ui_activity::refresh(app);
}

/// 探测 collector 是否处于暂停（`None` = 管道不可达，即未运行，§5.1）。
fn query_paused() -> Option<bool> {
    match commands::collector_ctl::ctl_request(&clrecoder_core::ipc::CtlRequest::Status) {
        Ok(resp) if resp.ok => resp.data.map(|d| d.paused),
        _ => None,
    }
}

/// 按 collector 实际状态刷新托盘暂停项文案：运行中 → "暂停统计"，已暂停 → "恢复统计"。
fn sync_pause_label(item: tauri::menu::MenuItem<tauri::Wry>) {
    let paused = query_paused();
    let _ = item.set_text(match paused {
        Some(true) => "恢复统计",
        _ => "暂停统计",
    });
}

/// 托盘"暂停-恢复"（任务菜单契约）：管道往返 ≤500ms（§5.1），放独立线程执行，
/// 绝不阻塞主事件循环；collector 未运行时打开主窗口并落到设置页（"启用采集器"引导）。
fn spawn_toggle_pause(app: tauri::AppHandle, item: tauri::menu::MenuItem<tauri::Wry>) {
    if let Err(e) = std::thread::Builder::new().name("tray-toggle-pause".into()).spawn(move || {
        match query_paused() {
            Some(current) => {
                let req = clrecoder_core::ipc::CtlRequest::SetPaused { paused: !current };
                let accepted =
                    matches!(commands::collector_ctl::ctl_request(&req), Ok(resp) if resp.ok);
                if accepted {
                    // 新状态 = !current；文案 = 对新状态的动作
                    let _ = item.set_text(if !current { "恢复统计" } else { "暂停统计" });
                } else {
                    gui_log!("WARN: 托盘暂停/恢复请求未被 collector 接受");
                }
            }
            None => {
                // 未运行：给引导态（设置页"启用采集器自启"/"立即启动采集器"）
                show_main(&app);
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.eval(
                        "if (window.location.hash !== '#settings') window.location.hash = 'settings';",
                    );
                }
            }
        }
    }) {
        gui_log!("WARN: 托盘暂停/恢复线程创建失败: {e}");
    }
}

/// 托盘"开机自启"复选项：GUI 自启（HKCU Run，tauri-plugin-autostart）。
/// 复用 `settings::apply_patch` 的"先副作用后落盘"语义（与设置页 `set_settings` 完全一致），
/// 保证托盘与设置页对 settings.json 的写入同一套规则。
fn toggle_gui_autostart(app: &tauri::AppHandle, item: &CheckMenuItem<tauri::Wry>) {
    let st = app.state::<state::AppState>().inner().clone();
    let current = st
        .settings
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let next = !current.gui_autostart;
    let patch = commands::settings::SettingsPatch {
        gui_autostart: Some(next),
        ..Default::default()
    };
    let plugin_app = app.clone();
    let result = commands::settings::apply_patch(
        &current,
        &patch,
        move |gui| match gui {
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
        },
        &db::settings_path(),
    );
    match result {
        Ok(next_settings) => {
            *st.settings
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = next_settings;
            let _ = item.set_checked(next);
            gui_log!("INFO: 托盘切换 GUI 开机自启 → {next}");
        }
        Err(e) => {
            gui_log!("WARN: 托盘切换 GUI 开机自启失败: {e}");
            let _ = item.set_checked(current.gui_autostart); // 失败回写真实状态
        }
    }
}

fn main() {
    tauri::Builder::default()
        // single-instance 必须第一个注册（PLAN §3 main.rs 注释）：
        // 二次启动（新进程即刻退出）聚焦已有主窗口——S12 单实例聚焦。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            gui_log!("INFO: 检测到二次启动，聚焦已有主窗口");
            show_main(app);
        }))
        // 对话框（WhatPulse 选择 / 导出保存框，PLAN §4.10/§5.4-§5.5）。
        .plugin(tauri_plugin_dialog::init())
        // “打开所在文件夹”（导出完成后，PLAN §5.5）。
        .plugin(tauri_plugin_opener::init())
        // GUI 自启 = HKCU Run（PLAN §6）；set_settings 里由 tauri-plugin-autostart 落实（PLAN §4.7）。
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // S10：托管 GUI 全局状态（ro 连接 + settings，§3 state.rs）。
        // AppState::new 不失败：stats.db 缺失/损坏 → 内存兜底连接（§5.1 引导态空数据）。
        .setup(|app| {
            // —— S1 诊断初始化（§4.1）——single-instance 插件已成立（二次启动进程已在
            // 插件期退出），此处先开 gui.log 并安装 facade adapter，**再**构造 AppState：
            // 防止同角色两个进程争轮转；AppState::new 里的 gui_log! 因此可被持久化适配。
            app.manage(commands::diagnostics::init_gui_diagnostics());
            app.manage(state::AppState::new());
            // S3（§4.4）：UI 活动状态（初始 revision=0/inactive），事件 + get_ui_activity 共用。
            app.manage(ui_activity::UiActivityState::new());
            let st = app.state::<state::AppState>().inner().clone();

            // —— S12 首启引导 —— first_run_done=false：自动弹出主窗口（默认无窗口启动，§8-S1）。
            // 首启时 collector 必未运行（计划任务 ClRecoderCollector 尚未安装），设置页会显示
            // "启用采集器自启（需一次 UAC）"引导空态；仪表盘空设备表同样有引导入口。
            // 弹窗后即标记完成——之后每次启动静默进托盘（§2.1 托盘常驻形态）。
            let first_run = !st
                .settings
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .first_run_done;
            if first_run {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
                // S3（§4.4）：首启 show 之后显式发布（读实际状态 → active）。
                ui_activity::refresh(app.handle());
                let mut next = st
                    .settings
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                next.first_run_done = true;
                match next.save_to(&db::settings_path()) {
                    Ok(()) => {
                        *st.settings
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
                        gui_log!(
                            "INFO: 首启引导——已弹出主窗口（引导启用采集器），first_run_done=true"
                        );
                    }
                    Err(e) => {
                        gui_log!("WARN: first_run_done 落盘失败（下次启动仍弹引导窗）: {e}")
                    }
                }
            }

            // —— S12 托盘 —— 菜单契约：打开仪表盘 / 暂停-恢复 / 开机自启 / 退出。
            let open_item = MenuItem::with_id(app, ID_OPEN, "打开仪表盘", true, None::<&str>)?;
            let pause_item =
                MenuItem::with_id(app, ID_TOGGLE_PAUSE, "暂停统计", true, None::<&str>)?;
            let autostart_checked = st
                .settings
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .gui_autostart;
            let autostart_item = CheckMenuItem::with_id(
                app,
                ID_AUTOSTART,
                "开机自启",
                true,
                autostart_checked,
                None::<&str>,
            )?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit_item = MenuItem::with_id(app, ID_QUIT, "退出", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[&open_item, &pause_item, &autostart_item, &sep, &quit_item],
            )?;

            let pause_for_menu = pause_item.clone();
            let autostart_for_menu = autostart_item.clone();

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(
                    app.default_window_icon()
                        .expect("bundle 图标必须存在")
                        .clone(),
                )
                .tooltip("CL Recoder")
                .menu(&menu)
                // Windows（tray-icon 平台实现）：右键抬起弹菜单；左键抬起走下方
                // on_tray_icon_event → 显示主窗口（§8-S12 验收"托盘点击显窗"）。
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    ID_OPEN => show_main(app),
                    ID_TOGGLE_PAUSE => spawn_toggle_pause(app.clone(), pause_for_menu.clone()),
                    ID_AUTOSTART => toggle_gui_autostart(app, &autostart_for_menu),
                    ID_QUIT => {
                        commands::diagnostics::record_gui_event(
                            clrecoder_diagnostics::Level::Info, "service.stopped", "GUI 已退出",
                        );
                        // 退出仅结束 GUI；采集器是独立进程继续统计（§1 故障隔离）。
                        app.cleanup_before_exit();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // 初始同步暂停文案（collector 已在运行且处于暂停 → 显示"恢复统计"）。
            sync_pause_label(pause_item);

            Ok(())
        })
        // S12 关闭到托盘：主窗口点 × → 阻止关闭并隐藏，进程驻留托盘（§2.1）。
        // S3（§4.4）：Focused/Resized 读取实际状态（失焦但可见保持 active；最小化/还原在
        // Windows 上表现为 Resized），Destroyed 置 inactive——原生状态是 UI 活动的唯一权威。
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                    ui_activity::refresh(window.app_handle());
                    gui_log!("INFO: 主窗口关闭请求 → 隐藏到托盘");
                }
                WindowEvent::Focused(_) | WindowEvent::Resized(_) => {
                    ui_activity::refresh(window.app_handle());
                }
                WindowEvent::Destroyed => ui_activity::mark_destroyed(window.app_handle()),
                _ => {}
            }
        })
        // S10：注册全部 Tauri commands（§4.7 契约，commands/mod.rs 汇总）。
        // S3（§4.4）：get_ui_activity 与既有 commands 联合注册——commands::handler() 保持原样
        //（mod.rs 不在本 Stage 文件清单），两个 handler 命令集不相交且对未知命令都返回
        // false，这里按命令名分发组合，语义不变。
        .invoke_handler({
            let ui_activity_handler: Box<tauri::ipc::InvokeHandler<tauri::Wry>> =
                Box::new(tauri::generate_handler![ui_activity::get_ui_activity]);
            let commands_handler = commands::handler();
            move |invoke| {
                if invoke.message.command() == ui_activity::COMMAND_GET_UI_ACTIVITY {
                    return ui_activity_handler(invoke);
                }
                commands_handler(invoke)
            }
        })
        .run(tauri::generate_context!())
        .expect("CL Recoder GUI 启动失败");
}
