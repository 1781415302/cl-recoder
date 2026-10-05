// Tauri command invoke 包装（PLAN §4.7 全部命令，逐字对齐）。
//
// S12 已接线真实 commands：Tauri 环境内全部命令走 invoke（命令名与参数键与
// src-tauri 契约逐字一致，§4.7；多词参数按 Tauri 约定用 camelCase 键，如 deviceId）。
//
// 调试开关（保留）：设环境变量 VITE_CLRECODER_MOCK=1（npm run dev / build 前均可）强制走
// §4.7 契约 mock（api/mock.ts）。纯浏览器预览（无 Tauri runtime）也自动兜底 mock，
// 避免无 invoke 环境下抛错——真实运行请始终在 Tauri 窗口内。
import { invoke } from "@tauri-apps/api/core";
import type {
  AppRowLabeled,
  CollectorStatus,
  ComboRowLabeled,
  DeviceRow,
  DiagnosticsInfo,
  ExportReport,
  GamepadMotionSummary,
  ImportReport,
  KeyDailyRowLabeled,
  LegacyMouseSummary,
  MouseDistance,
  MouseMotionSummary,
  MouseSourceRow,
  MouseSources,
  Overview,
  Settings,
  SettingsPatch,
  TaskPolicy,
  TopKeyRow,
  WpAppRow,
  WpComboRow,
  WpKeyRow,
  WpMeta,
  WpMouseButtonRow,
  WpMouseRow,
  WpMouseScrollRow,
  WpOverview,
} from "./types";
import * as mock from "./mock";

const USE_MOCK = import.meta.env.VITE_CLRECODER_MOCK === "1"; // 调试开关：置 "1" 回退 mock
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(
  cmd: string,
  args: Record<string, unknown> | undefined,
  fallback: () => T | Promise<T>,
): Promise<T> {
  if (USE_MOCK || !inTauri) return fallback();
  return invoke<T>(cmd, args);
}

/* ===== 查询类（§4.7 逐字对齐） ===== */

/** get_overview(from, to) -> Overview */
export function getOverview(from: string, to: string): Promise<Overview> {
  return call("get_overview", { from, to }, () => mock.mockOverview(from, to));
}

/** get_devices() -> Vec<DeviceRow> */
export function getDevices(): Promise<DeviceRow[]> {
  return call("get_devices", undefined, () => mock.mockDevices());
}

/** get_key_daily(device_id, from, to) -> Vec<KeyDailyRowLabeled> */
export function getKeyDaily(deviceId: number, from: string, to: string): Promise<KeyDailyRowLabeled[]> {
  return call("get_key_daily", { deviceId, from, to }, () => mock.mockKeyDaily(deviceId, from, to));
}

/** get_top_keys(device_id, from, to, limit) -> Vec<TopKeyRow> */
export function getTopKeys(deviceId: number, from: string, to: string, limit: number): Promise<TopKeyRow[]> {
  return call("get_top_keys", { deviceId, from, to, limit }, () =>
    mock.mockTopKeys(deviceId, from, to, limit));
}

/** get_apps(from, to, limit) -> Vec<AppRowLabeled> */
export function getApps(from: string, to: string, limit: number): Promise<AppRowLabeled[]> {
  return call("get_apps", { from, to, limit }, () => mock.mockApps(from, to, limit));
}

/** get_combos(from, to, limit) -> Vec<ComboRowLabeled> */
export function getCombos(from: string, to: string, limit: number): Promise<ComboRowLabeled[]> {
  return call("get_combos", { from, to, limit }, () => mock.mockCombos(from, to, limit));
}

/** get_wp_meta() -> Option<WpMeta> */
export function getWpMeta(): Promise<WpMeta | null> {
  return call("get_wp_meta", undefined, () => mock.mockWpMeta());
}

/** get_wp_overview(from, to) -> WpOverview */
export function getWpOverview(from: string, to: string): Promise<WpOverview> {
  return call("get_wp_overview", { from, to }, () => mock.mockWpOverview(from, to));
}

/** get_wp_keys(from, to, limit) -> Vec<WpKeyRow> */
export function getWpKeys(from: string, to: string, limit: number): Promise<WpKeyRow[]> {
  return call("get_wp_keys", { from, to, limit }, () => mock.mockWpKeys(from, to, limit));
}

/** get_wp_combos(from, to, limit) -> Vec<WpComboRow> */
export function getWpCombos(from: string, to: string, limit: number): Promise<WpComboRow[]> {
  return call("get_wp_combos", { from, to, limit }, () => mock.mockWpCombos(from, to, limit));
}

/** get_wp_apps(from, to, limit) -> Vec<WpAppRow> */
export function getWpApps(from: string, to: string, limit: number): Promise<WpAppRow[]> {
  return call("get_wp_apps", { from, to, limit }, () => mock.mockWpApps(from, to, limit));
}

/** get_wp_mouse(from, to) -> Vec<WpMouseRow> */
export function getWpMouse(from: string, to: string): Promise<WpMouseRow[]> {
  return call("get_wp_mouse", { from, to }, () => mock.mockWpMouse(from, to));
}

/** get_wp_mouse_buttons(from, to, limit) -> Vec<WpMouseButtonRow> */
export function getWpMouseButtons(from: string, to: string, limit: number): Promise<WpMouseButtonRow[]> {
  return call("get_wp_mouse_buttons", { from, to, limit }, () => mock.mockWpMouseButtons(from, to, limit));
}

/** get_wp_mouse_scrolls(from, to, limit) -> Vec<WpMouseScrollRow> */
export function getWpMouseScrolls(from: string, to: string, limit: number): Promise<WpMouseScrollRow[]> {
  return call("get_wp_mouse_scrolls", { from, to, limit }, () => mock.mockWpMouseScrolls(from, to, limit));
}

