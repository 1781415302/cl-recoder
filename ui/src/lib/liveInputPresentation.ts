// 实时投影纯函数（input-live-trigger §4.6 S7）：目标过滤、pressSeq/pulse、摇杆径向裁剪。
//
// 边界：纯模块，零运行时 import（type 擦除后无 import）；内部字段私有；
// 首次连接 / 隐藏恢复 / 切目标 / sessionId·resetEpoch 变化时建立 pressSeq 基线，不回放旧短按；
// 当前 held 立即显示；ready 期间序号增加 → pulse 到接收时刻+100ms；held 优先于 pulse；
// 新来源/新 code 且 ageMs≤500 也可产生一次 pulse。同键多次边沿合并一次视觉反馈。
import type {
  DeviceLiveView,
  LiveDeviceKey,
  LiveGamepadAnalog,
  LiveInputState,
  LivePoint,
  LiveSnapshot,
  LiveSource,
  LiveTarget,
  LiveConnectionOption,
} from "../api/types";

const PULSE_MS = 100;
const NEW_CODE_PULSE_AGE_MS = 500;

/** 投影内部态（字段对调用方私有；切目标/会话时用 createLiveProjectionState 重建） */
export interface LiveProjectionState {
  /** @internal */
  _targetKey: string | null;
  /** @internal */
  _sessionId: string | null;
  /** @internal */
  _resetEpoch: string | null;
  /** @internal 已建立基线（本会话/目标下已见过至少一帧 ready） */
  _baselined: boolean;
  /** @internal sourceId:code → 已消费的 pressSeq（十进制字符串） */
  _pressSeq: Map<string, string>;
  /** @internal code → pulse 到期时刻（ms，与传入 nowMs 同源） */
  _pulseUntil: Map<number, number>;
}

export function createLiveProjectionState(): LiveProjectionState {
  return {
    _targetKey: null,
    _sessionId: null,
    _resetEpoch: null,
    _baselined: false,
    _pressSeq: new Map(),
    _pulseUntil: new Map(),
  };
}

/** 四字段型号键一致（原名，非 nickname） */
export function sameDevice(a: LiveDeviceKey, b: LiveDeviceKey): boolean {
  return (
    a.kind === b.kind &&
    a.vid === b.vid &&
    a.pid === b.pid &&
    a.name === b.name
  );
}

/** 只径向裁到单位圆，保持方向；不用统计死区 */
export function projectStick(point: LivePoint): LivePoint {
  const x = point.x;
  const y = point.y;
  if (!Number.isFinite(x) || !Number.isFinite(y)) return { x: 0, y: 0 };
  const r = Math.hypot(x, y);
  if (r <= 1 || r === 0) return { x, y };
  return { x: x / r, y: y / r };
}

function cmpSeq(a: string, b: string): number {
  try {
    const ba = BigInt(a);
    const bb = BigInt(b);
    return ba < bb ? -1 : ba > bb ? 1 : 0;
  } catch {
    return a < b ? -1 : a > b ? 1 : 0;
  }
}

export function targetStableKey(target: LiveTarget | null): string {
  if (target === null) return "";
  if (target.kind === "keyboard") {
    const d = target.device;
    return d
      ? `keyboard|${d.kind}|${d.vid}|${d.pid}|${d.name}`
      : "keyboard|";
  }
  if (target.kind === "mouse") {
    const d = target.device;
    const dk = d ? `${d.kind}|${d.vid}|${d.pid}|${d.name}` : "";
    return `mouse|${dk}|${target.sourceKey ?? ""}`;
  }
  const d = target.device;
  const dk = d ? `${d.kind}|${d.vid}|${d.pid}|${d.name}` : "";
  return `gamepad|${dk}|${target.sourceId ?? ""}`;
}

