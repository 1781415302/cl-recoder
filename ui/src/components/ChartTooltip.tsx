// Recharts 自定义 tooltip（令牌样式：白卡 + token 边框 + elevation-2）。
import { fmtNum } from "../lib/format";

export interface TooltipEntry {
  value?: number | string;
  name?: string | number;
  color?: string;
}

interface ChartTooltipProps {
  active?: boolean;
  label?: string | number;
  payload?: TooltipEntry[];
  unit?: string;
}

export function ChartTooltip({
  active,
  payload,
  label,
  unit,
}: ChartTooltipProps) {
  if (!active || !payload || payload.length === 0) return null;
  return (
    <div className="chart-tooltip">
      <div style={{ fontWeight: 600, marginBottom: 2 }}>{label}</div>
      {payload.map((p, i) => (
        <div
          key={i}
          style={{
            display: "flex",
            gap: "var(--space-2)",
            justifyContent: "space-between",
          }}
        >
          <span>{p.name}</span>
          <span className="num">
            {fmtNum(Number(p.value ?? 0))}
            {unit ? ` ${unit}` : ""}
          </span>
        </div>
      ))}
    </div>
  );
}
