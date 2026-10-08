// 组合键页：仅修饰键组合（Ctrl/Shift/Alt/Win + 非修饰键），× 每天（R7）。
// §4.5/§4.8：默认今日；active 且范围含今日时 1s 轮询（历史固定范围不轮询）；
// 排行用 RankedList（稳定 id=mods:code）；完整表分页，全部返回行可访问；
// 跨范围不显示旧数据（不用 keepPreviousData）。
import { useQuery } from "@tanstack/react-query";
import * as client from "../api/client";
import { uiQueryPolicy } from "../api/queryPolicy";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { RankedList } from "../components/RankedList";
import { SkeletonCard } from "../components/Skeleton";
import { fmtNum } from "../lib/format";
import { useAppActivity } from "../lib/AppActivityProvider";
import { useStatisticsRange } from "../lib/useStatisticsRange";
import type { ComboRowLabeled } from "../api/types";

const columns: Column<ComboRowLabeled>[] = [
  {
    key: "label",
    header: "组合键",
    value: (r) => r.label,
    render: (r) => <span style={{ fontWeight: 600 }}>{r.label}</span>,
  },
  {
    key: "code",
    header: "编码",
    value: (r) => r.code,
    render: (r) => (
      <span className="mono">{`0x${r.code.toString(16).toUpperCase()}`}</span>
    ),
  },
  {
    key: "total",
    header: "累计次数",
    value: (r) => r.total,
    numeric: true,
    render: (r) => fmtNum(r.total),
  },
];

export function Combos() {
  const { range, onChange } = useStatisticsRange();
  const activity = useAppActivity();
  // §4.5：interval 仅范围包含当前 today 时传入（历史固定范围不轮询）
  const includesToday =
    range.from <= activity.today && activity.today <= range.to;
  const policy = uiQueryPolicy(
    activity.active,
    includesToday ? 1_000 : undefined,
  );
  const { data, isLoading } = useQuery({
    queryKey: ["combos", range.from, range.to],
    queryFn: () => client.getCombos(range.from, range.to, 200),
    ...policy,
  });
  // 活动初始化完成前查询被 gating（无凭据不臆断"无数据"），按加载态呈现
  const loading = isLoading || !activity.ready;

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">组合键</h1>
          <p className="page-desc">常用快捷键与组合使用次数</p>
        </div>
        <DateRangePicker value={range} onChange={onChange} />
      </div>

      {loading ? (
        <SkeletonCard rows={6} />
      ) : (
        <RankedList
          title="组合键排行"
          rows={(data ?? []).map((r) => ({
            id: `${r.mods}:${r.code}`,
            label: r.label,
            value: r.total,
          }))}
          unit="次"
          labelHeader="组合键"
          valueHeader="累计次数"
          queryLimit={200}
          fullTable={
            <DataTable
              columns={columns}
              rows={data ?? []}
              rowKey={(r) => `${r.mods}:${r.code}`}
              initialSort={{ key: "total", dir: "desc" }}
              caption="组合键排行（所选范围）"
              pageSize={50}
              resetKey={`${range.from}|${range.to}`}
              empty={
                <EmptyState
                  title="还没有组合键记录"
                  description="使用带修饰键的快捷键（如 Ctrl+C）时会自动记录。"
                  action={{
                    label: "前往设置",
                    onClick: () => {
                      window.location.hash = "settings";
                    },
                  }}
                />
              }
            />
          }
        />
      )}
    </>
  );
}
