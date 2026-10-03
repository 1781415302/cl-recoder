// useStatisticsRange —— 统计范围 hook（usability-runtime-v3 §4.5，S6）。
//
// 只持有 selection 状态；range 在同一次 render 中直接用 activity.today 派生
// （resolveStatisticsRange 纯映射），不用 useEffect 维护第二份 range 状态——
// 防止恢复（inactive→active）时出现"一帧旧 day 已 enabled"的旧范围查询。
// onChange 的 mode 缺省按 "fixed" 处理（手动选定为常态）；DateRangePicker 对
// Today/非 Today preset 传显式 mode；旧调用方（WP/导出的 setState）忽略第二参数。
// 必须在 AppActivityProvider 内使用（today 来自 useAppActivity 的同一快照）。
import { useCallback, useState } from "react";
import type { Range } from "../api/types";
import { useAppActivity } from "./AppActivityProvider";
import {
  resolveStatisticsRange,
  selectStatisticsRange,
  type RangeChangeMode,
  type StatisticsRangeSelection,
} from "./statisticsRange";

export function useStatisticsRange(): {
  range: Range;
  selection: StatisticsRangeSelection;
  onChange: (range: Range, mode?: RangeChangeMode) => void;
} {
  const activity = useAppActivity();
  const [selection, setSelection] = useState<StatisticsRangeSelection>({ mode: "today" });
  const onChange = useCallback((range: Range, mode?: RangeChangeMode) => {
    setSelection(selectStatisticsRange(range, mode ?? "fixed"));
  }, []);
  // 同一次 render 的同一 activity.today 采样：range.from/to 必然来自同一天
  const range = resolveStatisticsRange(selection, activity.today);
  return { range, selection, onChange };
}
