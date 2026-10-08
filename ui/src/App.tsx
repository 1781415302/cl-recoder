// 应用外壳：侧边导航 + 页面切换（白名单无路由库，用状态 + hash 同步实现 8 页导航）。
import { useEffect, useState } from "react";
import { IconLocal } from "./components/icons";
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
  return (
    Object.prototype.hasOwnProperty.call(PAGES, h) ? h : "dashboard"
  ) as PageId;
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
    if (window.location.hash !== `#${next}`) window.location.hash = next;
  }
  const Page = PAGES[page];
  const labels: Record<PageId, string> = {
    dashboard: "仪表盘",
    keyboard: "键盘",
    mouse: "鼠标",
    gamepad: "手柄",
    apps: "应用",
    combos: "组合键",
    whatpulse: "WhatPulse",
    settings: "设置",
  };
  return (
    <div className="app-shell">
      <Sidebar page={page} onNavigate={navigate} />
      <div className="workspace">
        <header className="app-bar">
          <div className="app-breadcrumb">
            <span>工作台</span>
            <span aria-hidden="true">/</span>
            <strong>{labels[page]}</strong>
          </div>
          <div className="app-local">
            <IconLocal size={15} />
            本地记录
          </div>
        </header>
        <main key={page} className="app-main" aria-live="off">
          <div className="page">
            <Page />
          </div>
        </main>
      </div>
    </div>
  );
}