function emptyView(
  state: DeviceLiveView["state"],
  message: string | null = null,
  extras: Partial<DeviceLiveView> = {},
): DeviceLiveView {
  return {
    state,
    heldCodes: extras.heldCodes ?? [],
    pulseCodes: extras.pulseCodes ?? [],
    analog: extras.analog ?? null,
    unsupportedCodes: extras.unsupportedCodes ?? [],
    statisticsPaused: extras.statisticsPaused ?? false,
    message,
    selectedDevice: extras.selectedDevice ?? null,
    connections: extras.connections ?? [],
    selectedSourceId: extras.selectedSourceId ?? null,
  };
}

function resetBaseline(state: LiveProjectionState): void {
  state._baselined = false;
  state._pressSeq.clear();
  state._pulseUntil.clear();
  state._sessionId = null;
  state._resetEpoch = null;
}

function projectAnalog(analog: LiveGamepadAnalog | null): LiveGamepadAnalog | null {
  if (analog === null) return null;
  return {
    leftStick: projectStick(analog.leftStick),
    rightStick: projectStick(analog.rightStick),
    leftTrigger: analog.leftTrigger,
    rightTrigger: analog.rightTrigger,
  };
}

function sourcesOfKind(
  snapshot: LiveSnapshot,
  kind: LiveTarget["kind"],
): LiveSource[] {
  return snapshot.sources.filter((s) => s.kind === kind);
}

function matchKeyboardMouse(
  sources: LiveSource[],
  target: Extract<LiveTarget, { kind: "keyboard" | "mouse" }>,
): { matched: LiveSource[]; selectedDevice: LiveDeviceKey | null } {
  if (target.kind === "mouse" && target.sourceKey) {
    const byKey = sources.filter(
      (s) => s.kind === "mouse" && s.sourceKey === target.sourceKey,
    );
    return { matched: byKey, selectedDevice: byKey[0]?.device ?? target.device };
  }
  if (target.device) {
    const byDev = sources.filter((s) => sameDevice(s.device, target.device!));
    if (byDev.length > 0) {
      return { matched: byDev, selectedDevice: target.device };
    }
    // 用户已明确选过型号：保留已知身份，即使当前无实时来源
    return { matched: [], selectedDevice: target.device };
  }
  // 尚未绑定历史型号：从第一份可用实时来源建立
  if (sources.length > 0) {
    const first = sources[0];
    const selected = first.device;
    const matched = sources.filter((s) => sameDevice(s.device, selected));
    return { matched, selectedDevice: selected };
  }
  return { matched: [], selectedDevice: null };
}

function matchGamepad(
  sources: LiveSource[],
  target: Extract<LiveTarget, { kind: "gamepad" }>,
): {
  matched: LiveSource | null;
  selectedDevice: LiveDeviceKey | null;
  connections: LiveConnectionOption[];
  selectedSourceId: string | null;
} {
  const pads = sources.filter((s): s is Extract<LiveSource, { kind: "gamepad" }> => s.kind === "gamepad");
  let pool = pads;
  let selectedDevice: LiveDeviceKey | null = target.device;
  if (target.device) {
    pool = pads.filter((s) => sameDevice(s.device, target.device!));
  } else if (pads.length > 0) {
    // 按最低 slot 的型号建立
    const sorted = [...pads].sort((a, b) => a.slot - b.slot);
    selectedDevice = sorted[0].device;
    pool = pads.filter((s) => sameDevice(s.device, selectedDevice!));
  }

  const connections: LiveConnectionOption[] = [...pool]
    .sort((a, b) => a.slot - b.slot)
    .map((s) => ({
      sourceId: s.sourceId,
      label: `手柄${s.slot + 1}`,
      available: s.availability === "ready",
    }));

  let selected: Extract<LiveSource, { kind: "gamepad" }> | null = null;
  let selectedSourceId: string | null = target.sourceId;

  if (selectedSourceId) {
    selected = pool.find((s) => s.sourceId === selectedSourceId) ?? null;
    if (!selected || selected.availability !== "ready") {
      // 选中连接断连：回落最低可用 slot，清 sourceId（由调用方感知 selectedSourceId=null 建议）
      selectedSourceId = null;
      selected = null;
    }
  }
  if (!selected) {
    const available = pool
      .filter((s) => s.availability === "ready")
      .sort((a, b) => a.slot - b.slot);
    selected = available[0] ?? null;
    selectedSourceId = selected ? selected.sourceId : null;
  }

  return {
    matched: selected,
    selectedDevice: selectedDevice ?? (selected ? selected.device : null),
    connections,
    selectedSourceId,
  };
}

