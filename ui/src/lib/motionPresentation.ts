// 运动展示纯函数与控件几何（motion-dpi §4.6 S7）——热力归一化 / 里程与 DPI 文案 / 鼠标·手柄控件几何。
//
// 边界：纯模块，零运行时 import（type 擦除后产物无 import/require/DOM/时钟，由 motion-presentation 测试锚定，
// 同 deviceLayouts.ts 惯例）；只做展示层换算，不查询 API；控件几何是本轮固定的参考模板
// （mouse=9 控件、gamepad=Xbox 17 控件），kind 内 code 唯一（计数由组件按 code 查找，不 SUM、缺行=0）。

/** 热力网格边长（25×25 = 625 格；与 crates/engine stick_motion 的 bin=row×25+column 口径逐字一致） */
const STICK_GRID = 25;

/** 边界格只要与单位圆相交就能承载满幅摇杆采样，不能仅看格中心。 */
export function heatCellIntersectsDisc(row: number, col: number): boolean {
  const dx = Math.max(0, Math.abs(col + 0.5 - STICK_GRID / 2) - 0.5);
  const dy = Math.max(0, Math.abs(row + 0.5 - STICK_GRID / 2) - 0.5);
  return Math.hypot(dx, dy) <= STICK_GRID / 2 + 1e-9;
}

/** 热力单格（§4.6）：bin 为 0..624 行主序格号；seconds 原值透传（仅图形归一化，不改数值） */
export interface HeatCell {
  bin: number;
  seconds: number;
  /** 格中心（格单位）：x 向右增、y 向下增；行 0 为摇杆前推（圆盘顶部）——"圆盘 y 向上"由渲染层把行 0 画在顶部实现 */
  x: number;
  y: number;
  /** 显示强度 = sqrt(seconds/scaleMaxSeconds)，clamp 到 [0,1]；scaleMaxSeconds<=0 或秒数非正/非有限恒 0 */
  intensity: number;
}

/** 热力格序列（§4.6）：输入恰 625 个秒数与页面两侧共同最大值，输出恰 625 个单元。
 *  秒数原值透传不做任何换算/排序/缩放（仅 intensity 参与图形）；输入不是 625 格属于上游合同错误——
 *  抛中文错误由测试捕获，绝不补造数据填充。 */
export function heatCells(
  dwellSeconds: readonly number[],
  scaleMaxSeconds: number,
): HeatCell[] {
  if (dwellSeconds.length !== STICK_GRID * STICK_GRID) {
    throw new Error(
      `热力数据必须恰好 ${STICK_GRID * STICK_GRID} 格（25×25 行主序），实际 ${dwellSeconds.length} 格——上游合同错误，不补造数据`,
    );
  }
  const scale =
    typeof scaleMaxSeconds === "number" &&
    Number.isFinite(scaleMaxSeconds) &&
    scaleMaxSeconds > 0
      ? scaleMaxSeconds
      : 0;
  const cells: HeatCell[] = [];
  for (let bin = 0; bin < dwellSeconds.length; bin++) {
    const seconds = dwellSeconds[bin];
    const col = bin % STICK_GRID;
    const row = Math.floor(bin / STICK_GRID);
    let intensity = 0;
    if (
      scale > 0 &&
      typeof seconds === "number" &&
      Number.isFinite(seconds) &&
      seconds > 0
    ) {
      intensity = Math.min(1, Math.sqrt(seconds / scale));
    }
    cells.push({ bin, seconds, x: col + 0.5, y: row + 0.5, intensity });
  }
  return cells;
}

/* 旧颜色接口保留兼容；当前圆盘使用 theme.css 的 heat 色带。 */
const TEAL_DIM = [13, 148, 136];
const TEAL_BRIGHT = [20, 184, 166];

/** 热力单格颜色（canvas fillStyle 用）：低强度 = 深青绿低不透明度，高强度 = 明亮青绿近实心；
 *  非有限值/越界收敛到 [0,1]。返回稳定格式 "rgba(r,g,b,a)"。 */
export function heatCellColor(intensity: number): string {
  const t = Number.isFinite(intensity)
    ? Math.min(1, Math.max(0, intensity))
    : 0;
  const r = Math.round(TEAL_DIM[0] + (TEAL_BRIGHT[0] - TEAL_DIM[0]) * t);
  const g = Math.round(TEAL_DIM[1] + (TEAL_BRIGHT[1] - TEAL_DIM[1]) * t);
  const b = Math.round(TEAL_DIM[2] + (TEAL_BRIGHT[2] - TEAL_DIM[2]) * t);
  const a = Math.round((0.16 + 0.84 * t) * 100) / 100;
  return `rgba(${r},${g},${b},${a})`;
}

