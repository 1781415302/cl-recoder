import { StrictMode, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AppActivityProvider } from "./lib/AppActivityProvider";
import App from "./App";
import "./theme.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      staleTime: 5_000,
      // 今日数据近实时轮询在 api/queries.ts（POLL_TODAY_MS）按查询设置
    },
  },
});

function FrontendReady() {
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    const epoch = (window as Window & { __CL_RECODER_VIEW_EPOCH__?: number }).__CL_RECODER_VIEW_EPOCH__;
    // 报告 React 提交成功，不依赖绘制帧；隐藏/被遮挡时 rAF 可能暂停。
    if (epoch !== undefined) void invoke("frontend_ready", { epoch }).catch(console.error);
  }, []);
  return null;
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <FrontendReady />
      {/* S3（§4.4）：UI 活动权威——Provider 初始化/清理共享 store，共用既有 QueryClient */}
      <AppActivityProvider>
        <App />
      </AppActivityProvider>
    </QueryClientProvider>
  </StrictMode>,
);
