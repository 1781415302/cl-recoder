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
  taskEvidence: Evidence;
  health: CollectorHealth;
  pipeReachable: boolean;
  processEvidence: Evidence;
  diagnosticCode: CollectorDiagnosticCode;
  diagnosticMessage: string | null;
  paused?: boolean;
  startedAt?: string;
  lastEventAt: string | null;
}

/** 证据强度（usability-runtime-v3 §4.2：默认 unknown——不以默认 false 先显示未运行） */
export type Evidence = "present" | "absent" | "unknown";

/** 采集器健康分类（§4.2 纯分类规则表；unreachable=进程在但管道不可达，不等于已停止采集） */
export type CollectorHealth =
  | "running"
  | "paused"
  | "not_running"
  | "unreachable"
  | "access_denied"
  | "unknown";

/** 稳定诊断短码（诊断文字可改善，短码不改；probe_unavailable = 探测 worker 不可用） */
export type CollectorDiagnosticCode =
  | "pipe_not_found"
  | "pipe_access_denied"
  | "pipe_busy"
  | "pipe_timeout"
  | "pipe_io"
  | "pipe_protocol"
  | "probe_unavailable"
  | null;

/** 采集器自启任务三项策略只读查询（§4.3；appliesOnNextStart 固定 true——定义更新对后续启动生效） */
export interface TaskPolicy {
  evidence: Evidence;
  executionTimeLimit: string | null;
  disallowStartIfOnBatteries: boolean | null;
  stopIfGoingOnBatteries: boolean | null;
  compliant: boolean | null;
  appliesOnNextStart: true;
}

/** GUI 诊断日志状态（§4.1；仅描述 GUI sink，不承诺 collector sink 可写） */
export interface DiagnosticsInfo {
  logDirectory: string | null;
  guiLoggingAvailable: boolean;
  lastError: string | null;
  maxTotalBytes: number;
}

export interface MouseDistanceDay {
  day: string;
  distanceInches: number;
}

export interface MouseDistance {
  totalInches: number;
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
  /** 范围内应用前台秒数总和（非条目数量；按全部应用日行计算，不受 Top-N 截断影响） */
  appsTotal: number;
  mouseClicksTotal: number;
}

export interface WpKeyRow {
  day: string;
  /** Qt 键码（稳定身份：day:qtKey 唯一，同名不同 Qt 码不冲突） */
  qtKey: number;
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
  /** 源 path 原样（稳定身份：day:path 唯一，同名不同路径不冲突） */
  path: string;
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

/* ===== 运动命令（motion-dpi §4.5 逐字对齐；DTO camelCase 由 src-tauri serde 保证） ===== */

/** 运动查询可用性：needs_upgrade=库仍是旧 schema（组件旁引导"启动/更新采集器后可用"，不阻挡原按钮页） */
export type MotionAvailability = "ready" | "needs_upgrade";

/** DPI 取值来源（effectiveDpi 的出处；unknown=未配置，无换算） */
export type DpiOrigin = "auto" | "manual" | "unknown";

/** DPI 探测状态（unavailable=探测失败，业务降级；disconnected=来源已断连） */
export type DpiProbeStatus =
  | "pending"
  | "available"
  | "unsupported"
  | "ambiguous"
  | "unavailable"
  | "disconnected";

/** 鼠标运动来源行：id 是独立物理来源（同型号两只鼠标是两行）；deviceId 只供型号按钮查询 */
export interface MouseSourceRow {
  id: number;
  deviceId: number;
  name: string;
  /** 用户自定义昵称（可空；展示优先于 name） */
  nickname: string | null;
  /** 虚拟/未知桶为 false，禁配置/换算 DPI */
  physical: boolean;
  /** collector 发布的在线证据且观测 ≤5 秒，否则 false */
  connected: boolean;
  /** 手动配置 DPI（持久，可离线显示；仅用于之后采集，不回算历史） */
  manualDpi: number | null;
  /** 当前有效的自动 DPI（要求 connected 且未过期，否则 null） */
  autoDpi: number | null;
  /** 自动 DPI 失效时刻（RFC3339；仅与 autoDpi 同真） */
  autoValidUntil: string | null;
  /** 换算里程所用有效 DPI（auto 有效 > manual > null） */
  effectiveDpi: number | null;
  dpiOrigin: DpiOrigin;
  probeStatus: DpiProbeStatus;
}

export interface MouseSources {
  availability: MotionAvailability;
  sources: MouseSourceRow[];
}

export interface MouseMotionDay {
  day: string;
  rawCounts: number;
  /** 当日已配置部分折算米数；没有任何已配置移动时 null（不用 0 米冒充，UI 显示"未配置/暂无可换算数据"） */
  meters: number | null;
  unconfiguredCounts: number;
}

/** 单来源运动区间汇总；days 随 summary 返回（展开表复用它，不额外查询） */
export interface MouseMotionSummary {
  availability: MotionAvailability;
  sourceId: number;
  rawCounts: number;
  meters: number | null;
  unconfiguredCounts: number;
  /** 已配置 counts / 总 counts（total=0 时 null） */
  coverage: number | null;
  days: MouseMotionDay[];
}

/** 旧算法鼠标移动读数（旧表×80 只还原原始累计量，不换算米、不归物理来源） */
export interface LegacyMouseSummary {
  deviceId: number;
  rawCounts: number;
  quality: "legacy_uncalibrated";
}

/** 单侧摇杆运动汇总（dwellSeconds 恰 625、row-major；无数据全 0） */
export interface StickMotionSummary {
  side: "left" | "right";
  activeSeconds: number;
  travelR: number;
  dwellSeconds: number[];
}

export interface GamepadMotionSummary {
  availability: MotionAvailability;
  deviceId: number;
  gridSize: 25;
  left: StickMotionSummary;
  right: StickMotionSummary;
}
