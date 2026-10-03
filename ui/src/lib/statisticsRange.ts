// 统计范围纯模块（usability-runtime-v3 §4.5，S6）。
//
// 只做 selection ↔ Range 的映射：不持有 React 状态、不读取时钟——today 一律由调用方
// 传入（hook 在同一次 render 中用 activity.today 派生，"今日只采样一次"由此保证）：
// - 默认 { mode: "today" }：resolve 时 from=to=传入的同一 today 采样；午夜后 selection
//   不变、range 随新采样跟随；
// - 手动输入 / 非 Today preset → selectStatisticsRange(range, "fixed")：此后午夜不改；
// - 点击 Today → selectStatisticsRange(range, "today")：恢复跟随。
// 与 lib/appActivity.ts 同策略：对 api/types 只 type import（零运行时依赖，可在 Node
// 内建测试经 typescript.transpileModule → data URL 直载，见 tests/presentation-contracts.test.mjs）。
import type { Range } from "../api/types";

/** 统计范围选择：today=跟随本地当日（hook 只持有它）；fixed=用户固定范围（午夜不改） */
export type StatisticsRangeSelection = { mode: "today" } | { mode: "fixed"; range: Range };

/** 范围变化来源：Today preset → "today"（恢复跟随）；手动输入/其它 preset → "fixed" */
export type RangeChangeMode = "today" | "fixed";

/** selection + 某次 today 采样 → 查询用闭区间（合法本地日，from ≤ to）。
 * today 模式 from=to=同一次采样；fixed 模式原样返回 range 副本。 */
export function resolveStatisticsRange(selection: StatisticsRangeSelection, today: string): Range {
  if (selection.mode === "today") {
    return { from: today, to: today };
  }
  return { ...selection.range };
}

/** 用户选定 range + 变化来源 → 新 selection。Today 恢复跟随（丢弃 range）；
 * fixed 拷贝 range，防调用方后续原地改写污染 selection（同 wp-mouse-query 的可变 range 教训）。 */
export function selectStatisticsRange(range: Range, mode: RangeChangeMode): StatisticsRangeSelection {
  if (mode === "today") return { mode: "today" };
  return { mode: "fixed", range: { ...range } };
}
