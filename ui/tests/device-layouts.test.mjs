// S7 回归（usability-runtime-v3 §4.8 U4）：设备参考物理布局 / 排行几何 / 表格分页。
//
// 用 Node 内建 test 验证纯契约：
// - keyboard ANSI-104 四区 13/61/13/17=104、mouse 9、gamepad 17：code/id 在 kind 内唯一，
//   控件几何全部落在所属区内且 ≥1×1u（X/Y 位置与形状锚定：宽键/竖跨/横跨/方向菱形/四肩扳机分离）；
// - 手柄 17 码短标签表（§4.6：1=A…17=十字右）与鼠标 9 控件命名逐字；
// - getUnmappedRows：零码/未知码/超长 label 原样保留、输入行顺序不变、不改入参、已知 code 剔除；
// - nextControlId：几何中心半平面最近（宽键按中心）、同距按模板顺序、无候选保持 currentId、
//   Home/End 仅当前区首末、currentId 不存在返回模板第一个控件、导航永不跨区；
// - rankTopRows：降序稳定、最多 10 行、fraction∈[0,1]（全 0→0、NaN/负值→0）、不改入参、id/label/value 透传；
// - DataTable 分页纯逻辑：resetKey 变化回第一页、key 不变的轮询行值变动不重置、页数缩小时 clamp、
//   未启用分页恒 1；DataTable 模块（含 JSX/react import）经 CommonJS 转译 + require 桩加载冒烟。
//
// 纯模块（../src/lib/*.ts，零运行时 import）经 typescript.transpileModule 转为 ESM 后由 data URL
// 导入（同 presentation-contracts.test.mjs 惯例）；DataTable.tsx 含 JSX，转 CommonJS 后以
// new Function + react 桩执行模块顶层（不渲染组件——渲染层由 tsc --noEmit 与 S10 页面验收覆盖）。
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

/** DataTable.tsx：含 JSX 与 react import，转 CommonJS 后用 require 桩执行模块顶层（不渲染） */
async function loadDataTableModule(relPath) {
  const src = await readFile(new URL(relPath, import.meta.url), "utf8");
  const js = ts.transpileModule(src, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2020,
      jsx: ts.JsxEmit.React,
    },
  }).outputText;
  const mod = { exports: {} };
  const fakeRequire = (spec) => {
    if (spec === "react") return {}; // 组件体不执行：模块加载只需 require 可解析
    throw new Error(`测试环境不允许加载模块: ${spec}`);
  };
  new Function("require", "module", "exports", js)(fakeRequire, mod, mod.exports);
  return mod.exports;
}

