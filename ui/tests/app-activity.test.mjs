// S3 回归（usability-runtime-v3 §4.4/§4.5）：AppActivityStore + uiQueryPolicy。
//
// 用 Node 内建 test 验证：
// - listener 先注册、再取快照（start 顺序）；健康 started 再 start 幂等；并发 start 同一 pending；
// - 首个 revision=0/inactive 也必须接受并 ready=true；同版本/旧版本不回退；
// - snapshot 失败但已接受有效 event → 保留权威状态，retry 只补快照不加第二监听；
//   listener 注册失败才重做订阅（重试后恢复）；
// - 迟到 unlisten：stop 后 subscribe 才 resolve → 补退订；stop 后旧代 snapshot 返回不重新激活；
// - 隐藏启动 0 请求（初始 ready=false + inactive 双重 gating），真实 uiQueryPolicy 供
//   QueryObserver：enabled gating、interval 轮询、meta.uiOwned + refetchType:none 失效；
// - 失焦但可见保持 active（原生事件权威）、minimize → inactive 且取消 timer；
// - 恢复时同一状态发布先重算本地 today 再 active=true（onWillActivate 先于发布）；
//   午夜 timer 用当地日历构造（非固定 24h），触发后 today 前进并重排；
// - stop 清理：timer 取消、unlisten 调用、状态复位。
//
// 纯模块（../src/lib/appActivity.ts、../src/api/queryPolicy.ts，无运行时 import）经
// typescript.transpileModule 转为 ESM 后由 data URL 导入（同 wp-mouse-query.test.mjs 惯例）。
import assert from "node:assert/strict";
import { test, afterEach } from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

// QueryObserver 的 refetchInterval/staleTime 定时器由 query-core 的 isServer 守卫调度
// （isServer = typeof window === "undefined"，模块级常量），纯 Node 进程不会排任何 interval。
// 测试在加载 react-query 前伪装浏览器宿主（Node 的 globalThis 无 addEventListener，
// focusManager/onlineManager 的监听注册自带守卫，不会崩），使 interval 真实生效可供断言。
if (typeof globalThis.window === "undefined") globalThis.window = globalThis;

const { QueryClient, QueryObserver } = await import("@tanstack/react-query");

async function loadModule(relPath) {
  const src = await readFile(new URL(relPath, import.meta.url), "utf8");
  const js = ts.transpileModule(src, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2020 },
  }).outputText;
  return import("data:text/javascript;base64," + Buffer.from(js).toString("base64"));
}

const { AppActivityStore } = await loadModule("../src/lib/appActivity.ts");
const { uiQueryPolicy } = await loadModule("../src/api/queryPolicy.ts");

test("订阅resolve前的有效事件在getter失败时仍然保留", async () => {
  const store = new AppActivityStore({
    async subscribe(handler) {
      handler({ revision: 1, active: true });
      return () => {};
    },
    async snapshot() { throw new Error("getter failed"); },
  }, fakeClock("2026-10-03T10:00:00"));
  await store.start();
  assert.equal(store.getSnapshot().active, true);
  assert.equal(store.getSnapshot().error, null);
  store.stop();
});

test("stop再start后旧订阅的迟到事件不能覆盖新状态", async () => {
  const handlers = [];
  const store = new AppActivityStore({
    async subscribe(handler) { handlers.push(handler); return () => {}; },
    async snapshot() { return { revision: 0, active: false }; },
  }, fakeClock("2026-10-03T10:00:00"));
  await store.start();
  store.stop();
  await store.start();
  handlers[0]({ revision: 100, active: true });
  assert.equal(store.getSnapshot().active, false);
  handlers[1]({ revision: 1, active: true });
  assert.equal(store.getSnapshot().active, true);
  store.stop();
});

/** 排空若干微任务：store 的 doStart 在已 resolve 的 promise 上续跑需要 tick */
const flush = () => new Promise((r) => queueMicrotask(r));
async function flushUntil(cond, label) {
  for (let i = 0; i < 100 && !cond(); i += 1) await flush();
  assert.ok(cond(), `等待超时：${label}`);
}

/** 假时钟：可推进的本地时间 + 记账 timer（到点顺序触发） */
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
    pendingCount: () => timers.size,
    /** 当前所有 pending timer 的剩余毫秒 */
    pendingDelays: () => [...timers.values()].map((t) => t.at - nowMs),
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