function applyPresses(
  state: LiveProjectionState,
  matched: LiveSource[],
  receivedAtMs: number,
  establishBaseline: boolean,
): void {
  if (establishBaseline) {
    state._pressSeq.clear();
    state._pulseUntil.clear();
    for (const src of matched) {
      for (const press of src.digital.presses) {
        state._pressSeq.set(`${src.sourceId}:${press.code}`, press.pressSeq);
      }
    }
    state._baselined = true;
    return;
  }

  for (const src of matched) {
    for (const press of src.digital.presses) {
      const key = `${src.sourceId}:${press.code}`;
      const prev = state._pressSeq.get(key);
      if (prev === undefined) {
        // 新来源/新 code：ageMs≤500 可产生一次 pulse
        state._pressSeq.set(key, press.pressSeq);
        if (press.ageMs <= NEW_CODE_PULSE_AGE_MS) {
          const until = receivedAtMs + PULSE_MS;
          const existing = state._pulseUntil.get(press.code) ?? 0;
          if (until > existing) state._pulseUntil.set(press.code, until);
        }
        continue;
      }
      if (cmpSeq(press.pressSeq, prev) > 0) {
        state._pressSeq.set(key, press.pressSeq);
        const until = receivedAtMs + PULSE_MS;
        const existing = state._pulseUntil.get(press.code) ?? 0;
        if (until > existing) state._pulseUntil.set(press.code, until);
      }
    }
  }
}

function collectHeld(matched: LiveSource[]): number[] {
  const set = new Set<number>();
  for (const src of matched) {
    for (const code of src.digital.heldCodes) set.add(code);
  }
  return [...set].sort((a, b) => a - b);
}

function collectPulses(
  state: LiveProjectionState,
  held: readonly number[],
  nowMs: number,
): number[] {
  const heldSet = new Set(held);
  const active: number[] = [];
  for (const [code, until] of state._pulseUntil) {
    if (until <= nowMs) {
      state._pulseUntil.delete(code);
      continue;
    }
    if (heldSet.has(code)) continue; // 当前按住优先于 pulse
    active.push(code);
  }
  return active.sort((a, b) => a - b);
}

/** 下一笔 pulse 到期剩余 ms；无则 null（供 hook 排期，即使快照等待中也要到期更新） */
export function nextPulseDelayMs(
  state: LiveProjectionState,
  nowMs: number,
): number | null {
  let min: number | null = null;
  for (const until of state._pulseUntil.values()) {
    const d = until - nowMs;
    if (d <= 0) continue;
    if (min === null || d < min) min = d;
  }
  return min;
}

/**
 * 纯计算 held 并集、连接选择、短按与过期。
 * state 内部字段私有；切目标/会话时由调用方重建或本函数自动复位基线。
 */
