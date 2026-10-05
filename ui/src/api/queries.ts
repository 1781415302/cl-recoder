// 共享查询 hooks：统一 query key 与轮询节奏（§4.5 activity gating + meta.uiOwned）。
//
// S9 接线（usability-runtime-v3 §4.5）：全部 hooks 消费 S3 的 uiQueryPolicy——
// - enabled = 活动状态（AppActivityProvider 的 Tauri 权威）：inactive 时 focus/invalidation
//   均不发请求；正在执行的一次请求允许完成（不扩 I/O 取消）；
// - 统计 interval 仅在范围包含当前 today 时传入（历史固定范围不轮询）；
// - refetchIntervalInBackground 恒 false、staleTime=0（重新启用后刷新）、meta.uiOwned=true；
// - 跨范围不使用 keepPreviousData：范围变化先显示加载状态，不在新标题下显示旧数字
//   （同 key 普通刷新仍显示缓存）。
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import * as client from "./client";
import { uiQueryPolicy } from "./queryPolicy";
import type {
  GamepadMotionSummary,
  MouseMotionSummary,
  MouseSources,
  Range,
} from "./types";
import { useAppActivity } from "../lib/AppActivityProvider";

/** 今日/总览数据轮询间隔（近实时；数据 0.5s flush 落库，感知延迟 ≤1s） */
export const POLL_TODAY_MS = 500;

/** 鼠标来源列表轮询间隔（motion-dpi §4.5：sources 活动时 2 秒） */
export const POLL_SOURCES_MS = 2_000;

/** 运动/按钮查询在范围含今日时的轮询间隔（motion-dpi §4.5：活动且 range 含今日时 1 秒） */
export const POLL_MOTION_MS = 1_000;

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

/* ===== 运动查询 hooks（motion-dpi §4.5；全部复用 uiQueryPolicy，跨 source/range 不 keepPreviousData） ===== */

/** 鼠标运动来源列表（§4.5：sources 活动时 2 秒；needs_upgrade 由页面引导"启动/更新采集器后可用"） */
export function useMouseSources(): UseQueryResult<MouseSources, Error> {
  const activity = useAppActivity();
  return useQuery({
    queryKey: ["mouseSources"],
    queryFn: () => client.getMouseSources(),
    ...uiQueryPolicy(activity.active, POLL_SOURCES_MS),
  });
}

/** 单来源鼠标运动汇总（§4.5：活动且 range 含今日时 1 秒，历史无 interval）；
 * days 随 summary 返回（展开表复用，不额外查询）。 */
export function useMouseMotion(
  sourceId: number | null,
  range: Range,
): UseQueryResult<MouseMotionSummary, Error> {
  const activity = useAppActivity();
  const policy = uiQueryPolicy(
    activity.active,
    rangeIncludesToday(range, activity.today) ? POLL_MOTION_MS : undefined,
  );
  return useQuery({
    queryKey: ["mouseMotion", sourceId, range.from, range.to],
    queryFn: () => {
      if (sourceId === null) throw new Error("未选择鼠标运动来源（enabled 保证不触发）");
      return client.getMouseMotion(sourceId, range.from, range.to);
    },
    ...policy,
    // §4.5：页面自身条件（来源已选）与 policy.enabled 取 AND，必须写在 spread 之后
    enabled: sourceId !== null && policy.enabled,
  });
}

/** 手柄摇杆运动汇总（按型号 deviceId；§4.5 轮询节奏与 useMouseMotion 相同）。 */
export function useGamepadMotion(
  deviceId: number | null,
  range: Range,
): UseQueryResult<GamepadMotionSummary, Error> {
  const activity = useAppActivity();
  const policy = uiQueryPolicy(
    activity.active,
    rangeIncludesToday(range, activity.today) ? POLL_MOTION_MS : undefined,
  );
  return useQuery({
    queryKey: ["gamepadMotion", deviceId, range.from, range.to],
    queryFn: () => {
      if (deviceId === null) throw new Error("未选择手柄设备（enabled 保证不触发）");
      return client.getGamepadMotion(deviceId, range.from, range.to);
    },
    ...policy,
    // §4.5：页面自身条件（设备已选）与 policy.enabled 取 AND，必须写在 spread 之后
    enabled: deviceId !== null && policy.enabled,
  });
}
