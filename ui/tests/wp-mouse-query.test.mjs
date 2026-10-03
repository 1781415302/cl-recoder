// F7 回归：WhatPulse 鼠标排行（buttons/scrolls）queryKey 必须绑定 (from, to, limit)。
//
// 用 Node 内建 test + @tanstack/react-query 的 QueryClient/QueryObserver 验证真实缓存行为：
// - 日期 A→B 必须请求 B 并展示 B，回 A 命中 A 缓存；
// - limit 隔离；key 形状与 loader 收到的标量逐字绑定；
// - 可变 range 改写后旧 key/旧 queryFn 仍绑定旧范围；
// - ['wpButtons'] 前缀失效覆盖全部 buttons 日期且不影响 scrolls。
// 工厂模块（../src/api/wpMouseQuery.ts，纯 TS、仅 type-import）经 typescript.transpileModule
// 转为 ESM 后由 data URL 导入：不落盘构建目录、不触碰 client.ts 的 import.meta.env。
import assert from "node:assert/strict";
import { test, afterEach } from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";
import { QueryClient, QueryObserver } from "@tanstack/react-query";

const { wpMouseQueryOptions } = await import(
  "data:text/javascript;base64," +
    Buffer.from(
      ts.transpileModule(
        await readFile(new URL("../src/api/wpMouseQuery.ts", import.meta.url), "utf8"),
        { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2020 } },
      ).outputText,
    ).toString("base64"),
);

// 假 loader：数据随 (from, to, limit) 变化，并记录每次实际收到的标量参数
function makeLoader(kind) {
  const calls = [];
  const fn = (from, to, limit) => {
    calls.push([from, to, limit]);
    return Promise.resolve(`${kind}:${from}~${to}#${limit}`);
  };
  return { calls, fn };
}

// 测试用 QueryClient：staleTime=Infinity 排除自动过期干扰；retry=false 让失败立刻暴露
function makeClient() {
  return new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
}

let qc = null;
const tracked = [];

function track(options) {
  const observer = new QueryObserver(qc, options);
  tracked.push(observer);
  return observer;
}

// 等待 observer 到达稳定成功态（success 且不在请求中且非失效）。
// 两条已实测语义（@tanstack/react-query 5.104）决定了这里不能只靠订阅回调：
// - 纯缓存命中时订阅不触发任何回调 → 必须先查 getCurrentResult()；
// - 前缀失效后的旧缓存 getCurrentResult 也是 success/idle 但 isStale=true → 必须排除，
//   否则会把失效前的旧数据当稳态、漏掉重取。
async function awaitStableSuccess(observer) {
  const initial = observer.getCurrentResult();
  if (initial.isSuccess && initial.fetchStatus === "idle" && !initial.isStale) return initial;
  return new Promise((resolve, reject) => {
    const unsub = observer.subscribe((result) => {
      if (result.isSuccess && result.fetchStatus === "idle" && !result.isStale) {
        unsub();
        resolve(result);
      } else if (result.isError) {
        unsub();
        reject(result.error);
      }
    });
  });
}

// 每个测试后销毁 observer 并清空 client，避免 gcTime 定时器把 node 进程挂住
afterEach(() => {
  for (const observer of tracked) observer.destroy();
  tracked.length = 0;
  if (qc) {
    qc.clear();
    qc = null;
  }
});

test("buttons 日期 A→B 拉取并展示 B，回 A 命中 A 缓存", { timeout: 10_000 }, async () => {
  qc = makeClient();
  const loader = makeLoader("buttons");
  const A = { from: "2026-09-01", to: "2026-09-10" };
  const B = { from: "2026-10-01", to: "2026-10-10" };

  const resA = await awaitStableSuccess(track(wpMouseQueryOptions("buttons", A, 20, loader.fn)));
  assert.equal(resA.data, "buttons:2026-09-01~2026-09-10#20");
  assert.deepEqual(loader.calls, [["2026-09-01", "2026-09-10", 20]]);

  const resB = await awaitStableSuccess(track(wpMouseQueryOptions("buttons", B, 20, loader.fn)));
  assert.equal(resB.data, "buttons:2026-10-01~2026-10-10#20");
  assert.deepEqual(loader.calls, [
    ["2026-09-01", "2026-09-10", 20],
    ["2026-10-01", "2026-10-10", 20],
  ]);

  const resA2 = await awaitStableSuccess(track(wpMouseQueryOptions("buttons", A, 20, loader.fn)));
  assert.equal(resA2.data, "buttons:2026-09-01~2026-09-10#20");
  assert.equal(loader.calls.length, 2); // 未新增请求：回 A 命中 A 缓存
});

test("scrolls 日期 A→B 拉取并展示 B，回 A 命中 A 缓存", { timeout: 10_000 }, async () => {
  qc = makeClient();
  const loader = makeLoader("scrolls");
  const A = { from: "2026-09-01", to: "2026-09-10" };
  const B = { from: "2026-11-01", to: "2026-11-30" };

  const resA = await awaitStableSuccess(track(wpMouseQueryOptions("scrolls", A, 20, loader.fn)));
  assert.equal(resA.data, "scrolls:2026-09-01~2026-09-10#20");
  assert.deepEqual(loader.calls, [["2026-09-01", "2026-09-10", 20]]);

  const resB = await awaitStableSuccess(track(wpMouseQueryOptions("scrolls", B, 20, loader.fn)));
  assert.equal(resB.data, "scrolls:2026-11-01~2026-11-30#20");
  assert.deepEqual(loader.calls, [
    ["2026-09-01", "2026-09-10", 20],
    ["2026-11-01", "2026-11-30", 20],
  ]);

  const resA2 = await awaitStableSuccess(track(wpMouseQueryOptions("scrolls", A, 20, loader.fn)));
  assert.equal(resA2.data, "scrolls:2026-09-01~2026-09-10#20");
  assert.equal(loader.calls.length, 2); // 未新增请求：回 A 命中 A 缓存
});

