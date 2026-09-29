// §4.7 契约 mock 数据（S11 开发用；S12 接真实 commands 后本文件仅作浏览器预览兜底）。
// 注意：聚合/TopN 都在本文件内完成——mock 扮演的是"后端 SQL"的角色，前端页面本身不做聚合（PLAN §2.5）。
import type {
  AppRowLabeled,
  CollectorStatus,
  ComboRowLabeled,
  DeviceRow,
  ExportReport,
  ImportReport,
  KeyDailyRowLabeled,
  Overview,
  Settings,
  SettingsPatch,
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
const GAMEPAD_LABELS: Record<number, string> = {
  1: "A (南)", 2: "B (东)", 3: "X (西)", 4: "Y (北)",
  5: "LB", 6: "LT", 7: "RB", 8: "RT",
  9: "Back", 10: "Start", 11: "Guide", 12: "LS 按下", 13: "RS 按下",
  14: "十字键 ↑", 15: "十字键 ↓", 16: "十字键 ←", 17: "十字键 →",
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

const APPS: { exe: string; name: string; secs: number; keys: number; clicks: number }[] = [
  { exe: "chrome.exe", name: "Google Chrome", secs: 11800, keys: 5200, clicks: 890 },
  { exe: "code.exe", name: "Visual Studio Code", secs: 9600, keys: 12800, clicks: 640 },
  { exe: "weixin.exe", name: "微信", secs: 5400, keys: 3800, clicks: 760 },
  { exe: "explorer.exe", name: "文件资源管理器", secs: 2100, keys: 420, clicks: 980 },
  { exe: "dota2.exe", name: "Dota 2", secs: 4700, keys: 900, clicks: 5400 },
  { exe: "devenv.exe", name: "Visual Studio 2022", secs: 3300, keys: 4100, clicks: 260 },
  { exe: "cmd.exe", name: "命令提示符", secs: 900, keys: 1500, clicks: 45 },
  { exe: "msedge.exe", name: "Microsoft Edge", secs: 2600, keys: 1200, clicks: 320 },
  { exe: "notepad.exe", name: "记事本", secs: 400, keys: 620, clicks: 30 },
  { exe: "steam.exe", name: "Steam", secs: 1200, keys: 300, clicks: 520 },
  { exe: "unknown", name: "未知应用", secs: 180, keys: 60, clicks: 40 },
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

/* ---------- WhatPulse（2025-04-01 ~ 昨天，约 1.5 年历史） ---------- */
const WP_START = "2025-04-01";
const WP_END = (() => { const d = new Date(); d.setDate(d.getDate() - 1); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`; })();
const WP_KEYS: [string, number][] = [
  ["Space", 9.2], ["E", 4.6], ["I", 3.4], ["Backspace", 3.1], ["A", 3.0], ["N", 2.9],
  ["T", 2.8], ["O", 2.6], ["S", 2.5], ["H", 2.3], ["R", 2.2], ["D", 2.0], ["L", 1.8],
  ["Enter", 1.7], ["U", 1.6], ["C", 1.5], ["M", 1.3], ["W", 1.2], ["F", 1.1], ["G", 1.0],
  ["Y", 0.95], ["P", 0.9], [",", 0.8], [".", 0.78], ["V", 0.6], ["Shift", 2.4],
  ["Ctrl", 1.4], ["B", 0.5], ["K", 0.45], ["1", 0.4],
];
function wpDayActive(day: string): boolean { return day >= WP_START && day <= WP_END; }
export function mockWpMeta(): WpMeta | null {
  return {
    importedAt: state.wpImportedAt,
    sourcePath: state.settings.wpDbPath ?? "C:\\Users\\17814\\AppData\\Local\\WhatPulse\\whatpulse.db",
    sourceSize: 218_364_928,
    dateMin: WP_START,
    dateMax: WP_END,
    note: "整体快照导入（重复导入即刷新）",
  };
}
export function mockWpOverview(from: string, to: string): WpOverview {
  const days = rangeDays(from, to).filter(wpDayActive);
  const perDay = (day: string) => Math.round(88000 * weekendFactor(day) * (0.8 + rng(`wp:${day}`) * 0.4));
  return {
    days: days.map((day) => ({ day, total: perDay(day) })),
    keysTotal: days.reduce((s, d) => s + perDay(d), 0),
    combosTotal: Math.round(days.length * 9600),
    appsTotal: days.length * APPS.length,
    mouseClicksTotal: Math.round(days.length * 5200),
  };
}
export function mockWpKeys(from: string, to: string, limit: number): WpKeyRow[] {
  const n = rangeDays(from, to).filter(wpDayActive).length;
  return WP_KEYS.slice(0, limit)
    .map(([label, dailyK]) => ({ day: "", label, count: Math.round(dailyK * 1_000_000 * (n / 545)) }))
    .sort((a, b) => b.count - a.count);
}
const WP_COMBOS: [string, string, number][] = [
  ["control,67", "Ctrl+C", 96_400], ["control,86", "Ctrl+V", 91_200], ["control,83", "Ctrl+S", 41_800],
  ["control,84", "Ctrl+T", 38_900], ["control,87", "Ctrl+W", 31_500], ["alt,9", "Alt+Tab", 28_700],
  ["control,90", "Ctrl+Z", 24_100], ["shift,16777248", "Shift+Space", 19_800],
  ["control,16777249", "Ctrl+Esc", 8_900], ["meta,68", "Win+D", 7_600],
];
export function mockWpCombos(from: string, to: string, limit: number): WpComboRow[] {
  const n = rangeDays(from, to).filter(wpDayActive).length;
  return WP_COMBOS.slice(0, limit)
    .map(([combo, label, daily]) => ({ day: "", combo, label, count: Math.round(daily * (n / 545)) }));
}
export function mockWpApps(from: string, to: string, limit: number): WpAppRow[] {
  const rows: WpAppRow[] = [];
  for (const day of rangeDays(from, to).filter(wpDayActive)) {
    for (const a of APPS) {
      const j = 0.75 + rng(`wpapp:${a.exe}:${day}`) * 0.5;
      rows.push({
        day, name: a.name,
        seconds: Math.round((a.secs / 30) * j),
        keys: Math.round((a.keys / 30) * j),
        clicks: Math.round((a.clicks / 30) * j),
      });
    }
  }
  rows.sort((a, b) => b.seconds - a.seconds);
  return rows.slice(0, limit);
}
export function mockWpMouse(from: string, to: string): WpMouseRow[] {
  return rangeDays(from, to).filter(wpDayActive).map((day) => {
    const clicks = Math.round(5200 * weekendFactor(day) * (0.8 + rng(`wpm:${day}`) * 0.4));
    const inches = 9000 * (0.7 + rng(`wpd:${day}`) * 0.6);
    return { day, clicks, distanceMeters: Math.round(inches * 0.0254 * 10) / 10 };
  });
}
export function mockWpMouseButtons(limit: number): WpMouseButtonRow[] {
  return [
    { label: "左键", total: 1_642_300 }, { label: "右键", total: 902_100 },
    { label: "中键", total: 118_400 }, { label: "其他", total: 21_950 },
  ].slice(0, limit);
}
export function mockWpMouseScrolls(limit: number): WpMouseScrollRow[] {
  return [
    { label: "向上", total: 412_600 }, { label: "向下", total: 398_200 },
    { label: "向左", total: 4_300 }, { label: "向右", total: 5_100 },
  ].slice(0, limit);
}

/* ---------- 有状态命令（采集器控制 / 设置 / 导入 / 导出） ---------- */
export const state: {
  collectorRunning: boolean;
  paused: boolean;
  startedAt: string;
  taskInstalled: boolean;
  wpImportedAt: string;
  settings: Settings;
} = {
  collectorRunning: true,
  paused: false,
  startedAt: new Date(Date.now() - 137 * 60_000).toISOString(),
  taskInstalled: true,
  wpImportedAt: new Date(Date.now() - 26 * 3600_000).toISOString(),
  settings: {
    guiAutostart: false,
    wpDbPath: "C:\\Users\\17814\\AppData\\Local\\WhatPulse\\whatpulse.db",
    firstRunDone: true,
  },
};

export async function mockCollectorStatus(): Promise<CollectorStatus> {
  await sleep(120);
  return state.collectorRunning
    ? {
        running: true,
        taskExists: state.taskInstalled,
        paused: state.paused,
        startedAt: state.startedAt,
        lastEventAt: new Date(Date.now() - 2_000).toISOString(),
      }
    : { running: false, taskExists: state.taskInstalled, lastEventAt: null };
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
export async function mockMouseDistance(_from: string, _to: string): Promise<import("./types").MouseDistance> {
  await sleep(100);
  return {
    total_inches: 12345.6,
    days: [
      { day: "2026-09-27", distance_inches: 6000.2 },
      { day: "2026-09-28", distance_inches: 6345.4 },
    ],
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
