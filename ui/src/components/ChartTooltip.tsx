// Recharts 自定义 tooltip。
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

export function ChartTooltip({ active, payload, label, unit }: ChartTooltipProps) {
  if (!active || !payload || payload.length === 0) return null;
  return (
    <div className="chart-tip">
      <div className="chart-tip-label">{label}</div>
      {payload.map((p, i) => (
        <div key={i} className="chart-tip-row">
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
