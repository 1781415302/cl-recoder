// S8 验收（input-live-trigger §4.7）：新外设图映射 / 状态 / 几何行为；
// TriggerHeatmap 仅历史（无实时 props）；9/17 码、ABXY、hit≥44、摇杆 Y 向上与对角径向、
// LT/RT 百分比文案、历史共同色标。不渲染 React——源码合同 + 纯几何/热力函数。
import assert from "node:assert/strict";
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

async function loadModule(relPath) {
  const src = await readFile(new URL(relPath, import.meta.url), "utf8");
  const js = ts.transpileModule(src, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2020 },
  }).outputText;
  return {
    src,
    js,
    module: await import(
      "data:text/javascript;base64," + Buffer.from(js).toString("base64")
    ),
  };
}

async function loadComponentSource(relPath) {
  return readFile(new URL(relPath, import.meta.url), "utf8");
}

function stripComments(js) {
  return js.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

const { module: lib } = await loadModule("../src/lib/motionPresentation.ts");
const {
  motionControlSpecs,
  MOTION_CANVAS,
  MOTION_HIT_MIN_PX,
  peripheralShellSpec,
  triggerHeatCells,
  stickTipPosition,
  heatCellColor,
} = lib;

function assertHitLayout(specs, kind) {
  const canvas = MOTION_CANVAS[kind];
  for (const c of specs) {
    assert.ok(
      c.hit.w >= MOTION_HIT_MIN_PX && c.hit.h >= MOTION_HIT_MIN_PX,
      `${kind} ${c.shortLabel} hit ≥44`,
    );
    assert.ok(
      c.hit.x >= 0 &&
        c.hit.y >= 0 &&
        c.hit.x + c.hit.w <= canvas.width &&
        c.hit.y + c.hit.h <= canvas.height,
      `${kind} ${c.shortLabel} hit 在画布内`,
    );
  }
  for (let i = 0; i < specs.length; i++) {
    for (let j = i + 1; j < specs.length; j++) {
      const a = specs[i],
        b = specs[j];
      const overlap =
        a.hit.x < b.hit.x + b.hit.w &&
        b.hit.x < a.hit.x + a.hit.w &&
        a.hit.y < b.hit.y + b.hit.h &&
        b.hit.y < a.hit.y + a.hit.h;
      assert.equal(
        overlap,
        false,
        `${kind}: ${a.shortLabel} 与 ${b.shortLabel} 不可重叠`,
      );
    }
  }
}

test("mouse 9 码完整、标签与分列、hit≥44 不重叠", () => {
  const mouse = motionControlSpecs("mouse");
  assert.equal(mouse.length, 9);
  assert.deepEqual(
    [...mouse.map((c) => c.code)].sort((a, b) => a - b),
    [1, 2, 3, 4, 5, 6, 7, 8, 9],
  );
  assertHitLayout(mouse, "mouse");
  const by = (code) => mouse.find((c) => c.code === code);
  const cx = (c) => c.hit.x + c.hit.w / 2;
  assert.ok(cx(by(1)) < MOTION_CANVAS.mouse.width / 2);
  assert.ok(cx(by(2)) > MOTION_CANVAS.mouse.width / 2);
});

test("mouse 左右键 hit 等于区域 bbox（§4.1）", () => {
  const mouse = motionControlSpecs("mouse");
  const by = (code) => mouse.find((c) => c.code === code);
  assert.deepEqual(by(1).hit, { x: 34, y: 10, w: 104, h: 174 });
  assert.deepEqual(by(2).hit, { x: 182, y: 10, w: 104, h: 174 });
  // cx(1) < 160 < cx(2) 仍成立（视觉中心）
  assert.ok(by(1).cx < 160 && 160 < by(2).cx);
});

test("MouseDeviceDiagram：无 mainButton 全画布分支，按钮盒取 spec.hit", async () => {
  const src = await loadComponentSource("../src/components/MouseDeviceDiagram.tsx");
  assert.doesNotMatch(src, /mainButton/);
  // 不得再用 size.width/size.height 作按钮盒（画布容器仍可用）
  assert.doesNotMatch(
    src,
    /left:\s*mainButton\s*\?\s*0|width:\s*mainButton\s*\?\s*size\.width/,
  );
  // 按钮定位统一用 hit
  assert.match(src, /left:\s*spec\.hit\.x/);
  assert.match(src, /top:\s*spec\.hit\.y/);
  assert.match(src, /width:\s*spec\.hit\.w/);
  assert.match(src, /height:\s*spec\.hit\.h/);
  assert.match(src, /focus-visible/);
  assert.match(src, /focusCode/);
});

test("gamepad 17 码完整、ABXY 菱形正确、hit≥44 不重叠", () => {
  const gp = motionControlSpecs("gamepad");
  assert.equal(gp.length, 17);
  assert.deepEqual(
    [...gp.map((c) => c.code)].sort((a, b) => a - b),
    [...Array(17).keys()].map((i) => i + 1),
  );
  assertHitLayout(gp, "gamepad");
  const by = (code) => gp.find((c) => c.code === code);
  const cx = (c) => c.hit.x + c.hit.w / 2;
  const cy = (c) => c.hit.y + c.hit.h / 2;
  assert.equal(by(1).shortLabel, "A");
  assert.equal(by(2).shortLabel, "B");
  assert.equal(by(3).shortLabel, "Y");
  assert.equal(by(4).shortLabel, "X");
  assert.ok(cy(by(3)) < cy(by(4)), "Y 上");
  assert.ok(cy(by(1)) > cy(by(4)), "A 下");
  assert.ok(cx(by(4)) < cx(by(2)), "X 左 / B 右");
  assert.equal(cx(by(3)), cx(by(1)));
  assert.equal(cy(by(4)), cy(by(2)));
});

test("摇杆 Y 向上 / 对角径向投影（stickTipPosition）", () => {
  const wells = peripheralShellSpec("gamepad").stickWells;
  assert.equal(wells.length, 2);
  const left = wells.find((w) => w.code === 12);
  const up = stickTipPosition(left, 0, 1);
  const down = stickTipPosition(left, 0, -1);
  assert.ok(up.y < left.cy, "前推画在上方");
  assert.ok(down.y > left.cy, "后拉画在下方");
  const diag = stickTipPosition(left, 2, 2);
  const expected = stickTipPosition(left, Math.SQRT1_2, Math.SQRT1_2);
  assert.ok(Math.abs(diag.x - expected.x) < 1e-9);
  assert.ok(Math.abs(diag.y - expected.y) < 1e-9);
  // 投影后仍在井半径内
  assert.ok(Math.hypot(diag.x - left.cx, diag.y - left.cy) <= left.radius + 1e-9);
});

test("GamepadDeviceDiagram：摇杆用 stickTipPosition、LT/RT 显示整数百分比、Guide 不支持实时", async () => {
  const src = await loadComponentSource("../src/components/GamepadDeviceDiagram.tsx");
  assert.match(src, /stickTipPosition/);
  assert.match(src, /triggerPct|Math\.round/);
  assert.match(src, /不支持实时/);
  assert.match(src, /leftTrigger|rightTrigger/);
  assert.match(src, /data-trigger-pct|trigger-pct/);
  // 不把历史 bin 当实时行程
  assert.doesNotMatch(stripComments(src), /selectedBin/);
});

test("MouseDeviceDiagram / GamepadDeviceDiagram：无常驻累计数字、消费 live held/pulse", async () => {
  for (const file of [
    "MouseDeviceDiagram.tsx",
    "GamepadDeviceDiagram.tsx",
  ]) {
    const src = await loadComponentSource(`../src/components/${file}`);
    assert.doesNotMatch(src, /peripheral-count/);
    assert.doesNotMatch(src, /fmtCompact/);
    assert.match(src, /heldCodes/);
    assert.match(src, /pulseCodes/);
    assert.match(src, /is-live/);
    assert.match(src, /historyState/);
    assert.match(src, /历史次数暂不可用/);
  }
});

test("PeripheralControlsStats：委托图组件、历史 loading 不遮整图、追加 live props", async () => {
  const src = await loadComponentSource(
    "../src/components/PeripheralControlsStats.tsx",
  );
  assert.match(src, /MouseDeviceDiagram/);
  assert.match(src, /GamepadDeviceDiagram/);
  assert.match(src, /live\?/);
  assert.match(src, /historyState/);
  assert.match(src, /liveToolbar/);
  // 不再用整卡 Skeleton 盖住分布图
  assert.doesNotMatch(src, /Skeleton/);
  assert.match(src, /历史次数暂不可用/);
});

test("DeviceLayoutStats：键盘保留 roving、追加 live/history、loading 不整图遮挡", async () => {
  const src = await loadComponentSource("../src/components/DeviceLayoutStats.tsx");
  assert.match(src, /nextControlId/);
  assert.match(src, /live\?/);
  assert.match(src, /historyState/);
  assert.match(src, /is-live/);
  assert.match(src, /is-pulse/);
  assert.match(src, /历史次数暂不可用/);
  // 不再在 loading 时用大骨架替换整图
  assert.doesNotMatch(
    stripComments(src),
    /loading\s*\?\s*\([\s\S]*skeleton[\s\S]*\)\s*:/,
  );
});

test("TriggerHeatmap：仅历史 props、无实时字段、空记录文案、101 格共同色标", async () => {
  const src = await loadComponentSource("../src/components/TriggerHeatmap.tsx");
  // Props 合同：不得出现实时字段（aria-live 除外）
  assert.doesNotMatch(src, /DeviceLiveView/);
  assert.doesNotMatch(src, /heldCodes|pulseCodes|\banalog\b/);
  assert.doesNotMatch(src, /live\?:|\blive:/);
  const propsBlock = src.match(/export interface TriggerHeatmapProps \{([\s\S]*?)\n\}/)[1];
  assert.doesNotMatch(propsBlock, /live/);
  assert.match(propsBlock, /summary:\s*TriggerHeatSummary/);
  assert.match(propsBlock, /scaleMaxSeconds/);
  assert.match(src, /从新版开始记录/);
  assert.match(src, /TriggerHeatSummary/);
  assert.match(src, /triggerHeatCells/);
  assert.match(src, /scaleMaxSeconds/);

  const values = new Array(101).fill(0);
  values[10] = 16;
  values[90] = 16;
  const left = triggerHeatCells(values, 64);
  const right = triggerHeatCells(values, 64);
  assert.equal(left[10].intensity, right[90].intensity);
  assert.equal(left[10].intensity, 0.5);
  assert.equal(heatCellColor(0).startsWith("rgba("), true);
});

async function loadThemeCssBundle() {
  // S3：theme.css 只留 @import；把引用的 styles/* 按顺序拼回再断言
  const themePath = new URL("../src/theme.css", import.meta.url);
  const theme = await readFile(themePath, "utf8");
  const imports = [...theme.matchAll(/@import\s+"(\.\/styles\/[^"]+)"\s*;/g)].map(
    (m) => m[1],
  );
  assert.ok(imports.length > 0, "theme.css 应引用 styles/*");
  const parts = await Promise.all(
    imports.map((rel) => readFile(new URL(rel, themePath), "utf8")),
  );
  return parts.join("");
}

test("theme.css：实时 token 与 live/pulse/扳机热力样式就绪", async () => {
  const css = await loadThemeCssBundle();
  assert.match(css, /--input-live:\s*#0e9a76/);
  assert.match(css, /--input-live-soft:\s*#dcf5ec/);
  assert.match(css, /\.peripheral-button\.is-live|\.is-live/);
  assert.match(css, /\.is-pulse/);
  assert.match(css, /\.trigger-heat-bar/);
  assert.match(css, /\.stick-tip-dot/);
  assert.match(css, /prefers-reduced-motion/);
});

test("画布尺寸保持 mouse 320×360、gamepad 620×380", () => {
  assert.deepEqual(MOTION_CANVAS.mouse, { width: 320, height: 360 });
  assert.deepEqual(MOTION_CANVAS.gamepad, { width: 620, height: 380 });
});
