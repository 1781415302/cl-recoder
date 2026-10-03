import { StrictMode } from "react";
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

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      {/* S3（§4.4）：UI 活动权威——Provider 初始化/清理共享 store，共用既有 QueryClient */}
      <AppActivityProvider>
        <App />
      </AppActivityProvider>
    </QueryClientProvider>
  </StrictMode>,
);
