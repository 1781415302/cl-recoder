// 实时工具栏（input-live-trigger §4.7 S7）：开关、状态、手柄连接选择；不展示输入日志。
// 连接选择状态归 LiveDeviceControls 容器；本组件只回调 onSourceChange。
import { useId } from "react";
import type { DeviceLiveView } from "../api/types";

export interface LiveInputToolbarProps {
  enabled: boolean;
  onEnabledChange: (enabled: boolean) => void;
  view: DeviceLiveView;
  onSourceChange?: (sourceId: string) => void;
}

function statusLabel(view: DeviceLiveView): string {
  switch (view.state) {
    case "off":
      return "已关闭";
    case "connecting":
      return "连接中";
    case "ready":
      return view.statisticsPaused ? "实时 · 统计已暂停" : "实时";
    case "waiting":
      return view.message ?? "等待设备";
    case "unavailable":
      return view.message ?? "不可用";
    case "suspended":
      return view.message ?? "已挂起";
    default:
      return "";
  }
}

function statusChipClass(view: DeviceLiveView): string {
  switch (view.state) {
    case "ready":
      return "chip chip-positive";
    case "unavailable":
    case "suspended":
      return "chip chip-danger";
    case "waiting":
    case "connecting":
      return "chip chip-accent";
    default:
      return "chip";
  }
}

export function LiveInputToolbar({
  enabled,
  onEnabledChange,
  view,
  onSourceChange,
}: LiveInputToolbarProps) {
  const toggleId = useId();
  const sourceId = useId();
  const showConnections =
    view.connections.length > 0 && typeof onSourceChange === "function";

  return (
    <div className="source-toolbar" role="group" aria-label="实时输入">
      <label className="field-label" htmlFor={toggleId}>
        <input
          id={toggleId}
          type="checkbox"
          checked={enabled}
          onChange={(e) => onEnabledChange(e.currentTarget.checked)}
          style={{ marginRight: 8 }}
        />
        实时显示
      </label>
      <span className={statusChipClass(view)} aria-live="polite">
        {enabled ? statusLabel(view) : "已关闭"}
      </span>
      {showConnections && (
        <>
          <label className="field-label" htmlFor={sourceId}>
            手柄连接
          </label>
          <select
            id={sourceId}
            className="input"
            style={{ minHeight: 36, maxWidth: 160 }}
            value={view.selectedSourceId ?? ""}
            onChange={(e) => {
              const v = e.currentTarget.value;
              if (v) onSourceChange!(v);
            }}
          >
            {view.connections.map((c) => (
              <option key={c.sourceId} value={c.sourceId} disabled={!c.available}>
                {c.label}
                {!c.available ? "（离线）" : ""}
              </option>
            ))}
          </select>
        </>
      )}
    </div>
  );
}
