// UI 查询策略（usability-runtime-v3 §4.5，S3 交付模块本身；S8/S9 再消费）。
//
// 用法约束（§4.5 逐字）：
// - 所有查询（含 Sidebar、settings 和 WP）加 meta.uiOwned 与 activity gating；
// - 页面既有 enabled 条件与 policy.enabled 取 AND（例如有 device 且展开），且必须写在
//   spread 之后——不能被 spread 顺序覆盖为 true：
//     useQuery({ ...uiQueryPolicy(activity.active, 500), enabled: hasDevice && policy.enabled })
// - meta 如需扩展必须保留 uiOwned；staleTime=0 确保重新启用后刷新；
//   不引入后台 refetchIntervalInBackground（恒 false）。
// - 统计 interval 仅在 range 包含当前 today 时由调用方传入，历史固定范围不传（不轮询）；
//   WP/Settings 不新加 interval（不传 interval 即可）。
// - 跨设备/范围不使用 keepPreviousData：重新启用后先显示加载状态，不在新标题下显示旧数字。

export interface UiQueryPolicy {
  enabled: boolean;
  refetchInterval: number | false;
  refetchIntervalInBackground: false;
  staleTime: 0;
  meta: { uiOwned: true };
}

/** UI 查询策略工厂：默认 live=true。enabled=active；仅 active 且 live 且提供 interval
 * 时返回该 interval，其余 false。 */
export function uiQueryPolicy(active: boolean, interval?: number, live = true): UiQueryPolicy {
  return {
    enabled: active,
    refetchInterval: active && live && interval !== undefined ? interval : false,
    refetchIntervalInBackground: false,
    staleTime: 0,
    meta: { uiOwned: true },
  };
}