/* ===== 动作类（§4.7 逐字对齐） ===== */

/** import_whatpulse(path) -> ImportReport */
export function importWhatpulse(path: string): Promise<ImportReport> {
  return call("import_whatpulse", { path }, () => mock.mockImport(path));
}

/** export_data(format: "csv"|"json", scope: "own"|"wp", from, to, path) -> ExportReport */
export function exportData(
  format: "csv" | "json",
  scope: "own" | "wp",
  from: string,
  to: string,
  path: string,
): Promise<ExportReport> {
  return call("export_data", { format, scope, from, to, path }, () =>
    mock.mockExport(format, scope, from, to));
}

/** collector_status() -> CollectorStatus */
export function collectorStatus(): Promise<CollectorStatus> {
  return call("collector_status", undefined, () => mock.mockCollectorStatus());
}

/** set_device_nickname(id, nickname|null) */
export function setDeviceNickname(id: number, nickname: string | null): Promise<void> {
  return call("set_device_nickname", { id, nickname }, () => mock.mockSetDeviceNickname(id, nickname));
}

/** get_mouse_distance(from, to) -> MouseDistance（本软件统计的鼠标移动距离） */
export function getMouseDistance(from: string, to: string): Promise<MouseDistance> {
  return call("get_mouse_distance", { from, to }, () => mock.mockMouseDistance(from, to));
}

/** set_collector_paused(paused) -> Result<(), String> */
export function setCollectorPaused(paused: boolean): Promise<void> {
  return call("set_collector_paused", { paused }, () => mock.mockSetPaused(paused));
}

/** collector_autostart_enable() -> Result<(), String> */
export function collectorAutostartEnable(): Promise<void> {
  return call("collector_autostart_enable", undefined, () => mock.mockAutostartEnable());
}

/** collector_autostart_disable() -> Result<(), String> */
export function collectorAutostartDisable(): Promise<void> {
  return call("collector_autostart_disable", undefined, () => mock.mockAutostartDisable());
}

/** collector_start_now() -> Result<(), String> */
export function collectorStartNow(): Promise<void> {
  return call("collector_start_now", undefined, () => mock.mockStartNow());
}

/** collector_autostart_repair() -> Result<(), String>（S2 §4.3：修复既有任务三项策略，一次 UAC） */
export function collectorAutostartRepair(): Promise<void> {
  return call("collector_autostart_repair", undefined, () => mock.mockAutostartRepair());
}

/** get_collector_task_policy() -> TaskPolicy（S2 §4.3：只读三项策略，仅 Settings 活动时查询） */
export function getCollectorTaskPolicy(): Promise<TaskPolicy> {
  return call("get_collector_task_policy", undefined, () => mock.mockTaskPolicy());
}

/** get_diagnostics_info() -> DiagnosticsInfo（S1 §4.1：GUI sink 日志状态） */
export function getDiagnosticsInfo(): Promise<DiagnosticsInfo> {
  return call("get_diagnostics_info", undefined, () => mock.mockDiagnosticsInfo());
}

/** open_diagnostics_directory() -> Result<(), String>（S1 §4.1：打开后端固定的日志目录） */
export function openDiagnosticsDirectory(): Promise<void> {
  return call("open_diagnostics_directory", undefined, () => mock.mockOpenDiagnosticsDirectory());
}

/** get_settings() -> Settings */
export function getSettings(): Promise<Settings> {
  return call("get_settings", undefined, () => mock.mockGetSettings());
}

/** set_settings(patch: SettingsPatch) -> Settings */
export function setSettings(patch: SettingsPatch): Promise<Settings> {
  return call("set_settings", { patch }, () => mock.mockSetSettings(patch));
}

/* ===== 运动查询与配置（motion-dpi §4.5：查询返回 Result，SQL 失败不伪装成空数据） ===== */

/** get_mouse_sources() -> MouseSources（availability=needs_upgrade 为旧 schema 引导态） */
export function getMouseSources(): Promise<MouseSources> {
  return call("get_mouse_sources", undefined, () => mock.mockGetMouseSources());
}

/** get_mouse_motion(sourceId, from, to) -> MouseMotionSummary（未知 sourceId 返回错误，不静默切全部鼠标） */
export function getMouseMotion(
  sourceId: number,
  from: string,
  to: string,
): Promise<MouseMotionSummary> {
  return call("get_mouse_motion", { sourceId, from, to }, () =>
    mock.mockGetMouseMotion(sourceId, from, to));
}

/** get_mouse_legacy(deviceId, from, to) -> LegacyMouseSummary（按型号查询旧算法读数） */
export function getMouseLegacy(
  deviceId: number,
  from: string,
  to: string,
): Promise<LegacyMouseSummary> {
  return call("get_mouse_legacy", { deviceId, from, to }, () =>
    mock.mockGetMouseLegacy(deviceId, from, to));
}

/** set_mouse_dpi(sourceId, dpi|null) -> MouseSourceRow（null 清除手动后备；失败抛错由调用方保留编辑内容） */
export function setMouseDpi(sourceId: number, dpi: number | null): Promise<MouseSourceRow> {
  return call("set_mouse_dpi", { sourceId, dpi }, () => mock.mockSetMouseDpi(sourceId, dpi));
}

/** get_gamepad_motion(deviceId, from, to) -> GamepadMotionSummary（按型号查询，dwellSeconds 恰 625 格） */
export function getGamepadMotion(
  deviceId: number,
  from: string,
  to: string,
): Promise<GamepadMotionSummary> {
  return call("get_gamepad_motion", { deviceId, from, to }, () =>
    mock.mockGetGamepadMotion(deviceId, from, to));
}
