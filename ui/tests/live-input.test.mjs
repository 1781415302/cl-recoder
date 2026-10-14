// S7 回归（input-live-trigger §4.6）：LiveInputStore / 投影 / mock / toolbar / hook 签名。
//
// 验收：fake clock/transport single-flight、100ms pulse、500ms 过期、1s 失败重试、
// 旧 cleanup/迟到 reply、隐藏恢复不回放、原名/来源/slot 匹配、日期无参数。
import assert from "node:assert/strict";
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

async function loadModule(relPath) {
  const src = await readFile(new URL(relPath, import.meta.url), "utf8");
  const js = ts.transpileModule(src, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2020 },
  }).outputText;
  return import("data:text/javascript;base64," + Buffer.from(js).toString("base64"));
}

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
    return {};
  };
  new Function("require", "module", "exports", js)(fakeRequire, mod, mod.exports);
  return { exports: mod.exports, required, js };
}

const flush = () => new Promise((r) => queueMicrotask(r));
async function flushUntil(cond, label, max = 200) {
  for (let i = 0; i < max && !cond(); i += 1) await flush();
  assert.ok(cond(), `等待超时：${label}`);
}

/** 可推进假时钟 + 记账 timer */
function fakeClock(startMs = 0) {
  let nowMs = startMs;
  let nextId = 1;
  const timers = new Map();
  return {
    nowMs: () => nowMs,
    setTimeout(fn, ms) {
      const id = nextId++;
      timers.set(id, { fn, at: nowMs + ms });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
    pendingCount: () => timers.size,
    advance(ms) {
      const target = nowMs + ms;
      for (;;) {
        let next = null;
        for (const t of timers.values()) {
          if (t.at <= target && (next === null || t.at < next.at)) next = t;
        }
        if (!next) {
          nowMs = target;
          return;
        }
        nowMs = next.at;
        // 触发：先删再跑，避免 clear 同 id
        for (const [id, t] of timers) {
          if (t === next) {
            timers.delete(id);
            break;
          }
        }
        next.fn();
      }
    },
  };
}

function deviceKey(kind, vid, pid, name) {
  return { kind, vid, pid, name };
}

function okReply(overrides = {}) {
  const snapshot = {
    sessionId: "sess-1",
    sampleSeq: "1",
    resetEpoch: "0",
    statisticsPaused: false,
    captureState: "active",
    rawInputAgeMs: 10,
    sources: [],
    ...overrides.snapshot,
  };
  return {
    protocolVersion: 1,
    snapshot: overrides.snapshot === null ? null : snapshot,
    error: overrides.error ?? null,
  };
}

const { LiveInputStore } = await loadModule("../src/api/liveInputStore.ts");
const presentation = await loadModule("../src/lib/liveInputPresentation.ts");
const {
  createLiveProjectionState,
  updateLiveProjection,
  sameDevice,
  projectStick,
  targetStableKey,
  nextPulseDelayMs,
} = presentation;

const mockMod = await loadModule("../src/api/mock.ts");

/* ---------- Store：single-flight / stale / retry / ownership ---------- */

test("store：single-flight——in-flight 期间不发第二笔；完成后才下一轮", async () => {
  const clock = fakeClock(1000);
  let starts = 0;
  let resolveFetch;
  const fetchFn = () => {
    starts += 1;
    return new Promise((resolve) => {
      resolveFetch = resolve;
    });
  };
  const store = new LiveInputStore(fetchFn, clock);
  store.start();
  await flushUntil(() => starts === 1, "首笔已发");
  clock.advance(40);
  clock.advance(40);
  assert.equal(starts, 1, "in-flight 时不发第二笔");
  resolveFetch(okReply({ snapshot: { sampleSeq: "1", sources: [] } }));
  await flushUntil(() => store.getSnapshot().state === "ready", "ready");
  assert.equal(starts, 1);
  clock.advance(40);
  await flushUntil(() => starts === 2, "完成后下一笔");
  store.stop(1);
});

test("store：失败立刻 unavailable 并清态，1s 后再试", async () => {
  const clock = fakeClock(0);
  let starts = 0;
  const fetchFn = async () => {
    starts += 1;
    if (starts === 1) throw new Error("boom");
    return okReply({ snapshot: { sampleSeq: String(starts), sources: [] } });
  };
  const store = new LiveInputStore(fetchFn, clock);
  const token = store.start();
  await flushUntil(() => store.getSnapshot().state === "unavailable", "失败清态");
  assert.equal(store.getSnapshot().reply, null);
  assert.equal(starts, 1);
  clock.advance(999);
  await flush();
  assert.equal(starts, 1, "未满 1s 不重试");
  clock.advance(1);
  await flushUntil(() => starts === 2, "1s 后重试");
  await flushUntil(() => store.getSnapshot().state === "ready", "重试成功");
  store.stop(token);
});

test("store：结构化 error 立刻 unavailable，1s 重试", async () => {
  const clock = fakeClock(0);
  let starts = 0;
  const fetchFn = async () => {
    starts += 1;
    if (starts === 1) {
      return {
        protocolVersion: 1,
        snapshot: null,
        error: { code: "not_running", message: "采集器未运行" },
      };
    }
    return okReply({ snapshot: { sampleSeq: "2", sources: [] } });
  };
  const store = new LiveInputStore(fetchFn, clock);
  const token = store.start();
  await flushUntil(() => store.getSnapshot().state === "unavailable", "结构化失败");
  assert.equal(store.getSnapshot().reply?.error?.code, "not_running");
  clock.advance(1000);
  await flushUntil(() => store.getSnapshot().state === "ready", "重试成功");
  store.stop(token);
});

test("store：距最后成功 >500ms 自动 unavailable 并清态", async () => {
  const clock = fakeClock(0);
  let gate = true;
  const fetchFn = async () => {
    if (!gate) {
      await new Promise(() => {}); // hang
    }
    return okReply({ snapshot: { sampleSeq: "1", sources: [] } });
  };
  const store = new LiveInputStore(fetchFn, clock);
  const token = store.start();
  await flushUntil(() => store.getSnapshot().state === "ready", "首次 ready");
  gate = false;
  // 推进到 stale：成功后 schedule stale@500；其间 poll 会 hang 在第二笔
  clock.advance(40); // 触发第二笔（hang）
  await flush();
  clock.advance(500);
  await flushUntil(() => store.getSnapshot().state === "unavailable", "500ms stale");
  assert.equal(store.getSnapshot().reply, null);
  assert.equal(store.getSnapshot().receivedAtMs, null);
  store.stop(token);
});

test("store：旧 cleanup 不能 stop 新所有者；迟到 reply 丢弃", async () => {
  const clock = fakeClock(0);
  const resolvers = [];
  let starts = 0;
  const fetchFn = () => {
    starts += 1;
    return new Promise((resolve) => resolvers.push(resolve));
  };
  const store = new LiveInputStore(fetchFn, clock);
  const token1 = store.start();
  await flushUntil(() => starts === 1, "owner1 首笔");
  store.stop(token1);
  assert.equal(store.getSnapshot().state, "off");
  const token2 = store.start();
  assert.equal(store.getSnapshot().state, "connecting");
  // 旧 I/O 仍在飞：不得发第二笔
  assert.equal(starts, 1);
  // 迟到的旧 reply
  resolvers[0](
    okReply({
      snapshot: {
        sessionId: "old",
        sampleSeq: "9",
        sources: [
          {
            kind: "keyboard",
            sourceId: "k1",
            device: deviceKey("keyboard", 1, 1, "Old"),
            digital: { heldCodes: [1], presses: [] },
          },
        ],
      },
    }),
  );
  await flushUntil(() => starts === 2, "旧结果丢弃后立刻服务新所有者");
  // 新所有者的 reply
  resolvers[1](
    okReply({
      snapshot: {
        sessionId: "new",
        sampleSeq: "1",
        sources: [],
      },
    }),
  );
  await flushUntil(() => store.getSnapshot().state === "ready", "新所有者 ready");
  assert.equal(store.getSnapshot().reply.snapshot.sessionId, "new");
  // 旧 token 不能 stop 新所有者
  store.stop(token1);
  assert.equal(store.getSnapshot().state, "ready");
  store.stop(token2);
  assert.equal(store.getSnapshot().state, "off");
});

/* ---------- 投影：pulse / 基线 / 匹配 ---------- */

test("projectStick：径向裁到单位圆，保持方向", () => {
  assert.deepEqual(projectStick({ x: 0.3, y: 0.4 }), { x: 0.3, y: 0.4 });
  const p = projectStick({ x: 3, y: 4 });
  assert.ok(Math.abs(Math.hypot(p.x, p.y) - 1) < 1e-9);
  assert.ok(Math.abs(p.x - 0.6) < 1e-9);
  assert.ok(Math.abs(p.y - 0.8) < 1e-9);
});

test("sameDevice：四字段一致；nickname 不参与", () => {
  const a = deviceKey("keyboard", 0x46d, 0xc31c, "Logitech K845 机械键盘");
  const b = { ...a };
  assert.equal(sameDevice(a, b), true);
  assert.equal(sameDevice(a, { ...a, name: "别名" }), false);
  assert.equal(sameDevice(a, { ...a, vid: 1 }), false);
});

test("投影：选中物理鼠标缺席时不回落到同型号另一来源", () => {
  const mouse = deviceKey("mouse", 1, 2, "Mouse");
  const input = {
    state: "ready", receivedAtMs: 0,
    reply: okReply({ snapshot: { sources: [{
      kind: "mouse", sourceId: "b", sourceKey: "key-B", device: mouse,
      digital: { heldCodes: [1], presses: [{ code: 1, pressSeq: "1", ageMs: 0 }] },
    }] } }),
  };
  for (const device of [mouse, null]) {
    const view = updateLiveProjection(createLiveProjectionState(), input,
      { kind: "mouse", device, sourceKey: "key-A" }, 0);
    assert.equal(view.state, "waiting");
    assert.deepEqual([...view.heldCodes], []);
    assert.deepEqual([...view.pulseCodes], []);
    assert.deepEqual(view.selectedDevice, device);
  }
});

test("投影：失败或过期恢复首帧只建基线，当前 held 立即显示", () => {
  const keyboard = deviceKey("keyboard", 1, 2, "Keyboard");
  const target = { kind: "keyboard", device: keyboard };
  const ready = (seq, held, time, ageMs) => ({
    state: "ready", receivedAtMs: time,
    reply: okReply({ snapshot: { sources: [{
      kind: "keyboard", sourceId: "k", device: keyboard,
      digital: { heldCodes: held, presses: [{ code: 30, pressSeq: String(seq), ageMs }] },
    }] } }),
  });
  for (const failure of [
    { state: "unavailable", receivedAtMs: null, reply: null },
    { state: "unavailable", receivedAtMs: null, reply: { snapshot: null, error: { code: "io", message: "失败" } } },
    { state: "ready", receivedAtMs: 10, reply: { snapshot: null, error: null } },
  ]) {
    const state = createLiveProjectionState();
    updateLiveProjection(state, ready(1, [], 0, 1000), target, 0);
    updateLiveProjection(state, ready(2, [], 5, 0), target, 5);
    assert.notEqual(nextPulseDelayMs(state, 5), null);
    const failed = updateLiveProjection(state, failure, target, 10);
    assert.equal(failed.state, "unavailable");
    assert.equal(nextPulseDelayMs(state, 10), null);
    const restored = updateLiveProjection(state, ready(3, [31], 1010, 800), target, 1010);
    assert.deepEqual([...restored.pulseCodes], []);
    assert.deepEqual([...restored.heldCodes], [31]);
    const next = updateLiveProjection(state, ready(4, [], 1050, 0), target, 1050);
    assert.deepEqual([...next.pulseCodes], [30]);
  }
});

test("投影：首次建立基线不回放旧短按；held 立即显示", () => {
  const state = createLiveProjectionState();
  const kb = deviceKey("keyboard", 1, 2, "KB");
  const input = {
    state: "ready",
    receivedAtMs: 1000,
    reply: okReply({
      snapshot: {
        sessionId: "s1",
        resetEpoch: "0",
        sampleSeq: "1",
        sources: [
          {
            kind: "keyboard",
            sourceId: "k1",
            device: kb,
            digital: {
              heldCodes: [0x1d],
              presses: [
                { code: 0x1d, pressSeq: "5", ageMs: 10 },
                { code: 0x39, pressSeq: "3", ageMs: 20 },
              ],
            },
          },
        ],
      },
    }),
  };
  const view = updateLiveProjection(
    state,
    input,
    { kind: "keyboard", device: kb },
    1000,
  );
  assert.equal(view.state, "ready");
  assert.deepEqual([...view.heldCodes], [0x1d]);
  assert.deepEqual([...view.pulseCodes], [], "基线帧不回放旧短按");
});

test("投影：ready 期间 pressSeq 增加 → 100ms pulse；held 优先", () => {
  const state = createLiveProjectionState();
  const kb = deviceKey("keyboard", 1, 2, "KB");
  const target = { kind: "keyboard", device: kb };
  const base = (pressSeq, held, receivedAtMs) => ({
    state: "ready",
    receivedAtMs,
    reply: okReply({
      snapshot: {
        sessionId: "s1",
        resetEpoch: "0",
        sampleSeq: String(receivedAtMs),
        sources: [
          {
            kind: "keyboard",
            sourceId: "k1",
            device: kb,
            digital: {
              heldCodes: held,
              presses: [{ code: 0x39, pressSeq, ageMs: 5 }],
            },
          },
        ],
      },
    }),
  });
  updateLiveProjection(state, base("1", [], 1000), target, 1000);
  const view = updateLiveProjection(state, base("2", [], 1040), target, 1040);
  assert.deepEqual([...view.pulseCodes], [0x39]);
  assert.ok(nextPulseDelayMs(state, 1040) === 100 || nextPulseDelayMs(state, 1040) === 100);
  // 到期前仍 pulse
  const mid = updateLiveProjection(state, base("2", [], 1040), target, 1100);
  assert.deepEqual([...mid.pulseCodes], [0x39]);
  // 100ms 后过期
  const done = updateLiveProjection(state, base("2", [], 1040), target, 1140);
  assert.deepEqual([...done.pulseCodes], []);
  // held 优先于 pulse
  updateLiveProjection(state, base("3", [0x39], 1200), target, 1200);
  const heldView = updateLiveProjection(state, base("3", [0x39], 1200), target, 1200);
  assert.deepEqual([...heldView.heldCodes], [0x39]);
  assert.ok(!heldView.pulseCodes.includes(0x39));
});

test("投影：新来源/新 code 且 ageMs≤500 可产生一次 pulse", () => {
  const state = createLiveProjectionState();
  const kb = deviceKey("keyboard", 1, 2, "KB");
  const target = { kind: "keyboard", device: kb };
  updateLiveProjection(
    state,
    {
      state: "ready",
      receivedAtMs: 1000,
      reply: okReply({
        snapshot: {
          sessionId: "s1",
          resetEpoch: "0",
          sources: [
            {
              kind: "keyboard",
              sourceId: "k1",
              device: kb,
              digital: { heldCodes: [], presses: [] },
            },
          ],
        },
      }),
    },
    target,
    1000,
  );
  const view = updateLiveProjection(
    state,
    {
      state: "ready",
      receivedAtMs: 1100,
      reply: okReply({
        snapshot: {
          sessionId: "s1",
          resetEpoch: "0",
          sources: [
            {
              kind: "keyboard",
              sourceId: "k1",
              device: kb,
              digital: {
                heldCodes: [],
                presses: [{ code: 0x1e, pressSeq: "1", ageMs: 30 }],
              },
            },
          ],
        },
      }),
    },
    target,
    1100,
  );
  assert.deepEqual([...view.pulseCodes], [0x1e]);
});

test("投影：隐藏恢复（session 不变但重建投影态）不回放", () => {
  const kb = deviceKey("keyboard", 1, 2, "KB");
  const target = { kind: "keyboard", device: kb };
  const mk = (seq) => ({
    state: "ready",
    receivedAtMs: 5000,
    reply: okReply({
      snapshot: {
        sessionId: "s1",
        resetEpoch: "0",
        sources: [
          {
            kind: "keyboard",
            sourceId: "k1",
            device: kb,
            digital: {
              heldCodes: [0x1d],
              presses: [{ code: 0x39, pressSeq: seq, ageMs: 10 }],
            },
          },
        ],
      },
    }),
  });
  const state1 = createLiveProjectionState();
  updateLiveProjection(state1, mk("10"), target, 5000);
  updateLiveProjection(state1, mk("11"), target, 5040);
  // 隐藏：stop 清显示；恢复：新投影态 + 同 session 快照
  const state2 = createLiveProjectionState();
  const resumed = updateLiveProjection(state2, mk("11"), target, 6000);
  assert.deepEqual([...resumed.heldCodes], [0x1d], "held 立即显示");
  assert.deepEqual([...resumed.pulseCodes], [], "不回放隐藏期间短按");
});

test("投影：sessionId/resetEpoch 变化重建基线", () => {
  const state = createLiveProjectionState();
  const kb = deviceKey("keyboard", 1, 2, "KB");
  const target = { kind: "keyboard", device: kb };
  updateLiveProjection(
    state,
    {
      state: "ready",
      receivedAtMs: 1000,
      reply: okReply({
        snapshot: {
          sessionId: "s1",
          resetEpoch: "0",
          sources: [
            {
              kind: "keyboard",
              sourceId: "k1",
              device: kb,
              digital: {
                heldCodes: [],
                presses: [{ code: 0x39, pressSeq: "5", ageMs: 5 }],
              },
            },
          ],
        },
      }),
    },
    target,
    1000,
  );
  const afterReset = updateLiveProjection(
    state,
    {
      state: "ready",
      receivedAtMs: 2000,
      reply: okReply({
        snapshot: {
          sessionId: "s1",
          resetEpoch: "1",
          sources: [
            {
              kind: "keyboard",
              sourceId: "k1",
              device: kb,
              digital: {
                heldCodes: [],
                presses: [{ code: 0x39, pressSeq: "5", ageMs: 5 }],
              },
            },
          ],
        },
      }),
    },
    target,
    2000,
  );
  assert.deepEqual([...afterReset.pulseCodes], [], "epoch 变化只建基线");
});

test("投影：键盘同型号多来源 held 并集；原名匹配", () => {
  const state = createLiveProjectionState();
  const kb = deviceKey("keyboard", 0x46d, 0xc31c, "Logitech K845 机械键盘");
  const view = updateLiveProjection(
    state,
    {
      state: "ready",
      receivedAtMs: 1000,
      reply: okReply({
        snapshot: {
          sessionId: "s1",
          resetEpoch: "0",
          sources: [
            {
              kind: "keyboard",
              sourceId: "a",
              device: kb,
              digital: {
                heldCodes: [0x1d],
                presses: [{ code: 0x1d, pressSeq: "1", ageMs: 10 }],
              },
            },
            {
              kind: "keyboard",
              sourceId: "b",
              device: kb,
              digital: {
                heldCodes: [0x1d, 0x2a],
                presses: [
                  { code: 0x1d, pressSeq: "1", ageMs: 10 },
                  { code: 0x2a, pressSeq: "1", ageMs: 10 },
                ],
              },
            },
            {
              kind: "keyboard",
              sourceId: "c",
              device: deviceKey("keyboard", 9, 9, "Other"),
              digital: {
                heldCodes: [0x39],
                presses: [{ code: 0x39, pressSeq: "1", ageMs: 10 }],
              },
            },
          ],
        },
      }),
    },
    { kind: "keyboard", device: kb },
    1000,
  );
  assert.deepEqual([...view.heldCodes], [0x1d, 0x2a]);
  assert.ok(sameDevice(view.selectedDevice, kb));
});

test("投影：鼠标按 sourceKey 精确匹配", () => {
  const state = createLiveProjectionState();
  const mouse = deviceKey("mouse", 0x46d, 0xc092, "Logitech G102 游戏鼠标");
  const view = updateLiveProjection(
    state,
    {
      state: "ready",
      receivedAtMs: 1000,
      reply: okReply({
        snapshot: {
          sessionId: "s1",
          resetEpoch: "0",
          sources: [
            {
              kind: "mouse",
              sourceId: "m1",
              device: mouse,
              sourceKey: "key-A",
              digital: {
                heldCodes: [1],
                presses: [{ code: 1, pressSeq: "1", ageMs: 10 }],
              },
            },
            {
              kind: "mouse",
              sourceId: "m2",
              device: mouse,
              sourceKey: "key-B",
              digital: {
                heldCodes: [2],
                presses: [{ code: 2, pressSeq: "1", ageMs: 10 }],
              },
            },
          ],
        },
      }),
    },
    { kind: "mouse", device: mouse, sourceKey: "key-B" },
    1000,
  );
  assert.deepEqual([...view.heldCodes], [2]);
});

test("投影：手柄默认最低可用 slot；连接列表标签 手柄N", () => {
  const state = createLiveProjectionState();
  const pad = deviceKey("gamepad", 0, 0, "XInput 手柄");
  const view = updateLiveProjection(
    state,
    {
      state: "ready",
      receivedAtMs: 1000,
      reply: okReply({
        snapshot: {
          sessionId: "s1",
          resetEpoch: "0",
          sources: [
            {
              kind: "gamepad",
              sourceId: "p1",
              device: pad,
              slot: 1,
              connectionId: "2",
              availability: "ready",
              ageMs: 10,
              digital: { heldCodes: [2], presses: [] },
              analog: {
                leftStick: { x: 2, y: 0 },
                rightStick: { x: 0, y: 0 },
                leftTrigger: 0.5,
                rightTrigger: 0,
              },
              unsupportedCodes: [11],
            },
            {
              kind: "gamepad",
              sourceId: "p0",
              device: pad,
              slot: 0,
              connectionId: "1",
              availability: "ready",
              ageMs: 10,
              digital: { heldCodes: [1], presses: [] },
              analog: {
                leftStick: { x: 0, y: 0 },
                rightStick: { x: 0, y: 0 },
                leftTrigger: 0,
                rightTrigger: 0,
              },
              unsupportedCodes: [],
            },
          ],
        },
      }),
    },
    { kind: "gamepad", device: pad, sourceId: null },
    1000,
  );
  assert.equal(view.selectedSourceId, "p0");
  assert.deepEqual([...view.heldCodes], [1]);
  assert.deepEqual(
    view.connections.map((c) => c.label),
    ["手柄1", "手柄2"],
  );
  assert.ok(Math.abs(view.analog.leftStick.x) <= 1 + 1e-9, "摇杆已径向裁剪");
});

test("投影：sources 空 → waiting；target=null → off", () => {
  const state = createLiveProjectionState();
  const waiting = updateLiveProjection(
    state,
    {
      state: "ready",
      receivedAtMs: 1000,
      reply: okReply({
        snapshot: { sessionId: "s1", resetEpoch: "0", sources: [] },
      }),
    },
    { kind: "keyboard", device: null },
    1000,
  );
  assert.equal(waiting.state, "waiting");
  const off = updateLiveProjection(state, { state: "off", reply: null, receivedAtMs: null }, null, 1000);
  assert.equal(off.state, "off");
});

test("targetStableKey：按值键而非对象引用", () => {
  const d = deviceKey("mouse", 1, 2, "M");
  const a = { kind: "mouse", device: d, sourceKey: "x" };
  const b = { kind: "mouse", device: { ...d }, sourceKey: "x" };
  assert.equal(targetStableKey(a), targetStableKey(b));
  assert.notEqual(
    targetStableKey(a),
    targetStableKey({ ...a, sourceKey: "y" }),
  );
});

/* ---------- mock / client / toolbar / hook 签名 ---------- */

test("mockLiveInputSnapshot：确定性相位含 held/短按/摇杆/扳机/断连；保留 trigger 历史字段未破坏", async () => {
  mockMod.__resetMockLiveInputForTests();
  const phases = [];
  for (let i = 0; i < 10; i++) {
    phases.push(await mockMod.mockLiveInputSnapshot());
  }
  assert.ok(phases.some((r) => r.snapshot?.sources.some((s) => s.kind === "keyboard" && s.digital.heldCodes.length)));
  assert.ok(phases.some((r) => r.snapshot?.sources.some((s) => s.kind === "mouse" && s.sourceKey)));
  assert.ok(
    phases.some((r) =>
      r.snapshot?.sources.some(
        (s) => s.kind === "gamepad" && s.analog && s.analog.leftTrigger >= 0,
      ),
    ),
  );
  assert.ok(phases.some((r) => r.error?.code === "not_running"));
  // S6 历史字段仍在
  const motion = await mockMod.mockGetGamepadMotion(5, "2026-10-01", "2026-10-01");
  assert.equal(motion.triggers.binCount, 101);
  assert.equal(motion.triggers.left.dwellSeconds.length, 101);
  const sources = await mockMod.mockGetMouseSources();
  assert.ok(sources.sources.every((s) => typeof s.sourceKey === "string"));
});

test("LiveInputToolbar：导出组件且不查询 API", async () => {
  const { exports, required } = await loadComponentModule(
    "../src/components/LiveInputToolbar.tsx",
  );
  assert.equal(typeof exports.LiveInputToolbar, "function");
  assert.ok(!required.some((s) => /api\/(client|mock|queries)/.test(s)));
});

test("useLiveInput：仅 (target, enabled) 两参数——无日期范围", async () => {
  const src = await readFile(
    new URL("../src/lib/useLiveInput.ts", import.meta.url),
    "utf8",
  );
  assert.match(src, /export function useLiveInput\s*\([\s\S]*?target:\s*LiveTarget\s*\|\s*null[\s\S]*?enabled:\s*boolean[\s\S]*?\)/);
  assert.equal((src.match(/export function useLiveInput/g) || []).length, 1);
  assert.doesNotMatch(src, /from:\s*string|to:\s*string|dateRange|StatisticsRange/);
  // 稳定值键判断目标变化
  assert.match(src, /targetStableKey/);
  assert.match(src, /targetKey/);
});

test("client.getLiveInputSnapshot 存在于源码", async () => {
  const src = await readFile(
    new URL("../src/api/client.ts", import.meta.url),
    "utf8",
  );
  assert.match(src, /export function getLiveInputSnapshot/);
  assert.match(src, /get_live_input_snapshot/);
  assert.match(src, /mockLiveInputSnapshot/);
});
