// S10 回归（usability-runtime-v3 §8.2 S10）：最终 page 契约 / QueryObserver 集成。
//
// 用 Node 内建 test + 真实 @tanstack/react-query（5.104）QueryObserver 验证页面运行时合同：
// - 隐藏后不周期 fetch：隐藏启动 0 请求；活动期 refetchInterval 轮询真实生效；再次隐藏轮询即停；
// - 恢复跨午夜恰好一次更新：新 day 恰好一次请求，旧 day 只被标 stale 不再请求；
// - 固定范围不漂：跨午夜 key 与请求标量不变，历史固定范围恢复后不轮询；
// - 跨设备旧数据不绘制：切设备先显示加载态（无 keepPreviousData/placeholder 旧数据），
//   解析后呈现新设备数据，请求参数绑定 (deviceId, from, to, limit)。
//
// 页面是 React 组件而仓库 harness 无渲染器（渲染依赖被"不新增第三方依赖"禁止，同 S7/S8/S9
// 先例）。useQuery 每次渲染调用 observer.setOptions（@tanstack/react-query 5.104
// useBaseQuery.js:36 同一路径），故页面接线以"逐字转录 + 行号"钉在 QueryObserver 接缝上：
// - overview key/interval：ui/src/api/queries.ts:27-29（rangeIncludesToday → POLL_TODAY_MS）；
// - topKeys key/enabled 与 statsPolicy：ui/src/components/DeviceStatsPage.tsx:56,79-84；
// - 恢复时 willActivate 失效：ui/src/lib/AppActivityProvider.tsx:29-34（refetchType:none）。
// 范围派生与活动状态用真实纯模块（statisticsRange/queryPolicy/appActivity，零运行时依赖、
// 只 type import）经 transpileModule → data URL 直载（同 app-activity.test.mjs 惯例）。
// §8.3：带运行时依赖的模块（queries.ts 的 react hook、client.ts 的 Tauri invoke）不在 Node
// 直载、不以 data URL 硬解相对路径——其接线契约由上述转录承担，形状回归由 tsc/vite 构建覆盖。
import assert from "node:assert/strict";
import { test, afterEach } from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

// QueryObserver 的 refetchInterval/staleTime 定时器由 query-core 的 isServer 守卫调度
// （isServer = typeof window === "undefined"，模块级常量），纯 Node 进程不会排任何 interval。
// 测试在加载 react-query 前伪装浏览器宿主（同 app-activity.test.mjs），使轮询真实生效可供断言。
if (typeof globalThis.window === "undefined") globalThis.window = globalThis;

const { QueryClient, QueryObserver } = await import("@tanstack/react-query");

async function loadModule(relPath) {
  const src = await readFile(new URL(relPath, import.meta.url), "utf8");
  const js = ts.transpileModule(src, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2020 },
  }).outputText;
  return await import("data:text/javascript;base64," + Buffer.from(js).toString("base64"));
}

const { AppActivityStore } = await loadModule("../src/lib/appActivity.ts");
const { uiQueryPolicy } = await loadModule("../src/api/queryPolicy.ts");
const { resolveStatisticsRange, selectStatisticsRange } = await loadModule("../src/lib/statisticsRange.ts");

/* ===== 页面接线逐字转录（行号见各注释；生产接线变化必须同步改这里） ===== */

/** 今日/总览数据轮询间隔（ui/src/api/queries.ts:17 POLL_TODAY_MS） */
const POLL_TODAY_MS = 500;

/** 闭区间 [from, to] 是否包含当前 today（ui/src/api/queries.ts:20-22，ISO 日字典序比较） */
function rangeIncludesToday(range, today) {
  return range.from <= today && today <= range.to;
}

/** useOverview 查询构造（ui/src/api/queries.ts:26-31）：key 绑 (from, to)；
 * interval 仅范围含当前 today 时传入（历史固定范围不轮询），策略来自真实 uiQueryPolicy */