/** 累计行程格式化（§4.6）：千分位＋最多 2 小数＋" R"；负数/非有限值按 0。 */
export function formatTravelR(value: number): string {
  const v = Number.isFinite(value) && value > 0 ? value : 0;
  return `${new Intl.NumberFormat("zh-Hans-CN", { maximumFractionDigits: 2 }).format(v)} R`;
}

/** 估算距离格式化（§4.6）：<1m 显示 cm、≥1m 显示 m、≥1000m 显示 km，最多 3 位小数；
 *  null/非有限值显示"—"（未配置/暂无可换算数据，不用 0 米冒充）。 */
export function formatMouseDistance(meters: number | null): string {
  if (meters === null || !Number.isFinite(meters)) return "—";
  const fmt = (v: number, unit: string): string =>
    `${new Intl.NumberFormat("zh-Hans-CN", { maximumFractionDigits: 3 }).format(v)} ${unit}`;
  if (meters < 1) return fmt(meters * 100, "cm");
  if (meters < 1000) return fmt(meters, "m");
  return fmt(meters / 1000, "km");
}

/** 手动 DPI 合同范围（§4.6/§6.1 schema CHECK：仅十进制整数 1..100000） */
export const MANUAL_DPI_MIN = 1;
export const MANUAL_DPI_MAX = 100000;

/** 手动 DPI 解析（§4.6）：仅十进制整数——无小数/负号/科学计数法（也拒绝全角与阿拉伯数字等非 [0-9] 形态），
 *  范围 1..100000；首尾空白容忍，前导零按数值归一。错误消息为中文，区分形态错误与范围错误。 */
export function parseManualDpi(
  text: string,
): { ok: true; dpi: number } | { ok: false; message: string } {
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    return {
      ok: false,
      message: `请输入 ${MANUAL_DPI_MIN}–${MANUAL_DPI_MAX} 之间的整数 DPI`,
    };
  }
  if (!/^\d+$/.test(trimmed)) {
    return {
      ok: false,
      message: "DPI 必须是十进制整数：不能含小数点、负号或科学计数法",
    };
  }
  const dpi = Number(trimmed);
  if (
    !Number.isSafeInteger(dpi) ||
    dpi < MANUAL_DPI_MIN ||
    dpi > MANUAL_DPI_MAX
  ) {
    return {
      ok: false,
      message: `DPI 需在 ${MANUAL_DPI_MIN}–${MANUAL_DPI_MAX} 之间`,
    };
  }
  return { ok: true, dpi };
}

/** 选中热力格的精确停留时长（§4.6"精确停留时间"）：千分位＋最多 2 位小数＋"秒"；非有限/负值按 0。 */
export function formatCellDwell(seconds: number): string {
  const v = Number.isFinite(seconds) && seconds > 0 ? seconds : 0;
  return `${new Intl.NumberFormat("zh-Hans-CN", { maximumFractionDigits: 2 }).format(v)} 秒`;
}

/** 选中热力格占活动时间的比例（§4.6）：最多 1 位小数的百分比并 clamp 到 [0,100]；
 *  activeSeconds<=0（无活动）或秒数非有限时返回"—"。 */
export function dwellShareText(seconds: number, activeSeconds: number): string {
  if (
    !Number.isFinite(seconds) ||
    !Number.isFinite(activeSeconds) ||
    activeSeconds <= 0
  )
    return "—";
  const pct = Math.min(100, Math.max(0, (seconds / activeSeconds) * 100));
  return `${new Intl.NumberFormat("zh-Hans-CN", { maximumFractionDigits: 1 }).format(pct)}%`;
}

/* ===== 鼠标/手柄控件参考几何（PeripheralControlsStats 专用） =====
 * 视觉可小于 44px，但实际点击区域（hit）≥44×44 且两两不重叠（§4.6 合同定值，测试钉死）；
 * 坐标为组件画布像素（MOTION_CANVAS），组件以固定尺寸渲染 + 容器横向滚动，不缩放（保证 hit 不缩水）。 */

/** 展示控件形状（组件按形状绘制本地 SVG：panel=面板/肩键扳机，circle=圆钮，pill=小胶囊键，arrow=方向三角） */
export type MotionControlShape = "panel" | "circle" | "pill" | "arrow";

export type MotionDirection = "up" | "down" | "left" | "right";

