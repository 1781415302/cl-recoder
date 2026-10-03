// 趋势折线图（§4.9：折线 ≤6 序列且带直接标签；图例可见可切换；tooltip 键盘可达）。
// §4.8：数值更新无动画（isAnimationActive=false 恒定，与偏好无关）；数据仅一个点时折线
// 不可见，以 dot 呈现保证读数可见（"只有一个有数据点也显示 dot"）。
// 时间序列趋势不适用"Top N ≤15"规则（该规则针对类别柱图）。
import { useState } from "react";
import {
  CartesianGrid,
  Line,
  LineChart,
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
  seriesName = "每日总事件",
  unit = "次",
  height = 240,
}: TrendChartProps) {
  const [visible, setVisible] = useState(true);
  const [idx, setIdx] = useState<number | null>(null);

  const last = data.length > 0 ? data[data.length - 1] : undefined;
  const selected = idx !== null ? data[idx] : undefined;
  // §4.8：仅一个数据点时折线不可见——显示常驻 dot（多点多边形由折线本身呈现，dot 关闭）
  const singlePoint = data.length === 1;

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (data.length === 0) return;
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      setIdx((prev) => {
        const cur = prev ?? data.length - 1;
        const next = e.key === "ArrowLeft" ? cur - 1 : cur + 1;
        return Math.min(data.length - 1, Math.max(0, next));
      });
    } else if (e.key === "Home") {
      e.preventDefault();
      setIdx(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setIdx(data.length - 1);
    }
  }

  return (
    <div className="card" style={{ display: "flex", flexDirection: "column", gap: "var(--space-2)" }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", flexWrap: "wrap", gap: "var(--space-2)" }}>
        <h2 className="card-title">{title}</h2>
        {/* 图例：可见、可点击切换（§4.9）；同时充当系列直接标签（系列名 + 最新值） */}
        <button
          type="button"
          className="legend-chip"
          aria-pressed={visible}
          onClick={() => setVisible((v) => !v)}
          title="点击显示/隐藏该系列"
        >
          <span className="legend-dot" style={{ background: `var(${colorVar})` }} />
          {seriesName}
          {last ? (
            <span className="num" style={{ color: "var(--color-text)" }}>
              最新 {fmtCompact(last.total)}
            </span>
          ) : null}
        </button>
      </div>
      {visible && data.length > 0 ? (
        <div
          tabIndex={0}
          role="group"
          aria-label={`${title}。折线图，共 ${data.length} 天数据，最新 ${fmtNum(last?.total ?? 0)}${unit}。使用左右方向键逐日查看数值。`}
          onKeyDown={onKeyDown}
          onFocus={() => setIdx(data.length - 1)}
          onBlur={() => setIdx(null)}
          style={{ outlineOffset: 4 }}
        >
          <ResponsiveContainer width="100%" height={height}>
            <LineChart data={data} margin={{ top: 12, right: 48, bottom: 0, left: 0 }}>
              <CartesianGrid strokeDasharray="3 3" stroke="var(--color-divider)" vertical={false} />
              <XAxis
                dataKey="day"
                tickFormatter={fmtDayShort}
                interval="preserveStartEnd"
                minTickGap={28}
                tickLine={false}
              />
              <YAxis tickFormatter={fmtCompact} tickLine={false} axisLine={false} width={44} />
              <Tooltip content={<ChartTooltip unit={unit} />} />
              <Line
                type="monotone"
                dataKey="total"
                name={seriesName}
                style={{ stroke: `var(${colorVar})` }}
                strokeWidth={2}
                dot={singlePoint ? { r: 3, style: { fill: `var(${colorVar})` } } : false}
                activeDot={{ r: 4, style: { fill: `var(${colorVar})` } }}
                isAnimationActive={false}
              />
              {/* 键盘选中点（tooltip 的键盘可达替代 + 视觉定位）；系列直接标签由图例芯片承载（系列名 + 最新值） */}
              {selected ? (
                <ReferenceDot
                  x={selected.day}
                  y={selected.total}
                  r={5}
                  style={{ fill: "var(--color-accent)", stroke: "none" }}
                  isFront
                />
              ) : null}
            </LineChart>
          </ResponsiveContainer>
          <p className="chart-readout" aria-live="polite">
            {selected
              ? `${fmtDay(selected.day)} · ${fmtNum(selected.total)} ${unit}`
              : "聚焦图表后用 ←/→ 键逐日查看数值（Home/End 跳到首尾）"}
          </p>
        </div>
      ) : (
        <p className="chart-readout" aria-live="polite">
          {visible ? "暂无数据" : "系列已隐藏，点击图例恢复"}
        </p>
      )}
    </div>
  );
}