function overviewOptions(range, activity, queryFn) {
  return {
    queryKey: ["overview", range.from, range.to],
    queryFn: () => queryFn(range.from, range.to),
    ...uiQueryPolicy(activity.active, rangeIncludesToday(range, activity.today) ? POLL_TODAY_MS : undefined),
  };
}

/** 设备输入专用 limit——u16 完整值域（ui/src/components/DeviceStatsPage.tsx:39 TOP_KEYS_LIMIT） */
const TOP_KEYS_LIMIT = 65_536;

/** 设备页 topKeys 查询构造（ui/src/components/DeviceStatsPage.tsx:54-84）：
 * statsPolicy 仅范围含 today 时 1s 轮询；跨设备不使用 keepPreviousData（先显示加载态） */
function topKeysOptions(deviceId, range, activity, queryFn) {
  const includesToday = rangeIncludesToday(range, activity.today);
  const statsPolicy = uiQueryPolicy(activity.active, includesToday ? 1_000 : undefined);
  return {
    queryKey: ["topKeys", deviceId, range.from, range.to, TOP_KEYS_LIMIT],
    queryFn: () => queryFn(deviceId, range.from, range.to, TOP_KEYS_LIMIT),
    ...statsPolicy,
    enabled: deviceId !== null && statsPolicy.enabled,
  };
}

/** 恢复可见时的失效（ui/src/lib/AppActivityProvider.tsx:29-34 markUiOwnedStale 逐字）：
 * 把 meta.uiOwned=true 的缓存标 stale 但不立即发请求（refetchType:none）——随后的 active
 * 发布才让各查询以新范围恢复，恢复动作本身不产生额外请求 */
function markUiOwnedStale(client) {
  void client.invalidateQueries({
    predicate: (query) => query.meta?.uiOwned === true,
    refetchType: "none",
  });
}

/* ===== 假桥 / 假时钟 / 假 loader（app-activity.test.mjs 同款形状的最小子集） ===== */

/** 假桥：emit 模拟原生活动事件（revision 递增后广播；原生状态是唯一权威） */
function fakeBridge() {
  const state = {
    revision: 0,
    active: false,
    subscribeCalls: 0,
    snapshotCalls: 0,
    handlers: new Set(),
  };
  return {
    state,
    emit(active) {
      state.revision += 1;
      state.active = active;
      for (const handler of [...state.handlers]) handler({ revision: state.revision, active });
    },
    subscribe(handler) {
      state.subscribeCalls += 1;
      state.handlers.add(handler);
      return Promise.resolve(() => {
        state.handlers.delete(handler);
      });
    },
    snapshot() {
      state.snapshotCalls += 1;
      return Promise.resolve({ revision: state.revision, active: state.active });
    },
  };
}

