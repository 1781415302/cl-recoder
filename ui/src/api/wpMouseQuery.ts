// WhatPulse 鼠标排行（按键/滚轮）查询配置工厂（F7 缓存修复，§4.8）。
//
// 修复点：WhatPulse 页两个排行的 queryKey 原先只有 ["wpButtons"]/["wpScrolls"]，
// 不含日期与 limit，导致改日期不触发查询、不同日期/limit 的缓存互相串用。
// 此处把 (from, to, limit) 全部编入 key，queryFn 只捕获本次调用的标量快照，
// 不读取后续可能被改写的 range 对象。
//
// 纯模块约束：只 type-import Range，不导入 client（含 import.meta.env）与 React
// 运行时；可在 Node 内建测试中经 typescript.transpileModule 转为 ESM 后由 data URL 加载。
import type { Range } from "./types";

export type WpMouseQueryKind = "buttons" | "scrolls";

/** queryKey 固定形状：[前缀, from, to, limit]；前缀与导入完成后的失效前缀逐字一致 */
export type WpMouseQueryKey =
  readonly ["wpButtons" | "wpScrolls", string, string, number];

export interface WpMouseQueryOptions<T> {
  queryKey: WpMouseQueryKey;
  queryFn: () => Promise<T>;
}

const PREFIX: Record<WpMouseQueryKind, "wpButtons" | "wpScrolls"> = {
  buttons: "wpButtons",
  scrolls: "wpScrolls",
};

/** 生成 useQuery 配置：key 绑定 (from, to, limit)，queryFn 只用标量快照请求 loader */
export function wpMouseQueryOptions<T>(
  kind: WpMouseQueryKind,
  range: Range,
  limit: number,
  fetch: (from: string, to: string, limit: number) => Promise<T>,
): WpMouseQueryOptions<T> {
  const from = range.from;
  const to = range.to;
  const prefix = PREFIX[kind];
  return {
    queryKey: [prefix, from, to, limit] as const,
    queryFn: () => fetch(from, to, limit),
  };
}
