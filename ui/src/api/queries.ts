// 共享查询 hooks：统一 query key 与轮询节奏（§4.5 activity gating + meta.uiOwned）。
//
// S9 接线（usability-runtime-v3 §4.5）：全部 hooks 消费 S3 的 uiQueryPolicy——
// - enabled = 活动状态（AppActivityProvider 的 Tauri 权威）：inactive 时 focus/invalidation
//   均不发请求；正在执行的一次请求允许完成（不扩 I/O 取消）；
// - 统计 interval 仅在范围包含当前 today 时传入（历史固定范围不轮询）；
// - refetchIntervalInBackground 恒 false、staleTime=0（重新启用后刷新）、meta.uiOwned=true；
// - 跨范围不使用 keepPreviousData：范围变化先显示加载状态，不在新标题下显示旧数字
//   （同 key 普通刷新仍显示缓存）。
import { useQuery } from "@tanstack/react-query";
import * as client from "./client";
import { uiQueryPolicy } from "./queryPolicy";
import type { Range } from "./types";
import { useAppActivity } from "../lib/AppActivityProvider";

/** 今日/总览数据轮询间隔（近实时；数据 0.5s flush 落库，感知延迟 ≤1s） */
export const POLL_TODAY_MS = 500;

/** 闭区间 [from, to] 是否包含当前 today（§4.5：仅包含 today 的范围才轮询） */
function rangeIncludesToday(range: Range, today: string): boolean {
  return range.from <= today && today <= range.to;
}

export function useOverview(range: Range) {
  const activity = useAppActivity();
  return useQuery({
    queryKey: ["overview", range.from, range.to],
    queryFn: () => client.getOverview(range.from, range.to),
    ...uiQueryPolicy(activity.active, rangeIncludesToday(range, activity.today) ? POLL_TODAY_MS : undefined),
  });
}

export function useCollectorStatus() {
  const activity = useAppActivity();
  return useQuery({
    queryKey: ["collectorStatus"],
    queryFn: () => client.collectorStatus(),
    ...uiQueryPolicy(activity.active, POLL_TODAY_MS),
  });
}

export function useDevices() {
  const activity = useAppActivity();
  return useQuery({
    queryKey: ["devices"],
    queryFn: () => client.getDevices(),
    ...uiQueryPolicy(activity.active, 2_000),
  });
}

export function useSettings() {
  const activity = useAppActivity();
  return useQuery({
    queryKey: ["settings"],
    queryFn: () => client.getSettings(),
    // §4.5：Settings 不新加 interval（仅 activity gating + uiOwned）
    ...uiQueryPolicy(activity.active),
  });
}
