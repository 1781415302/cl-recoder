// 排行几何（usability-runtime-v3 §4.8 U4）——降序/稳定同值次序的 Top10 与比较轨道 fraction。
// 仅做展示层排序与几何换算；不做任何数据聚合（聚合一律在 SQL，PLAN §2.5），
// 也不做数据求和/占比：fraction = value / topMax（榜首值），纯显示几何。

export interface RankRow {
  /** 稳定身份（Apps=exe、Combos=mods:code、WP keys=day:qtKey、WP combos=day:combo），作 key 用 */
  id: string;
  label: string;
  value: number;
}

/** 排行项：rank 从 1 起；fraction ∈ [0,1]（value/topMax），全 0 时为 0 */
export interface RankedItem extends RankRow {
  rank: number;
  fraction: number;
}

/** 排行榜固定显示前 10 行（§4.8），完整集合由页面以完整表呈现 */
const MAX_ROWS = 10;

/** 把 [0,1] 之外的值收敛进区间：NaN → 0，±Infinity → 1/0（防御异常值，正常计数不会走到） */
function clamp01(x: number): number {
  if (Number.isNaN(x)) return 0;
  return Math.min(1, Math.max(0, x));
}

/** 降序排行（最多 10 行）：同值保留输入顺序（Array.sort 稳定序）；不修改入参数组。
 *  topMax 取有限值的最大者（正常计数 = 榜首值）：单个 NaN 不污染全部 fraction。 */
export function rankTopRows(rows: readonly RankRow[]): RankedItem[] {
  const sorted = [...rows].sort((a, b) => b.value - a.value).slice(0, MAX_ROWS);
  let topMax = 0;
  for (const row of sorted) {
    if (Number.isFinite(row.value) && row.value > topMax) topMax = row.value;
  }
  return sorted.map((row, index) => ({
    ...row,
    rank: index + 1,
    fraction: topMax > 0 ? clamp01(row.value / topMax) : 0,
  }));
}
