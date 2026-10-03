// S6 回归（usability-runtime-v3 §4.5 统计范围 / §4.7 时长格式）：presentation-contracts。
//
// 用 Node 内建 test 验证展示层纯契约：
// - statisticsRange 纯模块：只 type import（转译产物零运行时 import/require）、
//   不读取时钟（产物无 new Date/Date.now，today 一律来自调用方参数）；
// - 今日只采样一次：resolveStatisticsRange 的 from/to 来自同一 today 采样
//   （hook 在同一次 render 中用 activity.today 派生，不维护第二份 range 状态）；
// - 默认 today → 午夜跟随（selection 不变、range 随新采样前进）→ 手选 fixed
//   （午夜不改）→ 返回 Today（恢复跟随）；selection 与传入/返回的 Range 副本隔离；
// - defaultRange(days) 显式天数含义保留：defaultRange(90)=近 90 天（含今天），
//   WhatPulse/Settings（导出）既有初始范围不受本 Stage 影响；defaultRange(1)=单日今日；
// - fmtDuration（§4.7）：完整非零单位 天→小时→分→秒，合同全部示例逐字 +
//   异常值（0/负数/非整数向下取整/NaN/±Infinity → "0秒"）。
//
// 纯模块（../src/lib/statisticsRange.ts、../src/lib/format.ts，无运行时 import）经
// typescript.transpileModule 转为 ESM 后由 data URL 导入（同 app-activity.test.mjs 惯例）。
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

const { js: rangeJs, module: rangeModule } = await loadModule("../src/lib/statisticsRange.ts");
const { resolveStatisticsRange, selectStatisticsRange } = rangeModule;
const { defaultRange, parseDay, todayDay, fmtDuration } = await loadModule("../src/lib/format.ts").then((m) => m.module);

