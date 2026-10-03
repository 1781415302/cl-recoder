// 应用页：前台应用时长 + 应用内按键/点击（R8；get_apps 已按秒降序，§4.5）。
// §4.5/§4.8：默认今日；active 且范围含今日时 1s 轮询（历史固定范围不轮询）；
// 排行用 RankedList（稳定 id=exe，值文字 fmtDuration 自带单位）；完整表分页，
// 全部返回行可访问；跨范围不显示旧数据（不用 keepPreviousData）。
import { useQuery } from "@tanstack/react-query";
import * as client from "../api/client";
import { uiQueryPolicy } from "../api/queryPolicy";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { RankedList } from "../components/RankedList";
import { SkeletonCard } from "../components/Skeleton";
import { fmtDuration, fmtNum } from "../lib/format";
import { useAppActivity } from "../lib/AppActivityProvider";
import { useStatisticsRange } from "../lib/useStatisticsRange";
import type { AppRowLabeled } from "../api/types";

const columns: Column<AppRowLabeled>[] = [
  {
    key: "name", header: "应用", value: (r) => r.name,
    render: (r) => (
      <span>
        <span style={{ fontWeight: 600 }}>{r.name}</span>
        <span className="mono" style={{ marginLeft: "var(--space-2)" }}>{r.exe}</span>
      </span>
    ),
  },
  { key: "exe", header: "进程", value: (r) => r.exe, render: (r) => <span className="mono">{r.exe}</span> },
  { key: "seconds", header: "前台时长", value: (r) => r.seconds, numeric: true, render: (r) => fmtDuration(r.seconds) },
  { key: "keys", header: "按键", value: (r) => r.keys, numeric: true, render: (r) => fmtNum(r.keys) },
  { key: "clicks", header: "点击", value: (r) => r.clicks, numeric: true, render: (r) => fmtNum(r.clicks) },
];

export function Apps() {
  const { range, onChange } = useStatisticsRange();
  const activity = useAppActivity();
  // §4.5：interval 仅范围包含当前 today 时传入（历史固定范围不轮询）
  const includesToday = range.from <= activity.today && activity.today <= range.to;
  const policy = uiQueryPolicy(activity.active, includesToday ? 1_000 : undefined);
  const { data, isLoading } = useQuery({
    queryKey: ["apps", range.from, range.to],
    queryFn: () => client.getApps(range.from, range.to, 200),
    ...policy,
  });
  // 活动初始化完成前查询被 gating（无凭据不臆断"无数据"），按加载态呈现
  const loading = isLoading || !activity.ready;

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">应用</h1>
          <p className="page-desc">按 exe 记录的前台时长与该应用内的按键、点击次数</p>
        </div>
        <DateRangePicker value={range} onChange={onChange} />
      </div>

      {loading ? (
        <SkeletonCard rows={7} />
      ) : (
        <RankedList
          title="应用前台时长排行"
          rows={(data ?? []).map((r) => ({ id: r.exe, label: r.name || r.exe, value: r.seconds }))}
          formatValue={fmtDuration}
          labelHeader="应用"
          valueHeader="前台时长"
          queryLimit={200}
          fullTable={
            <DataTable
              columns={columns}
              rows={data ?? []}
              rowKey={(r) => r.exe}
              initialSort={{ key: "seconds", dir: "desc" }}
              caption="应用前台时长排行（所选范围）"
              pageSize={50}
              resetKey={`${range.from}|${range.to}`}
              empty={
                <EmptyState
                  title="还没有应用数据"
                  description="前台应用跟踪在采集器运行时自动记录；切换窗口后稍等片刻再来看。"
                  action={{ label: "前往设置", onClick: () => { window.location.hash = "settings"; } }}
                />
              }
            />
          }
        />
      )}
    </>
  );
}
