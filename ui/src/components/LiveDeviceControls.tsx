// 实时订阅容器（input-live-trigger §4.7 S9）：唯一连接型 UI；
// 拥有实时开关、手柄 sourceId 与 useLiveInput，组合 toolbar + 分布图，
// 隔离页面其余组件的 40ms 高频更新。页面只传历史数据/稳定目标/选择回调。
import { useEffect, useMemo, useRef, useState } from "react";
import type { LiveDeviceKey, LiveTarget, TopKeyRow } from "../api/types";
import { sameDevice } from "../lib/liveInputPresentation";
import { useLiveInput } from "../lib/useLiveInput";
import { DeviceLayoutStats } from "./DeviceLayoutStats";
import { LiveInputToolbar } from "./LiveInputToolbar";
import { PeripheralControlsStats } from "./PeripheralControlsStats";

export interface LiveDeviceControlsProps {
  kind: "keyboard" | "mouse" | "gamepad";
  target: LiveTarget;
  rows: readonly TopKeyRow[];
  historyState: "loading" | "ready" | "error";
  selectedCode: number | null;
  onSelect: (code: number | null) => void;
  onLiveDeviceResolved?: (device: LiveDeviceKey) => void;
}

/** 型号四字段稳定键（切型号时清手柄 sourceId） */
function deviceIdentityKey(target: LiveTarget): string {
  const d = target.device;
  if (!d) return `${target.kind}|`;
  return `${target.kind}|${d.kind}|${d.vid}|${d.pid}|${d.name}`;
}

export function LiveDeviceControls({
  kind,
  target,
  rows,
  historyState,
  selectedCode,
  onSelect,
  onLiveDeviceResolved,
}: LiveDeviceControlsProps) {
  // 实时开关默认开；仅本次页面会话内存，不持久化到设置文件
  const [enabled, setEnabled] = useState(true);
  // 手柄连接选择归本容器；切型号时清，断连时清后回落最低可用 slot
  const [ownedSourceId, setOwnedSourceId] = useState<string | null>(null);
  const [deviceKey, setDeviceKey] = useState(() => deviceIdentityKey(target));
  const nextDeviceKey = deviceIdentityKey(target);
  if (nextDeviceKey !== deviceKey) {
    setDeviceKey(nextDeviceKey);
    setOwnedSourceId(null);
  }

  const hookTarget: LiveTarget = useMemo(() => {
    if (target.kind === "gamepad") {
      return {
        kind: "gamepad",
        device: target.device,
        sourceId: ownedSourceId,
      };
    }
    return target;
  }, [target, ownedSourceId]);

  const view = useLiveInput(hookTarget, enabled);

  // 选中连接断连：清 ownedSourceId，投影回落最低可用 slot
  if (
    kind === "gamepad" &&
    ownedSourceId !== null &&
    view.connections.length > 0 &&
    !view.connections.some((c) => c.sourceId === ownedSourceId && c.available)
  ) {
    setOwnedSourceId(null);
  }

  // 四字段身份变化时低频回调；不向页面每帧传 view
  const lastResolved = useRef<LiveDeviceKey | null>(null);
  useEffect(() => {
    const d = view.selectedDevice;
    if (!d || !onLiveDeviceResolved) return;
    if (lastResolved.current && sameDevice(lastResolved.current, d)) return;
    lastResolved.current = d;
    onLiveDeviceResolved(d);
  }, [view.selectedDevice, onLiveDeviceResolved]);

  const toolbar = (
    <LiveInputToolbar
      enabled={enabled}
      onEnabledChange={setEnabled}
      view={view}
      onSourceChange={
        kind === "gamepad" ? (sourceId) => setOwnedSourceId(sourceId) : undefined
      }
    />
  );

  // off 时仍传 view（state:"off"），分布图不点亮；toolbar 始终可见
  const live = view;

  if (kind === "keyboard") {
    return (
      <DeviceLayoutStats
        kind="keyboard"
        rows={rows}
        title="按键使用"
        selectedCode={selectedCode}
        onSelect={onSelect}
        live={live}
        liveToolbar={toolbar}
        historyState={historyState}
      />
    );
  }

  return (
    <PeripheralControlsStats
      kind={kind}
      rows={rows}
      selectedCode={selectedCode}
      onSelect={onSelect}
      live={live}
      liveToolbar={toolbar}
      historyState={historyState}
    />
  );
}
