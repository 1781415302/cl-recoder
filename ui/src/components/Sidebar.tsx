// 桌面侧边导航（§4.9：图标 + 文字、当前项高亮、宽 240px）+ 采集器健康状态。
// 健康（usability-runtime-v3 §4.2）：按 health 分类显示，不以 running=false 一律显示"未运行"——
// unreachable/access_denied/unknown 各有文案与配色，无凭据不臆断。
import { useQuery } from "@tanstack/react-query";
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
import type { ReactNode } from "react";

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

const NAV: NavItem[] = [
  { id: "dashboard", label: "仪表盘", icon: <IconDashboard /> },
  { id: "keyboard", label: "键盘", icon: <IconKeyboard /> },
  { id: "mouse", label: "鼠标", icon: <IconMouse /> },
  { id: "gamepad", label: "手柄", icon: <IconGamepad /> },
  { id: "apps", label: "应用", icon: <IconApps /> },
  { id: "combos", label: "组合键", icon: <IconCombos /> },
  { id: "whatpulse", label: "WhatPulse", icon: <IconPulse /> },
  { id: "settings", label: "设置", icon: <IconSettings /> },
];

/** §4.2 健康分类 → 徽标文案/配色。unknown/access_denied 不显示"未运行"（无凭据不臆断）。 */
const HEALTH_CHIP: Record<
  CollectorHealth,
  { text: string; cls: string; dot: string }
> = {
  running: {
    text: "采集中",
    cls: "chip chip-positive",
    dot: "var(--color-positive)",
  },
  paused: {
    text: "已暂停",
    cls: "chip chip-accent",
    dot: "var(--color-accent)",
  },
  not_running: {
    text: "未运行",
    cls: "chip",
    dot: "var(--color-text-placeholder)",
  },
  unreachable: {
    text: "无响应",
    cls: "chip chip-danger",
    dot: "var(--color-danger)",
  },
  access_denied: {
    text: "访问受限",
    cls: "chip chip-danger",
    dot: "var(--color-danger)",
  },
  unknown: {
    text: "状态未知",
    cls: "chip",
    dot: "var(--color-text-placeholder)",
  },
};
const CHIP_PENDING = {
  text: "检测中…",
  cls: "chip",
  dot: "var(--color-text-placeholder)",
};

function CollectorChip() {
  const activity = useAppActivity();
  // §4.5 activity gating：仅活动时 500ms 轮询。Sidebar 常驻，是 ["collectorStatus"] 的
  // 唯一轮询者——设置页以同一 queryKey 消费该缓存，不另起定时器（避免双倍管道探测）。
  const { data } = useQuery({
    queryKey: ["collectorStatus"],
    queryFn: () => client.collectorStatus(),
    ...uiQueryPolicy(activity.active, 500),
  });
  const chip = data ? HEALTH_CHIP[data.health] : CHIP_PENDING;
  return (
    <div
      className={chip.cls}
      title={data?.diagnosticMessage ?? "采集器状态（近实时刷新）"}
    >
      <span
        aria-hidden="true"
        style={{
          width: 8,
          height: 8,
          borderRadius: "50%",
          background: chip.dot,
        }}
      />
      {chip.text}
    </div>
  );
}

interface SidebarProps {
  page: PageId;
  onNavigate: (page: PageId) => void;
}

export function Sidebar({ page, onNavigate }: SidebarProps) {
  return (
    <aside className="sidebar">
      <div className="sidebar-brand">
        <span className="sidebar-mark" aria-hidden="true">
          <svg width="28" height="32" viewBox="0 0 28 32" fill="none">
            <path
              d="M3 22V10l6 4v12M14 6v16l6 4V10M25 3v19"
              stroke="currentColor"
              strokeWidth="2.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </span>
        <div>
          <div className="sidebar-brand-name">CL Recoder</div>
          <div className="sidebar-brand-sub">输入与活动记录</div>
        </div>
      </div>
      <nav className="sidebar-nav" aria-label="主导航">
        {[
          { label: "统计", items: NAV.slice(0, 6) },
          { label: "管理", items: NAV.slice(6) },
        ].map((group) => (
          <section className="nav-section" key={group.label}>
            <div className="nav-group-label">{group.label}</div>
            {group.items.map((item) => (
              <button
                type="button"
                key={item.id}
                className="nav-item"
                aria-current={page === item.id ? "page" : undefined}
                onClick={() => onNavigate(item.id)}
              >
                {item.icon}
                <span>{item.label}</span>
              </button>
            ))}
          </section>
        ))}
      </nav>
      <div className="sidebar-footer">
        <div className="sidebar-status-label">采集器状态</div>
        <CollectorChip />
        <div className="sidebar-note">数据保存在本机</div>
      </div>
    </aside>
  );
}