/** 假桥：记录 subscribe/snapshot 调用与顺序；可注入错误与延迟 resolve（手动放行） */
function fakeBridge(opts = {}) {
  const state = {
    revision: 0,
    active: false,
    subscribeCalls: 0,
    snapshotCalls: 0,
    calls: [],
    unlistenCalls: 0,
    handlers: new Set(),
    subscribeError: null,
    deferredSubscribe: null, // 手动放行（迟到 unlisten 场景）
    deferredSnapshot: null, // { resolve, reject }（迟到 snapshot 场景）
  };
  const unlistenFor = (handler) => () => {
    state.unlistenCalls += 1;
    state.handlers.delete(handler);
  };
  const bridge = {
    state,
    /** 原生事件：revision 递增后广播 */
    emit(active) {
      state.revision += 1;
      state.active = active;
      for (const h of [...state.handlers]) h({ revision: state.revision, active });
    },
    /** 任意快照直派（测旧版本/同版本 gate） */
    dispatch(snapshot) {
      for (const h of [...state.handlers]) h(snapshot);
    },
    subscribe(handler) {
      state.subscribeCalls += 1;
      state.calls.push("subscribe");
      if (state.subscribeError) {
        const e = state.subscribeError;
        state.subscribeError = null;
        return Promise.reject(e);
      }
      if (opts.deferSubscribe) {
        return new Promise((resolve) => {
          state.deferredSubscribe = () => resolve(unlistenFor(handler));
        });
      }
      state.handlers.add(handler);
      return Promise.resolve(unlistenFor(handler));
    },
    snapshot() {
      state.snapshotCalls += 1;
      state.calls.push("snapshot");
      if (opts.deferSnapshot) {
        return new Promise((resolve, reject) => {
          state.deferredSnapshot = { resolve, reject };
        });
      }
      return Promise.resolve({ revision: state.revision, active: state.active });
    },
  };
  return bridge;
}

function makeStore(bridge, clock, onWillActivate) {
  const willCalls = [];
  const store = new AppActivityStore(bridge, clock, () => {
    willCalls.push("will");
    onWillActivate?.();
  });
  return { store, willCalls };
}

let qc = null;
const observers = [];

afterEach(() => {
  for (const o of observers) o.destroy();
  observers.length = 0;
  if (qc) {
    qc.clear();
    qc = null;
  }
});

test("uiQueryPolicy 形状与 live/interval 规则（§4.5）", () => {
  assert.deepEqual(uiQueryPolicy(false), {
    enabled: false,
    refetchInterval: false,
    refetchIntervalInBackground: false,
    staleTime: 0,
    meta: { uiOwned: true },
  });
  assert.equal(uiQueryPolicy(true, 500).refetchInterval, 500, "active+interval → 轮询");
  assert.equal(uiQueryPolicy(true).refetchInterval, false, "未提供 interval → false");
  assert.equal(uiQueryPolicy(true, 500, false).refetchInterval, false, "live=false → false");
  assert.equal(uiQueryPolicy(true, 500, false).enabled, true, "enabled=active，与 live 无关");
  assert.equal(uiQueryPolicy(true, 500).staleTime, 0, "staleTime 恒 0：重新启用后刷新");
  assert.equal(uiQueryPolicy(true).refetchIntervalInBackground, false, "不引入后台轮询");
});

test("listener 先注册再取快照；getter/订阅契约；首个 revision=0/inactive 接受并 ready", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge();
  const { store } = makeStore(bridge, clock);
  const seen = [];
  const unsub = store.subscribe(() => seen.push(store.getSnapshot()));

  assert.deepEqual(bridge.state.calls, [], "start 前 bridge 未被触碰");
  assert.deepEqual(store.getSnapshot(), {
    ready: false,
    active: false,
    today: "2026-10-02",
    error: null,
  });

  await store.start();
  assert.deepEqual(bridge.state.calls, ["subscribe", "snapshot"], "§4.4：先注册 listener 再取快照");
  assert.deepEqual(store.getSnapshot(), {
    ready: true,
    active: false,
    today: "2026-10-02",
    error: null,
  }, "revision=0/inactive 也必须接受并 ready=true");
  assert.equal(seen.length, 1, "ready 翻转恰好通知一次");

  unsub();
  bridge.emit(true); // 退订后不再收到通知
  assert.equal(seen.length, 1);
  assert.equal(store.getSnapshot().active, true, "事件仍被接受（store 权威更新）");
});

