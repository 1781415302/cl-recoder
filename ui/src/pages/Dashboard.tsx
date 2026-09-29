// 仪表盘：今日 KPI（0.5s 近实时轮询）+ 每日趋势 + 设备总计。
import { useState } from "react";
import { useOverview } from "../api/queries";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { SkeletonCard, SkeletonCards } from "../components/Skeleton";
import { StatCard } from "../components/StatCard";
import { TrendChart } from "../components/TrendChart";
import { IconGamepad, IconKeyboard, IconMouse } from "../components/icons";
import { defaultRange, fmtNum, kindLabel } from "../lib/format";

interface DeviceSummary {
  id: number;
  kind: "keyboard" | "mouse" | "gamepad";
  name: string;
  total: number;
}

const deviceColumns: Column<DeviceSummary>[] = [
  { key: "name", header: "设备", value: (r) => r.name },
  { key: "kind", header: "种类", value: (r) => r.kind, render: (r) => kindLabel(r.kind) },
  { key: "total", header: "范围内累计", value: (r) => r.total, numeric: true, render: (r) => fmtNum(r.total) },
];

export function Dashboard() {
  const [range, setRange] = useState(() => defaultRange(30));
  const { data, isLoading, isError, refetch } = useOverview(range);

  if (isError) {
    return (
      <EmptyState
        title="读取数据失败"
        description="统计库可能被采集器短暂占用（WAL 通常几秒内恢复），已自动重试。"
        action={{ label: "立即重试", onClick: () => void refetch() }}
      />
    );
  }

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">仪表盘</h1>
          <p className="page-desc">外设输入总览 —— 今日数据近实时自动刷新</p>
        </div>
        <DateRangePicker value={range} onChange={setRange} />
      </div>

      {isLoading ? (
        <SkeletonCards count={3} />
      ) : (
        <div className="grid-cards">
          <StatCard label="今日按键" value={fmtNum(data?.today.keys ?? 0)} tone="primary" icon={<IconKeyboard />} />
          <StatCard label="今日点击" value={fmtNum(data?.today.clicks ?? 0)} tone="default" icon={<IconMouse />} />
          <StatCard label="今日手柄" value={fmtNum(data?.today.gamepad ?? 0)} tone="accent" icon={<IconGamepad />} />
        </div>
      )}

      {isLoading ? (
        <SkeletonCard rows={5} height="120px" />
      ) : (
        <TrendChart
          data={data?.days ?? []}
          title="每日总事件（所选范围）"
          colorVar="--chart-1"
          seriesName="全部设备"
        />
      )}

      {isLoading ? (
        <SkeletonCard rows={4} />
      ) : (
        <div className="card">
          <h2 className="card-title">设备统计</h2>
          <p className="card-sub">按设备型号分开统计（同型号合并；XInput 手柄为单行合并）</p>
          <div style={{ marginTop: "var(--space-3)" }}>
            <DataTable
              columns={deviceColumns}
              rows={data?.devices ?? []}
              rowKey={(r) => `d${r.id}`}
              initialSort={{ key: "total", dir: "desc" }}
              caption="设备范围内累计"
              empty={
                <EmptyState
                  title="还没有任何设备记录"
                  description="启动采集器后，接入的键盘/鼠标/手柄会自动出现在这里。"
                  action={{ label: "前往设置启动采集器", onClick: () => { window.location.hash = "settings"; } }}
                />
              }
            />
          </div>
        </div>
      )}
    </>
  );
}
