// 空状态（§4.9：空状态给引导文案 + 动作按钮）。
import type { ReactNode } from "react";
import { IconDatabase } from "./icons";

interface EmptyStateProps {
  title: string;
  description?: string;
  icon?: ReactNode;
  action?: { label: string; onClick: () => void };
  secondary?: { label: string; onClick: () => void };
  hint?: string;
}

export function EmptyState({ title, description, icon, action, secondary, hint }: EmptyStateProps) {
  return (
    <div
      className="card"
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        textAlign: "center",
        gap: "var(--space-2)",
        padding: "var(--space-6) var(--space-5)",
      }}
      role="status"
    >
      <div style={{ color: "var(--color-text-placeholder)" }}>{icon ?? <IconDatabase size={36} />}</div>
      <div style={{ fontSize: "var(--text-body-lg)", fontWeight: 600 }}>{title}</div>
      {description ? (
        <p className="page-desc" style={{ maxWidth: "460px" }}>{description}</p>
      ) : null}
      <div style={{ display: "flex", gap: "var(--space-2)", marginTop: "var(--space-2)" }}>
        {action ? (
          <button type="button" className="btn btn-primary" onClick={action.onClick}>
            {action.label}
          </button>
        ) : null}
        {secondary ? (
          <button type="button" className="btn" onClick={secondary.onClick}>
            {secondary.label}
          </button>
        ) : null}
      </div>
      {hint ? <p className="chart-hint">{hint}</p> : null}
    </div>
  );
}
