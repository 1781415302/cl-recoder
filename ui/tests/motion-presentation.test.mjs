// S7 回归（motion-dpi §4.6 展示组件自包含合同）：motion-presentation。
//
// 用 Node 内建 test 验证纯契约（无渲染——渲染层由 tsc --noEmit 与 S8 页面/Browser mock 验收覆盖）：
// - heatCells：恰 625 格输出、x/y 为格中心（行主序）、圆盘 y 向上（bin 12 正上在顶、bin 612 正下在底、
//   312 中心）、intensity=sqrt(seconds/scaleMaxSeconds) 且 clamp [0,1]、scale=0 全 0、
//   seconds 原值透传（不改原数据）、输出键形状钉死、缺/多 625 抛中文错误（不补造数据）；
// - 共同色标：强度只依赖 seconds/scaleMaxSeconds 比值（页面取左右 1250 值共同 max 传两图，不每侧独自缩放）；
// - heatCellColor：端点钉在主题青绿→明亮青绿（theme.css 令牌色）、g/alpha 随强度单调、越界/非有限收敛；
// - formatTravelR / formatMouseDistance / formatCellDwell / dwellShareText：§4.6 单位与精度逐例；
// - parseManualDpi：仅十进制整数（拒绝空/小数/负号/科学计数法/分组符/全角/阿拉伯数字），1..100000 越界拒绝；
// - 控件几何：mouse 9 码 / gamepad 17 码与既有码表一致（含 ABXY 方位、十字四臂、扳机/肩键、
//   双摇杆左右半区锚定），hit 全部 ≥44×44、两两不重叠、落在画布内、短标签唯一、模板冻结；
// - 组件模块冒烟：五个组件可加载（CJS 转译 + require 桩），导出组件函数，且只依赖 react/本地展示层——
//   不 require API 客户端/mock/queries/react-query（组件不查询 API）。
//
// 纯模块（../src/lib/motionPresentation.ts，零运行时 import）经 typescript.transpileModule 转为 ESM
// 后由 data URL 导入；组件（含 JSX）转 CommonJS 后以 new Function + require 桩执行模块顶层（不渲染）。
import assert from "node:assert/strict";
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

async function loadModule(relPath) {
  const src = await readFile(new URL(relPath, import.meta.url), "utf8");
  const js = ts.transpileModule(src, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2020 },
  }).outputText;
  return { js, module: await import("data:text/javascript;base64," + Buffer.from(js).toString("base64")) };
}

/** 组件模块：含 JSX 与 react import，转 CommonJS 后用 require 桩执行模块顶层（不渲染），
 *  并记录全部 require 说明符（纯度断言：组件不得查询 API）。 */
async function loadComponentModule(relPath) {
  const src = await readFile(new URL(relPath, import.meta.url), "utf8");
  const js = ts.transpileModule(src, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2020,
      jsx: ts.JsxEmit.React,
    },
  }).outputText;
  const required = [];
  const mod = { exports: {} };
  const fakeRequire = (spec) => {
    required.push(spec);
    return {}; // 组件体不执行：模块加载只需 require 可解析
  };
  new Function("require", "module", "exports", js)(fakeRequire, mod, mod.exports);
  return { exports: mod.exports, required };
}

