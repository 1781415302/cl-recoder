// 简洁排行列表（usability-runtime-v3 §4.8 U4）：Top10 行（名次 + 可换行名称 + 右对齐完整值 + 4px 比较轨道）
// + 可展开的完整表。
// 行序/名次/轨道几何来自 lib/ranking.ts（fraction = value/topMax，不做求和/占比）；稳定 id 作 key；
// 值变化无过渡/入场动画（§4.8）；顶部如实说明显示 Top10/已加载 N，达到查询 limit 时提示
// “本次最多加载 N 项”，不宣称已证明库内更多行。组件不查询 API（页面管理数据与 limit）。
import { useState, type ReactNode } from "react";
import { rankTopRows, type RankRow } from "../lib/ranking";
import { fmtNum } from "../lib/format";

export interface RankedListProps {
  title: string;
  rows: RankRow[];
  /** 与 Top10 并存的完整表（页面传入 DataTable，建议 pageSize 分页让全部返回行可访问） */
  fullTable: ReactNode;
  /** 完整值格式化（如 fmtDuration）：输出自带单位，不再拼后缀（§4.7） */
  formatValue?: (value: number) => string;
  /** 未提供 formatValue 时的值单位后缀（如 “次”）；与 formatValue 同给时不重复拼接 */
  unit?: string;
  /** 完整表标题前缀与 aria 描述用（默认 “条目”） */
  labelHeader?: string;
  /** 值列语义（默认 “数量”），用于列表 aria 描述 */
  valueHeader?: string;
  /** 页面发起查询时的 limit（Apps/Combos/WP 通常 200）：rows.length 达到它时提示本次最多加载 N 项 */
  queryLimit?: number;
}

export function RankedList({
  title,
  rows,
  fullTable,
  formatValue,
  unit,
  labelHeader = "条目",
  valueHeader = "数量",
  queryLimit,
}: RankedListProps) {
  const [tableOpen, setTableOpen] = useState(false);
  const top = rankTopRows(rows);
  // formatValue 的输出已带单位（fmtDuration）；仅缺省路径用 fmtNum(+unit) 保证完整文本。
  const valueText = (v: number): string =>
    formatValue ? formatValue(v) : unit ? `${fmtNum(v)} ${unit}` : fmtNum(v);

  return (
    <section className="card">
      <div className="section-heading">
        <div>
          <h2 className="card-title">{title}</h2>
          <p className="card-sub">
            前 {top.length} 项 · 已加载 {fmtNum(rows.length)} 项
            {queryLimit !== undefined && rows.length >= queryLimit
              ? `（本次最多 ${fmtNum(queryLimit)} 项）`
              : ""}
          </p>
        </div>
        <span className="chip">{valueHeader}</span>
      </div>
      {top.length === 0 ? (
        <p className="chart-readout">暂无数据</p>
      ) : (
        <ol
          className="rank-list"
          aria-label={`${title}：按${valueHeader}降序的前 ${top.length} 名`}
        >
          {top.map((item) => (
            <li className="rank-row" key={item.id}>
              <span className="rank-no num">{item.rank}</span>
              <span className="rank-name">{item.label}</span>
              <span className="rank-value num">{valueText(item.value)}</span>
              <span className="rank-track" aria-hidden="true">
                <span
                  className="rank-fill"
                  style={{ width: `${item.fraction * 100}%` }}
                />
              </span>
            </li>
          ))}
        </ol>
      )}
      <hr className="card-divider" />
      <div className="detail-card-heading">
        <span>{labelHeader}完整记录</span>
        <button
          type="button"
          className="btn btn-sm"
          aria-expanded={tableOpen}
          onClick={() => setTableOpen((open) => !open)}
        >
          {tableOpen ? "收起记录" : "查看全部记录"}
        </button>
      </div>
      {tableOpen && (
        <div style={{ marginTop: "var(--space-4)" }}>{fullTable}</div>
      )}
    </section>
  );
}
