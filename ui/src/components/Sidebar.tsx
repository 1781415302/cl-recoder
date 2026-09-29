// 桌面侧边导航（§4.9：图标 + 文字、当前项高亮、宽 240px）+ 采集器运行状态（icon+文字，不只靠颜色）。
import { useCollectorStatus } from "../api/queries";
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
  | "dashboard" | "keyboard" | "mouse" | "gamepad"
  | "apps" | "combos" | "whatpulse" | "settings";

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

function CollectorChip() {
  const { data } = useCollectorStatus();
  let cls = "chip";
  let text = "未运行";
  if (data?.running) {
    text = data.paused ? "已暂停" : "采集中";
    cls = data.paused ? "chip chip-accent" : "chip chip-positive";
  }
  return (
    <div className={cls} title="采集器状态（近实时刷新）">
      <span
        aria-hidden="true"
        style={{
          width: 8,
          height: 8,
          borderRadius: "50%",
          background: data?.running
            ? data.paused ? "var(--color-accent)" : "var(--color-positive)"
            : "var(--color-text-placeholder)",
        }}
      />
      {text}
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
        <span
          aria-hidden="true"
          style={{
            width: 28, height: 28, borderRadius: "var(--radius-sm)",
            background: "var(--color-primary)", color: "var(--color-card)",
            display: "inline-flex", alignItems: "center", justifyContent: "center",
            fontWeight: 700, fontSize: "var(--text-caption)",
          }}
        >
          CL
        </span>
        <div>
          <div className="sidebar-brand-name">CL Recoder</div>
          <div style={{ fontSize: "var(--text-caption)", color: "var(--color-text-muted)" }}>外设输入统计</div>
        </div>
      </div>
      <nav className="sidebar-nav" aria-label="主导航">
        {NAV.map((item) => (
          <button
            key={item.id}
            type="button"
            className="nav-item"
            aria-current={page === item.id ? "page" : undefined}
            onClick={() => onNavigate(item.id)}
          >
            {item.icon}
            <span>{item.label}</span>
          </button>
        ))}
      </nav>
      <div className="sidebar-footer">
        <CollectorChip />
        <div style={{ fontSize: "var(--text-caption)", color: "var(--color-text-placeholder)" }}>
          本地优先 · 数据不出本机
        </div>
      </div>
    </aside>
  );
}
