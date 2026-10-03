// AppActivityProvider —— 生产 AppActivityStore 的初始化/清理 + 错误重试入口（§4.4，S3）。
//
// 共用既有 QueryClient：进入 active 时先经 onWillActivate 把所有 meta.uiOwned=true 缓存
// 标 stale（refetchType:none），后由 store 发布 active=true，查询以新范围恢复。
// DOM focus/pageshow/visibilitychange 仅校准本地日期（refreshDate），不作为 Tauri active 权威。
// 初始化失败时本组件给出错误 + 重试（不依赖任何数据 query，inactive 时也能调用 store.start；
// 不得把重试入口放进 enabled=false 的查询结果后面），不伪造 Tauri 可见。
import {
  createContext,
  useContext,
  useEffect,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { createActivityBridge, type ActivityClock, type AppActivity } from "../api/activity";
import { AppActivityStore } from "./appActivity";

/** 生产时钟：本地 Date + 全局 timer */
const systemClock: ActivityClock = {
  now: () => new Date(),
  setTimeout: (fn, ms) => window.setTimeout(fn, ms),
  clearTimeout: (handle) => window.clearTimeout(handle as number),
};

/** §4.4：把所有 meta.uiOwned=true 的缓存标 stale（refetchType:none——不立即发请求，
 * 由随后的 active 发布使各查询以新范围恢复）。 */
function markUiOwnedStale(client: QueryClient): void {
  void client.invalidateQueries({
    predicate: (query) => query.meta?.uiOwned === true,
    refetchType: "none",
  });
}

const StoreContext = createContext<AppActivityStore | null>(null);

export function AppActivityProvider(props: { children: ReactNode }): React.JSX.Element {
  const client = useQueryClient();
  // store 实例与挂载生命周期一致：mount → start，unmount → stop（StrictMode 双挂载安全）
  const [store] = useState(
    () => new AppActivityStore(createActivityBridge(), systemClock, () => markUiOwnedStale(client)),
  );
  const activity = useSyncExternalStore(store.subscribe, store.getSnapshot);

  useEffect(() => {
    void store.start();
    return () => store.stop();
  }, [store]);

  useEffect(() => {
    // 仅校准日期；active 权威只在原生活动事件（§4.4）
    const calibrate = () => store.refreshDate();
    window.addEventListener("focus", calibrate);
    window.addEventListener("pageshow", calibrate);
    document.addEventListener("visibilitychange", calibrate);
    return () => {
      window.removeEventListener("focus", calibrate);
      window.removeEventListener("pageshow", calibrate);
      document.removeEventListener("visibilitychange", calibrate);
    };
  }, [store]);

  if (activity.error !== null) {
    // 初始化失败：错误可见 + 重试；此 UI 不依赖任何数据 query（全部查询此时均被 gating）
    return (
      <div
        role="alert"
        style={{
          height: "100vh",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          gap: 12,
          padding: 24,
        }}
      >
        <p style={{ margin: 0, fontSize: 14 }}>UI 活动状态初始化失败：{activity.error}</p>
        <button type="button" style={{ padding: "8px 24px" }} onClick={() => void store.start()}>
          重试
        </button>
      </div>
    );
  }

  return <StoreContext.Provider value={store}>{props.children}</StoreContext.Provider>;
}

/** 页面消费入口：与 Provider 共享同一 store（组件不自行 listen，§4.4）。 */
export function useAppActivity(): AppActivity {
  const store = useContext(StoreContext);
  if (!store) throw new Error("useAppActivity 必须在 AppActivityProvider 内使用");
  return useSyncExternalStore(store.subscribe, store.getSnapshot);
}