test("limit 隔离：同日期不同 limit 各自请求、互不串用", { timeout: 10_000 }, async () => {
  qc = makeClient();
  const loader = makeLoader("buttons");
  const range = { from: "2026-09-01", to: "2026-09-10" };
  const o20 = wpMouseQueryOptions("buttons", range, 20, loader.fn);
  const o50 = wpMouseQueryOptions("buttons", range, 50, loader.fn);
  assert.deepEqual([...o20.queryKey], ["wpButtons", "2026-09-01", "2026-09-10", 20]);
  assert.deepEqual([...o50.queryKey], ["wpButtons", "2026-09-01", "2026-09-10", 50]);

  const r20 = await awaitStableSuccess(track(o20));
  const r50 = await awaitStableSuccess(track(o50));
  assert.equal(r20.data, "buttons:2026-09-01~2026-09-10#20");
  assert.equal(r50.data, "buttons:2026-09-01~2026-09-10#50");
  assert.deepEqual(loader.calls, [
    ["2026-09-01", "2026-09-10", 20],
    ["2026-09-01", "2026-09-10", 50],
  ]);

  const r20b = await awaitStableSuccess(track(o20));
  assert.equal(r20b.data, "buttons:2026-09-01~2026-09-10#20");
  assert.equal(loader.calls.length, 2); // 20 的缓存未被 50 串用
});

test("绑定参数：key 形状固定且 loader 收到同一组标量", () => {
  const loaderB = makeLoader("buttons");
  const loaderS = makeLoader("scrolls");
  const b = wpMouseQueryOptions("buttons", { from: "2026-09-01", to: "2026-09-10" }, 20, loaderB.fn);
  const s = wpMouseQueryOptions("scrolls", { from: "2026-11-01", to: "2026-11-30" }, 20, loaderS.fn);
  assert.deepEqual([...b.queryKey], ["wpButtons", "2026-09-01", "2026-09-10", 20]);
  assert.deepEqual([...s.queryKey], ["wpScrolls", "2026-11-01", "2026-11-30", 20]);

  return Promise.all([b.queryFn(), s.queryFn()]).then(([db, ds]) => {
    assert.equal(db, "buttons:2026-09-01~2026-09-10#20");
    assert.equal(ds, "scrolls:2026-11-01~2026-11-30#20");
    assert.deepEqual(loaderB.calls, [["2026-09-01", "2026-09-10", 20]]);
    assert.deepEqual(loaderS.calls, [["2026-11-01", "2026-11-30", 20]]);
  });
});

test("可变 range：改写原对象后旧 key 与旧 queryFn 仍绑定旧范围", async () => {
  const loader = makeLoader("scrolls");
  const range = { from: "2026-09-01", to: "2026-09-10" };
  const opts = wpMouseQueryOptions("scrolls", range, 20, loader.fn);

  range.from = "2026-12-01"; // 模拟同一可变对象随后被改写（如 setState 复用）
  range.to = "2026-12-31";

  assert.deepEqual([...opts.queryKey], ["wpScrolls", "2026-09-01", "2026-09-10", 20]);
  const data = await opts.queryFn();
  assert.equal(data, "scrolls:2026-09-01~2026-09-10#20");
  assert.deepEqual(loader.calls, [["2026-09-01", "2026-09-10", 20]]); // 旧 queryFn 仍请求旧范围
});

test("['wpButtons'] 前缀失效覆盖全部 buttons 日期且不影响 scrolls", { timeout: 10_000 }, async () => {
  qc = makeClient();
  const loaderB = makeLoader("buttons");
  const loaderS = makeLoader("scrolls");
  const bA = wpMouseQueryOptions("buttons", { from: "2026-09-01", to: "2026-09-10" }, 20, loaderB.fn);
  const bB = wpMouseQueryOptions("buttons", { from: "2026-10-01", to: "2026-10-10" }, 20, loaderB.fn);
  const sA = wpMouseQueryOptions("scrolls", { from: "2026-09-01", to: "2026-09-10" }, 20, loaderS.fn);

  await awaitStableSuccess(track(bA));
  await awaitStableSuccess(track(bB));
  await awaitStableSuccess(track(sA));
  assert.equal(loaderB.calls.length, 2);
  assert.equal(loaderS.calls.length, 1);

  // 与 WhatPulse 页导入成功后一致的失效调用（工厂 key 保留该前缀，仍能命中）
  await qc.invalidateQueries({ queryKey: ["wpButtons"] });
  assert.equal(loaderB.calls.length, 2); // 无活动 observer：只标记失效，不立即重取
  assert.equal(loaderS.calls.length, 1);

  await awaitStableSuccess(track(bA)); // 失效后 buttons 任意日期都重新请求
  await awaitStableSuccess(track(bB));
  assert.deepEqual(loaderB.calls, [
    ["2026-09-01", "2026-09-10", 20],
    ["2026-10-01", "2026-10-10", 20],
    ["2026-09-01", "2026-09-10", 20],
    ["2026-10-01", "2026-10-10", 20],
  ]);

  await awaitStableSuccess(track(sA)); // scrolls 未失效：staleTime=Infinity 下仍命中缓存
  assert.deepEqual(loaderS.calls, [["2026-09-01", "2026-09-10", 20]]);
});
