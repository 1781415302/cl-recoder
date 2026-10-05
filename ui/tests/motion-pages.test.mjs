// S10 回归（motion-dpi §8 S10）：专用鼠标/手柄页（S8）的集成合同 —— 真实 QueryObserver
// gating 与 mock 保存/选择契约。
//
// 两类合同各按仓库既有惯例钉住（页面是 React 组件而仓库 harness 无渲染器，渲染依赖被
// "不新增第三方依赖"禁止，同 S7/S8/S9 先例）：
// - QueryObserver gating：页面/hooks 接线以"逐字转录 + 行号"钉在 QueryObserver 接缝上
//   （同 runtime-ui-integration.test.mjs 惯例，生产接线变化必须同步改这里）：
//   · sources：ui/src/api/queries.ts:75-82（key ["mouseSources"]，活动 2s = POLL_SOURCES_MS）；
//   · useMouseMotion / useGamepadMotion：ui/src/api/queries.ts:86-127（今日 1s/历史无 interval；
//     页面自身条件与 policy.enabled 取 AND 且必须写在 spread 之后）；
//   · 来源未解析前运动不发请求：ui/src/pages/Mouse.tsx:178（sources.isPending → 选择为 null
//     → sourceId null → enabled false）；
//   · 鼠标页逐日（仅型号历史视图）/旧历史门控：ui/src/pages/Mouse.tsx:199-213（展开才查询、
//     legacy 无 interval）；手柄页逐日门控：ui/src/pages/Gamepad.tsx:108-113；
//   · 保存失效：ui/src/pages/Mouse.tsx:216-226（写成功才使 ["mouseSources"] 失效；未选来源
//     先拒绝）——用真实 MutationObserver + 真实 QueryClient 驱动 mock 保存。
//   范围派生与活动状态用真实纯模块 + 真实 AppActivityStore（假桥/假时钟，同
//   runtime-ui-integration.test.mjs）。
// - mock 保存/选择契约：ui/src/api/mock.ts 仅 type-import（types 为纯类型），经
//   transpileModule 转为 ESM 后 data URL 直载（同 app-activity.test.mjs 惯例），在 Node 内
//   真实驱动 §4.5 的 mock：保存守卫（schema 未就绪/越界/auto 有效只读/虚拟来源/failNextSetDpi）、
//   保存成功反映到来源列表、选择优先级（resolveSelection 逐字转录，Mouse.tsx:47-63）。
//   mock 的日期口径是真实本地时钟（mock.ts localToday），故本文件的 DAY/HIST_RANGE 从真实
//   时钟派生，保证查询范围恒落在"有数据的过去"；假时钟只驱动活动切换（隐藏/恢复）。
// 每测试前用模块加载时的深拷贝还原 motionState（mock 为模块级可变状态），互不泄漏。
import assert from "node:assert/strict";
import { test, beforeEach, afterEach } from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

// QueryObserver 的 refetchInterval/staleTime 定时器由 query-core 的 isServer 守卫调度
// （isServer = typeof window === "undefined"，模块级常量）。测试在加载 react-query 前伪装
// 浏览器宿主（同 runtime-ui-integration.test.mjs），使轮询真实生效可供断言。
if (typeof globalThis.window === "undefined") globalThis.window = globalThis;

const { QueryClient, QueryObserver, MutationObserver } = await import("@tanstack/react-query");

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
const mock = await loadModule("../src/api/mock.ts");

/* ===== 真实时钟派生日（mock.ts localToday 同规则；mock 数据按真实时钟生成） ===== */

function realLocalDay() {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
}

