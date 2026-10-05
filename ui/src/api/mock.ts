// §4.7 契约 mock 数据（S11 开发用；S12 接真实 commands 后本文件仅作浏览器预览兜底）。
// 注意：聚合/TopN 都在本文件内完成——mock 扮演的是"后端 SQL"的角色，前端页面本身不做聚合（PLAN §2.5）。
import type {
  AppRowLabeled,
  CollectorStatus,
  ComboRowLabeled,
  DeviceRow,
  DiagnosticsInfo,
  DpiProbeStatus,
  Evidence,
  ExportReport,
  GamepadMotionSummary,
  ImportReport,
  KeyDailyRowLabeled,
  LegacyMouseSummary,
  MouseMotionDay,
  MouseMotionSummary,
  MouseSourceRow,
  MouseSources,
  Overview,
  Settings,
  SettingsPatch,
  StickMotionSummary,
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

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/* ---------- 确定性伪随机（固定种子，刷新不变） ---------- */
function hashStr(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}
function rng(seedKey: string): number {
  let t = hashStr(seedKey) + 0x6d2b79f5;
  t = Math.imul(t ^ (t >>> 15), t | 1);
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
}

/* ---------- 设备与 code 剖面（code 值域/标签模拟后端 keylabel 输出） ---------- */
const SC_LABELS: Record<number, string> = {
  0x01: "Esc", 0x0e: "Backspace", 0x0f: "Tab", 0x1c: "Enter", 0x39: "Space",
  0x2a: "Shift", 0x36: "右 Shift", 0x1d: "Ctrl", 0x38: "Alt", 0xe05b: "Win",
  0x3a: "CapsLock", 0xe053: "Delete", 0xe052: "Insert",
  0xe047: "Home", 0xe04f: "End", 0xe049: "PageUp", 0xe051: "PageDown",
  0xe04b: "←", 0xe048: "↑", 0xe04d: "→", 0xe050: "↓",
  0xe11d: "Pause", 0xe037: "PrintScreen",
  0x33: ",", 0x34: ".", 0x35: "/", 0x27: ";", 0x28: "'", 0x2b: "\\",
  0x0c: "-", 0x0d: "=", 0x1a: "[", 0x1b: "]", 0x29: "`",
};
for (let i = 0; i < 10; i++) SC_LABELS[0x02 + i] = i === 9 ? "0" : String(i + 1);
"QWERTYUIOPASDFGHJKLZXCVBNM".split("").forEach((ch, i) => {
  // set-1 make code 区段：Q..P=0x10..0x19，A..L=0x1E..0x26，Z..M=0x2C..0x32
  const sc = i < 10 ? 0x10 + i : i < 19 ? 0x1e + (i - 10) : 0x2c + (i - 19);
  SC_LABELS[sc] = ch;
});
for (let i = 0; i < 12; i++) SC_LABELS[0x3b + i] = `F${i + 1}`;
function scLabel(sc: number): string {
  return SC_LABELS[sc] ?? `键 0x${sc.toString(16).toUpperCase()}`;
}

const MOUSE_LABELS: Record<number, string> = {
  1: "左键", 2: "右键", 3: "中键", 4: "侧键 X1", 5: "侧键 X2",
  6: "滚轮上", 7: "滚轮下", 8: "滚轮左", 9: "滚轮右",
};
// 17 码物理标签表（usability-runtime-v3 §4.6 U2；与 src-tauri keylabel.rs gamepad_button_label
// 同一字符串）：code 取 gilrs 枚举位次、显示用物理键位——XInput 后端物理 X→West、Y→North、
// 肩键→LeftTrigger/RightTrigger 枚举、模拟扳机→LeftTrigger2/RightTrigger2 枚举，
// 修正旧 mock 的 3/4 X/Y 错位与枚举名泄漏；未知 code 兜底 `按钮 N`（labelOf）。
// Guide（code 11）保留：当前后端可能不产出该事件，计数 0 属预期、不是故障。
const GAMEPAD_LABELS: Record<number, string> = {
  1: "A（南）", 2: "B（东）", 3: "Y（北）", 4: "X（西）",
  5: "LB（左肩）", 6: "LT（左扳机）", 7: "RB（右肩）", 8: "RT（右扳机）",
  9: "View（选择）", 10: "Menu（开始）", 11: "Guide", 12: "左摇杆按下", 13: "右摇杆按下",
  14: "十字上", 15: "十字下", 16: "十字左", 17: "十字右",
};

interface DeviceProfile {
  row: DeviceRow;
  rates: [number, number][]; // [code, 日均次数]
}
const KB_RATES: [number, number][] = [
  [0x39, 2400], [0x12, 880], [0x24, 760], [0x13, 700], [0x0b, 640], [0x18, 620],
  [0x21, 600], [0x16, 590], [0x22, 560], [0x26, 540], [0x1e, 500], [0x11, 490],
  [0x1f, 470], [0x20, 450], [0x23, 430], [0x15, 420], [0x17, 400], [0x2c, 330],
  [0x2d, 300], [0x2e, 360], [0x2f, 340], [0x30, 300], [0x31, 260], [0x32, 240],
  [0x10, 250], [0x19, 230], [0x0e, 560], [0x1c, 430], [0x2a, 820], [0x1d, 640],
  [0xe05b, 260], [0x33, 190], [0x34, 210], [0x35, 150], [0xe053, 180],
  [0xe04b, 150], [0xe048, 130], [0xe04d, 150], [0xe050, 140], [0xe047, 90],
  [0xe04f, 80], [0x0f, 120], [0xe11d, 2],
];
const MOUSE_RATES: [number, number][] = [
  [1, 3100], [2, 1420], [3, 170], [4, 110], [5, 55], [6, 760], [7, 820], [8, 22], [9, 28],
];
const GAMEPAD_RATES: [number, number][] = [
  [1, 410], [2, 250], [3, 205], [4, 175], [5, 140], [6, 155], [7, 148], [8, 185],
  [9, 88], [10, 92], [11, 38], [12, 115], [13, 105], [14, 70], [15, 66], [16, 58], [17, 62],
];
const PROFILES: DeviceProfile[] = [
  { row: { id: 1, kind: "keyboard", vid: 0x046d, pid: 0xc31c, name: "Logitech K845 机械键盘", nickname: "主力键盘", firstSeen: "2026-03-02", lastSeen: "2026-09-28", total: 0 }, rates: KB_RATES },
  { row: { id: 2, kind: "keyboard", vid: 0x0c45, pid: 0x7603, name: "Filco Majestouch 2", nickname: null, firstSeen: "2026-01-15", lastSeen: "2026-09-21", total: 0 }, rates: KB_RATES.slice(0, 30) },
  { row: { id: 3, kind: "mouse", vid: 0x046d, pid: 0xc092, name: "Logitech G102 游戏鼠标", nickname: "G102", firstSeen: "2026-03-02", lastSeen: "2026-09-28", total: 0 }, rates: MOUSE_RATES },
  { row: { id: 4, kind: "mouse", vid: 0x045e, pid: 0x0083, name: "Microsoft 基本光学鼠标", nickname: null, firstSeen: "2026-02-08", lastSeen: "2026-08-30", total: 0 }, rates: MOUSE_RATES },
  { row: { id: 5, kind: "gamepad", vid: 0, pid: 0, name: "XInput 手柄", nickname: null, firstSeen: "2026-05-11", lastSeen: "2026-09-27", total: 0 }, rates: GAMEPAD_RATES },
];
function profileOf(deviceId: number): DeviceProfile {
  return PROFILES.find((p) => p.row.id === deviceId) ?? PROFILES[0];
}
function labelOf(deviceId: number, code: number): string {
  const kind = profileOf(deviceId).row.kind;
  return kind === "mouse" ? (MOUSE_LABELS[code] ?? `按钮 ${code}`)
    : kind === "gamepad" ? (GAMEPAD_LABELS[code] ?? `按钮 ${code}`)
    : scLabel(code);
}

/* ---------- 自有数据：按 (设备, 天, code) 生成 ---------- */
function weekendFactor(day: string): number {
  const wd = new Date(`${day}T00:00:00`).getDay();
  return wd === 0 || wd === 6 ? 0.62 : 1;
}
const dayCountsCache = new Map<string, Map<number, number>>();
function dayCounts(deviceId: number, day: string): Map<number, number> {
  const key = `${deviceId}|${day}`;
  let m = dayCountsCache.get(key);
  if (!m) {
    m = new Map();
    const wf = weekendFactor(day);
    for (const [code, rate] of profileOf(deviceId).rates) {
      const jitter = 0.78 + rng(`${deviceId}:${code}:${day}`) * 0.44;
      const c = Math.round(rate * wf * jitter);
      if (c > 0) m.set(code, c);
    }
    dayCountsCache.set(key, m);
  }
  return m;
}
const dayTotalCache = new Map<string, number>();
function dayTotal(deviceId: number, day: string): number {
  const key = `${deviceId}|${day}`;
  let t = dayTotalCache.get(key);
  if (t === undefined) {
    t = 0;
    for (const v of dayCounts(deviceId, day).values()) t += v;
    dayTotalCache.set(key, t);
  }
  return t;
}
function rangeDays(from: string, to: string): string[] {
  const days: string[] = [];
  const end = new Date(`${to}T00:00:00`);
  for (const d = new Date(`${from}T00:00:00`); d <= end; d.setDate(d.getDate() + 1)) {
    days.push(
      `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`,
    );
  }
  return days;
}

/* ---------- 自有数据查询（对应 §4.7 各 get_* 命令） ---------- */
function sumRange(deviceId: number, from: string, to: string): number {
  let t = 0;
  for (const day of rangeDays(from, to)) t += dayTotal(deviceId, day);
  return t;
}

export function mockDevices(): DeviceRow[] {
  const now = new Date();
  const today = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
  return PROFILES.map((p) => ({ ...p.row, total: sumRange(p.row.id, "2026-01-01", today) }));
}

/** 每 1s tick 让"今日"数字微微增长，便于观察近实时轮询（S12 换真实数据后自然消失）。 */
function liveBump(): number {
  return Math.floor((Date.now() - new Date().setHours(0, 0, 0, 0)) / 1000) * 7;
}

export function mockOverview(from: string, to: string): Overview {
  const days = rangeDays(from, to).map((day) => ({
    day,
    total: PROFILES.reduce((s, p) => s + dayTotal(p.row.id, day), 0),
  }));
  const today = days[days.length - 1]?.day ?? from;
  const todayOf = (kind: "keyboard" | "mouse" | "gamepad") =>
    PROFILES.filter((p) => p.row.kind === kind).reduce((s, p) => s + dayTotal(p.row.id, today), 0);
  return {
    days,
    today: {
      keys: todayOf("keyboard") + liveBump() * 3,
      clicks: todayOf("mouse") + liveBump(),
      gamepad: todayOf("gamepad"),
    },
    devices: PROFILES.map((p) => ({
      id: p.row.id, kind: p.row.kind, name: p.row.name, total: sumRange(p.row.id, from, to),
    })),
  };
}

export function mockTopKeys(deviceId: number, from: string, to: string, limit: number): TopKeyRow[] {
  const days = rangeDays(from, to);
  const rows = profileOf(deviceId).rates.map(([code]) => {
    let total = 0;
    for (const day of days) total += dayCounts(deviceId, day).get(code) ?? 0;
    return { code, total, label: labelOf(deviceId, code) };
  });
  rows.sort((a, b) => b.total - a.total);
  return rows.slice(0, limit);
}

export function mockKeyDaily(deviceId: number, from: string, to: string): KeyDailyRowLabeled[] {
  const rows: KeyDailyRowLabeled[] = [];
  for (const day of rangeDays(from, to)) {
    for (const [code, count] of dayCounts(deviceId, day)) {
      rows.push({ day, code, count, label: labelOf(deviceId, code) });
    }
  }
  return rows;
}

const APPS: { exe: string; path: string; name: string; secs: number; keys: number; clicks: number }[] = [
  { exe: "chrome.exe", path: "c:/program files/google/chrome/application/chrome.exe", name: "Google Chrome", secs: 11800, keys: 5200, clicks: 890 },
  { exe: "code.exe", path: "c:/users/dev/appdata/local/programmes/microsoft vs code/code.exe", name: "Visual Studio Code", secs: 9600, keys: 12800, clicks: 640 },
  { exe: "weixin.exe", path: "c:/program files/tencent/wechat/weixin.exe", name: "微信", secs: 5400, keys: 3800, clicks: 760 },
  { exe: "explorer.exe", path: "c:/windows/explorer.exe", name: "文件资源管理器", secs: 2100, keys: 420, clicks: 980 },
  { exe: "dota2.exe", path: "d:/steam/steamapps/common/dota 2 beta/game/bin/win64/dota2.exe", name: "Dota 2", secs: 4700, keys: 900, clicks: 5400 },
  { exe: "devenv.exe", path: "c:/program files/microsoft visual studio/2022/community/common7/ide/devenv.exe", name: "Visual Studio 2022", secs: 3300, keys: 4100, clicks: 260 },
  { exe: "cmd.exe", path: "c:/windows/system32/cmd.exe", name: "命令提示符", secs: 900, keys: 1500, clicks: 45 },
  { exe: "msedge.exe", path: "c:/program files (x86)/microsoft/edge/application/msedge.exe", name: "Microsoft Edge", secs: 2600, keys: 1200, clicks: 320 },
  { exe: "notepad.exe", path: "c:/windows/system32/notepad.exe", name: "记事本", secs: 400, keys: 620, clicks: 30 },
  { exe: "steam.exe", path: "d:/steam/steam.exe", name: "Steam", secs: 1200, keys: 300, clicks: 520 },
  { exe: "unknown", path: "unknown", name: "未知应用", secs: 180, keys: 60, clicks: 40 },
];
export function mockApps(from: string, to: string, limit: number): AppRowLabeled[] {
  const n = rangeDays(from, to).length;
  const rows = APPS.map((a) => {
    const j = 0.8 + rng(a.exe) * 0.4;
    return {
      exe: a.exe, name: a.name,
      seconds: Math.round(a.secs * j * (n / 1)),
      keys: Math.round(a.keys * j * (n / 1)),
      clicks: Math.round(a.clicks * j * (n / 1)),
    };
  });
  rows.sort((a, b) => b.seconds - a.seconds);
  return rows.slice(0, limit);
}

const MOD_NAMES: [number, string][] = [[1, "Ctrl"], [2, "Shift"], [4, "Alt"], [8, "Win"]];
function modsLabel(mods: number): string {
  return MOD_NAMES.filter(([bit]) => mods & bit).map(([, n]) => n).join("+");
}
const COMBOS: [number, number, number][] = [
  [1, 0x2e, 920], [1, 0x2f, 880], [1, 0x1f, 540], [1, 0x14, 380], [1, 0x11, 360],
  [1, 0x21, 300], [1, 0x12, 260], [1, 0x2c, 240], [1, 0x2d, 220], [1, 0x1e, 180],
  [4, 0x0f, 520], [1 | 2, 0x01, 160], [1 | 4, 0xe053, 60], [8, 0x20, 140],
  [8, 0x12, 120], [8, 0x0f, 90], [8, 0x26, 30], [2, 0xe047, 110],
];
export function mockCombos(from: string, to: string, limit: number): ComboRowLabeled[] {
  const n = rangeDays(from, to).length;
  const rows = COMBOS.map(([mods, code, daily]) => ({
    mods, code, total: Math.round(daily * n * (0.8 + rng(`${mods}:${code}`) * 0.4)),
    label: `${modsLabel(mods)}+${scLabel(code)}`,
  }));
  rows.sort((a, b) => b.total - a.total);
  return rows.slice(0, limit);
}

/* ---------- WhatPulse（2025-04-01 ~ 昨天，约 1.5 年历史；可重复 fixture，模拟后端逐日行） ---------- */
const WP_START = "2025-04-01";
const WP_END = (() => { const d = new Date(); d.setDate(d.getDate() - 1); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`; })();
const WP_TOTAL_DAYS = 545; // 归一化基准：fixture 总次数按约 1.5 年（545 天）折算日均
function wpDayActive(day: string): boolean { return day >= WP_START && day <= WP_END; }
function wpActiveDays(from: string, to: string): string[] {
  return rangeDays(from, to).filter(wpDayActive);
}
// [显示名, Qt 键码, 范围总次数（百万）]；qtKey 与 WP_COMBOS 的码口径一致（Qt::Key_*）。
// day+qtKey 唯一（§4.6 稳定身份）；同名不同码不冲突，同日标签也不重复。
const WP_KEYS: [string, number, number][] = [
  ["Space", 0x20, 9.2], ["E", 0x45, 4.6], ["I", 0x49, 3.4], ["Backspace", 0x01000003, 3.1],
  ["A", 0x41, 3.0], ["N", 0x4e, 2.9], ["T", 0x54, 2.8], ["O", 0x4f, 2.6], ["S", 0x53, 2.5],
  ["H", 0x48, 2.3], ["R", 0x52, 2.2], ["D", 0x44, 2.0], ["L", 0x4c, 1.8],
  ["Enter", 0x01000004, 1.7], ["U", 0x55, 1.6], ["C", 0x43, 1.5], ["M", 0x4d, 1.3],
  ["W", 0x57, 1.2], ["F", 0x46, 1.1], ["G", 0x47, 1.0], ["Y", 0x59, 0.95], ["P", 0x50, 0.9],
  [",", 0x2c, 0.8], [".", 0x2e, 0.78], ["V", 0x56, 0.6], ["Shift", 0x01000020, 2.4],
  ["Ctrl", 0x01000021, 1.4], ["B", 0x42, 0.5], ["K", 0x4b, 0.45], ["1", 0x31, 0.4],
];
/** 逐日按键行（模拟后端 wp_key_daily 逐日行，非跨日聚合；无活动日为空） */
function wpKeyRows(from: string, to: string): WpKeyRow[] {
  const rows: WpKeyRow[] = [];
  for (const day of wpActiveDays(from, to)) {
    const wf = weekendFactor(day);
    for (const [label, qtKey, totalM] of WP_KEYS) {
      const jitter = 0.8 + rng(`wpkey:${qtKey}:${day}`) * 0.4;
      rows.push({ day, qtKey, label, count: Math.round((totalM * 1_000_000 * jitter * wf) / WP_TOTAL_DAYS) });
    }
  }
  return rows;
}
export function mockWpMeta(): WpMeta | null {
  return {
    importedAt: state.wpImportedAt,
    sourcePath: state.settings.wpDbPath ?? "%LOCALAPPDATA%\\WhatPulse\\whatpulse.db",
    sourceSize: 218_364_928,
    dateMin: WP_START,
    dateMax: WP_END,
    note: "整体快照导入（重复导入即刷新）",
  };
}
/** 全部汇总都从逐日行聚合（模拟后端 SQL 职责），与各明细查询同一 fixture 源 */
export function mockWpOverview(from: string, to: string): WpOverview {
  const keyRows = wpKeyRows(from, to);
  const mouseRows = wpMouseRows(from, to);
  const keysPerDay = new Map<string, number>();
  for (const r of keyRows) keysPerDay.set(r.day, (keysPerDay.get(r.day) ?? 0) + r.count);
  const clicksPerDay = new Map<string, number>();
  for (const r of mouseRows) clicksPerDay.set(r.day, r.clicks);
  return {
    days: wpActiveDays(from, to).map((day) => ({
      day,
      total: (keysPerDay.get(day) ?? 0) + (clicksPerDay.get(day) ?? 0),
    })),
    keysTotal: keyRows.reduce((s, r) => s + r.count, 0),
    combosTotal: wpComboRows(from, to).reduce((s, r) => s + r.count, 0),
    // 真实后端语义：范围内应用前台秒数总和（全部应用日行参与，不受 Top-N 截断影响）
    appsTotal: wpAppRows(from, to).reduce((s, r) => s + r.seconds, 0),
    mouseClicksTotal: mouseRows.reduce((s, r) => s + r.clicks, 0),
  };
}
export function mockWpKeys(from: string, to: string, limit: number): WpKeyRow[] {
  return wpKeyRows(from, to)
    .sort((a, b) => b.count - a.count || a.day.localeCompare(b.day) || a.qtKey - b.qtKey)
    .slice(0, limit);
}
// [combo 原文, 友好格式, 范围总次数]；day+combo 唯一
const WP_COMBOS: [string, string, number][] = [
  ["control,67", "Ctrl+C", 96_400], ["control,86", "Ctrl+V", 91_200], ["control,83", "Ctrl+S", 41_800],
  ["control,84", "Ctrl+T", 38_900], ["control,87", "Ctrl+W", 31_500], ["alt,9", "Alt+Tab", 28_700],
  ["control,90", "Ctrl+Z", 24_100], ["shift,16777248", "Shift+Space", 19_800],
  ["control,16777249", "Ctrl+Esc", 8_900], ["meta,68", "Win+D", 7_600],
];
/** 逐日组合键行（模拟后端 wp_combo_daily 逐日行；无活动日为空） */
function wpComboRows(from: string, to: string): WpComboRow[] {
  const rows: WpComboRow[] = [];
  for (const day of wpActiveDays(from, to)) {
    const wf = weekendFactor(day);
    for (const [combo, label, total] of WP_COMBOS) {
      const jitter = 0.8 + rng(`wpcombo:${combo}:${day}`) * 0.4;
      rows.push({ day, combo, label, count: Math.round((total * jitter * wf) / WP_TOTAL_DAYS) });
    }
  }
  return rows;
}
export function mockWpCombos(from: string, to: string, limit: number): WpComboRow[] {
  return wpComboRows(from, to)
    .sort((a, b) => b.count - a.count || a.day.localeCompare(b.day) || a.combo.localeCompare(b.combo))
    .slice(0, limit);
}
/** 逐日应用行（模拟后端 wp_app_daily：day+path 主键身份；无活动日为空） */
function wpAppRows(from: string, to: string): WpAppRow[] {
  const rows: WpAppRow[] = [];
  for (const day of wpActiveDays(from, to)) {
    for (const a of APPS) {
      const j = 0.75 + rng(`wpapp:${a.exe}:${day}`) * 0.5;
      rows.push({
        day, path: a.path, name: a.name,
        seconds: Math.round((a.secs / 30) * j),
        keys: Math.round((a.keys / 30) * j),
        clicks: Math.round((a.clicks / 30) * j),
      });
    }
  }
  return rows;
}
export function mockWpApps(from: string, to: string, limit: number): WpAppRow[] {
  return wpAppRows(from, to)
    .sort((a, b) => b.seconds - a.seconds || a.day.localeCompare(b.day) || a.path.localeCompare(b.path))
    .slice(0, limit);
}
function wpMouseRows(from: string, to: string): WpMouseRow[] {
  return wpActiveDays(from, to).map((day) => {
    const clicks = Math.round(5200 * weekendFactor(day) * (0.8 + rng(`wpm:${day}`) * 0.4));
    const inches = 9000 * (0.7 + rng(`wpd:${day}`) * 0.6);
    return { day, clicks, distanceMeters: Math.round(inches * 0.0254 * 10) / 10 };
  });
}
export function mockWpMouse(from: string, to: string): WpMouseRow[] {
  return wpMouseRows(from, to);
}
// [显示名, 日均次数]（原固定总量按 545 天折算日均）；范围内按活动天数缩放
const WP_MOUSE_BUTTONS: [string, number][] = [
  ["左键", 3013], ["右键", 1655], ["中键", 217], ["其他", 40],
];
const WP_MOUSE_SCROLLS: [string, number][] = [
  ["向上", 757], ["向下", 731], ["向左", 8], ["向右", 9],
];
/** 按码聚合（无 day 字段，与后端一致）；无活动日返回 [] */
export function mockWpMouseButtons(from: string, to: string, limit: number): WpMouseButtonRow[] {
  const n = wpActiveDays(from, to).length;
  if (n === 0) return [];
  return WP_MOUSE_BUTTONS
    .map(([label, daily]) => ({ label, total: Math.round(daily * n * (0.85 + rng(`wpbtn:${label}`) * 0.3)) }))
    .slice(0, limit);
}
/** 按方向码聚合（无 day 字段，与后端一致）；无活动日返回 [] */
export function mockWpMouseScrolls(from: string, to: string, limit: number): WpMouseScrollRow[] {
  const n = wpActiveDays(from, to).length;
  if (n === 0) return [];
  return WP_MOUSE_SCROLLS
    .map(([label, daily]) => ({ label, total: Math.round(daily * n * (0.85 + rng(`wpscr:${label}`) * 0.3)) }))
    .slice(0, limit);
}

/* ---------- 有状态命令（采集器控制 / 设置 / 导入 / 导出） ---------- */
export const state: {
  collectorRunning: boolean;
  paused: boolean;
  startedAt: string;
  taskInstalled: boolean;
  /** 任务策略（S2 §4.3 mock：与真实后端同线形状；修复后置为合规值） */
  taskPolicy: TaskPolicy;
  diagnostics: DiagnosticsInfo;
  wpImportedAt: string;
  settings: Settings;
} = {
  collectorRunning: true,
  paused: false,
  startedAt: new Date(Date.now() - 137 * 60_000).toISOString(),
  taskInstalled: true,
  taskPolicy: {
    evidence: "present",
    executionTimeLimit: "PT72H",
    disallowStartIfOnBatteries: true,
    stopIfGoingOnBatteries: true,
    compliant: false,
    appliesOnNextStart: true,
  },
  diagnostics: {
    logDirectory: "%LOCALAPPDATA%\\ClRecoder\\logs",
    guiLoggingAvailable: true,
    lastError: null,
    maxTotalBytes: 1_048_576,
  },
  wpImportedAt: new Date(Date.now() - 26 * 3600_000).toISOString(),
  settings: {
    guiAutostart: false,
    wpDbPath: null,
    firstRunDone: true,
  },
};

export async function mockCollectorStatus(): Promise<CollectorStatus> {
  await sleep(120);
  const taskEvidence: Evidence = state.taskInstalled ? "present" : "absent";
  return state.collectorRunning
    ? {
        running: true,
        taskExists: state.taskInstalled,
        taskEvidence,
        health: state.paused ? "paused" : "running",
        pipeReachable: true,
        processEvidence: "present",
        diagnosticCode: null,
        diagnosticMessage: null,
        paused: state.paused,
        startedAt: state.startedAt,
        lastEventAt: new Date(Date.now() - 2_000).toISOString(),
      }
    : {
        running: false,
        taskExists: state.taskInstalled,
        taskEvidence,
        health: "not_running",
        pipeReachable: false,
        processEvidence: "absent",
        diagnosticCode: "pipe_not_found",
        diagnosticMessage: "采集器未运行（控制管道不存在）",
        lastEventAt: null,
      };
}

/** S2 §4.3：修复既有任务三项策略（mock 仅改内存状态，不触发任何真实环境操作） */
export async function mockAutostartRepair(): Promise<void> {
  await sleep(900);
  state.taskInstalled = true;
  state.taskPolicy = {
    evidence: "present",
    executionTimeLimit: "PT0S",
    disallowStartIfOnBatteries: false,
    stopIfGoingOnBatteries: false,
    compliant: true,
    appliesOnNextStart: true,
  };
}

/** S2 §4.3：只读三项策略（mock 同线形状） */
export async function mockTaskPolicy(): Promise<TaskPolicy> {
  await sleep(150);
  return { ...state.taskPolicy, appliesOnNextStart: true };
}

/** S1 §4.1：GUI sink 日志状态（mock 同线形状） */
export async function mockDiagnosticsInfo(): Promise<DiagnosticsInfo> {
  await sleep(120);
  return { ...state.diagnostics };
}

/** S1 §4.1：打开日志目录（mock 不打开真实目录） */
export async function mockOpenDiagnosticsDirectory(): Promise<void> {
  await sleep(120);
}
export async function mockSetPaused(paused: boolean): Promise<void> {
  await sleep(180);
  if (!state.collectorRunning) throw new Error("采集器未运行");
  state.paused = paused;
}
export async function mockAutostartEnable(): Promise<void> { await sleep(900); state.taskInstalled = true; state.collectorRunning = true; state.startedAt = new Date().toISOString(); }
export async function mockAutostartDisable(): Promise<void> { await sleep(500); state.taskInstalled = false; }
export async function mockStartNow(): Promise<void> {
  await sleep(700);
  state.collectorRunning = true;
  state.startedAt = new Date().toISOString();
}
export async function mockSetDeviceNickname(_id: number, _nickname: string | null): Promise<void> {
  await sleep(120);
}
/** 自有鼠标移动距离：按收到的 [from, to] 逐日生成（可重复 fixture），total = 各日之和 */
export async function mockMouseDistance(from: string, to: string): Promise<import("./types").MouseDistance> {
  await sleep(100);
  const days = rangeDays(from, to).map((day) => {
    const inches = 9000 * (0.7 + rng(`mousedist:${day}`) * 0.6);
    return { day, distanceInches: Math.round(inches * 10) / 10 };
  });
  return {
    totalInches: Math.round(days.reduce((s, d) => s + d.distanceInches, 0) * 10) / 10,
    days,
  };
}
export async function mockGetSettings(): Promise<Settings> { await sleep(80); return { ...state.settings }; }
export async function mockSetSettings(patch: SettingsPatch): Promise<Settings> {
  await sleep(200);
  state.settings = { ...state.settings, ...patch, firstRunDone: true };
  return { ...state.settings };
}
export async function mockImport(_path: string): Promise<ImportReport> {
  await sleep(1500);
  state.wpImportedAt = new Date().toISOString();
  return {
    ok: true, keys: 46_318_940, combos: 4_882_100, apps: 3_905, mouseDays: 545,
    dateMin: WP_START, dateMax: WP_END,
    warnings: ["滚轮方向码为推断语义（WhatPulse 内部编码无公开文档）", "源库缺少部分日期的应用数据，已跳过"],
    durationMs: 1487,
  };
}
export async function mockExport(format: string, scope: string, from: string, to: string): Promise<ExportReport> {
  await sleep(900);
  const stem = scope === "wp" ? "wp_" : "";
  const files = format === "json"
    ? [`clrecoder_${scope}_${from}_${to}.json`]
    : scope === "own"
      ? [`keys_1_${from}_${to}.csv`, `keys_3_${from}_${to}.csv`, `apps_${from}_${to}.csv`, `combos_${from}_${to}.csv`, "devices.csv"]
      : [`${stem}keys_${from}_${to}.csv`, `${stem}apps_${from}_${to}.csv`, `${stem}mouse_${from}_${to}.csv`];
  return { ok: true, files, rows: 12_418 };
}

/* ---------- 运动命令 mock（motion-dpi §4.5：同签名、合法日/625 格、明确模拟数据，不发真实 HID/UAC） ---------- */
//
// 场景覆盖（§4.5 验收点）：同型号两来源（id 1/2 同为 device 3）、不同 DPI（800/1200/1600）、
// 自动值有效/探测失败+manual/离线无数据、partial coverage（source 1 含 unknown 桶）、
// 旧 schema（motionState.schemaReady=false → needs_upgrade，legacy 仍可读）、无数据（source 3）。
// 调试开关：改 motionState.schemaReady / motionState.failNextSetDpi 即可演练引导态与保存失败。

/** 米换算常数（1 英寸 = 0.0254 m，与后端 store 口径一致） */
const MOTION_METERS_PER_INCH = 0.0254;
/** 手动 DPI 合同上限（§6.1 schema CHECK 1..=100000） */
const MOTION_MANUAL_DPI_MAX = 100_000;

function localToday(): string {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
}

/** 来源 mock 记录：展示行由它按 §4.5 口径实时推导（connected/auto 需新鲜与未过期） */
interface MockMotionSource {
  id: number;
  deviceId: number;
  physical: boolean;
  /** 手动配置 DPI（set_mouse_dpi 的可变目标；null=未配置） */
  manualDpi: number | null;
  /** collector 发布的自动 DPI */
  autoDpi: number | null;
  /** 自动值失效时刻（绝对 ms；模块加载时给约 30 分钟有效期，过期即演示"回落 manual"） */
  autoUntilMs: number;
  /** collector 发布的在线证据（mock 心跳恒新鲜：发布即视为观测 ≤5 秒） */
  published: boolean;
  probeStatus: DpiProbeStatus;
  /** 运动桶剖面（dpi=0 为 unknown 桶；base=当日 counts 基准） */
  buckets: { dpi: number; base: number }[];
}

/** mock 内存的来源/配置状态（S7/S8 页面开发演练用） */
export const motionState: {
  /** false = 模拟旧 schema：sources 需求升级、运动查询全零、legacy 仍可读 */
  schemaReady: boolean;
  /** true = 下一次 setMouseDpi 失败（演示"失败保留编辑内容并展示错误"） */
  failNextSetDpi: boolean;
  sources: MockMotionSource[];
} = {
  schemaReady: true,
  failNextSetDpi: false,
  sources: [
    // id 1：同型号第一只——manual 800 + auto 1600 有效（available），含 unknown 桶 → partial coverage
    { id: 1, deviceId: 3, physical: true, manualDpi: 800, autoDpi: 1600, autoUntilMs: Date.now() + 30 * 60_000, published: true, probeStatus: "available", buckets: [{ dpi: 1600, base: 120_000 }, { dpi: 0, base: 20_000 }] },
    // id 2：同型号第二只（不同接口路径）——探测失败（unavailable），manual 1200 后备可离线编辑
    { id: 2, deviceId: 3, physical: true, manualDpi: 1200, autoDpi: null, autoUntilMs: 0, published: true, probeStatus: "unavailable", buckets: [{ dpi: 1200, base: 80_000 }] },
    // id 3：另一型号——离线、无配置、无运动桶 → 合法空（raw=0/meters=null/coverage=null）
    { id: 3, deviceId: 4, physical: true, manualDpi: null, autoDpi: null, autoUntilMs: 0, published: false, probeStatus: "disconnected", buckets: [] },
  ],
};

function motionSource(id: number): MockMotionSource {
  const s = motionState.sources.find((s) => s.id === id);
  if (!s) throw new Error(`未知鼠标运动来源 id: ${id}`);
  return s;
}

/** 展示行推导（§4.5 口径）：auto 有效要求在线且未过期；effective = auto > manual > null */
function displaySourceRow(s: MockMotionSource, now: number): MouseSourceRow {
  const row = profileOf(s.deviceId).row;
  const connected = s.published;
  const autoValid = connected && s.autoDpi !== null && s.autoUntilMs > now;
  const effectiveDpi = autoValid ? s.autoDpi : s.manualDpi;
  return {
    id: s.id,
    deviceId: s.deviceId,
    name: row.name,
    nickname: row.nickname,
    physical: s.physical,
    connected,
    manualDpi: s.manualDpi,
    autoDpi: autoValid ? s.autoDpi : null,
    autoValidUntil: autoValid ? new Date(s.autoUntilMs).toISOString() : null,
    effectiveDpi,
    dpiOrigin: autoValid ? "auto" : effectiveDpi !== null ? "manual" : "unknown",
    probeStatus: s.probeStatus,
  };
}

export async function mockGetMouseSources(): Promise<MouseSources> {
  await sleep(120);
  if (!motionState.schemaReady) {
    return { availability: "needs_upgrade", sources: [] };
  }
  const now = Date.now();
  return {
    availability: "ready",
    sources: motionState.sources.map((s) => displaySourceRow(s, now)),
  };
}

/** 单日桶剖面 → 当日行（factor 对历史日做确定性抖动；当日取满值便于观察近实时） */
function motionDay(s: MockMotionSource, day: string, today: string): MouseMotionDay {
  const factor = day === today ? 1 : 0.4 + rng(`motion:${s.id}:${day}`) * 0.6;
  let raw = 0;
  let configured = 0;
  let meters = 0;
  let unconfigured = 0;
  for (const b of s.buckets) {
    const counts = Math.round(b.base * factor * (0.85 + rng(`mb:${s.id}:${b.dpi}:${day}`) * 0.3));
    raw += counts;
    if (b.dpi > 0) {
      configured += counts;
      meters += counts / b.dpi * MOTION_METERS_PER_INCH;
    } else {
      unconfigured += counts;
    }
  }
  return {
    day,
    rawCounts: raw,
    meters: configured > 0 ? Math.round(meters * 1000) / 1000 : null,
    unconfiguredCounts: unconfigured,
  };
}

export async function mockGetMouseMotion(sourceId: number, from: string, to: string): Promise<MouseMotionSummary> {
  await sleep(100);
  const s = motionSource(sourceId); // 未知 sourceId 抛错（不静默切全部鼠标）
  if (!motionState.schemaReady) {
    return { availability: "needs_upgrade", sourceId, rawCounts: 0, meters: null, unconfiguredCounts: 0, coverage: null, days: [] };
  }
  const today = localToday();
  const days = rangeDays(from, to)
    .filter((day) => day <= today) // 未来的日无采集数据
    .map((day) => motionDay(s, day, today))
    .filter((d) => d.rawCounts > 0); // 只含有运动桶的日（与后端一致，无数据日的合法空）
  const rawCounts = days.reduce((acc, d) => acc + d.rawCounts, 0);
  const unconfiguredCounts = days.reduce((acc, d) => acc + d.unconfiguredCounts, 0);
  const configuredCounts = rawCounts - unconfiguredCounts;
  const meters = configuredCounts > 0
    ? Math.round(days.reduce((acc, d) => acc + (d.meters ?? 0), 0) * 1000) / 1000
    : null; // 没有任何已配置移动时 null——不用 0 米冒充（§4.5）
  return {
    availability: "ready",
    sourceId,
    rawCounts,
    meters,
    unconfiguredCounts,
    coverage: rawCounts > 0 ? configuredCounts / rawCounts : null,
    days,
  };
}

export async function mockSetMouseDpi(sourceId: number, dpi: number | null): Promise<MouseSourceRow> {
  await sleep(200);
  if (!motionState.schemaReady) throw new Error("运动配置表未就绪，请启动/更新采集器后再试");
  if (motionState.failNextSetDpi) {
    motionState.failNextSetDpi = false;
    throw new Error("模拟：保存手动 DPI 失败（编辑内容已保留，可重试）");
  }
  if (dpi !== null && (dpi < 1 || dpi > MOTION_MANUAL_DPI_MAX)) {
    throw new Error(`手动 DPI 超出合同范围（1..=${MOTION_MANUAL_DPI_MAX}）: ${dpi}`);
  }
  const s = motionSource(sourceId);
  if (!s.physical) throw new Error(`来源 ${sourceId} 为虚拟/未知桶，不支持配置 DPI`);
  const now = Date.now();
  if (s.published && s.autoDpi !== null && s.autoUntilMs > now) {
    throw new Error(`来源 ${sourceId} 自动 DPI 有效（${s.autoDpi}），手动配置只读；请等待自动值失效后再修改`);
  }
  s.manualDpi = dpi;
  return displaySourceRow(s, now);
}

/** 旧算法鼠标移动读数（旧表英寸×80 只还原原始量；与 mockMouseDistance 同一 inches fixture） */
export async function mockGetMouseLegacy(deviceId: number, from: string, to: string): Promise<LegacyMouseSummary> {
  await sleep(100);
  const profile = PROFILES.find((p) => p.row.id === deviceId);
  if (!profile) throw new Error(`未知设备 id: ${deviceId}`);
  if (profile.row.kind !== "mouse") throw new Error(`设备 ${deviceId} 不是鼠标，无旧版移动数据`);
  const today = localToday();
  const raw = rangeDays(from, to)
    .filter((day) => day <= today)
    .reduce((acc, day) => {
      const inches = 9000 * (0.7 + rng(`mousedist:${day}`) * 0.6);
      return acc + inches * 80; // 旧算法固定 80 counts/英寸（§6.2）
    }, 0);
  return { deviceId, rawCounts: Math.round(raw * 10) / 10, quality: "legacy_uncalibrated" };
}

/** 单侧摇杆停留剖面（归一化 625 权重：中心高斯 + 侧别热点；Σdwell == activeSeconds 的 mock 剖面） */
function stickProfile(side: "left" | "right"): number[] {
  const hotspots: [number, number][] = side === "left" ? [[11, 12], [13, 12]] : [[14, 13], [10, 12]];
  const w = new Array<number>(625).fill(0);
  for (let row = 0; row < 25; row++) {
    for (let col = 0; col < 25; col++) {
      const d = (col - 12) ** 2 + (row - 12) ** 2;
      let v = Math.exp(-d / 9);
      for (const [hr, hc] of hotspots) {
        const hd = (col - hc) ** 2 + (row - hr) ** 2;
        v += 0.7 * Math.exp(-hd / 2);
      }
      w[row * 25 + col] = v;
    }
  }
  const total = w.reduce((acc, v) => acc + v, 0);
  return w.map((v) => v / total);
}
const STICK_PROFILES: Record<"left" | "right", number[]> = {
  left: stickProfile("left"),
  right: stickProfile("right"),
};

function emptyStick(side: "left" | "right"): StickMotionSummary {
  return { side, activeSeconds: 0, travelR: 0, dwellSeconds: new Array<number>(625).fill(0) };
}

/** 手柄摇杆运动（§4.5：恰 625 格 row-major；仅 mock 手柄型号 device 5 有数据） */
export async function mockGetGamepadMotion(deviceId: number, from: string, to: string): Promise<GamepadMotionSummary> {
  await sleep(120);
  const profile = PROFILES.find((p) => p.row.id === deviceId);
  if (!profile) throw new Error(`未知设备 id: ${deviceId}`);
  if (profile.row.kind !== "gamepad") throw new Error(`设备 ${deviceId} 不是手柄，无摇杆运动数据`);
  const left = emptyStick("left");
  const right = emptyStick("right");
  if (!motionState.schemaReady) {
    return { availability: "needs_upgrade", deviceId, gridSize: 25, left, right };
  }
  const today = localToday();
  for (const side of ["left", "right"] as const) {
    const stick = side === "left" ? left : right;
    const prof = STICK_PROFILES[side];
    for (const day of rangeDays(from, to)) {
      if (day > today) continue; // 未来的日无数据（无数据场景）
      const active = (side === "left" ? 5400 : 1200)
        * (day === today ? 1 : 0.5 + rng(`gpm:${side}:${day}`) * 0.5)
        * weekendFactor(day);
      stick.activeSeconds += active;
      stick.travelR += active * (2.2 + rng(`gpt:${side}:${day}`) * 0.6);
      for (let i = 0; i < 625; i++) stick.dwellSeconds[i] += active * prof[i];
    }
    stick.activeSeconds = Math.round(stick.activeSeconds * 100) / 100;
    stick.travelR = Math.round(stick.travelR * 10) / 10;
    stick.dwellSeconds = stick.dwellSeconds.map((v) => Math.round(v * 1000) / 1000);
  }
  return { availability: "ready", deviceId, gridSize: 25, left, right };
}