export function updateLiveProjection(
  state: LiveProjectionState,
  input: LiveInputState,
  target: LiveTarget | null,
  nowMs: number,
): DeviceLiveView {
  if (target === null) {
    resetBaseline(state);
    state._targetKey = null;
    return emptyView("off");
  }

  const tKey = targetStableKey(target);
  if (state._targetKey !== tKey) {
    resetBaseline(state);
    state._targetKey = tKey;
  }

  if (input.state === "off") {
    resetBaseline(state);
    return emptyView("off");
  }

  if (input.state === "connecting") {
    return emptyView("connecting", null, {
      selectedDevice: target.device,
      selectedSourceId: target.kind === "gamepad" ? target.sourceId : null,
    });
  }

  if (input.state === "unavailable") {
    resetBaseline(state);
    const errMsg = input.reply?.error?.message ?? "实时输入不可用";
    return emptyView("unavailable", errMsg, {
      selectedDevice: target.device,
      selectedSourceId: target.kind === "gamepad" ? target.sourceId : null,
    });
  }

  // ready
  const reply = input.reply;
  const snapshot = reply?.snapshot ?? null;
  if (!snapshot) {
    resetBaseline(state);
    return emptyView("unavailable", "实时输入不可用", {
      selectedDevice: target.device,
    });
  }

  const sessionChanged =
    state._sessionId !== snapshot.sessionId ||
    state._resetEpoch !== snapshot.resetEpoch;
  const establishBaseline = !state._baselined || sessionChanged;
  if (sessionChanged) {
    state._sessionId = snapshot.sessionId;
    state._resetEpoch = snapshot.resetEpoch;
  }

  const paused = snapshot.statisticsPaused;
  const captureSuspended = snapshot.captureState === "suspended";

  if (target.kind === "gamepad") {
    const kindSources = sourcesOfKind(snapshot, "gamepad");
    const { matched, selectedDevice, connections, selectedSourceId } =
      matchGamepad(kindSources, target);

    if (kindSources.length === 0) {
      if (establishBaseline) {
        state._pressSeq.clear();
        state._pulseUntil.clear();
        state._baselined = true;
      }
      return emptyView(captureSuspended ? "suspended" : "waiting", "等待手柄连接", {
        statisticsPaused: paused,
        selectedDevice,
        connections,
        selectedSourceId: null,
      });
    }

    if (!matched || matched.kind !== "gamepad") {
      return emptyView("unavailable", "所选手柄连接不可用", {
        statisticsPaused: paused,
        selectedDevice,
        connections,
        selectedSourceId: null,
      });
    }

    const receivedAt = input.receivedAtMs ?? nowMs;
    applyPresses(state, [matched], receivedAt, establishBaseline);
    const held = collectHeld([matched]);
    const pulses = collectPulses(state, held, nowMs);
    const viewState: DeviceLiveView["state"] = captureSuspended
      ? "suspended"
      : matched.availability !== "ready"
        ? "unavailable"
        : "ready";

    return {
      state: viewState,
      heldCodes: held,
      pulseCodes: pulses,
      analog: projectAnalog(matched.analog),
      unsupportedCodes: [...matched.unsupportedCodes],
      statisticsPaused: paused,
      message: paused ? "统计已暂停" : captureSuspended ? "采集已挂起" : null,
      selectedDevice,
      connections,
      selectedSourceId,
    };
  }

  // keyboard / mouse
  const kindSources = sourcesOfKind(snapshot, target.kind);
  const { matched, selectedDevice } = matchKeyboardMouse(kindSources, target);

  if (kindSources.length === 0) {
    if (establishBaseline) {
      state._pressSeq.clear();
      state._pulseUntil.clear();
      state._baselined = true;
    }
    return emptyView(captureSuspended ? "suspended" : "waiting", "等待输入设备", {
      statisticsPaused: paused,
      selectedDevice,
    });
  }

  if (matched.length === 0) {
    // 有同 kind 来源但型号/sourceKey 未匹配
    return emptyView("waiting", "等待所选设备", {
      statisticsPaused: paused,
      selectedDevice,
    });
  }

  const receivedAt = input.receivedAtMs ?? nowMs;
  applyPresses(state, matched, receivedAt, establishBaseline);
  const held = collectHeld(matched);
  const pulses = collectPulses(state, held, nowMs);

  return {
    state: captureSuspended ? "suspended" : "ready",
    heldCodes: held,
    pulseCodes: pulses,
    analog: null,
    unsupportedCodes: [],
    statisticsPaused: paused,
    message: paused ? "统计已暂停" : captureSuspended ? "采集已挂起" : null,
    selectedDevice,
    connections: [],
    selectedSourceId: null,
  };
}
