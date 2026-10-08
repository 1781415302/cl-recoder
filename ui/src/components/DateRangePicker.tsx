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
  { key: "7", label: "近 7 天", days: 7 },
  { key: "30", label: "近 30 天", days: 30 },
  { key: "90", label: "近 90 天", days: 90 },
  { key: "365", label: "近一年", days: 365 },
  { key: "all", label: "全部历史", days: null },
];
const rangeOf = (days: number | null): Range =>
  days === null ? { from: "2000-01-01", to: todayDay() } : defaultRange(days);
export function DateRangePicker({ value, onChange }: DateRangePickerProps) {
  const selected =
    PRESETS.find((p) => {
      const r = rangeOf(p.days);
      return r.from === value.from && r.to === value.to;
    })?.key ?? "custom";
  return (
    <div className="date-toolbar" role="group" aria-label="日期范围">
      <select
        className="input date-preset-select"
        aria-label="快捷日期范围"
        value={selected}
        onChange={(e) => {
          const preset = PRESETS.find((p) => p.key === e.target.value);
          if (preset)
            onChange(
              rangeOf(preset.days),
              preset.key === "1" ? "today" : "fixed",
            );
        }}
      >
        {PRESETS.map((p) => (
          <option key={p.key} value={p.key}>
            {p.label}
          </option>
        ))}
        <option value="custom" disabled>
          自定日期
        </option>
      </select>
      <div className="date-inputs">
        <IconCalendar size={15} />
        <input
          type="date"
          value={value.from}
          max={value.to}
          aria-label="开始日期"
          onChange={(e) => {
            const from = e.target.value;
            if (from)
              onChange(
                { from, to: from > value.to ? from : value.to },
                "fixed",
              );
          }}
        />
        <span className="date-separator" aria-hidden="true">
          —
        </span>
        <input
          type="date"
          value={value.to}
          min={value.from}
          max={todayDay()}
          aria-label="结束日期"
          onChange={(e) => {
            const to = e.target.value;
            if (to)
              onChange(
                { from: to < value.from ? to : value.from, to },
                "fixed",
              );
          }}
        />
      </div>
    </div>
  );
}