/** 去掉注释后的转译产物（纯度断言用，避免注释里的词干扰） */
function stripComments(js) {
  return js.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

const { js: layoutJs, module: layoutModule } = await loadModule("../src/lib/deviceLayouts.ts");
const { getDeviceLayout, nextControlId, getUnmappedRows } = layoutModule;

test("手柄控件的点击区域不相互覆盖", () => {
  const controls = getDeviceLayout("gamepad").controls;
  for (let i = 0; i < controls.length; i++) {
    for (let j = i + 1; j < controls.length; j++) {
      const a = controls[i], b = controls[j];
      const overlap = a.x < b.x + b.width && b.x < a.x + a.width
        && a.y < b.y + b.height && b.y < a.y + a.height;
      assert.equal(overlap, false, `${a.id} 与 ${b.id} 的点击区域不能重叠`);
    }
  }
});
const { js: rankJs, module: rankModule } = await loadModule("../src/lib/ranking.ts");
const { rankTopRows } = rankModule;
const dataTable = await loadDataTableModule("../src/components/DataTable.tsx");
const { applyPageReset, clampPageIndex } = dataTable;

const NAV_KEYS = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"];

/** 布局几何不变式：区存在、控件落在区内、单控件 ≥1×1u、id/code 在 kind 内唯一 */
function assertGeometry(layout) {
  const zoneById = new Map(layout.zones.map((z) => [z.id, z]));
  const ids = new Set();
  const codes = new Set();
  for (const c of layout.controls) {
    const zone = zoneById.get(c.zone);
    assert.ok(zone, `控件 ${c.id} 的分区 ${c.zone} 必须存在`);
    assert.ok(c.x >= 0 && c.y >= 0, `控件 ${c.id} 的 X/Y 位置必须非负`);
    assert.ok(c.x + c.width <= zone.width, `控件 ${c.id} 横向超出分区 ${zone.id}`);
    assert.ok(c.y + c.height <= zone.height, `控件 ${c.id} 纵向超出分区 ${zone.id}`);
    assert.ok(c.width >= 1 && c.height >= 1, `控件 ${c.id} 不得小于 1×1u（44×44 显示单元）`);
    assert.ok(!ids.has(c.id), `控件 id 重复: ${c.id}`);
    assert.ok(!codes.has(c.code), `${layout.kind} 内 code 重复: ${c.code}`);
    ids.add(c.id);
    codes.add(c.code);
  }
}

const KEYBOARD = getDeviceLayout("keyboard");
const MOUSE = getDeviceLayout("mouse");
const GAMEPAD = getDeviceLayout("gamepad");

function controlOf(layout, id) {
  const c = layout.controls.find((x) => x.id === id);
  assert.ok(c, `布局 ${layout.kind} 缺控件 ${id}`);
  return c;
}

function codeOf(layout, code) {
  const c = layout.controls.find((x) => x.code === code);
  assert.ok(c, `布局 ${layout.kind} 缺 code ${code}`);
  return c;
}

test("纯模块零依赖：deviceLayouts/ranking 转译产物无 import/require/DOM/时钟", () => {
  for (const js of [layoutJs, rankJs]) {
    const bare = stripComments(js);
    assert.doesNotMatch(bare, /\bimport\b/); // TopKeyRow 为 type import，已擦除 = 零运行时依赖
    assert.doesNotMatch(bare, /\brequire\s*\(/);
    assert.doesNotMatch(bare, /\bdocument\b|\bwindow\b/); // 无 DOM 依赖
    assert.doesNotMatch(bare, /\bnew\s+Date\b|\bDate\.now\b/); // 无时钟依赖
  }
  assert.equal(typeof getDeviceLayout, "function");
  assert.equal(typeof nextControlId, "function");
  assert.equal(typeof getUnmappedRows, "function");
  assert.equal(typeof rankTopRows, "function");
});

test("keyboard ANSI-104：四区 13/61/13/17=104，code/id 唯一，几何落在区内且 ≥1×1u", () => {
  assert.equal(KEYBOARD.zones.length, 4);
  const zoneCount = (zoneId) => KEYBOARD.controls.filter((c) => c.zone === zoneId).length;
  assert.equal(zoneCount("function"), 13, "功能键区 13（Esc + F1–F12）");
  assert.equal(zoneCount("main"), 61, "主键区 61");
  assert.equal(zoneCount("nav"), 13, "编辑导航区 13（含 PrintScreen/ScrollLock/Pause）");
  assert.equal(zoneCount("pad"), 17, "数字键区 17");
  assert.equal(KEYBOARD.controls.length, 104);
  assertGeometry(KEYBOARD);
  // 参考布局形状锚定（区内行序/形状忠于 ANSI-104）
  assert.equal(controlOf(KEYBOARD, "backspace").width, 2);
  assert.equal(controlOf(KEYBOARD, "tab").width, 1.5);
  assert.equal(controlOf(KEYBOARD, "caps-lock").width, 1.75);
  assert.equal(controlOf(KEYBOARD, "enter").width, 2.25);
  assert.equal(controlOf(KEYBOARD, "r-shift").width, 2.75);
  assert.equal(controlOf(KEYBOARD, "space").width, 6.25);
  assert.equal(controlOf(KEYBOARD, "pad-plus").height, 2, "数字区 + 竖跨 2u");
  assert.equal(controlOf(KEYBOARD, "pad-enter").height, 2, "数字区 Enter 竖跨 2u");
  assert.equal(controlOf(KEYBOARD, "pad-0").width, 2, "数字区 0 横跨 2u");
  assert.deepEqual([controlOf(KEYBOARD, "up").x, controlOf(KEYBOARD, "up").y], [1, 3], "上方向键居中");
  // 三个导航区锚定码：PrintScreen/ScrollLock/Pause
  assert.equal(codeOf(KEYBOARD, 0xe037).id, "print-screen");
  assert.equal(codeOf(KEYBOARD, 0x46).id, "scroll-lock");
  assert.equal(codeOf(KEYBOARD, 0xe11d).id, "pause");
});

test("mouse：9 控件 code 恰为 1..9（左/右/中、X1/X2 与四滚向分列），id/code 唯一", () => {
  assert.equal(MOUSE.controls.length, 9);
  assert.deepEqual([...MOUSE.controls.map((c) => c.code)].sort((a, b) => a - b), [1, 2, 3, 4, 5, 6, 7, 8, 9]);
  assertGeometry(MOUSE);
  const expect = {
    1: "left", 2: "right", 3: "middle", 4: "x1", 5: "x2",
    6: "wheel-up", 7: "wheel-down", 8: "wheel-left", 9: "wheel-right",
  };
  for (const [code, id] of Object.entries(expect)) {
    assert.equal(codeOf(MOUSE, Number(code)).id, id, `鼠标 code ${code} → ${id}`);
  }
  assert.equal(codeOf(MOUSE, 3).shortLabel, "中键");
  assert.equal(codeOf(MOUSE, 6).shortLabel, "滚轮上");
  assert.equal(codeOf(MOUSE, 9).shortLabel, "滚轮右");
  // 同一模板引用稳定（kind/code 唯一、冻结不可改写）
  assert.equal(getDeviceLayout("mouse"), MOUSE);
  assert.ok(Object.isFrozen(MOUSE.controls) && Object.isFrozen(MOUSE.controls[0]));
});

test("gamepad：17 控件 code 恰为 1..17，Y 上/X 左/B 右/A 下，四肩扳机分离，短标签表逐字（§4.6）", () => {
  assert.equal(GAMEPAD.controls.length, 17);
  assert.deepEqual([...GAMEPAD.controls.map((c) => c.code)].sort((a, b) => a - b), [...Array(17).keys()].map((i) => i + 1));
  assertGeometry(GAMEPAD);
  // 17 码短标签表（§4.6 布局数据列：1=A…14..17=十字上/下/左/右）
  const expectLabels = {
    1: "A", 2: "B", 3: "Y", 4: "X", 5: "LB", 6: "LT", 7: "RB", 8: "RT",
    9: "View", 10: "Menu", 11: "Guide", 12: "LS", 13: "RS",
    14: "十字上", 15: "十字下", 16: "十字左", 17: "十字右",
  };
  for (const [code, label] of Object.entries(expectLabels)) {
    assert.equal(codeOf(GAMEPAD, Number(code)).shortLabel, label, `手柄 code ${code} 短标签`);
  }
  // 方向菱形：Y 上 / X 左 / B 右 / A 下（Xbox 物理位次，X→West、Y→North）
  const north = codeOf(GAMEPAD, 3);
  const west = codeOf(GAMEPAD, 4);
  const east = codeOf(GAMEPAD, 2);
  const south = codeOf(GAMEPAD, 1);
  assert.ok(north.y + north.height <= west.y, "Y（北）在 X（西）上方");
  assert.ok(south.y >= west.y + west.height, "A（南）在 X（西）下方");
  assert.ok(west.x + west.width <= east.x, "X（西）在 B（东）左侧");
  assert.equal(north.x + north.width / 2, south.x + south.width / 2, "菱形左右对称");
  // LB/LT/RB/RT 四个分离位置
  const shoulders = [5, 6, 7, 8].map((code) => codeOf(GAMEPAD, code));
  const positions = new Set(shoulders.map((c) => `${c.x},${c.y}`));
  assert.equal(positions.size, 4, "LB/LT/RB/RT 必须是四个分离位置");
  // 双摇杆按下、十字四向、View/Menu/Guide 明确存在且位置互异
  for (const id of ["ls", "rs", "dpad-up", "dpad-down", "dpad-left", "dpad-right", "view", "menu", "guide"]) {
    controlOf(GAMEPAD, id);
  }
  assert.equal(getDeviceLayout("gamepad"), GAMEPAD);
});

test("getDeviceLayout：未知 kind 抛中文错误", () => {
  assert.throws(() => getDeviceLayout("joystick"), /未知设备种类/);
});

test("getUnmappedRows：零/未知码与超长 label 原样保留、输入顺序不变、已知 code 剔除、不改入参", () => {
  const longLabel = "很".repeat(500);
  const rows = [
    { code: 0x1e, total: 5, label: "A" },
    { code: 0, total: 3, label: "键 0x0" }, // 零码：布局无此控件 → 保留
    { code: 0x63, total: 9, label: longLabel }, // 键盘未绘制码（媒体键等）→ 保留
    { code: 0x2a, total: 7, label: "Shift" },
  ];
  const snapshot = JSON.parse(JSON.stringify(rows));
  const unmapped = getUnmappedRows("keyboard", rows);
  assert.deepEqual(unmapped, [rows[1], rows[2]], "保持输入行顺序，只保留未绘制码");
  assert.equal(unmapped[1].label, longLabel, "超长 label 原样保留");
  assert.deepEqual(rows, snapshot, "不修改入参数组");
  assert.notEqual(unmapped, rows);

  // 零计数但 code 已知：剔除（计数只做 code 查找，与数值无关）
  assert.deepEqual(getUnmappedRows("keyboard", [{ code: 0x3b, total: 0, label: "F1" }]), []);

  // 手柄/鼠标值域外的未知码全部保留；值域内剔除
  const gp = [{ code: 0, total: 1, label: "按钮 0" }, { code: 5, total: 2, label: "LB" }, { code: 18, total: 3, label: "按钮 18" }, { code: 99, total: 4, label: longLabel }];
  assert.deepEqual(getUnmappedRows("gamepad", gp), [gp[0], gp[2], gp[3]]);
  const ms = [{ code: 1, total: 1, label: "左键" }, { code: 10, total: 2, label: "按钮 10" }];
  assert.deepEqual(getUnmappedRows("mouse", ms), [ms[1]]);
});

test("nextControlId：几何中心半平面最近（宽键按中心）、同距按模板顺序、无候选保持", () => {
  // 基础移动：q → 右 w、q → 下 a（(2.0,1.5) 的最近下方中心是 a(2.25,2.5)）
  assert.equal(nextControlId(KEYBOARD, "q", "ArrowRight"), "w");
  assert.equal(nextControlId(KEYBOARD, "w", "ArrowLeft"), "q");
  assert.equal(nextControlId(KEYBOARD, "q", "ArrowDown"), "a");
  // 同距按模板顺序：q 上方 "1"(1.5,0.5) 与 "2"(2.5,0.5) 等距 → 模板顺序先到者 "1"
  assert.equal(nextControlId(KEYBOARD, "q", "ArrowUp"), "1");
  // 几何中心而非左上角：右 Shift(中心13.625,3.5) 上方最近是 Enter(中心13.875,2.5)
  assert.equal(nextControlId(KEYBOARD, "r-shift", "ArrowUp"), "enter");
  // 半平面只约束方向轴、距离取欧氏最近：Space(中心6.875,4.5) → 右是 N(8.75,3.5)（对角 2.1u
  // 近于同行 RAlt 3.75u）；RAlt(10.625,4.5) → 左是 Comma(9.75,3.5)——同行为合同语义，非实现缺陷
  assert.equal(nextControlId(KEYBOARD, "space", "ArrowRight"), "n");
  assert.equal(nextControlId(KEYBOARD, "r-alt", "ArrowLeft"), "comma");
  // 无候选保持 currentId：功能区无上方；导航区 → 键在最右
  assert.equal(nextControlId(KEYBOARD, "esc", "ArrowUp"), "esc");
  assert.equal(nextControlId(KEYBOARD, "right", "ArrowRight"), "right");
  // 鼠标：滚轮列中心垂直链；左键右侧最近是滚轮上（中心距离最近，半平面语义）
  assert.equal(nextControlId(MOUSE, "wheel-up", "ArrowDown"), "middle");
  assert.equal(nextControlId(MOUSE, "middle", "ArrowDown"), "wheel-down");
  assert.equal(nextControlId(MOUSE, "left", "ArrowRight"), "wheel-up");
  // 手柄同距按模板顺序：Y(7.5,3.5) 下方 X(6.5,4.5) 与 B(8.5,4.5) 等距 → 模板序 B 在 X 前
  assert.equal(nextControlId(GAMEPAD, "north", "ArrowDown"), "east");
});

test("nextControlId：Home/End 仅当前区首末；currentId 不存在返回模板第一个控件；导航永不跨区", () => {
  assert.equal(nextControlId(KEYBOARD, "k", "Home"), "backquote", "主键区首");
  assert.equal(nextControlId(KEYBOARD, "k", "End"), "r-ctrl", "主键区末");
  assert.equal(nextControlId(KEYBOARD, "pad-7", "Home"), "num-lock", "数字区首（不是功能区 Esc）");
  assert.equal(nextControlId(KEYBOARD, "pad-7", "End"), "pad-dot");
  assert.equal(nextControlId(KEYBOARD, "f5", "Home"), "esc");
  assert.equal(nextControlId(KEYBOARD, "f5", "End"), "f12");
  assert.equal(nextControlId(KEYBOARD, "up", "Home"), "print-screen");
  assert.equal(nextControlId(KEYBOARD, "up", "End"), "right");
  // currentId 不存在 → 模板第一个控件
  assert.equal(nextControlId(KEYBOARD, "nope", "ArrowRight"), "esc");
  assert.equal(nextControlId(KEYBOARD, "nope", "End"), "esc");
  // 不变量：任何控件的任何导航键都停留在当前区（区内 roving）
  for (const layout of [KEYBOARD, MOUSE, GAMEPAD]) {
    for (const c of layout.controls) {
      for (const key of NAV_KEYS) {
        const nextId = nextControlId(layout, c.id, key);
        const next = layout.controls.find((x) => x.id === nextId);
        assert.ok(next, `导航结果必须存在: ${c.id} + ${key} → ${nextId}`);
        assert.equal(next.zone, c.zone, `${layout.kind}: ${c.id} + ${key} 不得跨区`);
      }
    }
  }
});

test("rankTopRows（合同示例）：值[20,10,0] → rank[1,2,3]、fraction[1,0.5,0]；降序稳定；最多10行", () => {
  const rows = [
    { id: "a", label: "甲", value: 20 },
    { id: "b", label: "乙", value: 10 },
    { id: "c", label: "丙", value: 0 },
  ];
  const ranked = rankTopRows(rows);
  assert.deepEqual(ranked.map((r) => r.rank), [1, 2, 3]);
  assert.deepEqual(ranked.map((r) => r.fraction), [1, 0.5, 0]);
  assert.deepEqual(ranked.map((r) => r.id), ["a", "b", "c"]);

  // 同值保留输入顺序（稳定序）
  const ties = rankTopRows([
    { id: "a", label: "a", value: 5 },
    { id: "b", label: "b", value: 5 },
    { id: "c", label: "c", value: 1 },
  ]);
  assert.deepEqual(ties.map((r) => r.id), ["a", "b", "c"]);
  assert.deepEqual(ties.map((r) => r.fraction), [1, 1, 0.2]);

  // 无序输入 → 降序；最多 10 行（15 取前 10）
  const many = Array.from({ length: 15 }, (_, i) => ({ id: `r${i + 1}`, label: `行${i + 1}`, value: i + 1 }));
  const top = rankTopRows(many);
  assert.equal(top.length, 10);
  assert.deepEqual(top.map((r) => r.value), [15, 14, 13, 12, 11, 10, 9, 8, 7, 6]);
  assert.deepEqual(rankTopRows([{ id: "x", label: "x", value: 3 }, { id: "y", label: "y", value: 20 }, { id: "z", label: "z", value: 10 }]).map((r) => r.value), [20, 10, 3]);
});

test("rankTopRows：fraction∈[0,1]、全0则0、异常值收敛0、不改入参、id/label/value 稳定透传", () => {
  // 全 0 → fraction 全 0，名次仍按输入顺序
  const zeros = rankTopRows([
    { id: "a", label: "a", value: 0 },
    { id: "b", label: "b", value: 0 },
    { id: "c", label: "c", value: 0 },
  ]);
  assert.deepEqual(zeros.map((r) => r.fraction), [0, 0, 0]);
  assert.deepEqual(zeros.map((r) => r.rank), [1, 2, 3]);

  // NaN/负值不污染有效行的 fraction（topMax 取有限值最大者）：
  // NaN 行的排序位置属引擎定义，不断言顺序；只断言 fraction 全部有限且 ∈[0,1]、
  // 唯一有效正值 big 恒为 fraction=1、负值行恒 0、超长 label 原样透传。
  const longLabel = "长".repeat(500);
  const odd = rankTopRows([
    { id: "nan", label: "NaN 行", value: NaN },
    { id: "neg", label: "负行", value: -5 },
    { id: "big", label: longLabel, value: 10 },
  ]);
  assert.equal(new Set(odd.map((r) => r.id)).size, 3, "三行都在（顺序属引擎定义）");
  for (const item of odd) {
    assert.ok(Number.isFinite(item.fraction) && item.fraction >= 0 && item.fraction <= 1, `fraction 越界: ${item.fraction}`);
    assert.equal(typeof item.rank, "number");
  }
  assert.equal(odd.find((r) => r.id === "big").fraction, 1);
  assert.equal(odd.find((r) => r.id === "neg").fraction, 0);
  assert.equal(odd.find((r) => r.id === "nan").fraction, 0);
  const big = odd.find((r) => r.id === "big");
  assert.equal(big.label, longLabel);
  assert.deepEqual(big, { id: "big", label: longLabel, value: 10, rank: big.rank, fraction: 1 });

  // 全负值：无有效 topMax → fraction 全 0
  const negative = rankTopRows([{ id: "a", label: "a", value: -5 }, { id: "b", label: "b", value: -3 }]);
  assert.deepEqual(negative.map((r) => r.fraction), [0, 0]);

  // 不修改入参数组；返回新对象（稳定身份透传而非引用复用）
  const input = [{ id: "b", label: "b", value: 1 }, { id: "a", label: "a", value: 2 }];
  const snapshot = JSON.parse(JSON.stringify(input));
  const out = rankTopRows(input);
  assert.deepEqual(input, snapshot);
  assert.notEqual(out, input);
  assert.notEqual(out[0], input[1]);
});

test("DataTable 分页纯逻辑：resetKey 变化回第一页、轮询行值变动不重置、页数缩小 clamp", () => {
  // DataTable 冒烟：模块（含 JSX/react import）可加载且导出齐全
  assert.equal(typeof dataTable.DataTable, "function");
  assert.equal(typeof applyPageReset, "function");
  assert.equal(typeof clampPageIndex, "function");

  // resetKey 不变（普通轮询行值变动：rows 内容/数值变化但语义键相同）→ 保持页号
  assert.deepEqual(applyPageReset({ key: "d1|2026-09-01..2026-09-30", page: 3 }, "d1|2026-09-01..2026-09-30"),
    { key: "d1|2026-09-01..2026-09-30", page: 3 });
  // resetKey 语义变化（设备/范围/排序语义）→ 回第一页
  assert.deepEqual(applyPageReset({ key: "d1|r1", page: 3 }, "d2|r1"), { key: "d2|r1", page: 1 });
  // resetKey 缺省按 "" 处理
  assert.deepEqual(applyPageReset({ key: "", page: 4 }, undefined), { key: "", page: 4 });
  assert.deepEqual(applyPageReset({ key: "x", page: 4 }, undefined), { key: "", page: 1 });

  // 页号 clamp：120 行每页 50 → 3 页
  assert.equal(clampPageIndex(5, 120, 50), 3);
  assert.equal(clampPageIndex(3, 101, 50), 3, "尾页允许不满页");
  assert.equal(clampPageIndex(7, 101, 50), 3, "页数缩小时收敛");
  assert.equal(clampPageIndex(2, 100, 50), 2, "范围内页号保留");
  assert.equal(clampPageIndex(0, 101, 50), 1, "下界 1");
  assert.equal(clampPageIndex(-2, 101, 50), 1);
  assert.equal(clampPageIndex(2, 0, 50), 1, "空行集恒第 1 页");
  assert.equal(clampPageIndex(4, 100, undefined), 1, "未启用分页恒 1");
  assert.equal(clampPageIndex(4, 100, 0), 1);
  assert.equal(clampPageIndex(4, 100, -3), 1);

  // 组合语义：轮询中行数缩小 → 页号 clamp 而不重置回 1 的语义边界（同 key 保页 + clamp 收敛）
  const state = applyPageReset({ key: "d1|r1", page: 3 }, "d1|r1");
  assert.equal(clampPageIndex(state.page, 200, 50), 3, "行数仍支持第 3 页 → 保留");
  assert.equal(clampPageIndex(state.page, 120, 50), 3);
  assert.equal(clampPageIndex(state.page, 90, 50), 2, "页数缩小 → clamp 到尾页");
});

test("分页组合：排序/语义重置后 clamp 仍生效（生产函数串联与组件调用一致）", () => {
  // 组件渲染序列：resetKey 变化 → applyPageReset；每次渲染 → clampPageIndex
  let state = { key: "old", page: 7 };
  state = applyPageReset(state, "new"); // 语义变化回第一页
  assert.deepEqual(state, { key: "new", page: 1 });
  state = applyPageReset(state, "new"); // 轮询不变
  assert.equal(state.page, 1);
  // 用户翻到第 2 页后轮询行数缩小（200 → 80，每页 50）
  state = { key: "new", page: 2 };
  assert.equal(clampPageIndex(state.page, 200, 50), 2);
  assert.equal(clampPageIndex(state.page, 80, 50), 2);
  assert.equal(clampPageIndex(state.page, 45, 50), 1);
});
