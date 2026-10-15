// 日期范围：分段式快捷项 + 「自定义」弹出两个日期框。
// 语义不变：「今天」以 mode="today" 回传（恢复跟随本地当日），其余一律 "fixed"。
import { useEffect, useRef, useState } from "react";
import { defaultRange, todayDay } from "../lib/format";
import type { Range } from "../api/types";
import type { RangeChangeMode } from "../lib/statisticsRange";
import { IconCalendar } from "./icons";

interface DateRangePickerProps {
  value: Range;
  onChange: (range: Range, mode?: RangeChangeMode) => void;
}

const PRESETS = [
  { key: "1", label: "今天", days: 1 },
  { key: "7", label: "7 天", days: 7 },
  { key: "30", label: "30 天", days: 30 },
  { key: "90", label: "90 天", days: 90 },
  { key: "365", label: "1 年", days: 365 },
  { key: "all", label: "全部", days: null },
] as const;

const rangeOf = (days: number | null): Range =>
  days === null ? { from: "2000-01-01", to: todayDay() } : defaultRange(days);

export function DateRangePicker({ value, onChange }: DateRangePickerProps) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const selected =
    PRESETS.find((p) => {
      const r = rangeOf(p.days);
      return r.from === value.from && r.to === value.to;
    })?.key ?? "custom";

  // 点击外部 / Esc 关闭弹层
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="range" role="group" aria-label="日期范围" ref={root}>
      <div className="seg" role="radiogroup" aria-label="快捷日期范围">
        {PRESETS.map((p) => (
          <button
            key={p.key}
            type="button"
            role="radio"
            aria-checked={selected === p.key}
            className="seg-btn"
            onClick={() => onChange(rangeOf(p.days), p.key === "1" ? "today" : "fixed")}
          >
            {p.label}
          </button>
        ))}
        <button
          type="button"
          className="seg-btn seg-btn--icon"
          role="radio"
          aria-checked={selected === "custom"}
          aria-expanded={open}
          aria-haspopup="dialog"
          onClick={() => setOpen((o) => !o)}
          title="自定义日期"
        >
          <IconCalendar size={15} />
          <span className="range-custom-label num">
            {selected === "custom" ? `${value.from.slice(5)} – ${value.to.slice(5)}` : "自定义"}
          </span>
        </button>
      </div>
      {open ? (
        <div className="range-pop" role="dialog" aria-label="自定义日期范围">
          <label className="field">
            <span className="field-label">开始日期</span>
            <input
              type="date"
              className="input"
              value={value.from}
              max={value.to}
              aria-label="开始日期"
              onChange={(e) => {
                const from = e.target.value;
                if (from)
                  onChange({ from, to: from > value.to ? from : value.to }, "fixed");
              }}
            />
          </label>
          <label className="field">
            <span className="field-label">结束日期</span>
            <input
              type="date"
              className="input"
              value={value.to}
              min={value.from}
              max={todayDay()}
              aria-label="结束日期"
              onChange={(e) => {
                const to = e.target.value;
                if (to)
                  onChange({ from: to < value.from ? to : value.from, to }, "fixed");
              }}
            />
          </label>
        </div>
      ) : null}
    </div>
  );
}
