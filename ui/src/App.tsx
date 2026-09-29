// 应用外壳：侧边导航 + 页面切换（白名单无路由库，用状态 + hash 同步实现 8 页导航）。
import { useEffect, useState } from "react";
import { Sidebar, type PageId } from "./components/Sidebar";
import { Dashboard } from "./pages/Dashboard";
import { Keyboard } from "./pages/Keyboard";
import { Mouse } from "./pages/Mouse";
import { Gamepad } from "./pages/Gamepad";
import { Apps } from "./pages/Apps";
import { Combos } from "./pages/Combos";
import { WhatPulse } from "./pages/WhatPulse";
import { Settings } from "./pages/Settings";

const PAGES: Record<PageId, () => React.JSX.Element> = {
  dashboard: Dashboard,
  keyboard: Keyboard,
  mouse: Mouse,
  gamepad: Gamepad,
  apps: Apps,
  combos: Combos,
  whatpulse: WhatPulse,
  settings: Settings,
};

function pageFromHash(): PageId {
  const h = window.location.hash.replace(/^#/, "");
  return (h in PAGES ? h : "dashboard") as PageId;
}

export default function App() {
  const [page, setPage] = useState<PageId>(pageFromHash);

  useEffect(() => {
    const onHash = () => setPage(pageFromHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  function navigate(next: PageId) {
    setPage(next);
    // 同步 hash：EmptyState 的引导按钮（window.location.hash = "settings"）也能回到同一套路由
    if (window.location.hash !== `#${next}`) window.location.hash = next;
  }

  const Page = PAGES[page];

  return (
    <div style={{ display: "flex", height: "100vh", overflow: "hidden" }}>
      <Sidebar page={page} onNavigate={navigate} />
      <main style={{ flex: 1, overflowY: "auto", minWidth: 0 }} aria-live="off">
        <div className="page" key={page}>
          <Page />
        </div>
      </main>
    </div>
  );
}
