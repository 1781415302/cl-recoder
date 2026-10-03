// UI 活动桥（usability-runtime-v3 §4.4，S3）。
//
// Tauri 与 Browser mock 在此适配，组件不自行 listen：原生 Rust（src-tauri/src/ui_activity.rs）
// 是 UI 活动的唯一权威——经固定事件 `ui-activity` 推送快照，`get_ui_activity` 命令提供
// 内存缓存读取（先注册 listener 再取快照由 store 保证，见 lib/appActivity.ts）。
// 纯浏览器预览（无 Tauri runtime）用 document visibility 兜底 mock——仅预览用，
// 不连接真实 Tauri/DB。类型（AppActivity/ActivityBridge/ActivityClock）供
// lib/appActivity.ts 纯 store 以 type-import 复用（零运行时依赖，可在 Node 内建测试直载）。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 原生快照（与 Rust `UiActivitySnapshot` serde camelCase 线形状逐字一致） */
export interface ActivitySnapshot {
  /** 状态变化时单调递增的版本号；旧版本/同版本一律忽略，首个 revision=0 也接受 */
  revision: number;
  /** main 窗口可见且未最小化（失焦但可见保持 true） */
  active: boolean;
}

/** 前端派生状态：ready=初始化完成（成功或失败）；active=Tauri 权威；today=本地日；error=初始化失败原因 */
export interface AppActivity {
  ready: boolean;
  active: boolean;
  today: string;
  error: string | null;
}

/** 活动桥：store 先 subscribe 后 snapshot；unlisten 可能延迟 resolve，由 store 保证最终退订 */
export interface ActivityBridge {
  subscribe(handler: (s: ActivitySnapshot) => void): Promise<() => void>;
  snapshot(): Promise<ActivitySnapshot>;
}

/** 时钟接缝：纯 store 可注入假时钟，测试午夜 timer 与本地日重算 */
export interface ActivityClock {
  now(): Date;
  setTimeout(fn: () => void, milliseconds: number): unknown;
  clearTimeout(handle: unknown): void;
}

/** §4.4 合同事件名（与 Rust EVENT_UI_ACTIVITY 逐字一致） */
export const UI_ACTIVITY_EVENT = "ui-activity";

/** Tauri 桥：事件只来自原生发布（原生状态是唯一权威），命令只读内存缓存 */
function createTauriBridge(): ActivityBridge {
  return {
    async subscribe(handler) {
      return listen<ActivitySnapshot>(UI_ACTIVITY_EVENT, (event) => handler(event.payload));
    },
    async snapshot() {
      return invoke<ActivitySnapshot>("get_ui_activity");
    },
  };
}

/** Browser mock 桥（纯浏览器预览兜底）：document.visibility 作 active 依据，
 * revision 本地单调递增；不连接真实 Tauri/DB（§4.4）。
 * 例外：显式调试开关（VITE_CLRECODER_MOCK=1）下始终视为可见——嵌入式预览容器
 * （如自动化 QA 的内嵌 WebView）可能恒报 document.hidden=true，会把预览查询全部
 * gating 掉；该开关本就是预览/QA 专用，生产构建不受影响（真实可见性语义不变）。 */
function createBrowserMockBridge(): ActivityBridge {
  let revision = 0;
  const handlers = new Set<(s: ActivitySnapshot) => void>();
  const forceActive = import.meta.env.VITE_CLRECODER_MOCK === "1";
  const current = (): ActivitySnapshot => ({ revision, active: forceActive || !document.hidden });
  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", () => {
      revision += 1;
      const snapshot = current();
      for (const handler of [...handlers]) handler(snapshot);
    });
  }
  return {
    async subscribe(handler) {
      handlers.add(handler);
      return () => {
        handlers.delete(handler);
      };
    },
    async snapshot() {
      return current();
    },
  };
}

const USE_MOCK = import.meta.env.VITE_CLRECODER_MOCK === "1"; // 与 client.ts 同一调试开关
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** 生产入口：Tauri 环境走原生桥；VITE_CLRECODER_MOCK=1 或纯浏览器走 mock 桥 */
export function createActivityBridge(): ActivityBridge {
  if (USE_MOCK || !inTauri) return createBrowserMockBridge();
  return createTauriBridge();
}
