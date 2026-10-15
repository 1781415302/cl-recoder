// 左侧窄导航轨：图标在上、文字在下；底部是采集器状态与主题切换。
// 健康（usability-runtime-v3 §4.2）：按 health 分类显示，不以 running=false 一律显示"未运行"——
// unreachable/access_denied/unknown 各有文案与配色，无凭据不臆断。
import { useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import * as client from "../api/client";
import { uiQueryPolicy } from "../api/queryPolicy";
import type { CollectorHealth } from "../api/types";
import { useAppActivity } from "../lib/AppActivityProvider";
import {
  IconApps,
  IconCombos,
  IconDashboard,
  IconGamepad,
  IconKeyboard,
  IconMouse,
  IconPulse,
  IconSettings,
} from "./icons";

export type PageId =
  | "dashboard"
  | "keyboard"
  | "mouse"
  | "gamepad"
  | "apps"
  | "combos"
  | "whatpulse"
  | "settings";

interface NavItem {
  id: PageId;
  label: string;
  icon: ReactNode;
}

const NAV_MAIN: NavItem[] = [
  { id: "dashboard", label: "总览", icon: <IconDashboard size={22} /> },
  { id: "keyboard", label: "键盘", icon: <IconKeyboard size={22} /> },
  { id: "mouse", label: "鼠标", icon: <IconMouse size={22} /> },
  { id: "gamepad", label: "手柄", icon: <IconGamepad size={22} /> },
  { id: "apps", label: "应用", icon: <IconApps size={22} /> },
  { id: "combos", label: "组合键", icon: <IconCombos size={22} /> },
];

const NAV_MANAGE: NavItem[] = [
  { id: "whatpulse", label: "WhatPulse", icon: <IconPulse size={22} /> },
  { id: "settings", label: "设置", icon: <IconSettings size={22} /> },
];

/** §4.2 健康分类 → 状态点文案/语义色。unknown/access_denied 不显示"未运行"（无凭据不臆断）。 */
const HEALTH: Record<CollectorHealth, { text: string; tone: string }> = {
  running: { text: "采集中", tone: "ok" },
  paused: { text: "已暂停", tone: "warn" },
  not_running: { text: "未运行", tone: "idle" },
  unreachable: { text: "无响应", tone: "bad" },
  access_denied: { text: "访问受限", tone: "bad" },
  unknown: { text: "状态未知", tone: "idle" },
};
const HEALTH_PENDING = { text: "检测中", tone: "idle" };

function CollectorBadge() {
  const activity = useAppActivity();
  // §4.5 activity gating：仅活动时 500ms 轮询。Sidebar 常驻，是 ["collectorStatus"] 的
  // 唯一轮询者——设置页以同一 queryKey 消费该缓存，不另起定时器（避免双倍管道探测）。
  const { data } = useQuery({
    queryKey: ["collectorStatus"],
    queryFn: () => client.collectorStatus(),
    ...uiQueryPolicy(activity.active, 500),
  });
  const h = data ? HEALTH[data.health] : HEALTH_PENDING;
  return (
    <div
      className="rail-status"
      data-tone={h.tone}
      title={data?.diagnosticMessage ?? "采集器状态（近实时刷新）"}
    >
      <span className="rail-status-dot" aria-hidden="true" />
      <span>{h.text}</span>
    </div>
  );
}

interface SidebarProps {
  page: PageId;
  onNavigate: (page: PageId) => void;
}

function NavButton({
  item,
  page,
  onNavigate,
}: { item: NavItem } & SidebarProps) {
  return (
    <button
      type="button"
      className="rail-item"
      aria-current={page === item.id ? "page" : undefined}
      onClick={() => onNavigate(item.id)}
    >
      {item.icon}
      <span>{item.label}</span>
    </button>
  );
}

export function Sidebar({ page, onNavigate }: SidebarProps) {
  return (
    <aside className="rail">
      <div className="rail-brand" title="CL Recoder · 输入与活动记录">
        <svg
          width="26"
          height="26"
          viewBox="0 0 28 28"
          fill="none"
          aria-hidden="true"
        >
          <rect
            x="1.5"
            y="1.5"
            width="25"
            height="25"
            rx="8"
            className="rail-brand-bg"
          />
          <path
            d="M7 17V11l4 3v5M14 8v9l4 3V11M21 7v10"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </div>
      <nav className="rail-nav" aria-label="主导航">
        {NAV_MAIN.map((item) => (
          <NavButton
            key={item.id}
            item={item}
            page={page}
            onNavigate={onNavigate}
          />
        ))}
        <div className="rail-sep" role="separator" />
        {NAV_MANAGE.map((item) => (
          <NavButton
            key={item.id}
            item={item}
            page={page}
            onNavigate={onNavigate}
          />
        ))}
      </nav>
      <div className="rail-foot">
        <CollectorBadge />
      </div>
    </aside>
  );
}
