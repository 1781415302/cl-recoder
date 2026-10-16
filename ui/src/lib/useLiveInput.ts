// useLiveInput —— 实时投影 hook（input-live-trigger §4.6 S7）。
//
// 只在 LiveDeviceControls 内调用；由 AppActivity.active、页面及开关控制启停，
// 不接日期范围参数。按 kind/四字段/sourceKey/sourceId 稳定值键判断目标变化，
// 不按 target 对象引用重启。100ms pulse 到期更新独立于下一笔快照；
// 500ms stale 由 store 独立排期。
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { getLiveInputSnapshot } from "../api/client";
import { LiveInputStore, systemLiveClock } from "../api/liveInputStore";
import type { DeviceLiveView, LiveTarget } from "../api/types";
import { useAppActivity } from "./AppActivityProvider";
import {
  createLiveProjectionState,
  nextPulseDelayMs,
  targetStableKey,
  updateLiveProjection,
  type LiveProjectionState,
} from "./liveInputPresentation";

/** 模块级单一轮询器（全页共享；所有权由各 hook 实例的 start/stop token 管理） */
let sharedStore: LiveInputStore | null = null;

function getSharedStore(): LiveInputStore {
  if (!sharedStore) {
    sharedStore = new LiveInputStore(getLiveInputSnapshot, systemLiveClock);
  }
  return sharedStore;
}

/** 测试接缝：替换/清空共享 store（仅测试） */
export function __setSharedLiveInputStoreForTests(
  store: LiveInputStore | null,
): void {
  sharedStore = store;
}

const OFF_VIEW: DeviceLiveView = {
  state: "off",
  heldCodes: [],
  pulseCodes: [],
  analog: null,
  unsupportedCodes: [],
  statisticsPaused: false,
  message: null,
  selectedDevice: null,
  connections: [],
  selectedSourceId: null,
};

/**
 * @param target 实时目标；null 表示不显示设备（整页卸载分布图）
 * @param enabled 实时开关（默认开由容器持有；仅本次页面会话内存）
 */
export function useLiveInput(
  target: LiveTarget | null,
  enabled: boolean,
): DeviceLiveView {
  const activity = useAppActivity();
  const store = getSharedStore();
  const input = useSyncExternalStore(store.subscribe, store.getSnapshot);

  const targetKey = targetStableKey(target);
  const shouldRun = enabled && activity.active && target !== null;

  const projectionRef = useRef<LiveProjectionState>(createLiveProjectionState());
  const [pulseTick, setPulseTick] = useState(0);

  useEffect(() => {
    if (!shouldRun) return;
    projectionRef.current = createLiveProjectionState();
    const token = store.start();
    return () => {
      store.stop(token);
      projectionRef.current = createLiveProjectionState();
    };
  }, [shouldRun, targetKey, store]);

  const view = useMemo(() => {
    if (!shouldRun || target === null) return OFF_VIEW;
    return updateLiveProjection(
      projectionRef.current,
      input,
      target,
      systemLiveClock.nowMs(),
    );
  // pulseTick 故意列入依赖：到期定时器触发时强制重算投影（input 可能未变）
  // eslint-disable-next-line react-hooks/exhaustive-deps -- pulseTick 有意触发重算
  }, [shouldRun, target, targetKey, input, pulseTick]);

  // 独立 pulse 到期定时器（即使下一笔快照仍在等待）
  useEffect(() => {
    if (!shouldRun) return;
    const delay = nextPulseDelayMs(projectionRef.current, systemLiveClock.nowMs());
    if (delay === null) return;
    const id = systemLiveClock.setTimeout(() => {
      setPulseTick((n) => n + 1);
    }, delay);
    return () => systemLiveClock.clearTimeout(id);
  }, [shouldRun, view.pulseCodes, input, pulseTick]);

  return view;
}
