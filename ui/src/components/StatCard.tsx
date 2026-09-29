// KPI 指标卡。
import type { ReactNode } from "react";

interface StatCardProps {
  label: string;
  /** 已格式化的展示值（调用方用 fmtNum/fmtCompact/fmtDuration） */
  value: string;
  sub?: string;
  icon?: ReactNode;
  tone?: "default" | "primary" | "accent";
}

export function StatCard({ label, value, sub, icon, tone = "default" }: StatCardProps) {
  const color =
    tone === "primary" ? "var(--color-primary)"
    : tone === "accent" ? "var(--color-accent)"
    : "var(--color-text)";
  return (
    <div className="card" style={{ display: "flex", flexDirection: "column", gap: "var(--space-1)" }}>
      <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)", color: "var(--color-text-muted)", fontSize: "var(--text-caption)" }}>
        {icon ? <span style={{ display: "inline-flex" }}>{icon}</span> : null}
        <span>{label}</span>
      </div>
      <div className="num" style={{ fontSize: "var(--text-kpi)", fontWeight: 600, lineHeight: 1.1, color }}>{value}</div>
      {sub ? <div style={{ fontSize: "var(--text-caption)", color: "var(--color-text-muted)" }}>{sub}</div> : null}
    </div>
  );
}
