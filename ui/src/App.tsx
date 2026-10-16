// 应用外壳：左侧导航轨 + 主区（每个页面自带吸顶页眉）。白名单无路由库，用状态 + hash 同步实现 8 页导航。
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
import { useAppNotices } from "./api/queries";
import { formatUpdateBanner } from "./lib/updateBanner";

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
  const notices = useAppNotices();
  const tooNew = notices.data?.schemaTooNew ?? null;
  const updateApplied = notices.data?.updateApplied ?? null;
  // 关闭仅隐藏本次会话（§4.7）
  const [updateBannerDismissed, setUpdateBannerDismissed] = useState(false);
  const showUpdateBanner = Boolean(updateApplied) && !updateBannerDismissed;
  const updateBanner = updateApplied ? formatUpdateBanner(updateApplied) : null;
  return (
    <div className="shell">
      <Sidebar page={page} onNavigate={navigate} />
      <div className="stage">
        {tooNew && (
          <div className="app-banner app-banner--danger" role="alert">
            数据库由更新版本创建（v{tooNew.found}），当前程序支持 v
            {tooNew.supported}，请升级程序；采集器已拒绝写入
          </div>
        )}
        {showUpdateBanner && updateBanner && (
          <div className="app-banner app-banner--warning" role="status">
            <span>{updateBanner.message}</span>
            {updateBanner.showGoSettings ? (
              <button
                type="button"
                className="btn btn-sm"
                style={{ marginLeft: 12 }}
                onClick={() => navigate("settings")}
              >
                前往设置
              </button>
            ) : null}
            <button
              type="button"
              className="btn btn-sm"
              style={{ marginLeft: 8 }}
              onClick={() => setUpdateBannerDismissed(true)}
            >
              关闭
            </button>
          </div>
        )}
        <main key={page} className="main" aria-live="off">
          <Page />
        </main>
      </div>
    </div>
  );
}
