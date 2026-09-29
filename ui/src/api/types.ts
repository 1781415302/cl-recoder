// CL Recoder 前端↔GUI Rust 的 TS 契约（PLAN §4.7 逐字对齐，手写、与 Rust serde camelCase DTO 对齐）。
// 锁定决策：所有 GUI→前端 DTO 一律 #[serde(rename_all="camelCase")]（PLAN §4.7）。
// S11/S12 消费；api/client.ts 的 invoke 包装由 S12 提供。

export type Range = { from: string; to: string }; // "YYYY-MM-DD"

export interface DeviceRow {
  id: number;
  kind: "keyboard" | "mouse" | "gamepad";
  vid: number;
  pid: number;
  name: string;
  /** 用户自定义昵称（可空；展示优先于 name） */
  nickname: string | null;
  firstSeen: string;
  lastSeen: string;
  total: number;
}

export interface CollectorStatus {
  running: boolean;
  taskExists: boolean;
  paused?: boolean;
  startedAt?: string;
  lastEventAt: string | null;
}

export interface MouseDistanceDay {
  day: string;
  distance_inches: number;
}

export interface MouseDistance {
  total_inches: number;
  days: MouseDistanceDay[];
}

export interface TopKeyRow {
  code: number;
  total: number;
  label: string;
}

export interface Overview {
  days: { day: string; total: number }[];
  today: { keys: number; clicks: number; gamepad: number };
  devices: { id: number; kind: "keyboard" | "mouse" | "gamepad"; name: string; total: number }[];
}

export interface KeyDailyRowLabeled {
  day: string;
  code: number;
  count: number;
  label: string;
}

export interface AppRowLabeled {
  exe: string;
  name: string;
  seconds: number;
  keys: number;
  clicks: number;
}

export interface ComboRowLabeled {
  mods: number;
  code: number;
  total: number;
  label: string;
}

export interface WpMeta {
  importedAt: string;
  sourcePath: string;
  sourceSize: number | null;
  dateMin: string | null;
  dateMax: string | null;
  note: string;
}

export interface WpOverview {
  days: { day: string; total: number }[];
  keysTotal: number;
  combosTotal: number;
  appsTotal: number;
  mouseClicksTotal: number;
}

export interface WpKeyRow {
  day: string;
  label: string;
  count: number;
}

export interface WpComboRow {
  day: string;
  combo: string;
  label: string;
  count: number;
}

export interface WpAppRow {
  day: string;
  name: string;
  seconds: number;
  keys: number;
  clicks: number;
}

export interface WpMouseRow {
  day: string;
  clicks: number;
  distanceMeters: number;
}

export interface WpMouseButtonRow {
  label: string;
  total: number;
}

export interface WpMouseScrollRow {
  label: string;
  total: number;
}

export interface ImportReport {
  ok: boolean;
  keys: number;
  combos: number;
  apps: number;
  mouseDays: number;
  dateMin: string | null;
  dateMax: string | null;
  warnings: string[];
  durationMs: number;
}

export interface ExportReport {
  ok: boolean;
  files: string[];
  rows: number;
}

export interface Settings {
  guiAutostart: boolean;
  wpDbPath: string | null;
  firstRunDone: boolean;
}

export interface SettingsPatch {
  guiAutostart?: boolean;
  wpDbPath?: string | null;
}