export interface MotionControlSpec {
  code: number;
  shortLabel: string;
  shape: MotionControlShape;
  /** 方向（shape 为 arrow 时给出） */
  dir?: MotionDirection;
  /** 视觉中心（画布 px；circle 时 w=h=直径，arrow 时为三角外接框） */
  cx: number;
  cy: number;
  w: number;
  h: number;
  /** 实际点击区域（≥44×44、两两不重叠；可比视觉略大以吸收边距） */
  hit: { x: number; y: number; w: number; h: number };
}

export interface MotionCanvasSize {
  width: number;
  height: number;
}

/** 控件图画布尺寸（px；组件固定尺寸渲染，窄容器横向滚动） */
export const MOTION_CANVAS: Record<"mouse" | "gamepad", MotionCanvasSize> = {
  mouse: { width: 320, height: 360 },
  gamepad: { width: 620, height: 340 },
};

/** 实际点击区域单边下限（§4.6 合同定值） */
export const MOTION_HIT_MIN_PX = 44;

/** 中性参考鼠标：9 控件与既有码表一致——左右大面板、中键/滚轮轴居中、X1/X2 沿左侧、
 *  四个滚动方向各自成箭头控件（按下与滚动不合计）。 */
const MOUSE_CONTROLS: MotionControlSpec[] = [
  {
    code: 1,
    shortLabel: "左键",
    shape: "panel",
    cx: 86,
    cy: 100,
    w: 92,
    h: 148,
    hit: { x: 40, y: 26, w: 92, h: 148 },
  },
  {
    code: 2,
    shortLabel: "右键",
    shape: "panel",
    cx: 234,
    cy: 100,
    w: 92,
    h: 148,
    hit: { x: 188, y: 26, w: 92, h: 148 },
  },
  {
    code: 3,
    shortLabel: "中键",
    shape: "pill",
    cx: 160,
    cy: 102,
    w: 40,
    h: 44,
    hit: { x: 138, y: 80, w: 44, h: 44 },
  },
  {
    code: 4,
    shortLabel: "X1",
    shape: "pill",
    cx: 48,
    cy: 226,
    w: 40,
    h: 40,
    hit: { x: 26, y: 204, w: 44, h: 44 },
  },
  {
    code: 5,
    shortLabel: "X2",
    shape: "pill",
    cx: 48,
    cy: 278,
    w: 40,
    h: 40,
    hit: { x: 26, y: 256, w: 44, h: 44 },
  },
  {
    code: 6,
    shortLabel: "滚轮上",
    shape: "arrow",
    dir: "up",
    cx: 160,
    cy: 48,
    w: 18,
    h: 14,
    hit: { x: 138, y: 26, w: 44, h: 44 },
  },
  {
    code: 7,
    shortLabel: "滚轮下",
    shape: "arrow",
    dir: "down",
    cx: 160,
    cy: 150,
    w: 18,
    h: 14,
    hit: { x: 138, y: 128, w: 44, h: 44 },
  },
  {
    code: 8,
    shortLabel: "滚轮左",
    shape: "arrow",
    dir: "left",
    cx: 108,
    cy: 215,
    w: 14,
    h: 18,
    hit: { x: 86, y: 193, w: 44, h: 44 },
  },
  {
    code: 9,
    shortLabel: "滚轮右",
    shape: "arrow",
    dir: "right",
    cx: 212,
    cy: 215,
    w: 14,
    h: 18,
    hit: { x: 190, y: 193, w: 44, h: 44 },
  },
];

