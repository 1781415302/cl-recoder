// Top N 柱状图（§4.9 硬规则：Top N 柱图 N≤15 + 完整表格并存——fullTable 必传，强制并存）。
import { useState } from "react";
import {
  Bar,
  BarChart,
  Cell,
  LabelList,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { fmtCompact, fmtNum } from "../lib/format";
import { useReducedMotion } from "../lib/useReducedMotion";
import { ChartTooltip } from "./ChartTooltip";
import type { ReactNode } from "react";

export interface BarRow {
  label: string;
  value: number;
}

const MAX_BARS = 15; // §4.9：N ≤ 15

interface TopBarChartProps {
  title: string;
  rows: BarRow[];
  /** 与柱图并存的完整表格（页面传入全量行的 DataTable） */
  fullTable: ReactNode;
  colorVar?: string;
  unit?: string;
  labelHeader?: string;
  valueHeader?: string;
}

export function TopBarChart({
  title,
  rows,
  fullTable,
  colorVar = "--chart-1",
  unit = "次",
  labelHeader = "条目",
  valueHeader = "数量",
}: TopBarChartProps) {
  const reduceMotion = useReducedMotion();
  const [idx, setIdx] = useState<number | null>(null);

  // 展示层切片：行已由后端按值降序返回；此处再排序保证柱图取 Top15（不做聚合，PLAN §2.5）
  const top = [...rows].sort((a, b) => b.value - a.value).slice(0, MAX_BARS);
  const selected = idx !== null ? top[idx] : undefined;
  const chartHeight = Math.max(120, top.length * 30 + 42);

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (top.length === 0) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setIdx((prev) => {
        const cur = prev ?? 0;
        const next = e.key === "ArrowDown" ? cur + 1 : cur - 1;
        return Math.min(top.length - 1, Math.max(0, next));
      });
    } else if (e.key === "Home") {
      e.preventDefault();
      setIdx(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setIdx(top.length - 1);
    }
  }

  return (
    <div className="card">
      <h2 className="card-title">{title}</h2>
      <p className="card-sub">
        Top {Math.min(MAX_BARS, top.length)} / 共 {rows.length} 项 ·
        完整数据见下方表格
      </p>
      {top.length > 0 ? (
        <div
          tabIndex={0}
          role="group"
          aria-label={`${title}。水平柱状图，共 ${top.length} 项，最高为 ${top[0].label} ${fmtNum(top[0].value)}${unit}。使用上下方向键逐项查看数值。`}
          onKeyDown={onKeyDown}
          onFocus={() => setIdx(0)}
          onBlur={() => setIdx(null)}
          style={{ outlineOffset: 4 }}
        >
          <ResponsiveContainer width="100%" height={chartHeight}>
            <BarChart
              data={top}
              layout="vertical"
              margin={{ top: 4, right: 56, bottom: 0, left: 0 }}
              barCategoryGap={6}
            >
              <XAxis
                type="number"
                tickFormatter={fmtCompact}
                tickLine={false}
                axisLine={false}
              />
              <YAxis
                type="category"
                dataKey="label"
                width={110}
                tickLine={false}
                axisLine={false}
                tickFormatter={(v: string) =>
                  v.length > 9 ? `${v.slice(0, 9)}…` : v
                }
              />
              <Tooltip content={<ChartTooltip unit={unit} />} />
              <Bar
                dataKey="value"
                name={valueHeader}
                style={{ fill: `var(${colorVar})` }}
                radius={[0, 4, 4, 0]}
                isAnimationActive={!reduceMotion}
              >
                <LabelList
                  dataKey="value"
                  position="right"
                  formatter={(v: number) => fmtCompact(v)}
                  style={{ fill: "var(--color-text-muted)" }}
                />
                {top.map((row, i) => (
                  <Cell
                    key={row.label}
                    style={{
                      fill:
                        i === idx
                          ? "var(--color-primary-hover)"
                          : `var(${colorVar})`,
                    }}
                    cursor="pointer"
                    onClick={() => setIdx(i)}
                  />
                ))}
              </Bar>
            </BarChart>
          </ResponsiveContainer>
          <p className="chart-readout" aria-live="polite">
            {selected
              ? `${selected.label} · ${fmtNum(selected.value)} ${unit}`
              : "聚焦图表后用 ↑/↓ 键逐项查看数值；完整清单见下方表格"}
          </p>
        </div>
      ) : (
        <p className="chart-readout">暂无数据</p>
      )}
      <hr className="card-divider" />
      <div
        style={{
          display: "flex",
          alignItems: "baseline",
          justifyContent: "space-between",
          marginBottom: "var(--space-2)",
        }}
      >
        <h3
          style={{ margin: 0, fontSize: "var(--text-body)", fontWeight: 600 }}
        >
          {labelHeader}完整列表
        </h3>
      </div>
      {fullTable}
    </div>
  );
}
