// 仪表盘：今日 KPI（近实时轮询）+ 每日趋势 + 设备总计。
// §4.5：默认今日（useStatisticsRange，跨午夜跟随）；单日隐藏无意义的每日趋势卡；
// 设备总计为所选闭区间总量（overview.devices[].total，usability-runtime-v3 §4.6）。
import { useOverview } from "../api/queries";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { SkeletonCard, SkeletonCards } from "../components/Skeleton";
import { StatCard } from "../components/StatCard";
import { TrendChart } from "../components/TrendChart";
import { IconGamepad, IconKeyboard, IconMouse } from "../components/icons";
import { fmtDayShort, fmtNum, kindLabel } from "../lib/format";
import { useAppActivity } from "../lib/AppActivityProvider";
import { useStatisticsRange } from "../lib/useStatisticsRange";

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
  const { range, onChange } = useStatisticsRange();
  const activity = useAppActivity();
  const { data, isLoading, isError, refetch } = useOverview(range);
  // §4.8：仅单日时每日趋势卡无意义——隐藏（多日时保留，单数据点由 TrendChart 显示 dot）
  const singleDay = range.from === range.to;
  // 活动初始化完成前查询被 gating（无凭据不臆断"无数据"），按加载态呈现
  const loading = isLoading || !activity.ready;

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
        <DateRangePicker value={range} onChange={onChange} />
      </div>

      {loading ? (
        <SkeletonCards count={3} />
      ) : (
        <div className="grid-cards">
          {/* Overview.today 仍为 to 日拆分（§4.6）：卡片文案按 to 日如实标注 */}
          <StatCard label={range.to === activity.today ? "今日按键" : `${fmtDayShort(range.to)} 按键`} value={fmtNum(data?.today.keys ?? 0)} tone="primary" icon={<IconKeyboard />} />
          <StatCard label={range.to === activity.today ? "今日点击" : `${fmtDayShort(range.to)} 点击`} value={fmtNum(data?.today.clicks ?? 0)} tone="default" icon={<IconMouse />} />
          <StatCard label={range.to === activity.today ? "今日手柄" : `${fmtDayShort(range.to)} 手柄`} value={fmtNum(data?.today.gamepad ?? 0)} tone="accent" icon={<IconGamepad />} />
        </div>
      )}

      {!singleDay && loading ? <SkeletonCard rows={5} height="120px" /> : null}
      {!singleDay && !loading ? (
        <TrendChart
          data={data?.days ?? []}
          title="每日总事件（所选范围）"
          colorVar="--chart-1"
          seriesName="全部设备"
        />
      ) : null}

      {loading ? (
        <SkeletonCard rows={4} />
      ) : (
        <div className="card">
          <h2 className="card-title">设备统计</h2>
          <p className="card-sub">按设备型号分开统计（同型号合并；XInput 手柄为单行合并）；数值为所选范围累计</p>
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
