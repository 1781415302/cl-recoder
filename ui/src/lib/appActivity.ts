// AppActivityStore —— UI 活动纯 store（usability-runtime-v3 §4.4，S3）。
//
// 订阅原生活动快照（先注册 listener 再取快照）、拒绝旧版本/旧 start 代际、统一"活动 +
// 本地日期"（today），供 useSyncExternalStore 消费（getSnapshot 引用仅真正变化时更新）。
// 活动期只安排"下一个当地午夜"的一次 timer（当地日历构造，非固定 24h）；隐藏取消，
// 恢复/跨午夜重算。DOM focus/pageshow/visibilitychange 只经 refreshDate 校准日期，
// 不作为 Tauri active 权威。
//
// 纯模块约束（与 api/wpMouseQuery.ts 同策略）：对 api/activity 只 type-import，
// 无 React/Tauri 运行时依赖——可在 Node 内建测试中经 typescript.transpileModule 转为
// ESM 后由 data URL 直载；本地日格式化因此内联（与 lib/format.ts toDay 同规则）。
import type { ActivityBridge, ActivityClock, ActivitySnapshot, AppActivity } from "../api/activity";

/** 本地日 → "YYYY-MM-DD"（与 lib/format.ts toDay 同规则） */
function toLocalDay(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

export class AppActivityStore {
  private readonly bridge: ActivityBridge;
  private readonly clock: ActivityClock;
  private readonly onWillActivate?: () => void;

  private state: AppActivity;
  private listeners = new Set<() => void>();
  /** start 代际：stop 后旧代返回（迟到的 subscribe/snapshot）不得重新激活 */
  private generation = 0;
  private subscriptionGeneration = 0;
  private pendingStart: Promise<void> | null = null;
  /** 已 resolve 的退订（非 null = listener 在手；重试只补快照不加第二监听） */
  private unlisten: (() => void) | null = null;
  /** 上一轮 start 是否完整成功（snapshot 取到）——"健康 started 再 start 幂等"的判据 */
  private snapshotOk = false;
  /** 已接受的最新版本；null = 尚未接受任何版本（首个 revision=0/inactive 也必须接受） */
  private lastRevision: number | null = null;
  /** 活动期唯一的下一午夜 timer 句柄（null = 无 pending timer） */
  private timer: unknown = null;

  constructor(bridge: ActivityBridge, clock: ActivityClock, onWillActivate?: () => void) {
    this.bridge = bridge;
    this.clock = clock;
    this.onWillActivate = onWillActivate;
    // 初始 ready=false/active=false：阻止隐藏启动的首轮查询（§4.4）
    this.state = { ready: false, active: false, today: toLocalDay(clock.now()), error: null };
  }

  /** 供 useSyncExternalStore：引用仅真正变化时更新（commit 去重） */
  getSnapshot = (): AppActivity => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /** 并发 start 返回同一 pending；健康 started 再 start 幂等；失败重试只补快照不加第二监听 */
  start = (): Promise<void> => {
    if (this.snapshotOk && this.unlisten) return Promise.resolve();
    if (this.pendingStart) return this.pendingStart;
    const gen = ++this.generation;
    const pending = this.doStart(gen).finally(() => {
      if (gen === this.generation) this.pendingStart = null;
    });
    this.pendingStart = pending;
    return pending;
  };

  /** 清理：取消 pending start、退订（含延迟 resolve 的补退订）、午夜 timer，状态复位；
   * 此后旧代 start 的返回一律作废（代际检查），不得重新激活。 */
  stop = (): void => {
    this.generation += 1;
    this.subscriptionGeneration += 1;
    this.pendingStart = null;
    this.snapshotOk = false;
    const unlisten = this.unlisten;
    this.unlisten = null;
    unlisten?.();
    this.clearTimer();
    this.lastRevision = null;
    this.commit({ ready: false, active: false, today: toLocalDay(this.clock.now()), error: null });
  };

  /** 仅校准本地日期（DOM focus/pageshow/visibilitychange / 午夜 timer 共用）：
   * 不改 active。活动期日期未变时确保下一午夜 timer 仍在（休眠唤醒兜底），
   * 日期变化则同一次发布更新 today 并重排到下一个当地午夜。 */
  refreshDate = (): void => {
    const today = toLocalDay(this.clock.now());
    if (today === this.state.today) {
      if (this.state.active) this.scheduleMidnightTimer();
      return;
    }
    this.commit({ ...this.state, today });
    if (this.state.active) this.scheduleMidnightTimer();
  };

  private async doStart(gen: number): Promise<void> {
    // —— 先注册 listener（§4.4 顺序）；已有 listener（上次 start 失败于 snapshot）不重复注册，
    //    listener 注册失败是唯一重做订阅的情形。
    if (!this.unlisten) {
      const subscription = ++this.subscriptionGeneration;
      try {
        const unlisten = await this.bridge.subscribe((s) => {
          // 注册完成前也可能收到事件；订阅代际独立于快照重试代际。
          if (subscription === this.subscriptionGeneration) this.accept(s);
        });
        if (gen !== this.generation) {
          unlisten(); // stop 已发生：立即退订，绝不保留悬挂监听
          return;
        }
        this.unlisten = unlisten;
      } catch (e) {
        if (gen !== this.generation) return;
        this.subscriptionGeneration += 1;
        this.setInitError(e);
        return;
      }
    }
    // —— 再取快照（读内存缓存，不与事件竞争）。
    try {
      const snapshot = await this.bridge.snapshot();
      if (gen !== this.generation) return; // stop 后旧代返回不得激活
      this.snapshotOk = true;
      this.accept(snapshot);
    } catch (e) {
      if (gen !== this.generation) return;
      if (this.lastRevision === null) {
        // 尚无有效事件：ready=true/active=false/error（重试入口可见；不伪造 Tauri 可见）
        this.setInitError(e);
      }
      // 已接受有效事件：事件是权威，保留该状态、不置 error；listener 保持可用
    }
  }

  /** 版本 gate：旧版本/同版本忽略（不回退状态）；接受时同一次发布先重算 today 再置 active，
   * 进入 active 先 onWillActivate（标 uiOwned 缓存 stale）后发布。 */
  private accept(s: ActivitySnapshot): void {
    if (this.lastRevision !== null && s.revision <= this.lastRevision) return;
    this.lastRevision = s.revision;
    const next: AppActivity = {
      ready: true,
      active: s.active,
      today: toLocalDay(this.clock.now()),
      error: null,
    };
    if (!this.state.active && s.active) this.onWillActivate?.();
    this.commit(next);
    if (s.active) this.scheduleMidnightTimer();
    else this.clearTimer();
  }

  /** 初始化失败（listener 注册失败 / 尚无有效事件时 snapshot 失败）：
   * ready=true/active=false/error——查询保持 disabled，Provider 提供不依赖数据 query 的重试。 */
  private setInitError(e: unknown): void {
    const message = e instanceof Error ? e.message : String(e);
    this.commit({ ready: true, active: false, today: this.state.today, error: message });
  }

  /** 引用仅真正变化时更新 + 通知（useSyncExternalStore 合同） */
  private commit(next: AppActivity): void {
    const prev = this.state;
    if (
      prev.ready === next.ready &&
      prev.active === next.active &&
      prev.today === next.today &&
      prev.error === next.error
    ) {
      return;
    }
    this.state = next;
    for (const listener of [...this.listeners]) listener();
  }

  /** 下一个当地午夜的一次 timer（当地日历构造，非固定 24h；同一时刻只有一个 pending） */
  private scheduleMidnightTimer(): void {
    this.clearTimer();
    const now = this.clock.now();
    const next = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 0, 0, 0, 0);
    this.timer = this.clock.setTimeout(
      () => {
        this.timer = null;
        this.refreshDate();
      },
      Math.max(0, next.getTime() - now.getTime()),
    );
  }

  private clearTimer(): void {
    if (this.timer !== null) {
      this.clock.clearTimeout(this.timer);
      this.timer = null;
    }
  }
}