/** UTC 日历加减天（测试内固定范围用，避免 DST 歧义） */
function dayOffset(iso, delta) {
  const d = new Date(`${iso}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + delta);
  return d.toISOString().slice(0, 10);
}

/** mock 意义上的"今日"（假时钟只控制活动切换，today 取真实时钟与 mock 对齐） */
const DAY = realLocalDay();
/** 不含 today 的历史固定范围（恒 ≤ 真实 today：mock 只为 day ≤ today 的日生成数据） */
const HIST_RANGE = { from: dayOffset(DAY, -20), to: dayOffset(DAY, -10) };

/* ===== 页面/hooks 接线逐字转录（行号见各注释；生产接线变化必须同步改这里） ===== */

/** 来源列表轮询间隔（ui/src/api/queries.ts:25 POLL_SOURCES_MS） */
const POLL_SOURCES_MS = 2_000;

/** 运动/按钮查询今日轮询间隔（ui/src/api/queries.ts:28 POLL_MOTION_MS） */
const POLL_MOTION_MS = 1_000;

/** 闭区间 [from, to] 是否包含当前 today（ui/src/api/queries.ts:31-33，ISO 日字典序比较） */
function rangeIncludesToday(range, today) {
  return range.from <= today && today <= range.to;
}

/** useMouseSources 查询构造（ui/src/api/queries.ts:75-82）：key ["mouseSources"]，活动 2s */
function sourcesOptions(activity, queryFn) {
  return {
    queryKey: ["mouseSources"],
    queryFn: () => queryFn(),
    ...uiQueryPolicy(activity.active, POLL_SOURCES_MS),
  };
}

/** useMouseMotion 查询构造（ui/src/api/queries.ts:86-105）：key 绑 (sourceId, from, to)；
 * interval 仅范围含当前 today 时传入；页面自身条件（已选来源）与 policy.enabled 取 AND
 * 且必须写在 spread 之后 */
function mouseMotionOptions(sourceId, range, activity, queryFn) {
  const policy = uiQueryPolicy(
    activity.active,
    rangeIncludesToday(range, activity.today) ? POLL_MOTION_MS : undefined,
  );
  return {
    queryKey: ["mouseMotion", sourceId, range.from, range.to],
    queryFn: () => {
      if (sourceId === null) throw new Error("未选择鼠标运动来源（enabled 保证不触发）");
      return queryFn(sourceId, range.from, range.to);
    },
    ...policy,
    enabled: sourceId !== null && policy.enabled,
  };
}

/** useGamepadMotion 查询构造（ui/src/api/queries.ts:108-127）：按型号 deviceId，节奏同 useMouseMotion */
function gamepadMotionOptions(deviceId, range, activity, queryFn) {
  const policy = uiQueryPolicy(
    activity.active,
    rangeIncludesToday(range, activity.today) ? POLL_MOTION_MS : undefined,
  );
  return {
    queryKey: ["gamepadMotion", deviceId, range.from, range.to],
    queryFn: () => {
      if (deviceId === null) throw new Error("未选择手柄设备（enabled 保证不触发）");
      return queryFn(deviceId, range.from, range.to);
    },
    ...policy,
    enabled: deviceId !== null && policy.enabled,
  };
}

/** 鼠标页型号历史逐日明细（ui/src/pages/Mouse.tsx:199-204）：展开才查询、仅型号历史视图
 * （selectedSource === null）、与键盘共享页同构的 statsPolicy */
function modelDailyOptions(deviceId, range, activity, open, selectedSourceIsNull, queryFn) {
  const includesToday = rangeIncludesToday(range, activity.today);
  const statsPolicy = uiQueryPolicy(activity.active, includesToday ? 1_000 : undefined);
  return {
    queryKey: ["keyDaily", deviceId, range.from, range.to],
    queryFn: () => queryFn(deviceId, range.from, range.to),
    ...statsPolicy,
    enabled: deviceId !== null && open && selectedSourceIsNull && statsPolicy.enabled,
  };
}

/** 手柄页逐日明细（ui/src/pages/Gamepad.tsx:108-113）：展开才查询 */
function gamepadKeyDailyOptions(deviceId, range, activity, open, queryFn) {
  const includesToday = rangeIncludesToday(range, activity.today);
  const statsPolicy = uiQueryPolicy(activity.active, includesToday ? 1_000 : undefined);
  return {
    queryKey: ["keyDaily", deviceId, range.from, range.to],
    queryFn: () => queryFn(deviceId, range.from, range.to),
    ...statsPolicy,
    enabled: deviceId !== null && open && statsPolicy.enabled,
  };
}

/** 鼠标页旧历史（ui/src/pages/Mouse.tsx:208-213）：单独折叠、展开才查询、无 interval */
function legacyOptions(deviceId, range, activity, open, queryFn) {
  return {
    queryKey: ["mouseLegacy", deviceId, range.from, range.to],
    queryFn: () => queryFn(deviceId, range.from, range.to),
    ...uiQueryPolicy(activity.active),
    enabled: deviceId !== null && open && activity.active,
  };
}

/** 鼠标页保存接线（ui/src/pages/Mouse.tsx:216-222 dpiSave 逐字）：写成功才使
 * ["mouseSources"] 失效；失败不失效（错误交给编辑器展示） */
function dpiSaveMutation(qc, mutationFn) {
  return new MutationObserver(qc, {
    mutationFn: (input) => mutationFn(input.sourceId, input.dpi),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["mouseSources"] });
    },
  });
}

/** 页面保存包装（ui/src/pages/Mouse.tsx:223-226 saveDpi 逐字）：未选来源先拒绝，不触达 mutation。
 * React useMutation 的 mutateAsync 即 MutationObserver.mutate（useMutation.js:192 绑定，
 * 返回 awaitable execute promise），故这里直接调 observer.mutate。 */
function makeSaveDpi(dpiSave, selectedSource) {
  return (dpi) => {
    if (selectedSource === null) return Promise.reject(new Error("未选择鼠标来源"));
    return dpiSave.mutate({ sourceId: selectedSource.id, dpi }).then(() => undefined);
  };
}

/** 初始/回落选择（ui/src/pages/Mouse.tsx:47-63 resolveSelection 逐字）：
 * 第一个已连接物理来源 > 已保存来源（列表首位）> 已有型号；用户已选且仍存在时原样保留 */
function resolveSelection(picked, sourceRows, models) {
  if (picked !== null) {
    if (picked.kind === "source" && sourceRows.some((s) => s.id === picked.id)) return picked;
    if (picked.kind === "model" && models.some((m) => m.id === picked.id)) return picked;
  }
  const connected = sourceRows.find((s) => s.physical && s.connected);
  if (connected) return { kind: "source", id: connected.id };
  if (sourceRows.length > 0) return { kind: "source", id: sourceRows[0].id };
  if (models.length > 0) return { kind: "model", id: models[0].id };
  return null;
}

/* ===== mock 状态还原（mock.ts:616-633 motionState 是模块级可变状态） ===== */

const initialMotionState = JSON.parse(JSON.stringify(mock.motionState));

function resetMotionState() {
  mock.motionState.schemaReady = initialMotionState.schemaReady;
  mock.motionState.failNextSetDpi = initialMotionState.failNextSetDpi;
  mock.motionState.sources = initialMotionState.sources.map((s) => ({
    ...s,
    buckets: s.buckets.map((b) => ({ ...b })),
  }));
}

/* ===== 假桥 / 假时钟（runtime-ui-integration.test.mjs 同款） ===== */

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

/** 假时钟：可推进的本地时间 + 记账 timer（本文件只驱动活动切换，不推进时钟） */
function fakeClock(startIso) {
  let nowMs = Date.parse(startIso);
  let nextId = 1;
  const timers = new Map();
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
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 轮询等待条件成立（真实 setInterval 时刻有调度抖动，用截止时间替代固定 sleep 断言） */
async function waitUntil(cond, label, deadlineMs = 8_000) {
  for (let waited = 0; waited <= deadlineMs; waited += 25) {
    if (cond()) return;
    await sleep(25);
  }
  assert.ok(cond(), `等待超时：${label}`);
}

/** 等待 observer 到达稳定成功态（success 且不在请求中）。
 * uiQueryPolicy 恒 staleTime=0 → isStale 恒 true；纯缓存命中时订阅不触发回调 →
 * 先查 getCurrentResult()（同 runtime-ui-integration 实测语义）。 */
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

/** 记录调用参数的 mock 包装（同 runtime-ui-integration 的 makeXxxLoader：{ calls, fn }） */
function recorder(fn) {
  const calls = [];
  return {
    calls,
    fn: (...args) => {
      calls.push(args);
      return fn(...args);
    },
  };
}

/* ===== 每测试资源 ===== */

let qc = null;
const observers = [];
const mutations = [];
const stores = [];

function makeClient() {
  qc = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return qc;
}

/** 真实 AppActivityStore + 假桥/假时钟（恢复动作与 AppActivityProvider 一致：标 stale 后发布） */
function makeStore(bridge) {
  const store = new AppActivityStore(bridge, fakeClock(`${DAY}T10:00:00`), () => {});
  stores.push(store);
  return store;
}

/** 页面消费的 AppActivity 形状（AppActivityStore.getSnapshot 同形状；无切换需求时直接用） */
function activeSnapshot() {
  return { ready: true, active: true, today: DAY, error: null };
}

function track(options) {
  const observer = new QueryObserver(qc, options);
  observers.push(observer);
  return observer;
}

function trackMutation(qcClient, mutationFn) {
  const mutation = dpiSaveMutation(qcClient, mutationFn);
  mutations.push(mutation);
  return mutation;
}

// 每个测试后销毁 observer/mutation、停止 store、清空 client：避免 interval/gcTime 把进程挂住
afterEach(() => {
  for (const observer of observers) observer.destroy();
  observers.length = 0;
  for (const mutation of mutations) mutation.reset();
  mutations.length = 0;
  for (const store of stores) store.stop();
  stores.length = 0;
  if (qc) {
    // MutationCache.clear() 只 notify+清表、不销毁条目（query-core mutationCache.js clear），
    // Mutation 继承 Removable 的 5 分钟 gcTime 定时器会挂住 Node 进程——先显式销毁清定时器
    for (const mutation of qc.getMutationCache().getAll()) mutation.destroy();
    qc.clear();
    qc = null;
  }
});

beforeEach(() => {
  resetMotionState();
});

/* ===== 1. 隐藏/恢复 gating 集成（sources 2s、今日 motion 1s、来源未解析前不发运动请求） ===== */

test("隐藏启动全禁用 0 请求；恢复后 sources 2s、今日运动 1s 轮询生效、再隐藏即停", { timeout: 30_000 }, async () => {
  const bridge = fakeBridge();
  makeClient();
  const store = makeStore(bridge);
  await store.start(); // 隐藏启动：revision=0/inactive

  const hidden = store.getSnapshot();
  const sourcesLoader = recorder(() => mock.mockGetMouseSources());
  const motionLoader = recorder((sourceId, from, to) => mock.mockGetMouseMotion(sourceId, from, to));
  const gpLoader = recorder((deviceId, from, to) => mock.mockGetGamepadMotion(deviceId, from, to));

  const sourcesObserver = track(sourcesOptions(hidden, sourcesLoader.fn));
  assert.equal(sourcesObserver.getCurrentResult().fetchStatus, "idle");
  const unsubS = sourcesObserver.subscribe(() => {});
  assert.equal(sourcesOptions(hidden, sourcesLoader.fn).enabled, false, "隐藏启动：sources 被活动 gating 关闭");

  // 来源未解析（sources.isPending）→ 页面选择为 null（Mouse.tsx:178）→ 运动查询关闭：
  // 即便 activity 恢复，运动观察者也保持 enabled=false 不发请求
  const motionObserver = track(mouseMotionOptions(null, { from: DAY, to: DAY }, hidden, motionLoader.fn));
  const gpObserver = track(gamepadMotionOptions(null, { from: DAY, to: DAY }, hidden, gpLoader.fn));
  const unsubM = motionObserver.subscribe(() => {});
  const unsubG = gpObserver.subscribe(() => {});
  await sleep(200);
  assert.equal(sourcesLoader.calls.length, 0, "隐藏后 sources 不得发起任何请求（含周期请求）");
  assert.equal(motionLoader.calls.length, 0, "隐藏后运动不得发起任何请求");
  assert.equal(gpLoader.calls.length, 0, "隐藏后手柄运动不得发起任何请求");

  mock.motionState.sources[0].autoUntilMs = Date.now() + 60_000; // 固定 auto 有效窗口（先于取数）

  bridge.emit(true); // 原生事件恢复可见
  const active = store.getSnapshot();
  sourcesObserver.setOptions(sourcesOptions(active, sourcesLoader.fn));
  assert.equal(sourcesOptions(active, sourcesLoader.fn).refetchInterval, POLL_SOURCES_MS, "活动期 sources 2s 轮询");
  const result = await onceSuccess(sourcesObserver);

  // mock 来源列表契约（§4.5 口径）：ready + 三来源按存储序；auto 有效 > manual > 未配置
  const rows = result.data.sources;
  assert.equal(result.data.availability, "ready");
  assert.deepEqual(rows.map((s) => s.id), [1, 2, 3], "来源按存储序（列表首位 = 已保存来源）");
  assert.equal(rows[0].connected, true);
  assert.equal(rows[0].effectiveDpi, 1600, "auto 有效 → effective=auto");
  assert.equal(rows[0].dpiOrigin, "auto");
  assert.ok(rows[0].autoValidUntil !== null && new Date(rows[0].autoValidUntil).getTime() > Date.now());
  assert.equal(rows[1].effectiveDpi, 1200, "探测失败 → manual 后备");
  assert.equal(rows[1].dpiOrigin, "manual");
  assert.equal(rows[1].autoDpi, null);
  assert.equal(rows[2].effectiveDpi, null, "离线无配置 → 无有效 DPI");
  assert.equal(rows[2].dpiOrigin, "unknown");

  // 选择接线：首次解析完成后按优先级派生（第一连接物理来源）→ sourceId 绑定其来源 id
  const selection = resolveSelection(null, rows, []);
  assert.deepEqual(selection, { kind: "source", id: 1 }, "初始选择 = 第一个已连接物理来源");
  const selectedSource = rows.find((s) => s.id === selection.id);
  assert.equal(selectedSource.deviceId, 3, "来源项的按钮/旧历史走其型号 deviceId");

  assert.equal(motionLoader.calls.length, 0, "来源解析前运动不发请求（pickerSelection null）");
  const todayRange = resolveStatisticsRange({ mode: "today" }, active.today);
  const motionActive = mouseMotionOptions(selectedSource.id, todayRange, active, motionLoader.fn);
  assert.equal(motionActive.enabled, true);
  assert.equal(motionActive.refetchInterval, POLL_MOTION_MS, "今日范围 + 活动期 → 运动 1s 轮询");
  motionObserver.setOptions(motionActive);
  await onceSuccess(motionObserver);

  const gpActive = gamepadMotionOptions(5, todayRange, active, gpLoader.fn);
  assert.equal(gpActive.enabled, true);
  assert.equal(gpActive.refetchInterval, POLL_MOTION_MS, "手柄运动节奏与鼠标运动相同");
  gpObserver.setOptions(gpActive);
  const gpResult = await onceSuccess(gpObserver);
  assert.equal(gpResult.data.availability, "ready");
  assert.equal(gpResult.data.gridSize, 25);
  assert.equal(gpResult.data.left.dwellSeconds.length, 625);
  assert.equal(gpResult.data.right.dwellSeconds.length, 625);
  assert.ok(gpResult.data.left.activeSeconds > 0, "有数据的型号呈现非零活动时长");

  await waitUntil(
    () => motionLoader.calls.length >= 3 && sourcesLoader.calls.length >= 2 && gpLoader.calls.length >= 2,
    "活动期周期请求生效（motion ≥3、sources ≥2、gamepad ≥2 次）",
  );
  assert.ok(
    motionLoader.calls.every(([sourceId, from, to]) => sourceId === 1 && from === DAY && to === DAY),
    "运动请求参数绑定 (sourceId, from, to)",
  );

  bridge.emit(false); // 最小化/隐藏
  const hidden2 = store.getSnapshot();
  sourcesObserver.setOptions(sourcesOptions(hidden2, sourcesLoader.fn));
  motionObserver.setOptions(mouseMotionOptions(selectedSource.id, todayRange, hidden2, motionLoader.fn));
  gpObserver.setOptions(gamepadMotionOptions(5, todayRange, hidden2, gpLoader.fn));
  const frozenS = sourcesLoader.calls.length;
  const frozenM = motionLoader.calls.length;
  const frozenG = gpLoader.calls.length;
  await sleep(2_600); // > sources 间隔 2s：隐藏后两个轮询都不应再触发
  assert.equal(sourcesLoader.calls.length, frozenS, "隐藏后 sources 周期 fetch 必须停止");
  assert.equal(motionLoader.calls.length, frozenM, "隐藏后运动周期 fetch 必须停止");
  assert.equal(gpLoader.calls.length, frozenG, "隐藏后手柄运动周期 fetch 必须停止");
  assert.equal(motionObserver.getCurrentResult().fetchStatus, "idle");
  unsubS();
  unsubM();
  unsubG();
});

/* ===== 2. 历史固定范围：无运动轮询；legacy/逐日展开才查询 ===== */

test("历史固定范围不轮询：sources 仍 2s、运动无 interval；legacy/逐日展开才查询且 legacy 恒无 interval", { timeout: 15_000 }, async () => {
  makeClient();
  const activity = activeSnapshot();
  const range = resolveStatisticsRange(selectStatisticsRange(HIST_RANGE, "fixed"), activity.today);
  assert.equal(rangeIncludesToday(range, activity.today), false, "历史固定范围不含 today");

  // 仅检查 options 形状（不观察不取数）；queryFn 用哨兵，意外触发即失败
  const neverFetch = () => {
    throw new Error("本断言不应触发该查询");
  };
  assert.equal(sourcesOptions(activity, neverFetch).refetchInterval, POLL_SOURCES_MS,
    "sources 无范围语义，活动期恒 2s");
  assert.equal(mouseMotionOptions(1, range, activity, neverFetch).refetchInterval, false,
    "历史范围运动不轮询");
  assert.equal(gamepadMotionOptions(5, range, activity, neverFetch).refetchInterval, false,
    "历史范围手柄运动不轮询");

  // 鼠标页旧历史：闭合时即便活动也不查询；展开后恰好一次且无周期请求
  const legacyLoader = recorder((deviceId, from, to) => mock.mockGetMouseLegacy(deviceId, from, to));
  const legacyClosed = legacyOptions(3, range, activity, false, legacyLoader.fn);
  assert.equal(legacyClosed.enabled, false, "旧历史折叠时不查询");
  const legacyObserver = track(legacyClosed);
  const unsubL = legacyObserver.subscribe(() => {});
  await sleep(200);
  assert.equal(legacyLoader.calls.length, 0, "折叠的旧历史不得发请求");

  const legacyOpen = legacyOptions(3, range, activity, true, legacyLoader.fn);
  assert.equal(legacyOpen.enabled, true);
  assert.equal(legacyOpen.refetchInterval, false, "旧历史无 interval（含今日范围也不轮询）");
  legacyObserver.setOptions(legacyOpen);
  const legacyResult = await onceSuccess(legacyObserver);
  assert.equal(legacyResult.data.deviceId, 3);
  assert.ok(legacyResult.data.rawCounts > 0, "鼠标型号有旧算法移动量");
  assert.equal(legacyResult.data.quality, "legacy_uncalibrated");
  await sleep(1_200);
  assert.equal(legacyLoader.calls.length, 1, "旧历史无周期请求");

  // 鼠标页逐日（型号历史视图门控）：来源视图展开也不查询
  const mouseDailyLoader = recorder((deviceId, from, to) => mock.mockKeyDaily(deviceId, from, to));
  const inSourceView = modelDailyOptions(3, range, activity, true, false, mouseDailyLoader.fn);
  assert.equal(inSourceView.enabled, false, "来源视图不提供型号逐日查询");
  const inModelView = modelDailyOptions(3, range, activity, true, true, mouseDailyLoader.fn);
  assert.equal(inModelView.enabled, true, "型号历史视图展开才查询");
  assert.equal(inModelView.refetchInterval, false, "历史范围逐日不轮询");

  // 手柄页逐日：展开才查询
  const gpDailyLoader = recorder((deviceId, from, to) => mock.mockKeyDaily(deviceId, from, to));
  const gpClosed = gamepadKeyDailyOptions(5, range, activity, false, gpDailyLoader.fn);
  assert.equal(gpClosed.enabled, false, "手柄逐日折叠时不查询");
  const gpDailyObserver = track(gpClosed);
  const unsubD = gpDailyObserver.subscribe(() => {});
  await sleep(200);
  assert.equal(gpDailyLoader.calls.length, 0, "折叠的手柄逐日不得发请求");
  const gpOpen = gamepadKeyDailyOptions(5, range, activity, true, gpDailyLoader.fn);
  assert.equal(gpOpen.enabled, true);
  gpDailyObserver.setOptions(gpOpen);
  await onceSuccess(gpDailyObserver);
  assert.deepEqual(gpDailyLoader.calls, [[5, range.from, range.to]], "逐日请求参数绑定 (deviceId, from, to)");
  unsubL();
  unsubD();
});

/* ===== 3. 跨来源切换：旧数据不绘制、sourceId null 不发请求、请求参数绑定 ===== */

test("跨来源切换旧数据不绘制（无 keepPreviousData）；sourceId null 不发请求；mock 运动汇总口径", { timeout: 15_000 }, async () => {
  makeClient();
  const activity = activeSnapshot();
  const motionLoader = recorder((sourceId, from, to) => mock.mockGetMouseMotion(sourceId, from, to));

  // 未选来源（active）：查询关闭、不发请求
  const observer = track(mouseMotionOptions(null, HIST_RANGE, activity, motionLoader.fn));
  const unsub = observer.subscribe(() => {});
  await sleep(200);
  assert.equal(motionLoader.calls.length, 0, "sourceId null 不发请求");

  // 来源 1：partial coverage（含 unknown 桶）→ 未配置计数 > 0、覆盖率 < 1、有米数
  observer.setOptions(mouseMotionOptions(1, HIST_RANGE, activity, motionLoader.fn));
  const r1 = await onceSuccess(observer);
  assert.equal(r1.data.availability, "ready");
  assert.equal(r1.data.sourceId, 1);
  assert.ok(r1.data.rawCounts > 0);
  assert.ok(r1.data.unconfiguredCounts > 0, "unknown 桶计入未配置计数");
  assert.ok(r1.data.rawCounts > r1.data.unconfiguredCounts, "已配置部分存在");
  assert.ok(r1.data.coverage > 0 && r1.data.coverage < 1, "partial coverage 介于 (0,1)");
  assert.ok(r1.data.meters > 0, "有已配置移动 → 有估算距离");
  assert.ok(r1.data.days.length > 0 && r1.data.days.every((d) => d.rawCounts > 0), "逐日行只含有运动的日");

  // 用户切到来源 2：同一 observer 新 key → 先加载态、不呈现旧来源数据
  observer.setOptions(mouseMotionOptions(2, HIST_RANGE, activity, motionLoader.fn));
  const switching = observer.getCurrentResult();
  assert.equal(switching.data, undefined, "切换后立即不呈现旧来源数据");
  assert.equal(switching.isPending, true, "跨来源不用 keepPreviousData：先显示加载态");
  assert.equal(switching.fetchStatus, "fetching");
  assert.equal(switching.isPlaceholderData, false, "不得出现 placeholder 旧数据");
  const r2 = await onceSuccess(observer);
  assert.equal(r2.data.sourceId, 2);
  assert.equal(r2.data.coverage, 1, "来源 2 全部已配置 → 覆盖率 1");
  assert.equal(r2.data.unconfiguredCounts, 0);
  assert.ok(r2.data.meters > 0);

  // 来源 3（离线、无运动桶）：合法空——不用 0 米冒充
  observer.setOptions(mouseMotionOptions(3, HIST_RANGE, activity, motionLoader.fn));
  const r3 = await onceSuccess(observer);
  assert.equal(r3.data.rawCounts, 0);
  assert.equal(r3.data.meters, null, "无任何已配置移动时 meters=null");
  assert.equal(r3.data.coverage, null);
  assert.deepEqual(r3.data.days, []);

  assert.deepEqual(
    motionLoader.calls,
    [
      [1, HIST_RANGE.from, HIST_RANGE.to],
      [2, HIST_RANGE.from, HIST_RANGE.to],
      [3, HIST_RANGE.from, HIST_RANGE.to],
    ],
    "请求参数绑定 (sourceId, from, to)，无多余请求",
  );
  unsub();
});

/* ===== 4. mock 保存守卫（真实 MutationObserver 接线）：拒绝不失效 mouseSources ===== */

test("保存守卫：schema 未就绪/越界/auto 只读/虚拟来源/failNextSetDpi 均拒绝且不失效 mouseSources；失败一次后可重试", { timeout: 15_000 }, async () => {
  makeClient();
  await qc.fetchQuery({ queryKey: ["mouseSources"], queryFn: () => mock.mockGetMouseSources() });
  const state = () => qc.getQueryState(["mouseSources"]);
  const mutationFnCalls = recorder((sourceId, dpi) => mock.mockSetMouseDpi(sourceId, dpi));
  const mutation = trackMutation(qc, mutationFnCalls.fn);
  const attempt = (sourceId, dpi) => mutation.mutate({ sourceId, dpi });

  // schema 未就绪（旧库）：保存拒绝
  mock.motionState.schemaReady = false;
  await assert.rejects(attempt(2, 1400), /运动配置表未就绪/);
  assert.equal(state().isInvalidated, false, "失败的保存不失效 mouseSources");

  // DPI 越界（§6.1 CHECK 1..=100000）
  mock.motionState.schemaReady = true;
  await assert.rejects(attempt(2, 0), /超出合同范围/);
  await assert.rejects(attempt(2, 100001), /超出合同范围/);
  assert.equal(state().isInvalidated, false);

  // auto DPI 有效（在线且未过期）：手动配置只读
  mock.motionState.sources[0].autoUntilMs = Date.now() + 60_000;
  await assert.rejects(attempt(1, 900), /自动 DPI 有效[\s\S]*只读/);
  assert.equal(state().isInvalidated, false);

  // 虚拟/未知桶：禁配置
  mock.motionState.sources.push({
    id: 99, deviceId: 3, physical: false, manualDpi: null, autoDpi: null,
    autoUntilMs: 0, published: false, probeStatus: "disconnected", buckets: [],
  });
  await assert.rejects(attempt(99, 800), /虚拟\/未知桶/);
  assert.equal(state().isInvalidated, false);

  // failNextSetDpi：失败一次（不失效），随后同一保存自动恢复可重试并成功
  mock.motionState.failNextSetDpi = true;
  await assert.rejects(attempt(2, 1400), /模拟：保存手动 DPI 失败/);
  assert.equal(state().isInvalidated, false);
  const retryRow = await attempt(2, 1400);
  assert.equal(retryRow.manualDpi, 1400, "失败标志一次性消耗后同一保存成功");
});

/* ===== 5. 保存成功接线：onSuccess 才失效 ["mouseSources"]，重取反映新 manual ===== */

test("保存成功接线：写成功才使 [\"mouseSources\"] 失效并重取出新 manualDpi；null 清除手动；未选来源先拒绝", { timeout: 15_000 }, async () => {
  makeClient();
  const activity = activeSnapshot();
  const sourcesLoader = recorder(() => mock.mockGetMouseSources());
  const mutationFnCalls = recorder((sourceId, dpi) => mock.mockSetMouseDpi(sourceId, dpi));
  const mutation = trackMutation(qc, mutationFnCalls.fn);
  const saveDpi = makeSaveDpi(mutation, { id: 2 });

  // 未选来源：saveDpi 先拒绝且不触达 mutation
  await assert.rejects(makeSaveDpi(mutation, null)(1400), /未选择鼠标来源/);
  assert.equal(mutationFnCalls.calls.length, 0, "未选来源不发起保存");

  // 来源列表已呈现（活动观察者）：基线 manual 1200
  const sourcesObserver = track(sourcesOptions(activity, sourcesLoader.fn));
  const unsub = sourcesObserver.subscribe(() => {});
  const baseline = await onceSuccess(sourcesObserver);
  assert.equal(baseline.data.sources.find((s) => s.id === 2).manualDpi, 1200);
  assert.equal(qc.getQueryState(["mouseSources"]).isInvalidated, false, "呈现期缓存未被失效");

  // 保存 1400（页面 dpiSave.mutateAsync 即 observer.mutate，返回展示行）：
  // manual/effective/origin 正确；onSuccess 已使缓存失效
  const row = await mutation.mutate({ sourceId: 2, dpi: 1400 });
  assert.equal(row.id, 2);
  assert.equal(row.manualDpi, 1400);
  assert.equal(row.effectiveDpi, 1400, "探测失败来源的有效 DPI = manual");
  assert.equal(row.dpiOrigin, "manual");
  assert.equal(row.autoDpi, null);
  assert.equal(row.autoValidUntil, null);
  assert.equal(qc.getQueryState(["mouseSources"]).isInvalidated, true, "写成功才使 mouseSources 失效");

  // 失效触发活动观察者重取：新 manualDpi 出现在来源列表
  await waitUntil(
    () => sourcesObserver.getCurrentResult().data?.sources.find((s) => s.id === 2)?.manualDpi === 1400,
    "重取后的来源列表反映新 manualDpi",
  );

  // 保存 null（页面包装 saveDpi 透传 mutation、返回 void）：清除手动后备 → 重取回落 unknown
  const cleared = await saveDpi(null);
  assert.equal(cleared, undefined, "页面 saveDpi 返回 void");
  await waitUntil(
    () => sourcesObserver.getCurrentResult().data?.sources.find((s) => s.id === 2)?.effectiveDpi === null
      && sourcesObserver.getCurrentResult().data?.sources.find((s) => s.id === 2)?.dpiOrigin === "unknown",
    "重取后的来源列表回落 unknown",
  );
  const rowAfter = sourcesObserver.getCurrentResult().data.sources.find((s) => s.id === 2);
  assert.equal(rowAfter.manualDpi, null);
  assert.equal(rowAfter.autoDpi, null);
  assert.equal(rowAfter.autoValidUntil, null);
  // 数据已呈现清除结果 → 重取已完成 → 失效标记被消费清零（isInvalidated 只由 invalidateQueries 置位）
  assert.equal(qc.getQueryState(["mouseSources"]).isInvalidated, false, "重取完成后失效标记已清零");
  unsub();
});

/* ===== 6. 选择契约（resolveSelection）+ needs_upgrade 形状 + 未知身份抛错 ===== */

test("选择契约：已连接物理 > 列表首位 > 型号；已选保留不跳源；needs_upgrade 空来源回落型号且运动全 needs_upgrade", { timeout: 15_000 }, async () => {
  // ready：来源 1/2 在线（mock 默认 published）
  mock.motionState.sources[0].autoUntilMs = Date.now() + 60_000;
  const rows = (await mock.mockGetMouseSources()).sources;
  assert.deepEqual(rows.map((s) => s.id), [1, 2, 3]);

  const models = [{ id: 4, name: "Microsoft 基本光学鼠标", nickname: null }, { id: 3, name: "Logitech G102 游戏鼠标", nickname: "G102" }];
  assert.deepEqual(resolveSelection(null, rows, models), { kind: "source", id: 1 }, "初始 = 第一个已连接物理来源");
  assert.deepEqual(resolveSelection({ kind: "source", id: 2 }, rows, models), { kind: "source", id: 2 },
    "用户已选且仍存在 → 原样保留（轮询不跳源）");
  assert.deepEqual(resolveSelection({ kind: "source", id: 99 }, rows, models), { kind: "source", id: 1 },
    "已选来源消失 → 按优先级回落");
  assert.deepEqual(resolveSelection({ kind: "model", id: 4 }, rows, models), { kind: "model", id: 4 },
    "已选型号仍存在 → 原样保留");
  assert.deepEqual(resolveSelection({ kind: "model", id: 99 }, rows, models), { kind: "source", id: 1 },
    "已选型号消失 → 回落连接来源");

  // 第一个已连接物理来源优先于列表首位：只有来源 2 在线时选 2
  mock.motionState.sources[0].published = false;
  const rowsOnly2 = (await mock.mockGetMouseSources()).sources;
  assert.deepEqual(resolveSelection(null, rowsOnly2, models), { kind: "source", id: 2 });

  // 全部离线：回落"已保存来源"（列表首位）
  mock.motionState.sources.forEach((s) => { s.published = false; });
  const rowsOffline = (await mock.mockGetMouseSources()).sources;
  assert.deepEqual(resolveSelection(null, rowsOffline, models), { kind: "source", id: 1 });
  assert.equal(rowsOffline.every((s) => s.connected === false), true);

  // needs_upgrade（旧 schema）：sources 为 needs_upgrade + 空列表 → 选择回落型号；全空为 null
  mock.motionState.schemaReady = false;
  const nu = await mock.mockGetMouseSources();
  assert.equal(nu.availability, "needs_upgrade");
  assert.deepEqual(nu.sources, []);
  assert.deepEqual(resolveSelection(null, nu.sources, models), { kind: "model", id: 4 }, "空来源回落第一个型号");
  assert.equal(resolveSelection(null, nu.sources, []), null, "无来源无型号 → 无选择");

  // needs_upgrade 下运动查询同为 needs_upgrade 形状（页面据此渲染引导，不伪造零数据）
  const nuMouse = await mock.mockGetMouseMotion(1, HIST_RANGE.from, HIST_RANGE.to);
  assert.equal(nuMouse.availability, "needs_upgrade");
  assert.equal(nuMouse.rawCounts, 0);
  assert.equal(nuMouse.meters, null);
  assert.deepEqual(nuMouse.days, []);
  const nuGamepad = await mock.mockGetGamepadMotion(5, HIST_RANGE.from, HIST_RANGE.to);
  assert.equal(nuGamepad.availability, "needs_upgrade");
  assert.equal(nuGamepad.gridSize, 25);
  assert.equal(nuGamepad.left.dwellSeconds.length, 625);
  assert.ok(nuGamepad.left.dwellSeconds.every((v) => v === 0));
  assert.ok(nuGamepad.right.dwellSeconds.every((v) => v === 0));

  // 未知身份抛中文错误（不静默切到别的来源/设备）
  await assert.rejects(mock.mockGetMouseMotion(99, HIST_RANGE.from, HIST_RANGE.to), /未知鼠标运动来源/);
  await assert.rejects(mock.mockGetGamepadMotion(3, HIST_RANGE.from, HIST_RANGE.to), /不是手柄/);
  await assert.rejects(mock.mockGetMouseLegacy(5, HIST_RANGE.from, HIST_RANGE.to), /不是鼠标/);
});
