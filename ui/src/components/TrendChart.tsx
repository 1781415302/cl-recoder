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

/** 每日趋势面积图：可隐藏序列、方向键逐日读数（无动画，软件渲染环境友好） */
export function TrendChart({
  data,
  title,
  colorVar = "--accent",
  seriesName = "输入次数",
  unit = "次",
  height = 240,
}: TrendChartProps) {
  const id = useId().replace(/:/g, "");
  const [visible, setVisible] = useState(true);
  const [index, setIndex] = useState<number | null>(null);
  const latest = data[data.length - 1];
  const selected = index === null ? undefined : data[index];

  return (
    <section className="card">
      <div className="card-head">
        <div>
          <h2 className="card-title">{title}</h2>
          {latest ? <p className="card-sub">最近记录 · {fmtDay(latest.day)}</p> : null}
        </div>
        <button
          type="button"
          className="legend"
          aria-pressed={visible}
          onClick={() => setVisible((v) => !v)}
          title="显示或隐藏趋势"
        >
          <span className="legend-dot" style={{ background: `var(${colorVar})` }} />
          {seriesName}
          {latest ? (
            <strong className="num">
              {fmtNum(latest.total)} {unit}
            </strong>
          ) : null}
        </button>
      </div>
      {visible && data.length > 0 ? (
        <div
          className="chart-focus"
          tabIndex={0}
          role="group"
          aria-label={`${title}，共 ${data.length} 天；用左右方向键查看每天的数值`}
          onFocus={() => setIndex(data.length - 1)}
          onBlur={() => setIndex(null)}
          onKeyDown={(e) => {
            if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
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
                        (current ?? data.length - 1) + (e.key === "ArrowLeft" ? -1 : 1),
                      ),
                    ),
            );
          }}
        >
          <ResponsiveContainer width="100%" height={height}>
            <AreaChart data={data} margin={{ top: 10, right: 12, bottom: 0, left: 0 }}>
              <defs>
                <linearGradient id={id} x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stopColor={`var(${colorVar})`} stopOpacity={0.32} />
                  <stop offset="100%" stopColor={`var(${colorVar})`} stopOpacity={0.02} />
                </linearGradient>
              </defs>
              <CartesianGrid stroke="var(--line)" vertical={false} strokeDasharray="3 6" />
              <XAxis
                dataKey="day"
                tickFormatter={fmtDayShort}
                tickLine={false}
                axisLine={false}
                minTickGap={28}
                tick={{ fill: "var(--text-3)", fontSize: 11 }}
              />
              <YAxis
                tickFormatter={fmtCompact}
                tickLine={false}
                axisLine={false}
                width={44}
                tick={{ fill: "var(--text-3)", fontSize: 11 }}
              />
              <Tooltip
                content={<ChartTooltip unit={unit} />}
                cursor={{ stroke: "var(--line-strong)", strokeDasharray: "3 4" }}
              />
              <Area
                type="monotone"
                dataKey="total"
                name={seriesName}
                stroke={`var(${colorVar})`}
                strokeWidth={2}
                fill={`url(#${id})`}
                dot={data.length === 1 ? { r: 4, fill: `var(${colorVar})` } : false}
                activeDot={{ r: 4, strokeWidth: 2, stroke: "var(--surface)" }}
                isAnimationActive={false}
              />
              {selected ? (
                <ReferenceDot
                  x={selected.day}
                  y={selected.total}
                  r={5}
                  fill={`var(${colorVar})`}
                  stroke="var(--surface)"
                  strokeWidth={2}
                  isFront
                />
              ) : null}
            </AreaChart>
          </ResponsiveContainer>
          <p className="readout" aria-live="polite">
            {selected
              ? `${fmtDay(selected.day)} · ${fmtNum(selected.total)} ${unit}`
              : "移到图中查看数值，或用方向键逐日浏览"}
          </p>
        </div>
      ) : (
        <div className="chart-empty">
          <span>{visible ? "所选日期没有趋势数据" : "趋势已隐藏"}</span>
        </div>
      )}
    </section>
  );
}