/** 去掉注释后的转译产物（纯度断言用，避免注释里的词干扰） */
function stripComments(js) {
  return js.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

const { js: libJs, module: lib } = await loadModule("../src/lib/motionPresentation.ts");

test("满幅斜向采样的边缘格保留热度，圆外角格不绘制", () => {
  const x = Math.SQRT1_2;
  const col = Math.floor((x + 1) / 2 * 25);
  const row = Math.floor((1 - x) / 2 * 25);
  assert.ok(Math.hypot(col + 0.5 - 12.5, row + 0.5 - 12.5) > 12.5);
  assert.equal(lib.heatCellIntersectsDisc(row, col), true);
  assert.equal(lib.heatCellIntersectsDisc(0, 0), false);
});
const {
  heatCells,
  heatCellColor,
  formatTravelR,
  formatMouseDistance,
  formatCellDwell,
  dwellShareText,
  parseManualDpi,
  MANUAL_DPI_MIN,
  MANUAL_DPI_MAX,
  motionControlSpecs,
  MOTION_CANVAS,
  MOTION_HIT_MIN_PX,
} = lib;

/** 构造一份 625 格 dwell（仅 bin 处为 seconds，其余 0） */
function makeDwell(bin, seconds) {
  const dwell = new Array(625).fill(0);
  dwell[bin] = seconds;
  return dwell;
}

test("motionPresentation 纯模块：零运行时 import/require、无 DOM/时钟依赖", () => {
  const bare = stripComments(libJs);
  assert.doesNotMatch(bare, /\bimport\b/);
  assert.doesNotMatch(bare, /\brequire\s*\(/);
  assert.doesNotMatch(bare, /\bdocument\b|\bwindow\b/); // 无 DOM 依赖
  assert.doesNotMatch(bare, /\bnew\s+Date\b|\bDate\.now\b/); // 无时钟依赖
  assert.equal(typeof heatCells, "function");
  assert.equal(typeof formatTravelR, "function");
  assert.equal(typeof formatMouseDistance, "function");
  assert.equal(typeof parseManualDpi, "function");
});

test("heatCells：恰 625 格输出，x/y 为格中心（行主序），圆盘 y 向上（bin 12 顶、612 底、312 中心）", () => {
  const dwell = new Array(625).fill(0);
  dwell[12] = 5;
  dwell[312] = 10;
  dwell[612] = 2.5;
  const cells = heatCells(dwell, 10);
  assert.equal(cells.length, 625);
  // 行主序：bin=row×25+column，格中心 = (col+0.5, row+0.5)
  assert.deepEqual([cells[0].x, cells[0].y], [0.5, 0.5], "bin 0 = 左上");
  assert.deepEqual([cells[24].x, cells[24].y], [24.5, 0.5], "bin 24 = 右上");
  assert.deepEqual([cells[600].x, cells[600].y], [0.5, 24.5], "bin 600 = 左下");
  assert.deepEqual([cells[12].x, cells[12].y], [12.5, 0.5], "bin 12 = 正上（行 0，圆盘顶部）");
  assert.deepEqual([cells[312].x, cells[312].y], [12.5, 12.5], "bin 312 = 中心");
  assert.deepEqual([cells[612].x, cells[612].y], [12.5, 24.5], "bin 612 = 正下（行 24，圆盘底部）");
  // y 方向：行号小 = 圆盘上方（渲染层把行 0 画在顶部 = 摇杆前推方向）
  assert.ok(cells[12].y < cells[312].y && cells[312].y < cells[612].y, "上<中心<下");
  for (let row = 0; row < 25; row++) {
    for (let col = 0; col < 25; col++) {
      const c = cells[row * 25 + col];
      assert.equal(c.bin, row * 25 + col);
      assert.equal(c.x, col + 0.5);
      assert.equal(c.y, row + 0.5);
    }
  }
});

test("heatCells intensity：sqrt(seconds/scaleMax)、clamp ≤1、scale=0 全 0、seconds 原值透传（不改原数据）", () => {
  const dwell = new Array(625).fill(0);
  dwell[0] = 25;
  dwell[1] = 100;
  dwell[2] = 200;
  dwell[3] = 0.5;
  dwell[4] = -3;
  dwell[5] = NaN;
  const snapshot = JSON.parse(JSON.stringify(dwell.map((v) => (Number.isNaN(v) ? null : v))));
  const cells = heatCells(dwell, 100);
  assert.equal(cells[0].intensity, 0.5);
  assert.equal(cells[1].intensity, 1);
  assert.equal(cells[2].intensity, 1, "超出共同色标 clamp 到 1");
  assert.ok(Math.abs(cells[3].intensity - Math.sqrt(0.005)) < 1e-12);
  assert.equal(cells[4].intensity, 0, "负值不产生强度");
  assert.equal(cells[5].intensity, 0, "NaN 不产生强度");
  // 秒数原值透传（仅图形归一化，不修改数值）
  assert.equal(cells[2].seconds, 200);
  assert.equal(cells[4].seconds, -3);
  assert.ok(Number.isNaN(cells[5].seconds));
  // 输出键形状钉死（§4.6：bin/seconds/x/y/intensity）
  assert.deepEqual(Object.keys(cells[0]).sort(), ["bin", "intensity", "seconds", "x", "y"]);
  // 不修改入参数组（NaN 槽位经 null 快照比对）
  const roundTrip = dwell.map((v) => (Number.isNaN(v) ? null : v));
  assert.deepEqual(roundTrip, snapshot);
  // scaleMaxSeconds = 0（左右共同 max 为 0 / 非法）→ 强度全 0
  for (const scale of [0, -5, NaN]) {
    const zeros = heatCells(makeDwell(312, 42), scale);
    assert.equal(zeros.length, 625);
    assert.ok(zeros.every((c) => c.intensity === 0), `scale=${scale} 时强度全 0`);
  }
});

test("共同色标：强度只依赖 seconds/scaleMaxSeconds（跨图同值同强度，共同 max 一致缩放）", () => {
  // 不同格位、同秒数、同 scaleMaxSeconds → 同强度（页面用左右共同 max，不每侧独自缩放）
  const a = heatCells(makeDwell(100, 90), 180)[100].intensity;
  const b = heatCells(makeDwell(500, 90), 180)[500].intensity;
  assert.equal(a, b);
  assert.ok(Math.abs(a - Math.sqrt(0.5)) < 1e-12);
  // 换更大的共同 max：两侧强度一致地缩小
  const a2 = heatCells(makeDwell(100, 90), 360)[100].intensity;
  const b2 = heatCells(makeDwell(500, 90), 360)[500].intensity;
  assert.ok(Math.abs(a2 - 0.5) < 1e-12);
  assert.equal(a2, b2);
});

test("缺/多 625 值属于上游合同错误：抛中文错误，不补造数据", () => {
  assert.throws(() => heatCells([], 10), /625/);
  assert.throws(() => heatCells(new Array(624).fill(0), 10), /625/);
  assert.throws(() => heatCells(new Array(626).fill(0), 10), /625/);
});

test("heatCellColor：端点钉在主题青绿→明亮青绿，g/alpha 随强度单调，越界/非有限收敛", () => {
  assert.equal(heatCellColor(0), "rgba(13,148,136,0.16)", "低强度 = --color-primary 低不透明度");
  assert.equal(heatCellColor(0.5), "rgba(17,166,151,0.58)");
  assert.equal(heatCellColor(1), "rgba(20,184,166,1)", "高强度 = --color-primary-hover 近实心");
  assert.equal(heatCellColor(-1), heatCellColor(0));
  assert.equal(heatCellColor(1.5), heatCellColor(1));
  assert.equal(heatCellColor(NaN), heatCellColor(0));
  // 单调：alpha 与 g 通道随强度非降
  const parse = (s) => s.match(/rgba\((\d+),(\d+),(\d+),([\d.]+)\)/).slice(1).map(Number);
  let prevG = -1;
  let prevA = -1;
  for (const t of [0, 0.2, 0.4, 0.6, 0.8, 1]) {
    const [, g, , a] = parse(heatCellColor(t));
    assert.ok(g >= prevG, `g 通道在 t=${t} 应非降`);
    assert.ok(a >= prevA, `alpha 在 t=${t} 应非降`);
    prevG = g;
    prevA = a;
  }
});

test("formatTravelR：千分位＋最多 2 小数＋“ R”", () => {
  assert.equal(formatTravelR(0), "0 R");
  assert.equal(formatTravelR(3), "3 R");
  assert.equal(formatTravelR(0.5), "0.5 R");
  assert.equal(formatTravelR(1234.5), "1,234.5 R");
  assert.equal(formatTravelR(12345.678), "12,345.68 R");
  assert.equal(formatTravelR(-5), "0 R");
  assert.equal(formatTravelR(NaN), "0 R");
});

test("formatMouseDistance：<1m cm、≥1m m、≥1000m km、最多 3 位小数、null/非有限“—”", () => {
  assert.equal(formatMouseDistance(null), "—");
  assert.equal(formatMouseDistance(NaN), "—");
  assert.equal(formatMouseDistance(Infinity), "—");
  assert.equal(formatMouseDistance(0), "0 cm");
  assert.equal(formatMouseDistance(0.25), "25 cm");
  assert.equal(formatMouseDistance(0.001), "0.1 cm");
  assert.equal(formatMouseDistance(1), "1 m");
  assert.equal(formatMouseDistance(42.195), "42.195 m");
  assert.equal(formatMouseDistance(999.999), "999.999 m");
  assert.equal(formatMouseDistance(1000), "1 km");
  assert.equal(formatMouseDistance(1500), "1.5 km");
  assert.equal(formatMouseDistance(2345.6), "2.346 km");
});

test("选中格精确读数：停留秒数最多 2 位小数、占活动比例最多 1 位小数", () => {
  assert.equal(formatCellDwell(0), "0 秒");
  assert.equal(formatCellDwell(5.234), "5.23 秒");
  assert.equal(formatCellDwell(1234.567), "1,234.57 秒");
  assert.equal(formatCellDwell(NaN), "0 秒");
  assert.equal(dwellShareText(25, 100), "25%");
  assert.equal(dwellShareText(1, 3), "33.3%");
  assert.equal(dwellShareText(90, 180), "50%");
  assert.equal(dwellShareText(5, 0), "—", "无活动时间不给比例");
  assert.equal(dwellShareText(5, -1), "—");
  assert.equal(dwellShareText(NaN, 100), "—");
});

test("parseManualDpi：十进制整数 1..100000 合法；非十进制整数形态拒绝", () => {
  assert.equal(MANUAL_DPI_MIN, 1);
  assert.equal(MANUAL_DPI_MAX, 100000);
  assert.deepEqual(parseManualDpi("800"), { ok: true, dpi: 800 });
  assert.deepEqual(parseManualDpi(" 1600 "), { ok: true, dpi: 1600 }, "首尾空白容忍");
  assert.deepEqual(parseManualDpi("000800"), { ok: true, dpi: 800 }, "前导零按数值归一");
  assert.deepEqual(parseManualDpi("1"), { ok: true, dpi: 1 });
  assert.deepEqual(parseManualDpi("100000"), { ok: true, dpi: 100000 });

  for (const bad of ["", "   ", "abc", "8.5", "800.", "-800", "+800", "8e3", "1e-3", "0x10", "1 000", "1,600", "８００", "١٢٣"]) {
    const r = parseManualDpi(bad);
    assert.equal(r.ok, false, `应拒绝: ${JSON.stringify(bad)}`);
    assert.ok(!r.ok && r.message.length > 0, `拒绝时给中文消息: ${JSON.stringify(bad)}`);
  }
  // 形态错误与范围错误消息区分
  assert.match(parseManualDpi("8.5").message, /整数/);
  assert.match(parseManualDpi("").message, /请输入/);

  // 范围拒绝（形态仍是十进制整数）
  for (const range of ["0", "100001", "999999999999999999999"]) {
    const r = parseManualDpi(range);
    assert.equal(r.ok, false, `范围应拒绝: ${range}`);
    assert.ok(!r.ok && r.message.includes("1–100000"));
  }
});

/* ===== 控件几何（mouse 9 / gamepad 17） ===== */

function assertHitLayout(specs, kind) {
  const canvas = MOTION_CANVAS[kind];
  const labels = new Set();
  for (const c of specs) {
    assert.ok(c.hit.w >= MOTION_HIT_MIN_PX && c.hit.h >= MOTION_HIT_MIN_PX,
      `${kind} ${c.shortLabel} 的点击区域须 ≥44×44，实际 ${c.hit.w}×${c.hit.h}`);
    assert.ok(c.hit.x >= 0 && c.hit.y >= 0, `${kind} ${c.shortLabel} hit 原点非负`);
    assert.ok(c.hit.x + c.hit.w <= canvas.width && c.hit.y + c.hit.h <= canvas.height,
      `${kind} ${c.shortLabel} hit 落在画布内`);
    assert.ok(!labels.has(c.shortLabel), `${kind} 短标签唯一: ${c.shortLabel}`);
    labels.add(c.shortLabel);
  }
  for (let i = 0; i < specs.length; i++) {
    for (let j = i + 1; j < specs.length; j++) {
      const a = specs[i];
      const b = specs[j];
      const overlap = a.hit.x < b.hit.x + b.hit.w && b.hit.x < a.hit.x + a.hit.w
        && a.hit.y < b.hit.y + b.hit.h && b.hit.y < a.hit.y + a.hit.h;
      assert.equal(overlap, false, `${kind}: ${a.shortLabel} 与 ${b.shortLabel} 的点击区域不能重叠`);
    }
  }
}

test("mouse 控件：恰 9 码 1..9、短标签与既有码表一致、hit ≥44 不重叠且在画布内", () => {
  const mouse = motionControlSpecs("mouse");
  assert.equal(mouse.length, 9);
  assert.deepEqual([...mouse.map((c) => c.code)].sort((a, b) => a - b), [1, 2, 3, 4, 5, 6, 7, 8, 9]);
  const labels = { 1: "左键", 2: "右键", 3: "中键", 4: "X1", 5: "X2", 6: "滚轮上", 7: "滚轮下", 8: "滚轮左", 9: "滚轮右" };
  for (const [code, label] of Object.entries(labels)) {
    assert.equal(mouse.find((c) => c.code === Number(code)).shortLabel, label, `鼠标 code ${code}`);
  }
  assertHitLayout(mouse, "mouse");
  // 形状锚定：左右大面板分列两半、四向滚轮为箭头控件（按下与滚动不合计）
  const byCode = (code) => mouse.find((c) => c.code === code);
  const cx = (c) => c.hit.x + c.hit.w / 2;
  const mw = MOTION_CANVAS.mouse.width;
  assert.ok(cx(byCode(1)) < mw / 2 && cx(byCode(2)) > mw / 2, "左/右键面板分列两半");
  assert.deepEqual([byCode(6).dir, byCode(7).dir, byCode(8).dir, byCode(9).dir], ["up", "down", "left", "right"]);
  assert.ok(byCode(6).hit.y + byCode(6).hit.h <= byCode(3).hit.y, "滚轮上箭头在中键上方");
  assert.ok(byCode(4).hit.y < byCode(5).hit.y, "X1 在 X2 上方");
});

test("gamepad 控件：恰 17 码 1..17、短标签逐字（§4.6）、hit ≥44 不重叠且在画布内", () => {
  const gp = motionControlSpecs("gamepad");
  assert.equal(gp.length, 17);
  assert.deepEqual([...gp.map((c) => c.code)].sort((a, b) => a - b), [...Array(17).keys()].map((i) => i + 1));
  const labels = {
    1: "A", 2: "B", 3: "Y", 4: "X", 5: "LB", 6: "LT", 7: "RB", 8: "RT",
    9: "View", 10: "Menu", 11: "Guide", 12: "LS", 13: "RS",
    14: "十字上", 15: "十字下", 16: "十字左", 17: "十字右",
  };
  for (const [code, label] of Object.entries(labels)) {
    assert.equal(gp.find((c) => c.code === Number(code)).shortLabel, label, `手柄 code ${code}`);
  }
  assertHitLayout(gp, "gamepad");
  const byCode = (code) => gp.find((c) => c.code === code);
  const cx = (c) => c.hit.x + c.hit.w / 2;
  const cy = (c) => c.hit.y + c.hit.h / 2;
  const gw = MOTION_CANVAS.gamepad.width;
  // ABXY 菱形：Y 上 / X 左 / B 右 / A 下（字母标记，不只靠颜色）
  assert.ok(cy(byCode(3)) < cy(byCode(4)), "Y 在 X 上方");
  assert.ok(cy(byCode(1)) > cy(byCode(4)), "A 在 X 下方");
  assert.ok(cx(byCode(4)) < cx(byCode(2)), "X 在 B 左侧");
  assert.equal(cx(byCode(3)), cx(byCode(1)), "Y/A 同列");
  assert.equal(cy(byCode(4)), cy(byCode(2)), "X/B 同行");
  // 十字 D-pad 四臂
  assert.deepEqual([byCode(14).dir, byCode(15).dir, byCode(16).dir, byCode(17).dir], ["up", "down", "left", "right"]);
  assert.equal(cx(byCode(14)), cx(byCode(15)), "十字上/下同列");
  assert.equal(cy(byCode(16)), cy(byCode(17)), "十字左/右同行");
  assert.ok(cy(byCode(14)) < cy(byCode(16)) && cy(byCode(15)) > cy(byCode(16)), "十字上/下分列上下");
  assert.ok(cx(byCode(16)) < cx(byCode(17)), "十字左在十字右左侧");
  // 顶部扳机在横向肩键上方；View/Menu/Guide 居中；双摇杆分列左右半区
  assert.ok(byCode(6).hit.y + byCode(6).hit.h <= byCode(5).hit.y, "LT 在 LB 上方");
  assert.ok(byCode(8).hit.y + byCode(8).hit.h <= byCode(7).hit.y, "RT 在 RB 上方");
  assert.ok(cx(byCode(11)) > gw * 0.4 && cx(byCode(11)) < gw * 0.6, "Guide 居中");
  assert.ok(cx(byCode(12)) < gw / 2 && cx(byCode(13)) > gw / 2, "LS 左半、RS 右半");
});

test("控件几何模板冻结：同一 kind 恒返回同一冻结模板，未知种类抛中文错误", () => {
  assert.equal(motionControlSpecs("mouse"), motionControlSpecs("mouse"));
  assert.equal(motionControlSpecs("gamepad"), motionControlSpecs("gamepad"));
  assert.ok(Object.isFrozen(motionControlSpecs("mouse")));
  assert.ok(Object.isFrozen(motionControlSpecs("gamepad")));
  assert.ok(Object.isFrozen(motionControlSpecs("mouse")[0].hit));
  assert.throws(() => motionControlSpecs("keyboard"), /未知控件种类/);
  assert.throws(() => motionControlSpecs("joystick"), /未知控件种类/);
});

/* ===== 组件模块冒烟（不渲染） ===== */

const COMPONENTS = [
  ["StickHeatmap.tsx", "StickHeatmap"],
  ["PeripheralControlsStats.tsx", "PeripheralControlsStats"],
  ["MouseDpiEditor.tsx", "MouseDpiEditor"],
  ["MouseSourcePicker.tsx", "MouseSourcePicker"],
  ["MotionSummary.tsx", "MotionSummary"],
];

test("五个组件模块可加载并导出组件函数；只依赖 react/本地展示层，不查询 API", async () => {
  // 组件允许的运行时依赖：react hooks、运动展示 lib、格式化 lib、骨架屏与本地 SVG 图标
  const allowed = new Set(["react", "../lib/motionPresentation", "../lib/format", "./Skeleton", "./icons"]);
  for (const [file, exportName] of COMPONENTS) {
    const { exports, required } = await loadComponentModule(`../src/components/${file}`);
    assert.equal(typeof exports[exportName], "function", `${file} 应导出 ${exportName}`);
    for (const spec of required) {
      assert.ok(allowed.has(spec), `${file} 引入了白名单外的模块: ${spec}`);
      assert.ok(!/client|mock|queries|react-query|tauri/i.test(spec), `${file} 不得依赖 API 层: ${spec}`);
    }
  }
});
