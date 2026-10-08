import { useId, useState } from "react";
import {
  Area,
  AreaChart,
  CartesianGrid,
  ReferenceDot,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { fmtCompact, fmtDay, fmtDayShort, fmtNum } from "../lib/format";
import { ChartTooltip } from "./ChartTooltip";
export interface TrendPoint {
  day: string;
  total: number;
}
interface TrendChartProps {
  data: TrendPoint[];
  title: string;
  colorVar?: string;
  seriesName?: string;
  unit?: string;
  height?: number;
}
export function TrendChart({
  data,
  title,
  colorVar = "--chart-1",
  seriesName = "输入次数",
  unit = "次",
  height = 230,
}: TrendChartProps) {
  const id = useId().replace(/:/g, ""),
    [visible, setVisible] = useState(true),
    [index, setIndex] = useState<number | null>(null);
  const latest = data[data.length - 1],
    selected = index === null ? undefined : data[index];
  return (
    <section className="card trend-card">
      <div className="section-heading">
        <div>
          <h2 className="card-title">{title}</h2>
          {latest && (
            <p className="card-sub">最近记录 · {fmtDay(latest.day)}</p>
          )}
        </div>
        <button
          type="button"
          className="legend-chip"
          aria-pressed={visible}
          onClick={() => setVisible((value) => !value)}
          title="显示或隐藏趋势"
        >
          <span
            className="legend-dot"
            style={{ background: `var(${colorVar})` }}
          />
          {seriesName}
          {latest && (
            <strong className="num">
              {fmtNum(latest.total)} {unit}
            </strong>
          )}
        </button>
      </div>
      {visible && data.length > 0 ? (
        <div
          tabIndex={0}
          role="group"
          aria-label={`${title}，共 ${data.length} 天；用左右方向键查看每天的数值`}
          onFocus={() => setIndex(data.length - 1)}
          onBlur={() => setIndex(null)}
          onKeyDown={(e) => {
            if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key))
              return;
            e.preventDefault();
            setIndex((current) =>
              e.key === "Home"
                ? 0
                : e.key === "End"
                  ? data.length - 1
                  : Math.max(
                      0,
                      Math.min(
                        data.length - 1,
                        (current ?? data.length - 1) +
                          (e.key === "ArrowLeft" ? -1 : 1),
                      ),
                    ),
            );
          }}
        >
          <ResponsiveContainer width="100%" height={height}>
            <AreaChart
              data={data}
              margin={{ top: 12, right: 22, bottom: 4, left: 0 }}
            >
              <defs>
                <linearGradient id={id} x1="0" y1="0" x2="0" y2="1">
                  <stop
                    offset="0%"
                    stopColor={`var(${colorVar})`}
                    stopOpacity={0.15}
                  />
                  <stop
                    offset="100%"
                    stopColor={`var(${colorVar})`}
                    stopOpacity={0.015}
                  />
                </linearGradient>
              </defs>
              <CartesianGrid
                stroke="var(--color-divider)"
                vertical={false}
                strokeDasharray="2 6"
              />
              <XAxis
                dataKey="day"
                tickFormatter={fmtDayShort}
                tickLine={false}
                axisLine={false}
                minTickGap={28}
              />
              <YAxis
                tickFormatter={fmtCompact}
                tickLine={false}
                axisLine={false}
                width={46}
              />
              <Tooltip
                content={<ChartTooltip unit={unit} />}
                cursor={{
                  stroke: "var(--color-border)",
                  strokeDasharray: "3 4",
                }}
              />
              <Area
                type="monotone"
                dataKey="total"
                name={seriesName}
                stroke={`var(${colorVar})`}
                strokeWidth={2}
                fill={`url(#${id})`}
                dot={
                  data.length === 1 ? { r: 4, fill: `var(${colorVar})` } : false
                }
                activeDot={{ r: 4, strokeWidth: 2, stroke: "white" }}
                isAnimationActive={false}
              />
              {selected && (
                <ReferenceDot
                  x={selected.day}
                  y={selected.total}
                  r={5}
                  fill={`var(${colorVar})`}
                  stroke="white"
                  strokeWidth={2}
                  isFront
                />
              )}
            </AreaChart>
          </ResponsiveContainer>
          <p className="chart-readout" aria-live="polite">
            {selected
              ? `${fmtDay(selected.day)} · ${fmtNum(selected.total)} ${unit}`
              : "移到图中查看数值，或用方向键逐日浏览"}
          </p>
        </div>
      ) : (
        <div className="trend-empty">
          <span>{visible ? "所选日期没有趋势数据" : "趋势已隐藏"}</span>
        </div>
      )}
    </section>
  );
}
