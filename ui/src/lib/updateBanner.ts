// 启动更新横幅文案（§4.7）：纯函数，供 App 渲染与 vitest。
import type { UpdateApplied } from "../api/types";

export interface UpdateBannerView {
  message: string;
  showGoSettings: boolean;
}

/** 由 UpdateApplied 推导横幅主文案与是否显示「前往设置」。 */
export function formatUpdateBanner(applied: UpdateApplied): UpdateBannerView {
  const needSettings =
    applied.collectorWasRunning && !applied.collectorRestored;
  if (applied.outcome === "applied") {
    return {
      message: needSettings
        ? `已更新到 v${applied.toVersion}。采集器在更新时被停止，请到设置页启动。`
        : `已更新到 v${applied.toVersion}。`,
      showGoSettings: needSettings,
    };
  }
  // not_applied：当前仍是 fromVersion（与 current 相同）
  return {
    message: needSettings
      ? `更新未完成，当前仍是 v${applied.fromVersion}。采集器已停止，请到设置页启动。`
      : `更新未完成，当前仍是 v${applied.fromVersion}。`,
    showGoSettings: needSettings,
  };
}