/** Xbox 的空间关系保留，控件与点击区域按紧凑仪器面板重新排布。 */
const GAMEPAD_CONTROLS: MotionControlSpec[] = [
  {
    code: 6,
    shortLabel: "LT",
    shape: "panel",
    cx: 140,
    cy: 26,
    w: 88,
    h: 30,
    hit: { x: 96, y: 4, w: 88, h: 44 },
  },
  {
    code: 8,
    shortLabel: "RT",
    shape: "panel",
    cx: 480,
    cy: 26,
    w: 88,
    h: 30,
    hit: { x: 436, y: 4, w: 88, h: 44 },
  },
  {
    code: 5,
    shortLabel: "LB",
    shape: "panel",
    cx: 140,
    cy: 76,
    w: 108,
    h: 34,
    hit: { x: 86, y: 54, w: 108, h: 44 },
  },
  {
    code: 7,
    shortLabel: "RB",
    shape: "panel",
    cx: 480,
    cy: 76,
    w: 108,
    h: 34,
    hit: { x: 426, y: 54, w: 108, h: 44 },
  },
  {
    code: 9,
    shortLabel: "View",
    shape: "circle",
    cx: 255,
    cy: 106,
    w: 30,
    h: 30,
    hit: { x: 233, y: 84, w: 44, h: 44 },
  },
  {
    code: 11,
    shortLabel: "Guide",
    shape: "circle",
    cx: 310,
    cy: 95,
    w: 38,
    h: 38,
    hit: { x: 288, y: 73, w: 44, h: 44 },
  },
  {
    code: 10,
    shortLabel: "Menu",
    shape: "circle",
    cx: 365,
    cy: 106,
    w: 30,
    h: 30,
    hit: { x: 343, y: 84, w: 44, h: 44 },
  },
  {
    code: 12,
    shortLabel: "LS",
    shape: "circle",
    cx: 130,
    cy: 178,
    w: 68,
    h: 68,
    hit: { x: 92, y: 140, w: 76, h: 76 },
  },
  {
    code: 13,
    shortLabel: "RS",
    shape: "circle",
    cx: 365,
    cy: 258,
    w: 64,
    h: 64,
    hit: { x: 329, y: 222, w: 72, h: 72 },
  },
  {
    code: 14,
    shortLabel: "十字上",
    shape: "arrow",
    dir: "up",
    cx: 235,
    cy: 184,
    w: 18,
    h: 14,
    hit: { x: 213, y: 162, w: 44, h: 44 },
  },
  {
    code: 15,
    shortLabel: "十字下",
    shape: "arrow",
    dir: "down",
    cx: 235,
    cy: 272,
    w: 18,
    h: 14,
    hit: { x: 213, y: 250, w: 44, h: 44 },
  },
  {
    code: 16,
    shortLabel: "十字左",
    shape: "arrow",
    dir: "left",
    cx: 191,
    cy: 228,
    w: 14,
    h: 18,
    hit: { x: 169, y: 206, w: 44, h: 44 },
  },
  {
    code: 17,
    shortLabel: "十字右",
    shape: "arrow",
    dir: "right",
    cx: 279,
    cy: 228,
    w: 14,
    h: 18,
    hit: { x: 257, y: 206, w: 44, h: 44 },
  },
  {
    code: 3,
    shortLabel: "Y",
    shape: "circle",
    cx: 485,
    cy: 137,
    w: 40,
    h: 40,
    hit: { x: 463, y: 115, w: 44, h: 44 },
  },
  {
    code: 4,
    shortLabel: "X",
    shape: "circle",
    cx: 437,
    cy: 185,
    w: 40,
    h: 40,
    hit: { x: 415, y: 163, w: 44, h: 44 },
  },
  {
    code: 2,
    shortLabel: "B",
    shape: "circle",
    cx: 533,
    cy: 185,
    w: 40,
    h: 40,
    hit: { x: 511, y: 163, w: 44, h: 44 },
  },
  {
    code: 1,
    shortLabel: "A",
    shape: "circle",
    cx: 485,
    cy: 233,
    w: 40,
    h: 40,
    hit: { x: 463, y: 211, w: 44, h: 44 },
  },
];

/** 深冻结：几何是共享纯数据，杜绝被渲染层意外改写（同 deviceLayouts 惯例） */
function freezeControls(
  controls: MotionControlSpec[],
): readonly MotionControlSpec[] {
  for (const c of controls) {
    Object.freeze(c.hit);
    Object.freeze(c);
  }
  Object.freeze(controls);
  return controls;
}

const CONTROL_LISTS: Record<"mouse" | "gamepad", readonly MotionControlSpec[]> =
  {
    mouse: freezeControls(MOUSE_CONTROLS),
    gamepad: freezeControls(GAMEPAD_CONTROLS),
  };

/** 控件几何模板查找：同一 kind 恒返回同一冻结模板（只有 mouse/gamepad 有控件图） */
export function motionControlSpecs(
  kind: "mouse" | "gamepad",
): readonly MotionControlSpec[] {
  const list = CONTROL_LISTS[kind];
  if (!list) throw new Error(`未知控件种类: ${String(kind)}`);
  return list;
}

/* ===== 鼠标来源选择类型（MouseSourcePicker 用；§4.6 原样） ===== */

export interface MouseModelRow {
  id: number;
  name: string;
  nickname: string | null;
}

export type MouseSelection =
  | { kind: "source"; id: number }
  | { kind: "model"; id: number };
