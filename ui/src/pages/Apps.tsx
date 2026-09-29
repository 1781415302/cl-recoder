// 应用页：前台应用时长 + 应用内按键/点击（R8；get_apps 已按秒降序，§4.5）。
import { useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import * as client from "../api/client";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { SkeletonCard } from "../components/Skeleton";
import { TopBarChart } from "../components/TopBarChart";
import { defaultRange, fmtDuration, fmtNum } from "../lib/format";
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
  const [range, setRange] = useState(() => defaultRange(30));
  const { data, isLoading } = useQuery({
    queryKey: ["apps", range.from, range.to],
    queryFn: () => client.getApps(range.from, range.to, 200),
    placeholderData: keepPreviousData,
    refetchInterval: 1_000,
    refetchIntervalInBackground: true,
    staleTime: 0,
  });

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">应用</h1>
          <p className="page-desc">按 exe 记录的前台时长与该应用内的按键、点击次数</p>
        </div>
        <DateRangePicker value={range} onChange={setRange} />
      </div>

      {isLoading ? (
        <SkeletonCard rows={7} />
      ) : (
        <TopBarChart
          title="应用前台时长排行"
          rows={(data ?? []).map((r) => ({ label: r.name || r.exe, value: r.seconds }))}
          colorVar="--chart-4"
          unit="秒"
          labelHeader="应用"
          valueHeader="前台时长"
          fullTable={
            <DataTable
              columns={columns}
              rows={data ?? []}
              rowKey={(r) => r.exe}
              initialSort={{ key: "seconds", dir: "desc" }}
              caption="应用前台时长排行（所选范围）"
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