test("start 幂等：健康 started 不重注册不重取；并发 start 返回同一 pending", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge({ deferSnapshot: true });
  const { store } = makeStore(bridge, clock);

  const p1 = store.start();
  const p2 = store.start();
  assert.equal(p1, p2, "并发 start 返回同一 pending，不重复启动");
  await flushUntil(() => bridge.state.deferredSnapshot !== null, "snapshot 进入在途");
  bridge.state.deferredSnapshot.resolve({ revision: 0, active: false });
  await p1;
  assert.deepEqual(bridge.state.calls, ["subscribe", "snapshot"]);

  const subscribeCalls = bridge.state.subscribeCalls;
  const snapshotCalls = bridge.state.snapshotCalls;
  await store.start(); // 健康 started：幂等
  assert.equal(bridge.state.subscribeCalls, subscribeCalls, "不重复注册");
  assert.equal(bridge.state.snapshotCalls, snapshotCalls, "不重复取快照");
});

test("snapshot 失败但已接受有效 event：保留权威状态；retry 只补快照不加第二监听", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge({ deferSnapshot: true });
  const { store } = makeStore(bridge, clock);

  const started = store.start();
  await flushUntil(() => bridge.state.deferredSnapshot !== null, "listener 已注册、snapshot 在途");
  // listener 已注册，事件先于快照到达（权威）
  bridge.emit(true);
  assert.equal(store.getSnapshot().active, true);
  assert.equal(store.getSnapshot().ready, true);
  // 快照失败：已有权威事件 → 保留状态、不置 error（doStart 内部消化错误，start 仍 resolve）
  bridge.state.deferredSnapshot.reject(new Error("快照不可用"));
  await started;
  assert.deepEqual(store.getSnapshot(), {
    ready: true,
    active: true,
    today: "2026-10-02",
    error: null,
  });

  // retry：listener 在手 → 不加第二监听，只补快照；快照同版本被 gate，状态不回退
  const retry = store.start();
  await flushUntil(
    () => bridge.state.snapshotCalls === 2 && bridge.state.deferredSnapshot !== null,
    "retry 的 snapshot 进入在途",
  );
  bridge.state.deferredSnapshot.resolve({ revision: 1, active: true });
  await retry;
  assert.equal(bridge.state.subscribeCalls, 1, "retry 不得注册第二监听");
  assert.equal(bridge.state.snapshotCalls, 2, "retry 只补快照");
  assert.equal(store.getSnapshot().active, true, "权威状态保持");
  // 之后健康 started 再 start 幂等
  await store.start();
  assert.equal(bridge.state.snapshotCalls, 2);
});

test("listener 注册失败 → error 可见（active=false）；retry 重做订阅并恢复", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge();
  bridge.state.subscribeError = new Error("listen 失败");
  const { store } = makeStore(bridge, clock);

  await store.start();
  assert.deepEqual(store.getSnapshot(), {
    ready: true,
    active: false,
    today: "2026-10-02",
    error: "listen 失败",
  });

  await store.start(); // retry：重做订阅（唯一允许重订阅的情形）
  assert.equal(bridge.state.subscribeCalls, 2);
  assert.deepEqual(store.getSnapshot(), {
    ready: true,
    active: false,
    today: "2026-10-02",
    error: null,
  }, "重试成功后 error 清除，revision=0 快照被接受");
});

test("迟到 unlisten：stop 后 subscribe 才 resolve → 补退订且不激活", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge({ deferSubscribe: true });
  const { store } = makeStore(bridge, clock);

  const started = store.start();
  store.stop(); // subscribe 仍在途
  bridge.state.deferredSubscribe(); // 迟到 resolve
  await started;
  assert.equal(bridge.state.unlistenCalls, 1, "stop 必须覆盖延迟 resolve 的 unlisten");
  assert.deepEqual(store.getSnapshot(), {
    ready: false,
    active: false,
    today: "2026-10-02",
    error: null,
  }, "stop 后状态复位，不得重新激活");
});

test("stop 后旧代 snapshot 返回不得重新激活", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge({ deferSnapshot: true });
  const { store } = makeStore(bridge, clock);

  const started = store.start(); // subscribe 同步调用，snapshot 在途
  assert.equal(bridge.state.subscribeCalls, 1);
  await flushUntil(() => bridge.state.deferredSnapshot !== null, "snapshot 进入在途");
  store.stop();
  bridge.state.deferredSnapshot.resolve({ revision: 7, active: true }); // 旧代返回
  await started;
  assert.equal(bridge.state.unlistenCalls, 1, "stop 先退订已 resolve 的监听");
  assert.equal(store.getSnapshot().ready, false, "旧代 snapshot 结果被代际 gate 丢弃");
  assert.equal(store.getSnapshot().active, false);
});

