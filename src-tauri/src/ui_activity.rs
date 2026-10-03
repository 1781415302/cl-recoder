//! ui_activity —— 原生窗口 UI 活动快照（usability-runtime-v3 §4.4，S3）。
//!
//! 原生窗口状态是前端"UI 是否活动"的**唯一权威**：`active = 实际 is_visible && !is_minimized`
//!（失焦但可见保持 active；禁止用 focused 代替 visible）。状态变化时 `revision` 单调增加，
//! 通过固定事件 [`EVENT_UI_ACTIVITY`] 只发给 main 窗口；[`get_ui_activity`] 命令只返回
//! 内存缓存（不重采样窗口，不与事件竞争较新版本）。
//!
//! 发布时机全部事件驱动（显式 show/hide + Native Focused/Resized/Destroyed，无新定时探测）：
//! - [`refresh`]：从 main 窗口读取实际 visible/minimized 后发布——`show_main`、首启 show、
//!   关闭 hide、Focused/Resized 统一走这里；
//! - [`mark_destroyed`]：窗口销毁直接置 inactive（窗口已不存在，无需读取）。
//!
//! 读取失败（窗口不存在 / Win32 查询出错）按 inactive 降级并记录诊断，不让错误触发
//! 高频轮询；发布与 emit 全程不 panic（emit 失败只记日志）。

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// §4.4 合同事件名：payload 同 [`UiActivitySnapshot`]，仅 emit 到 main 窗口。
pub const EVENT_UI_ACTIVITY: &str = "ui-activity";

/// §4.4 合同命令名（`generate_handler!` 按 [`get_ui_activity`] 函数名注册；main.rs 分发用）。
pub const COMMAND_GET_UI_ACTIVITY: &str = "get_ui_activity";

/// UI 活动快照（§4.4 契约逐字：serde camelCase，Clone + Serialize）。
/// 前端以 `revision` 识别新旧：旧版本/同版本一律忽略，首个 revision=0/inactive 也接受。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiActivitySnapshot {
    /// 状态变化时单调递增的版本号（初始 0）
    pub revision: u64,
    /// main 窗口可见且未最小化
    pub active: bool,
}

/// UI 活动状态（§4.4 契约）：Mutex 缓存当前快照，初始 inactive（revision=0）。
/// setup 阶段由 Tauri manage，[`get_ui_activity`] 经 `State` 注入；发布方经 [`refresh`]/
/// [`mark_destroyed`] 走 `try_state`（未建立时降级，不 panic）。
#[derive(Debug)]
pub struct UiActivityState {
    snapshot: Mutex<UiActivitySnapshot>,
}

impl Default for UiActivityState {
    fn default() -> Self {
        Self::new()
    }
}

impl UiActivityState {
    /// 初始 inactive 快照（revision=0，§4.4：前端首个 revision=0/inactive 也必须接受）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshot: Mutex::new(UiActivitySnapshot {
                revision: 0,
                active: false,
            }),
        }
    }

    /// 当前缓存快照（毒化恢复——事件发布线程 panic 不应让 getter 永久失败）。
    #[must_use]
    pub fn current(&self) -> UiActivitySnapshot {
        self.lock().clone()
    }

    /// 状态机：仅当 `active` 变化时 revision+1 并返回新快照；无变化返回 None（不重复发布）。
    fn transition(&self, active: bool) -> Option<UiActivitySnapshot> {
        let mut guard = self.lock();
        if guard.active == active {
            return None;
        }
        guard.revision += 1;
        guard.active = active;
        Some(guard.clone())
    }

    /// 毒化恢复为可用（与 state.rs 同策略）。
    fn lock(&self) -> MutexGuard<'_, UiActivitySnapshot> {
        self.snapshot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 统一发布入口（Native 事件 + 显式 show/hide 共用）：读取 main 窗口实际状态并发布。
/// 状态未变化时不重发（revision 不变，前端同版本忽略）。无 panic。
pub fn refresh(app: &AppHandle) {
    let active = read_active(app);
    publish(app, active);
}

/// 窗口销毁（§4.4 Destroyed）：置 inactive。窗口已不存在，无需读取实际状态。
pub fn mark_destroyed(app: &AppHandle) {
    publish(app, false);
}

/// 发布 `active`：revision 仅在状态变化时递增，变化时 emit 到 main（失败只记日志）。
fn publish(app: &AppHandle, active: bool) {
    let Some(state) = app.try_state::<UiActivityState>() else {
        crate::gui_log!("WARN: ui_activity 状态未建立（setup 未运行），忽略本次发布");
        return;
    };
    let Some(snapshot) = state.transition(active) else {
        return;
    };
    if let Err(e) = app.emit_to("main", EVENT_UI_ACTIVITY, snapshot.clone()) {
        crate::gui_log!("WARN: ui-activity 事件发布失败（前端将以 get_ui_activity 补齐）: {e}");
    }
}

