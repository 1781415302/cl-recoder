import type { ReactNode } from "react";
interface StatCardProps {
  label: string;
  value: string;
  sub?: string;
  icon?: ReactNode;
  tone?: "default" | "primary" | "accent";
}
export function StatCard({
  label,
  value,
  sub,
  icon,
  tone = "default",
}: StatCardProps) {
  return (
    <section className={`card metric-card metric-${tone}`} aria-label={label}>
      <div className="metric-heading">
        <span>{label}</span>
        {icon && <span className="metric-icon">{icon}</span>}
      </div>
      <div className="metric-value">{value}</div>
      {sub && <div className="metric-sub">{sub}</div>}
    </section>
  );
}