test("同版本/旧版本不回退状态", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge();
  const { store } = makeStore(bridge, clock);
  await store.start(); // 接受 revision 0/inactive

  bridge.emit(false); // revision 1/inactive
  assert.equal(store.getSnapshot().active, false);
  const before = store.getSnapshot();
  bridge.dispatch({ revision: 1, active: true }); // 同版本重复 → 忽略
  assert.equal(store.getSnapshot(), before, "无变化时 getSnapshot 引用稳定（useSyncExternalStore 合同）");
  bridge.emit(true); // revision 2/active
  assert.equal(store.getSnapshot().active, true);
  bridge.dispatch({ revision: 1, active: false }); // 旧版本 → 忽略
  assert.equal(store.getSnapshot().active, true, "旧版本不得回退");
});

test("真实 uiQueryPolicy 供 QueryObserver：隐藏 0 请求，恢复后请求 + interval 轮询 + uiOwned 失效", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge();
  const { store } = makeStore(bridge, clock);
  await store.start(); // 隐藏启动：revision=0/inactive

  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  let fetchCount = 0;
  const makeOpts = (active) => ({
    ...uiQueryPolicy(active, 40),
    queryKey: ["gated"],
    queryFn: () => {
      fetchCount += 1;
      return Promise.resolve("d");
    },
  });

  // 初始 ready=false + inactive：QueryObserver 全程 0 请求
  const hidden = new QueryObserver(qc, makeOpts(store.getSnapshot().active));
  observers.push(hidden);
  assert.equal(hidden.getCurrentResult().fetchStatus, "idle");
  await new Promise((r) => setTimeout(r, 60));
  assert.equal(fetchCount, 0, "隐藏启动不得发起任何首轮请求");

  // 原生事件恢复可见：enabled=true → 请求 + interval 生效（interval 定时器随订阅调度，
  // 见 query-core onSubscribe → #updateTimers；故保持订阅等待轮询）
  bridge.emit(true);
  assert.equal(store.getSnapshot().active, true);
  const active = new QueryObserver(qc, makeOpts(store.getSnapshot().active));
  observers.push(active);
  let firstResolve;
  const firstSuccess = new Promise((r) => {
    firstResolve = r;
  });
  const unsub = active.subscribe((r) => {
    if (r.isSuccess && r.fetchStatus === "idle") firstResolve(r);
  });
  await firstSuccess;
  assert.equal(fetchCount, 1);
  await new Promise((r) => setTimeout(r, 150));
  assert.ok(fetchCount >= 2, "refetchInterval 必须真实生效于 QueryObserver");
  unsub();

  // meta.uiOwned + refetchType:none：标 stale 但不立即发请求（Provider onWillActivate 同款调用）
  const noInterval = new QueryObserver(qc, {
    ...uiQueryPolicy(true),
    queryKey: ["gated"],
    queryFn: () => {
      fetchCount += 1;
      return Promise.resolve("d");
    },
  });
  observers.push(noInterval);
  let secondResolve;
  const secondSuccess = new Promise((r) => {
    secondResolve = r;
  });
  const unsub2 = noInterval.subscribe((r) => {
    if (r.isSuccess && r.fetchStatus === "idle") secondResolve(r);
  });
  await secondSuccess;
  const before = fetchCount;
  await qc.invalidateQueries({
    predicate: (q) => q.meta?.uiOwned === true,
    refetchType: "none",
  });
  assert.equal(fetchCount, before, "refetchType:none 不得立即发请求");
  assert.equal(qc.getQueryState(["gated"])?.isInvalidated, true, "uiOwned 缓存已标 stale");
  unsub2();
});

test("失焦但可见保持 active；minimize → inactive 且午夜 timer 取消", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge();
  const { store } = makeStore(bridge, clock);
  await store.start();

  bridge.emit(true); // 原生语义：失焦但可见 → 仍发布 active=true
  assert.equal(store.getSnapshot().active, true);
  assert.equal(clock.pendingCount(), 1, "活动期恰好一个午夜 timer");

  bridge.emit(false); // 最小化 → inactive
  assert.equal(store.getSnapshot().active, false);
  assert.equal(clock.pendingCount(), 0, "隐藏取消 timer");
});