/// 读取 main 窗口实际状态：`active = is_visible && !is_minimized`。
/// 窗口不存在或任一读取失败按 inactive 降级并记录诊断（§4.4）。
fn read_active(app: &AppHandle) -> bool {
    let Some(win) = app.get_webview_window("main") else {
        crate::gui_log!("WARN: ui_activity：main 窗口不存在，按 inactive 降级");
        return false;
    };
    let visible = win.is_visible();
    let minimized = win.is_minimized();
    let failed = visible.is_err() || minimized.is_err();
    let debug = (format!("{visible:?}"), format!("{minimized:?}"));
    let active = active_from_parts(visible.ok(), minimized.ok());
    if failed {
        crate::gui_log!(
            "WARN: ui_activity：窗口状态读取失败，按 inactive 降级（visible={} minimized={}）",
            debug.0,
            debug.1
        );
    }
    active
}

/// 纯映射（单测覆盖）：visible/minimized 读取结果 → active。
/// 任一读取失败（None）按 inactive 降级；失焦不影响（只看可见与最小化）。
fn active_from_parts(visible: Option<bool>, minimized: Option<bool>) -> bool {
    matches!((visible, minimized), (Some(true), Some(false)))
}

/// 前端命令（§4.4 契约逐字）：返回内存缓存快照。
/// **只返回缓存**——不重采样窗口，绝不以后台读取覆盖事件已发布的较新版本；
/// 缓存是 Mutex 内存读取（毒化已恢复），故恒 Ok；`Result` 形状与合同一致（async + State）。
#[tauri::command]
pub async fn get_ui_activity(
    state: tauri::State<'_, UiActivityState>,
) -> Result<UiActivitySnapshot, String> {
    Ok(state.current())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usability_v3_ui_activity_snapshot_serde_camel_shape() {
        // §4.4：serde camelCase，线上字段即 revision/active（载荷与事件一致）
        let json = serde_json::to_value(UiActivitySnapshot {
            revision: 3,
            active: true,
        })
        .unwrap();
        let obj = json.as_object().expect("快照必须序列化为对象");
        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["active", "revision"],
            "线上字段必须恰为 revision/active"
        );
        assert_eq!(obj["revision"], 3);
        assert_eq!(obj["active"], true);
    }

    #[test]
    fn usability_v3_ui_activity_initial_state_inactive_revision_zero() {
        // §4.4：初始 inactive（revision=0）——前端"首个 revision=0/inactive 也必须接受"
        let st = UiActivityState::new();
        let snap = st.current();
        assert_eq!((snap.revision, snap.active), (0, false));
    }

    #[test]
    fn usability_v3_ui_activity_revision_monotonic_only_on_change() {
        // §4.4：版本仅在状态变化时单调增加；无变化不产生新版本（同版本重复不回退）
        let st = UiActivityState::new();
        assert_eq!(
            st.transition(false),
            None,
            "初始 inactive → inactive 无变化"
        );
        let s1 = st.transition(true).expect("inactive → active 是状态变化");
        assert_eq!((s1.revision, s1.active), (1, true));
        assert_eq!(
            st.transition(true),
            None,
            "active → active 无变化，revision 不动"
        );
        assert_eq!(st.current().revision, 1, "无变化的发布不得推进版本");
        let s2 = st.transition(false).expect("active → inactive 是状态变化");
        assert_eq!((s2.revision, s2.active), (2, false), "版本严格单调递增");
        assert_eq!(st.current().revision, 2);
    }

    #[test]
    fn usability_v3_ui_activity_active_from_parts_degrades_to_inactive_on_read_failure() {
        // §4.4：active = is_visible && !is_minimized；失焦但可见保持 true；
        // 任一读取失败（None）按 inactive 降级。
        assert!(
            active_from_parts(Some(true), Some(false)),
            "可见未最小化 → active"
        );
        assert!(
            !active_from_parts(Some(true), Some(true)),
            "最小化 → inactive"
        );
        assert!(
            !active_from_parts(Some(false), Some(false)),
            "不可见 → inactive"
        );
        assert!(
            !active_from_parts(Some(false), Some(true)),
            "不可见且最小化 → inactive"
        );
        assert!(
            !active_from_parts(None, Some(false)),
            "visible 读失败 → inactive 降级"
        );
        assert!(
            !active_from_parts(Some(true), None),
            "minimized 读失败 → inactive 降级"
        );
        assert!(!active_from_parts(None, None), "全部读失败 → inactive 降级");
    }

    #[test]
    fn usability_v3_ui_activity_event_and_command_names_match_contract() {
        // §4.4：事件名固定 ui-activity；命令名与 generate_handler 注册的函数名一致
        assert_eq!(EVENT_UI_ACTIVITY, "ui-activity");
        assert_eq!(COMMAND_GET_UI_ACTIVITY, "get_ui_activity");
    }
}
