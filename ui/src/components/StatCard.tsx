import type { ReactNode } from "react";

export type StatTone = "default" | "kb" | "ms" | "gp";

interface StatCardProps {
  label: string;
  value: string;
  sub?: string;
  icon?: ReactNode;
  tone?: StatTone;
}

/** KPI 卡：色条 + 大数字；tone 对应键盘/鼠标/手柄的识别色 */
export function StatCard({ label, value, sub, icon, tone = "default" }: StatCardProps) {
  return (
    <section className="card kpi" data-tone={tone} aria-label={label}>
      <div className="kpi-head">
        <span className="kpi-label">{label}</span>
        {icon ? <span className="kpi-icon">{icon}</span> : null}
      </div>
      <div className="kpi-value num">{value}</div>
      {sub ? <div className="kpi-sub">{sub}</div> : null}
    </section>
  );
}
