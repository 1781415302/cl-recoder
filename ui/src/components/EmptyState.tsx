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
export function EmptyState({
  title,
  description,
  icon,
  action,
  secondary,
  hint,
}: EmptyStateProps) {
  return (
    <section className="card empty-state" role="status">
      <div className="empty-illustration" aria-hidden="true">
        {icon ?? <IconDatabase size={28} />}
      </div>
      <h2 className="empty-title">{title}</h2>
      {description && <p className="empty-description">{description}</p>}
      {(action || secondary) && (
        <div className="empty-actions">
          {action && (
            <button
              type="button"
              className="btn btn-primary"
              onClick={action.onClick}
            >
              {action.label}
            </button>
          )}
          {secondary && (
            <button type="button" className="btn" onClick={secondary.onClick}>
              {secondary.label}
            </button>
          )}
        </div>
      )}
      {hint && <p className="chart-hint">{hint}</p>}
    </section>
  );
}
