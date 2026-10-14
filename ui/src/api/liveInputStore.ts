// LiveInputStore —— 前端实时快照轮询（input-live-trigger §4.6 S7）。
//
// 可注入 fetch/clock；40ms 目标间隔；single-flight 持续到 Promise 实际完成；
// 失败或结构化 error 立刻清态、1s 后再试；距最后成功接收 >500ms 自动 unavailable 并清态。
// start/stop 用所有权 token：旧 cleanup 不能停止新所有者；迟到结果按 generation 丢弃。
// 新 start 不得在旧 I/O 结束前发第二笔；旧结果丢弃后立即服务新所有者。
//
// 纯模块：对 types 只 type-import，无 React/Tauri 运行时依赖——Node 测试可转译直载。
import type { LiveInputState, LiveSnapshotReply } from "./types";

/** 可注入时钟（生产用 performance.now + window timers） */
export interface LiveClock {
  nowMs(): number;
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(id: unknown): void;
}

const POLL_INTERVAL_MS = 40;
const STALE_AFTER_MS = 500;
const RETRY_AFTER_MS = 1000;

const OFF_STATE: LiveInputState = {
  state: "off",
  reply: null,
  receivedAtMs: null,
};

function isSuccess(reply: LiveSnapshotReply): boolean {
  return reply.snapshot !== null && reply.error === null;
}

export class LiveInputStore {
  private readonly fetchFn: () => Promise<LiveSnapshotReply>;
  private readonly clock: LiveClock;

  private state: LiveInputState = OFF_STATE;
  private listeners = new Set<() => void>();

  /** start 代际：迟到结果 / 旧 timer 一律作废 */
  private generation = 0;
  /** 当前页面所有者 token；0 = 无所有者 */
  private ownerToken = 0;
  private nextOwnerToken = 0;

  private inFlight: Promise<void> | null = null;
  private pollTimer: unknown = null;
  private staleTimer: unknown = null;
  /** stop 后若仍有 in-flight，完成后是否立刻为新所有者服务 */
  private pendingServe = false;

  constructor(
    fetchFn: () => Promise<LiveSnapshotReply>,
    clock: LiveClock,
  ) {
    this.fetchFn = fetchFn;
    this.clock = clock;
  }

  /** 供 useSyncExternalStore：引用仅真正变化时更新 */
  getSnapshot = (): LiveInputState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /**
   * 开始轮询；返回所有权 token。
   * 若旧 I/O 仍在飞：不发第二笔，等旧结果丢弃后立刻服务本所有者。
   */
  start = (): number => {
    const token = ++this.nextOwnerToken;
    this.ownerToken = token;
    this.generation += 1;
    const gen = this.generation;
    this.clearTimers();
    this.commit({
      state: "connecting",
      reply: null,
      receivedAtMs: null,
    });
    if (this.inFlight) {
      this.pendingServe = true;
    } else {
      this.pendingServe = false;
      this.kick(gen);
    }
    return token;
  };

  /**
   * 仅当 token 仍是当前所有者时停止；关闭后续 poll/stale 定时器并增 generation，
   * 迟到结果不生效。
   */
  stop = (ownerToken: number): void => {
    if (ownerToken !== this.ownerToken) return;
    this.ownerToken = 0;
    this.generation += 1;
    this.pendingServe = false;
    this.clearTimers();
    this.commit(OFF_STATE);
  };

  private notify(): void {
    for (const listener of this.listeners) listener();
  }

  private commit(next: LiveInputState): void {
    // 去重：同内容不换引用（useSyncExternalStore 稳定）
    const prev = this.state;
    if (
      prev.state === next.state &&
      prev.reply === next.reply &&
      prev.receivedAtMs === next.receivedAtMs
    ) {
      return;
    }
    this.state = next;
    this.notify();
  }

  private clearTimers(): void {
    if (this.pollTimer !== null) {
      this.clock.clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
    if (this.staleTimer !== null) {
      this.clock.clearTimeout(this.staleTimer);
      this.staleTimer = null;
    }
  }

  private schedule(gen: number, ms: number, kind: "poll" | "stale"): void {
    if (gen !== this.generation || this.ownerToken === 0) return;
    const handle = this.clock.setTimeout(() => {
      if (gen !== this.generation || this.ownerToken === 0) return;
      if (kind === "poll") {
        this.pollTimer = null;
        this.kick(gen);
      } else {
        this.staleTimer = null;
        this.onStale(gen);
      }
    }, ms);
    if (kind === "poll") this.pollTimer = handle;
    else this.staleTimer = handle;
  }

  private onStale(gen: number): void {
    if (gen !== this.generation || this.ownerToken === 0) return;
    const received = this.state.receivedAtMs;
    if (received === null) return;
    if (this.clock.nowMs() - received < STALE_AFTER_MS) return;
    this.commit({
      state: "unavailable",
      reply: null,
      receivedAtMs: null,
    });
    // 过期后继续按正常节奏尝试（若 in-flight 则等其结束）
    this.schedule(gen, POLL_INTERVAL_MS, "poll");
  }

  private kick(gen: number): void {
    if (gen !== this.generation || this.ownerToken === 0) return;
    if (this.inFlight) {
      this.pendingServe = true;
      return;
    }
    this.inFlight = this.runFetch(gen).finally(() => {
      this.inFlight = null;
      if (this.pendingServe && this.ownerToken !== 0) {
        const serveGen = this.generation;
        this.pendingServe = false;
        // 旧结果已丢弃；立刻为当前所有者服务（可能是新 start）
        this.kick(serveGen);
      }
    });
  }

  private async runFetch(gen: number): Promise<void> {
    let reply: LiveSnapshotReply | null = null;
    let threw = false;
    try {
      reply = await this.fetchFn();
    } catch {
      threw = true;
    }
    if (gen !== this.generation || this.ownerToken === 0) {
      // 迟到：丢弃；若有新所有者等待则由 finally 的 pendingServe 接手
      return;
    }
    if (threw || reply === null || !isSuccess(reply)) {
      this.clearTimers();
      this.commit({
        state: "unavailable",
        reply: reply !== null && !threw ? reply : null,
        receivedAtMs: null,
      });
      this.schedule(gen, RETRY_AFTER_MS, "poll");
      return;
    }
    const now = this.clock.nowMs();
    this.clearTimers();
    this.commit({
      state: "ready",
      reply,
      receivedAtMs: now,
    });
    this.schedule(gen, POLL_INTERVAL_MS, "poll");
    this.schedule(gen, STALE_AFTER_MS, "stale");
  }
}

/** 生产时钟：performance.now + window timers */
export const systemLiveClock: LiveClock = {
  nowMs: () =>
    typeof performance !== "undefined" && typeof performance.now === "function"
      ? performance.now()
      : Date.now(),
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (id) => clearTimeout(id as ReturnType<typeof setTimeout>),
};
