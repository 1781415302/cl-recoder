// 日期范围选择（§4.7 Range 契约："YYYY-MM-DD"；§4.9 日期本地化）。
// 原生 <input type="date">（键盘可达 + 本地化）+ 常用区间快捷键。
import { defaultRange, todayDay } from "../lib/format";
import type { Range } from "../api/types";

interface DateRangePickerProps {
  value: Range;
  onChange: (r: Range) => void;
}

const PRESETS: { label: string; days: number | "all" }[] = [
  { label: "今天", days: 1 },
  { label: "近 7 天", days: 7 },
  { label: "近 30 天", days: 30 },
  { label: "近 90 天", days: 90 },
  { label: "近一年", days: 365 },
  { label: "全部", days: "all" },
];

/** 全部历史：用足够早的起点，覆盖软件可能出现的最早数据 */
const ALL_FROM = "2000-01-01";

function presetRange(days: number | "all"): Range {
  return days === "all" ? { from: ALL_FROM, to: todayDay() } : defaultRange(days);
}

export function DateRangePicker({ value, onChange }: DateRangePickerProps) {
  const today = todayDay();

  function setFrom(from: string) {
    if (!from) return;
    // 保持 from ≤ to：from 越界时把 to 提上来
    onChange({ from, to: from > value.to ? from : value.to });
  }
  function setTo(to: string) {
    if (!to) return;
    onChange({ from: to < value.from ? to : value.from, to });
  }

  return (
    <div
      className="card"
      style={{
        display: "flex",
        alignItems: "center",
        gap: "var(--space-2)",
        flexWrap: "wrap",
        padding: "var(--space-2) var(--space-3)",
        boxShadow: "none",
      }}
      role="group"
      aria-label="日期范围"
    >
      {PRESETS.map((p) => {
        const r = presetRange(p.days);
        const active = r.from === value.from && r.to === value.to;
        return (
          <button
            key={p.label}
            type="button"
            className={`btn btn-sm${active ? " btn-primary" : ""}`}
            aria-pressed={active}
            onClick={() => onChange(r)}
          >
            {p.label}
          </button>
        );
      })}
      <div style={{ display: "flex", alignItems: "center", gap: "var(--space-1)", marginLeft: "auto" }}>
        <input
          type="date"
          className="input"
          value={value.from}
          max={value.to}
          onChange={(e) => setFrom(e.target.value)}
          aria-label="开始日期"
        />
        <span aria-hidden="true" style={{ color: "var(--color-text-muted)" }}>~</span>
        <input
          type="date"
          className="input"
          value={value.to}
          min={value.from}
          max={today}
          onChange={(e) => setTo(e.target.value)}
          aria-label="结束日期"
        />
      </div>
    </div>
  );
}