/** 假时钟：可推进的本地时间 + 记账 timer（store 的午夜 timer 到点顺序触发） */
function fakeClock(startIso) {
  let nowMs = Date.parse(startIso); // 无时区 ISO date-time → 本地时间
  let nextId = 1;
  const timers = new Map(); // id -> { fn, at }
  return {
    now: () => new Date(nowMs),
    setTimeout(fn, ms) {
      const id = nextId++;
      timers.set(id, { fn, at: nowMs + ms });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
    /** 推进 ms；途中按到点顺序触发 timer */
    advance(ms) {
      const target = nowMs + ms;
      for (;;) {
        let earliest = null;
        for (const [id, t] of timers) {
          if (t.at <= target && (earliest === null || t.at < earliest.t.at)) earliest = { id, t };
        }
        if (!earliest) break;
        timers.delete(earliest.id);
        nowMs = Math.max(nowMs, earliest.t.at);
        earliest.t.fn();
      }
      nowMs = target;
    },
  };
}

/** overview 假后端：记录每次 (from, to)，返回 Overview 形状载荷 */
function makeOverviewLoader() {
  const calls = [];
  const fn = (from, to) => {
    calls.push([from, to]);
    return Promise.resolve({ days: [], today: { keys: 1, clicks: 0, gamepad: 0 }, devices: [] });
  };
  return { calls, fn };
}

/** topKeys 假后端：记录每次 (deviceId, from, to, limit)，行身份含设备标记以断言"画的是谁" */
function makeTopKeysLoader() {
  const calls = [];
  const fn = (deviceId, from, to, limit) => {
    calls.push([deviceId, from, to, limit]);
    return Promise.resolve([{ code: deviceId, total: deviceId * 1000, label: `设备${deviceId}` }]);
  };
  return { calls, fn };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 轮询等待条件成立（真实 setInterval 时刻有调度抖动，用截止时间替代固定 sleep 断言） */
async function waitUntil(cond, label, deadlineMs = 3_000) {
  for (let waited = 0; waited <= deadlineMs; waited += 25) {
    if (cond()) return;
    await sleep(25);
  }
  assert.ok(cond(), `等待超时：${label}`);
}

/** 等待 observer 到达稳定成功态（success 且不在请求中）。
 * uiQueryPolicy 恒 staleTime=0 → isStale 恒 true，不能用 wp-mouse-query 的 !isStale 判据；
 * 纯缓存命中时订阅不触发回调 → 先查 getCurrentResult()（同 wp-mouse-query 实测语义）。 */
function onceSuccess(observer) {
  const current = observer.getCurrentResult();
  if (current.isSuccess && current.fetchStatus === "idle") return Promise.resolve(current);
  return new Promise((resolve, reject) => {
    const unsub = observer.subscribe((result) => {
      if (result.isSuccess && result.fetchStatus === "idle") {
        unsub();
        resolve(result);
      } else if (result.isError) {
        unsub();
        reject(result.error);
      }
    });
  });
}

/* ===== 每测试资源：QueryClient + 真实 AppActivityStore（假桥/假时钟）+ observer ===== */

let qc = null;
const observers = [];
const stores = [];

function makeClient() {
  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return qc;
}

/** 真实 AppActivityStore + 假桥/假时钟；onWillActivate 与 AppActivityProvider.tsx:42 的
 * 构造一致：进入 active 先 markUiOwnedStale 再发布 */
function makeStore(bridge, clock) {
  const store = new AppActivityStore(bridge, clock, () => markUiOwnedStale(qc));
  stores.push(store);
  return store;
}

function track(options) {
  const observer = new QueryObserver(qc, options);
  observers.push(observer);
  return observer;
}

// 每个测试后销毁 observer、停止 store、清空 client：避免 interval/gcTime/午夜 timer 把进程挂住
afterEach(() => {
  for (const observer of observers) observer.destroy();
  observers.length = 0;
  for (const store of stores) store.stop();
  stores.length = 0;
  if (qc) {
    qc.clear();
    qc = null;
  }
});

const DAY = "2026-10-02";
const NEXT_DAY = "2026-10-03";

test("隐藏后不周期 fetch：隐藏启动 0 请求；活动期轮询生效；再隐藏即停", { timeout: 15_000 }, async () => {
  const clock = fakeClock(`${DAY}T10:00:00`);
  const bridge = fakeBridge();
  makeClient();
  const store = makeStore(bridge, clock);
  await store.start(); // 隐藏启动：revision=0/inactive

  // 页面默认 selection（useStatisticsRange.ts:25）以 activity.today 派生
  const range = resolveStatisticsRange({ mode: "today" }, store.getSnapshot().today);
  const loader = makeOverviewLoader();
  const hiddenOptions = overviewOptions(range, store.getSnapshot(), loader.fn);
  assert.equal(hiddenOptions.enabled, false, "隐藏启动：activity gating 关闭查询");
  assert.equal(hiddenOptions.refetchInterval, false, "隐藏启动：无轮询");
  const observer = track(hiddenOptions);
  assert.equal(observer.getCurrentResult().fetchStatus, "idle");
  const unsub = observer.subscribe(() => {});
  await sleep(150);
  assert.equal(loader.calls.length, 0, "隐藏后不得发起任何请求（含周期请求）");

  bridge.emit(true); // 原生事件恢复可见
  const activeOptions = overviewOptions(range, store.getSnapshot(), loader.fn);
  assert.equal(activeOptions.refetchInterval, POLL_TODAY_MS, "今日范围 + 活动期 → 轮询间隔生效");
  observer.setOptions(activeOptions); // useQuery 重渲染同一路径（useBaseQuery.js:36）
  await onceSuccess(observer);
  assert.equal(loader.calls.length, 1);
  await waitUntil(() => loader.calls.length >= 3, "活动期周期请求生效（首轮 + ≥2 次周期请求）");

  bridge.emit(false); // 最小化/隐藏
  observer.setOptions(overviewOptions(range, store.getSnapshot(), loader.fn));
  const frozen = loader.calls.length;
  await sleep(650);
  assert.equal(loader.calls.length, frozen, "隐藏后周期 fetch 必须停止");
  assert.equal(observer.getCurrentResult().fetchStatus, "idle");
  unsub();
});

test("恢复跨午夜一次更新：新 day 恰好一次请求，旧 day 只标 stale 不再请求", { timeout: 15_000 }, async () => {
  const clock = fakeClock(`${DAY}T23:00:00`);
  const bridge = fakeBridge();
  makeClient();
  const store = makeStore(bridge, clock);
  await store.start();
  bridge.emit(true); // 活动于 DAY

  const selection = { mode: "today" }; // 页面默认（useStatisticsRange.ts:25）
  const rangeOf = () => resolveStatisticsRange(selection, store.getSnapshot().today);
  const loader = makeOverviewLoader();
  const observer = track(overviewOptions(rangeOf(), store.getSnapshot(), loader.fn));
  const unsub = observer.subscribe(() => {});
  await onceSuccess(observer);
  assert.deepEqual(loader.calls, [[DAY, DAY]]);

  bridge.emit(false); // 隐藏
  observer.setOptions(overviewOptions(rangeOf(), store.getSnapshot(), loader.fn));
  clock.advance(2 * 60 * 60 * 1000); // 隐藏期间跨过当地午夜（模拟休眠/隔日恢复）
  assert.equal(store.getSnapshot().today, DAY, "隐藏期间不重算 today（无 DOM/timer，恢复事件才重算）");

  bridge.emit(true); // 恢复：同一发布先 onWillActivate（标 stale）再 active=true/today 重算
  assert.equal(store.getSnapshot().today, NEXT_DAY, "恢复发布内 today 已重算");
  assert.equal(qc.getQueryState(["overview", DAY, DAY])?.isInvalidated, true, "willActivate 已把 uiOwned 缓存标 stale");
  assert.equal(loader.calls.length, 1, "标 stale 不得立即发请求（refetchType:none）");

  observer.setOptions(overviewOptions(rangeOf(), store.getSnapshot(), loader.fn)); // 页面以新范围重渲染
  assert.equal(observer.getCurrentResult().fetchStatus, "fetching");
  await sleep(150); // < 轮询间隔：排除周期请求干扰
  assert.deepEqual(
    loader.calls,
    [
      [DAY, DAY],
      [NEXT_DAY, NEXT_DAY],
    ],
    "恢复跨午夜后恰好一次新 day 请求，旧 day 不再请求",
  );

  await waitUntil(() => loader.calls.length >= 4, "新 day 轮询恢复（≥3 次新 day 请求）");
  assert.ok(
    loader.calls.slice(1).every(([from, to]) => from === NEXT_DAY && to === NEXT_DAY),
    "此后只有新 day 被请求（旧 day 保持 1 次）",
  );
  unsub();
});

test("固定范围不漂：跨午夜 key 与请求标量不变，历史固定范围不轮询", { timeout: 15_000 }, async () => {
  const clock = fakeClock(`${DAY}T23:00:00`);
  const bridge = fakeBridge();
  makeClient();
  const store = makeStore(bridge, clock);
  await store.start();
  bridge.emit(true);

  // 手选历史固定范围（不含今日）：selectStatisticsRange(range, "fixed")
  const picked = { from: "2026-09-15", to: "2026-10-01" };
  const selection = selectStatisticsRange(picked, "fixed");
  const rangeOf = () => resolveStatisticsRange(selection, store.getSnapshot().today);
  const loader = makeOverviewLoader();

  const initialOptions = overviewOptions(rangeOf(), store.getSnapshot(), loader.fn);
  assert.equal(initialOptions.refetchInterval, false, "历史固定范围活动期也不轮询");
  const observer = track(initialOptions);
  const unsub = observer.subscribe(() => {});
  await onceSuccess(observer);
  assert.deepEqual(loader.calls, [["2026-09-15", "2026-10-01"]]);

  bridge.emit(false); // 隐藏
  observer.setOptions(overviewOptions(rangeOf(), store.getSnapshot(), loader.fn));
  clock.advance(2 * 60 * 60 * 1000); // 跨午夜
  bridge.emit(true); // 恢复（today → NEXT_DAY）
  assert.equal(store.getSnapshot().today, NEXT_DAY);

  const resumedOptions = overviewOptions(rangeOf(), store.getSnapshot(), loader.fn);
  assert.deepEqual(resumedOptions.queryKey, ["overview", "2026-09-15", "2026-10-01"], "key 不随午夜漂移");
  assert.equal(resumedOptions.refetchInterval, false, "恢复后固定范围仍不含今日 → 不轮询");
  observer.setOptions(resumedOptions);
  await sleep(150);
  assert.deepEqual(
    loader.calls,
    [
      ["2026-09-15", "2026-10-01"],
      ["2026-09-15", "2026-10-01"],
    ],
    "恢复恰好重取同一固定范围一次，请求标量不漂",
  );
  await sleep(650);
  assert.equal(loader.calls.length, 2, "固定历史范围恢复后无周期请求（不再漂移）");
  unsub();
});

test("跨设备旧数据不绘制：切设备先显示加载态，解析后为新设备数据（无 keepPreviousData）", { timeout: 15_000 }, async () => {
  const clock = fakeClock(`${DAY}T10:00:00`);
  const bridge = fakeBridge();
  makeClient();
  const store = makeStore(bridge, clock);
  await store.start();
  bridge.emit(true);

  const range = resolveStatisticsRange({ mode: "today" }, store.getSnapshot().today);
  const loader = makeTopKeysLoader();
  const device7 = track(topKeysOptions(7, range, store.getSnapshot(), loader.fn));
  const unsub = device7.subscribe(() => {});
  const r7 = await onceSuccess(device7);
  assert.equal(r7.data[0].code, 7, "设备 7 数据已呈现");

  // 用户切换设备：页面派生 deviceId 变化 → 同一 observer 以新 key setOptions
  device7.setOptions(topKeysOptions(9, range, store.getSnapshot(), loader.fn));
  const switching = device7.getCurrentResult();
  assert.equal(switching.data, undefined, "切换后立即不呈现旧设备数据");
  assert.equal(switching.isPending, true, "跨设备不用 keepPreviousData：先显示加载态");
  assert.equal(switching.fetchStatus, "fetching");
  assert.equal(switching.isPlaceholderData, false, "不得出现 placeholder 旧数据");

  const r9 = await onceSuccess(device7);
  assert.equal(r9.data[0].code, 9, "解析后呈现新设备数据");
  assert.deepEqual(
    loader.calls,
    [
      [7, range.from, range.to, TOP_KEYS_LIMIT],
      [9, range.from, range.to, TOP_KEYS_LIMIT],
    ],
    "请求参数绑定 (deviceId, from, to, limit)，无多余请求",
  );
  unsub();
});
