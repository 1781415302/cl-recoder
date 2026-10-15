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
    <section className="card empty" role="status">
      <div className="empty-icon" aria-hidden="true">
        {icon ?? <IconDatabase size={26} />}
      </div>
      <h2 className="empty-title">{title}</h2>
      {description ? <p className="empty-desc">{description}</p> : null}
      {action || secondary ? (
        <div className="empty-actions">
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
      ) : null}
      {hint ? <p className="hint">{hint}</p> : null}
    </section>
  );
}