test("恢复时同一发布先重算 today 再 active=true；onWillActivate 先于发布；午夜 timer 当地日历构造", async () => {
  const clock = fakeClock("2026-10-02T23:00:00");
  const bridge = fakeBridge();
  const { store, willCalls } = makeStore(bridge, clock);
  await store.start(); // 隐藏启动
  assert.equal(store.getSnapshot().today, "2026-10-02");

  const atNotify = [];
  store.subscribe(() => atNotify.push({ ...store.getSnapshot() }));

  clock.advance(2 * 60 * 60 * 1000); // 隐藏期间跨过当地午夜（模拟休眠/隔日恢复）
  bridge.emit(true); // 恢复可见
  assert.deepEqual(willCalls, ["will"], "进入 active 时 onWillActivate 恰好一次");
  assert.equal(atNotify.length, 1);
  assert.equal(atNotify[0].today, "2026-10-03", "同一发布内 today 已重算");
  assert.equal(atNotify[0].active, true, "同一发布内 active 才置 true");
  assert.deepEqual(bridge.state.calls, ["subscribe", "snapshot"]);

  const delays = clock.pendingDelays();
  assert.equal(delays.length, 1);
  // 01:00（10-03）→ 下一个当地午夜（10-04 00:00）= 23h（固定 24h 实现会得出 22h）
  assert.equal(delays[0], 23 * 60 * 60 * 1000, "timer 指向下一个当地午夜（非固定 24h）");
});

test("午夜 timer 触发：today 前进、重排下一午夜、不重复 onWillActivate", async () => {
  const clock = fakeClock("2026-10-02T23:59:30");
  const bridge = fakeBridge();
  const { store, willCalls } = makeStore(bridge, clock);
  await store.start();
  bridge.emit(true);

  assert.equal(clock.pendingDelays()[0], 30_000, "23:59:30 → 下一午夜恰 30s（当地日历构造）");
  clock.advance(30_000);
  assert.equal(store.getSnapshot().today, "2026-10-03", "午夜触发后 today 前进");
  assert.equal(store.getSnapshot().active, true, "午夜不影响 active（DOM/timer 不是 active 权威）");
  assert.deepEqual(willCalls, ["will"], "午夜不是 active 转换，不重复 onWillActivate");
  assert.equal(clock.pendingCount(), 1, "仍只保留一个 timer");
  assert.equal(clock.pendingDelays()[0], 86_400_000, "午夜整点重排 = 到下一午夜 24h");
});

test("stop 清理：timer 取消、unlisten 调用、状态复位", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge();
  const { store } = makeStore(bridge, clock);
  await store.start();
  bridge.emit(true);
  assert.equal(clock.pendingCount(), 1);

  store.stop();
  assert.equal(clock.pendingCount(), 0, "stop 取消午夜 timer");
  assert.equal(bridge.state.unlistenCalls, 1, "stop 调用 unlisten");
  assert.deepEqual(store.getSnapshot(), {
    ready: false,
    active: false,
    today: "2026-10-02",
    error: null,
  }, "stop 后状态复位");

  // stop 后迟到事件（退订前已在途的旧订阅）不得重新激活
  bridge.dispatch({ revision: 5, active: true });
  assert.equal(store.getSnapshot().active, false);
});

test("refreshDate 仅校准日期：DOM 事件不改 active，隐藏期间跨午夜由校准兜底", async () => {
  const clock = fakeClock("2026-10-02T10:00:00");
  const bridge = fakeBridge();
  const { store } = makeStore(bridge, clock);
  await store.start();
  bridge.emit(true); // 10:00 → timer 指向午夜（14h）

  clock.advance(60_000); // 未跨午夜（模拟 focus/pageshow/visibilitychange 校准）
  store.refreshDate();
  assert.equal(store.getSnapshot().today, "2026-10-02");
  assert.equal(store.getSnapshot().active, true, "DOM 事件不作为 active 权威");
  assert.equal(clock.pendingCount(), 1, "仍只有一个 timer");
  assert.equal(clock.pendingDelays()[0], 14 * 60 * 60 * 1000 - 60_000, "重排后仍指向同一午夜");

  bridge.emit(false); // 隐藏：timer 取消
  clock.advance(14 * 60 * 60 * 1000 - 60_000); // 跨过午夜（已隐藏，无 timer 可触发）
  store.refreshDate(); // DOM 校准兜底：日期前进、active 不变、不排 timer
  assert.equal(store.getSnapshot().today, "2026-10-03");
  assert.equal(store.getSnapshot().active, false);
  assert.equal(clock.pendingCount(), 0, "隐藏期间不排 timer");

  bridge.emit(true); // 恢复可见：重算 timer 到下一个当地午夜
  assert.equal(clock.pendingDelays()[0], 24 * 60 * 60 * 1000, "00:00 → 下一午夜 24h");
});
