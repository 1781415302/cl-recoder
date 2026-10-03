// 设备参考物理布局纯数据（usability-runtime-v3 §4.8 U4）——参考模板 + 区内 roving 导航 + 未绘制行过滤。
//
// 边界（§2.1/§4.8）：纯模块，无 DOM/API/时钟依赖（零运行时 import，由测试锚定转译产物）；
// 布局是本轮固定的参考模板（keyboard=ANSI-104 四区 / mouse=中性 9 控件 / gamepad=Xbox 17 控件），
// 标准标签不声称识别实际键盘外观/语言；kind 内 code 唯一（计数只按 code 查找，不 SUM，缺行=0）；
// 位置单位由模板定义（1u = 一个标准键位，组件按 44px/u 渲染），允许分数，单控件 ≥1×1u。
import type { TopKeyRow } from "../api/types";

/** 设备种类（与 api/types.ts DeviceRow["kind"]、Rust codes::DeviceKind serde 小写逐字对齐） */
export type DeviceKind = "keyboard" | "mouse" | "gamepad";

/** 单个控件（键帽/鼠标控件/手柄按键）：code 是查询行的直接查找键；(x,y,width,height) 为区内 u 坐标 */
export interface DeviceControlSpec {
  id: string;
  code: number;
  shortLabel: string;
  zone: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** 布局分区（每区一个 Tab 入口；width/height 为该区的 u 尺寸） */
export interface DeviceZoneSpec {
  id: string;
  label: string;
  width: number;
  height: number;
}

/** 一种设备的完整参考布局（zones/controls 只读，运行时冻结） */
export interface DeviceLayoutSpec {
  kind: DeviceKind;
  zones: readonly DeviceZoneSpec[];
  controls: readonly DeviceControlSpec[];
}

/** 区内导航键（§4.8：方向键按半平面最近距离移动，同距按模板顺序；Home/End 仅当前区首末） */
export type NavigationKey = "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Home" | "End";

/** 控件构造（width/height 缺省 1u = 一个标准键位；键盘 scancode 与 core::codes 归一化口径一致） */
function ctrl(
  id: string,
  code: number,
  shortLabel: string,
  zone: string,
  x: number,
  y: number,
  width = 1,
  height = 1,
): DeviceControlSpec {
  return { id, code, shortLabel, zone, x, y, width, height };
}

/** ANSI-104 参考键盘：功能 13 + 主键 61 + 导航 13（含 PrintScreen/ScrollLock/Pause）+ 数字 17 = 104；
 *  区内形状/行序忠于参考布局（宽键 2u/1.5u/1.75u/2.25u/2.75u/6.25u/1.25u，+ 与 Enter 竖跨 2u、0 横跨 2u）。 */
const KEYBOARD: DeviceLayoutSpec = {
  kind: "keyboard",
  zones: [
    { id: "function", label: "功能键区", width: 13, height: 1 },
    { id: "main", label: "主键区", width: 15, height: 5 },
    { id: "nav", label: "编辑导航区", width: 3, height: 5 },
    { id: "pad", label: "数字键区", width: 4, height: 5 },
  ],
  controls: [
    // —— 功能键区（Esc + F1–F12，13）——
    ctrl("esc", 0x01, "Esc", "function", 0, 0),
    ctrl("f1", 0x3b, "F1", "function", 1, 0),
    ctrl("f2", 0x3c, "F2", "function", 2, 0),
    ctrl("f3", 0x3d, "F3", "function", 3, 0),
    ctrl("f4", 0x3e, "F4", "function", 4, 0),
    ctrl("f5", 0x3f, "F5", "function", 5, 0),
    ctrl("f6", 0x40, "F6", "function", 6, 0),
    ctrl("f7", 0x41, "F7", "function", 7, 0),
    ctrl("f8", 0x42, "F8", "function", 8, 0),
    ctrl("f9", 0x43, "F9", "function", 9, 0),
    ctrl("f10", 0x44, "F10", "function", 10, 0),
    ctrl("f11", 0x57, "F11", "function", 11, 0),
    ctrl("f12", 0x58, "F12", "function", 12, 0),
    // —— 主键区（61）：行 1 数字排 ——
    ctrl("backquote", 0x29, "`", "main", 0, 0),
    ctrl("1", 0x02, "1", "main", 1, 0),
    ctrl("2", 0x03, "2", "main", 2, 0),
    ctrl("3", 0x04, "3", "main", 3, 0),
    ctrl("4", 0x05, "4", "main", 4, 0),
    ctrl("5", 0x06, "5", "main", 5, 0),
    ctrl("6", 0x07, "6", "main", 6, 0),
    ctrl("7", 0x08, "7", "main", 7, 0),
    ctrl("8", 0x09, "8", "main", 8, 0),
    ctrl("9", 0x0a, "9", "main", 9, 0),
    ctrl("0", 0x0b, "0", "main", 10, 0),
    ctrl("minus", 0x0c, "-", "main", 11, 0),
    ctrl("equal", 0x0d, "=", "main", 12, 0),
    ctrl("backspace", 0x0e, "Backspace", "main", 13, 0, 2, 1),
    // 行 2 QWERTY 上排
    ctrl("tab", 0x0f, "Tab", "main", 0, 1, 1.5, 1),
    ctrl("q", 0x10, "Q", "main", 1.5, 1),
    ctrl("w", 0x11, "W", "main", 2.5, 1),
    ctrl("e", 0x12, "E", "main", 3.5, 1),
    ctrl("r", 0x13, "R", "main", 4.5, 1),
    ctrl("t", 0x14, "T", "main", 5.5, 1),
    ctrl("y", 0x15, "Y", "main", 6.5, 1),
    ctrl("u", 0x16, "U", "main", 7.5, 1),
    ctrl("i", 0x17, "I", "main", 8.5, 1),
    ctrl("o", 0x18, "O", "main", 9.5, 1),
    ctrl("p", 0x19, "P", "main", 10.5, 1),
    ctrl("bracket-left", 0x1a, "[", "main", 11.5, 1),
    ctrl("bracket-right", 0x1b, "]", "main", 12.5, 1),
    ctrl("backslash", 0x2b, "\\", "main", 13.5, 1, 1.5, 1),
    // 行 3 home 排
    ctrl("caps-lock", 0x3a, "Caps Lock", "main", 0, 2, 1.75, 1),
    ctrl("a", 0x1e, "A", "main", 1.75, 2),
    ctrl("s", 0x1f, "S", "main", 2.75, 2),
    ctrl("d", 0x20, "D", "main", 3.75, 2),
    ctrl("f", 0x21, "F", "main", 4.75, 2),
    ctrl("g", 0x22, "G", "main", 5.75, 2),
    ctrl("h", 0x23, "H", "main", 6.75, 2),
    ctrl("j", 0x24, "J", "main", 7.75, 2),
    ctrl("k", 0x25, "K", "main", 8.75, 2),
    ctrl("l", 0x26, "L", "main", 9.75, 2),
    ctrl("semicolon", 0x27, ";", "main", 10.75, 2),
    ctrl("quote", 0x28, "'", "main", 11.75, 2),
    ctrl("enter", 0x1c, "Enter", "main", 12.75, 2, 2.25, 1),
    // 行 4 ZXCVB 排
    ctrl("l-shift", 0x2a, "Shift", "main", 0, 3, 2.25, 1),
    ctrl("z", 0x2c, "Z", "main", 2.25, 3),
    ctrl("x", 0x2d, "X", "main", 3.25, 3),
    ctrl("c", 0x2e, "C", "main", 4.25, 3),
    ctrl("v", 0x2f, "V", "main", 5.25, 3),
    ctrl("b", 0x30, "B", "main", 6.25, 3),
    ctrl("n", 0x31, "N", "main", 7.25, 3),
    ctrl("m", 0x32, "M", "main", 8.25, 3),
    ctrl("comma", 0x33, ",", "main", 9.25, 3),
    ctrl("period", 0x34, ".", "main", 10.25, 3),
    ctrl("slash", 0x35, "/", "main", 11.25, 3),
    ctrl("r-shift", 0x36, "Shift", "main", 12.25, 3, 2.75, 1),
    // 行 5 底排修饰键
    ctrl("l-ctrl", 0x1d, "Ctrl", "main", 0, 4, 1.25, 1),
    ctrl("l-win", 0xe05b, "Win", "main", 1.25, 4, 1.25, 1),
    ctrl("l-alt", 0x38, "Alt", "main", 2.5, 4, 1.25, 1),
    ctrl("space", 0x39, "Space", "main", 3.75, 4, 6.25, 1),
    ctrl("r-alt", 0xe038, "Alt", "main", 10, 4, 1.25, 1),
    ctrl("r-win", 0xe05c, "Win", "main", 11.25, 4, 1.25, 1),
    ctrl("menu", 0xe05d, "Menu", "main", 12.5, 4, 1.25, 1),
    ctrl("r-ctrl", 0xe01d, "Ctrl", "main", 13.75, 4, 1.25, 1),
    // —— 编辑导航区（13，含 PrintScreen/ScrollLock/Pause）——
    ctrl("print-screen", 0xe037, "Print Screen", "nav", 0, 0),
    ctrl("scroll-lock", 0x46, "Scroll Lock", "nav", 1, 0),
    ctrl("pause", 0xe11d, "Pause", "nav", 2, 0),
    ctrl("insert", 0xe052, "Insert", "nav", 0, 1),
    ctrl("home", 0xe047, "Home", "nav", 1, 1),
    ctrl("page-up", 0xe049, "Page Up", "nav", 2, 1),
    ctrl("delete", 0xe053, "Delete", "nav", 0, 2),
    ctrl("end", 0xe04f, "End", "nav", 1, 2),
    ctrl("page-down", 0xe051, "Page Down", "nav", 2, 2),
    ctrl("up", 0xe048, "↑", "nav", 1, 3),
    ctrl("left", 0xe04b, "←", "nav", 0, 4),
    ctrl("down", 0xe050, "↓", "nav", 1, 4),
    ctrl("right", 0xe04d, "→", "nav", 2, 4),
    // —— 数字键区（17）：+ 与 Enter 竖跨 2u，0 横跨 2u ——
    ctrl("num-lock", 0x45, "Num Lock", "pad", 0, 0),
    ctrl("pad-slash", 0xe035, "/", "pad", 1, 0),
    ctrl("pad-asterisk", 0x37, "*", "pad", 2, 0),
    ctrl("pad-minus", 0x4a, "-", "pad", 3, 0),
    ctrl("pad-7", 0x47, "7", "pad", 0, 1),
    ctrl("pad-8", 0x48, "8", "pad", 1, 1),
    ctrl("pad-9", 0x49, "9", "pad", 2, 1),
    ctrl("pad-plus", 0x4e, "+", "pad", 3, 1, 1, 2),
    ctrl("pad-4", 0x4b, "4", "pad", 0, 2),
    ctrl("pad-5", 0x4c, "5", "pad", 1, 2),
    ctrl("pad-6", 0x4d, "6", "pad", 2, 2),
    ctrl("pad-1", 0x4f, "1", "pad", 0, 3),
    ctrl("pad-2", 0x50, "2", "pad", 1, 3),
    ctrl("pad-3", 0x51, "3", "pad", 2, 3),
    ctrl("pad-enter", 0xe01c, "Enter", "pad", 3, 3, 1, 2),
    ctrl("pad-0", 0x52, "0", "pad", 0, 4, 2, 1),
    ctrl("pad-dot", 0x53, ".", "pad", 2, 4),
  ],
};

/** 中性参考鼠标：9 个独立控件——左/右/中、X1/X2 与四向滚轮各自成项（按下与滚动不叠成同项）。 */
const MOUSE: DeviceLayoutSpec = {
  kind: "mouse",
  zones: [{ id: "body", label: "鼠标按键与滚轮", width: 5, height: 7.5 }],
  controls: [
    ctrl("left", 1, "左键", "body", 0, 0, 2, 2),
    ctrl("right", 2, "右键", "body", 3, 0, 2, 2),
    ctrl("wheel-up", 6, "滚轮上", "body", 2, 0, 1, 1.5),
    ctrl("middle", 3, "中键", "body", 2, 1.5, 1, 1.5),
    ctrl("wheel-down", 7, "滚轮下", "body", 2, 3, 1, 1.5),
    ctrl("x1", 4, "X1", "body", 0, 2.5, 1.5, 1.5),
    ctrl("x2", 5, "X2", "body", 0, 4, 1.5, 1.5),
    ctrl("wheel-left", 8, "滚轮左", "body", 1.25, 5.75, 1.25, 1.25),
    ctrl("wheel-right", 9, "滚轮右", "body", 2.75, 5.75, 1.25, 1.25),
  ],
};

/** Xbox 参考手柄：17 控件完整——Y 上/X 左/B 右/A 下；LB/LT/RB/RT 四个分离位置；
 *  双摇杆按下（LS/RS）、十字四向、View/Menu/Guide 明确（Guide 计 0 属后端边界、不是故障）。 */
const GAMEPAD: DeviceLayoutSpec = {
  kind: "gamepad",
  zones: [{ id: "body", label: "手柄按键", width: 10, height: 6.5 }],
  controls: [
    ctrl("lt", 6, "LT", "body", 1.5, 0, 1.75, 1),
    ctrl("rt", 8, "RT", "body", 6.75, 0, 1.75, 1),
    ctrl("lb", 5, "LB", "body", 1.25, 1, 2, 1),
    ctrl("rb", 7, "RB", "body", 6.75, 1, 2, 1),
    ctrl("view", 9, "View", "body", 3.5, 1),
    ctrl("menu", 10, "Menu", "body", 5.5, 1),
    ctrl("guide", 11, "Guide", "body", 4.5, 1),
    ctrl("ls", 12, "LS", "body", 0.5, 2, 1, 1),
    ctrl("rs", 13, "RS", "body", 8.25, 2.25, 1.25, 1.25),
    ctrl("dpad-up", 14, "十字上", "body", 1.5, 3),
    ctrl("dpad-down", 15, "十字下", "body", 1.5, 5),
    ctrl("dpad-left", 16, "十字左", "body", 0.5, 4),
    ctrl("dpad-right", 17, "十字右", "body", 2.5, 4),
    ctrl("north", 3, "Y", "body", 7, 3),
    ctrl("east", 2, "B", "body", 8, 4),
    ctrl("west", 4, "X", "body", 6, 4),
    ctrl("south", 1, "A", "body", 7, 5),
  ],
};

/** 深冻结：布局是共享纯数据，杜绝被渲染层意外改写 */
function freezeLayout(spec: DeviceLayoutSpec): DeviceLayoutSpec {
  for (const zone of spec.zones) Object.freeze(zone);
  Object.freeze(spec.zones);
  for (const control of spec.controls) Object.freeze(control);
  Object.freeze(spec.controls);
  return Object.freeze(spec);
}

const LAYOUTS: Record<DeviceKind, DeviceLayoutSpec> = {
  keyboard: freezeLayout(KEYBOARD),
  mouse: freezeLayout(MOUSE),
  gamepad: freezeLayout(GAMEPAD),
};

/** 模板查找：同一 kind 恒返回同一冻结模板（布局纯数据只有 keyboard/mouse/gamepad，kind/code 唯一） */
export function getDeviceLayout(kind: DeviceKind): DeviceLayoutSpec {
  const layout = LAYOUTS[kind];
  if (!layout) throw new Error(`未知设备种类: ${String(kind)}`);
  return layout;
}

/** 区内 roving 导航（纯几何，无 DOM/API 依赖）：
 *  - currentId 不存在 → 返回模板第一个控件；
 *  - Home/End → 仅当前区（模板顺序内）首/末控件；
 *  - 方向键 → 同区内、方向半平面上（严格前进）的候选中按控件几何中心最近距离选取，
 *    同距取模板顺序先到者（平方距离比较 + 严格小于 = 先到者优先）；无候选保持 currentId。 */
export function nextControlId(layout: DeviceLayoutSpec, currentId: string, key: NavigationKey): string {
  const controls = layout.controls;
  const current = controls.find((c) => c.id === currentId);
  if (!current) return controls[0].id;
  const zoneControls = controls.filter((c) => c.zone === current.zone);
  if (key === "Home") return zoneControls[0].id;
  if (key === "End") return zoneControls[zoneControls.length - 1].id;
  const centerX = current.x + current.width / 2;
  const centerY = current.y + current.height / 2;
  let best: DeviceControlSpec | null = null;
  let bestDist = Infinity;
  for (const c of zoneControls) {
    if (c.id === currentId) continue;
    const dx = c.x + c.width / 2 - centerX;
    const dy = c.y + c.height / 2 - centerY;
    if (key === "ArrowLeft" && dx >= 0) continue;
    if (key === "ArrowRight" && dx <= 0) continue;
    if (key === "ArrowUp" && dy >= 0) continue;
    if (key === "ArrowDown" && dy <= 0) continue;
    const dist = dx * dx + dy * dy;
    if (dist < bestDist) {
      best = c;
      bestDist = dist;
    }
  }
  return best ? best.id : currentId;
}

/** 参考布局未绘制位置的输入行（如键盘媒体键、超值域未知码）：保持输入行顺序原样返回，
 *  供“其它输入”区与完整表保留展示（不能只显示布局内控件）；不修改入参数组。 */
export function getUnmappedRows(kind: DeviceKind, rows: readonly TopKeyRow[]): TopKeyRow[] {
  const mapped = new Set(getDeviceLayout(kind).controls.map((c) => c.code));
  return rows.filter((r) => !mapped.has(r.code));
}