/** 去掉注释后的转译产物（纯度断言用，避免注释里的词干扰） */
function stripComments(js) {
  return js.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

test("statisticsRange 纯模块：只 type import（零运行时依赖）、不读取时钟", () => {
  const bare = stripComments(rangeJs);
  assert.doesNotMatch(bare, /\bimport\b/); // type import 已被转译擦除 = 零运行时依赖
  assert.doesNotMatch(bare, /\brequire\s*\(/);
  assert.doesNotMatch(bare, /\bnew\s+Date\b/); // 不读取时钟：today 一律来自参数
  assert.doesNotMatch(bare, /\bDate\.now\b/);
  assert.equal(typeof resolveStatisticsRange, "function");
  assert.equal(typeof selectStatisticsRange, "function");
});

test("今日只采样一次：resolveStatisticsRange 的 from/to 来自同一 today 参数", () => {
  // 故意用非当日采样：输出必须逐字等于传入采样（证明不读真实时钟、单次采样）
  const day = "2001-02-03";
  const r = resolveStatisticsRange({ mode: "today" }, day);
  assert.deepEqual(r, { from: day, to: day });
  assert.equal(r.from, r.to);
});

test("默认 today → 午夜跟随：selection 不变，range 随新采样前进", () => {
  const selection = selectStatisticsRange({ from: "2026-09-01", to: "2026-09-30" }, "today");
  assert.deepEqual(selection, { mode: "today" }); // Today 丢弃固定范围
  assert.deepEqual(resolveStatisticsRange(selection, "2026-10-01"), {
    from: "2026-10-01",
    to: "2026-10-01",
  });
  // 同一 selection（hook 不改状态）跨午夜后直接跟随新 today
  assert.deepEqual(resolveStatisticsRange(selection, "2026-10-02"), {
    from: "2026-10-02",
    to: "2026-10-02",
  });
});

test("手选 fixed：午夜后 resolve 不改（范围固定）", () => {
  const picked = { from: "2026-09-01", to: "2026-09-15" };
  const selection = selectStatisticsRange(picked, "fixed");
  assert.deepEqual(selection, { mode: "fixed", range: picked });
  assert.deepEqual(resolveStatisticsRange(selection, "2026-10-02"), picked);
  assert.deepEqual(resolveStatisticsRange(selection, "2026-10-03"), picked);
});

test("副本隔离：selection 不被传入/返回的 Range 原地改写污染", () => {
  const picked = { from: "2026-09-01", to: "2026-09-15" };
  const selection = selectStatisticsRange(picked, "fixed");
  assert.notEqual(selection.range, picked); // 存副本：调用方后续改写不影响 selection
  picked.from = "2026-12-01";
  assert.equal(selection.range.from, "2026-09-01");

  const view = resolveStatisticsRange(selection, "2026-10-02");
  assert.notEqual(view, selection.range); // 返回副本：外部改写 view 不污染 selection
  view.to = "2026-12-31";
  assert.equal(selection.range.to, "2026-09-15");
  assert.deepEqual(resolveStatisticsRange(selection, "2026-10-02"), { from: "2026-09-01", to: "2026-09-15" });
});

test("返回 Today：恢复跟随（午夜不改的固定范围被丢弃）", () => {
  const selection = selectStatisticsRange({ from: "2026-09-01", to: "2026-09-15" }, "today");
  assert.deepEqual(selection, { mode: "today" });
  assert.deepEqual(resolveStatisticsRange(selection, "2026-10-02"), {
    from: "2026-10-02",
    to: "2026-10-02",
  });
});

test("defaultRange(days) 显式天数含义保留（WhatPulse/Settings 导出 defaultRange90）", () => {
  const before = todayDay();
  const r90 = defaultRange(90);
  const after = todayDay();
  // 近 90 天（含今天）：to - from 恰为 89 个本地日（容差吸收极端夏令时日，仍排除差一天）
  const spanDays = (parseDay(r90.to).getTime() - parseDay(r90.from).getTime()) / 86_400_000;
  assert.ok(Math.abs(spanDays - 89) < 0.5, `近 90 天跨度应为 89 日，实际 ${spanDays}`);
  assert.match(r90.from, /^\d{4}-\d{2}-\d{2}$/);
  assert.match(r90.to, /^\d{4}-\d{2}-\d{2}$/);
  assert.ok(r90.from <= r90.to, "闭区间 from ≤ to");
  // to 锚定真实本地今日（before/after 容忍断言期间跨午夜的极端竞态）
  assert.ok(r90.to === before || r90.to === after, `defaultRange(90).to=${r90.to} 应为本地今日`);

  // 今天 preset（days=1）：单日 {today, today}
  const r1 = defaultRange(1);
  assert.equal(r1.from, r1.to);
  assert.ok(r1.from === before || r1.from === after, `defaultRange(1).from=${r1.from} 应为本地今日`);
});

test("fmtDuration（§4.7）：完整非零单位 天→小时→分→秒，合同示例逐字", () => {
  const cases = [
    [0, "0秒"],
    [1, "1秒"],
    [59, "59秒"],
    [60, "1分"],
    [90, "1分30秒"],
    [3599, "59分59秒"],
    [3600, "1小时"],
    [3661, "1小时1分1秒"],
    [90061, "1天1小时1分1秒"],
    // 单位内零值跳过（只输出非零单位）与进位边界
    [3601, "1小时1秒"],
    [86399, "23小时59分59秒"],
    [86400, "1天"],
    [86401, "1天1秒"],
    [90000, "1天1小时"],
  ];
  for (const [input, expected] of cases) {
    assert.equal(fmtDuration(input), expected, `fmtDuration(${input})`);
  }
});

test("fmtDuration 异常值：负数/非有限值按 0，非整数向下取整", () => {
  const cases = [
    [NaN, "0秒"],
    [Infinity, "0秒"],
    [-Infinity, "0秒"],
    [-1, "0秒"],
    [-90061, "0秒"],
    [-0.5, "0秒"],
    [0.9, "0秒"],
    [59.9, "59秒"],
    [90.9, "1分30秒"],
  ];
  for (const [input, expected] of cases) {
    assert.equal(fmtDuration(input), expected, `fmtDuration(${input})`);
  }
});
