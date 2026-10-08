// WhatPulse 页：只读导入的历史数据，独立命名空间展示，绝不与本软件数据混合（R11/设计原则 4）。
// §4.5：默认 90 天历史（defaultRange 显式天数含义保留）；不新加 interval，但 inactive 时
// 禁止 focus/invalidation 发请求（uiQueryPolicy gating）；历史 placeholder 保留并明确"更新中"。
// §4.6：排行仍逐日行（名称附日期，不跨日 SUM），稳定身份 day:qtKey / day:combo / day:path；
// appsTotal KPI 为范围内应用前台秒数总和（fmtDuration，非条目数量）。
import { useState } from "react";
import {
  useMutation,
  useQuery,
  useQueryClient,
  keepPreviousData,
} from "@tanstack/react-query";
import * as client from "../api/client";
import { useSettings } from "../api/queries";
import { uiQueryPolicy } from "../api/queryPolicy";
import { wpMouseQueryOptions } from "../api/wpMouseQuery";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { RankedList } from "../components/RankedList";
import { SkeletonCard, SkeletonCards } from "../components/Skeleton";
import { StatCard } from "../components/StatCard";
import { TrendChart } from "../components/TrendChart";
import {
  IconImport,
  IconPulse,
  IconRefresh,
  IconWarn,
} from "../components/icons";
import { useAppActivity } from "../lib/AppActivityProvider";
import { defaultRange, fmtDay, fmtDuration, fmtNum } from "../lib/format";
import type { ImportReport, WpAppRow } from "../api/types";

function ImportReportCard({ report }: { report: ImportReport }) {
  return (
    <div className="card" role="status">
      <h2
        className="card-title"
        style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}
      >
        <IconImport /> 导入{report.ok ? "成功" : "失败"}
      </h2>
      <p className="card-sub">
        按键 {fmtNum(report.keys)} · 组合 {fmtNum(report.combos)} · 应用{" "}
        {fmtNum(report.apps)} · 鼠标天数 {fmtNum(report.mouseDays)} · 范围{" "}
        {report.dateMin ?? "?"} ~ {report.dateMax ?? "?"} · 耗时{" "}
        {fmtNum(report.durationMs)} ms
      </p>
      {report.warnings.length > 0 ? (
        <ul
          style={{
            margin: "var(--space-2) 0 0",
            paddingLeft: "var(--space-4)",
            color: "var(--color-text-muted)",
            fontSize: "var(--text-caption)",
          }}
        >
          {report.warnings.map((w, i) => (
            <li
              key={i}
              style={{
                display: "flex",
                gap: "var(--space-1)",
                alignItems: "baseline",
              }}
            >
              <IconWarn size={14} /> {w}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

const wpAppColumns: Column<WpAppRow>[] = [
  {
    key: "day",
    header: "日期",
    value: (r) => r.day,
    render: (r) => fmtDay(r.day),
  },
  { key: "name", header: "应用", value: (r) => r.name },
  {
    key: "seconds",
    header: "时长",
    value: (r) => r.seconds,
    numeric: true,
    render: (r) => fmtDuration(r.seconds),
  },
  {
    key: "keys",
    header: "按键",
    value: (r) => r.keys,
    numeric: true,
    render: (r) => fmtNum(r.keys),
  },
  {
    key: "clicks",
    header: "点击",
    value: (r) => r.clicks,
    numeric: true,
    render: (r) => fmtNum(r.clicks),
  },
];

export function WhatPulse() {
  const [range, setRange] = useState(() => defaultRange(90));
  const qc = useQueryClient();
  const activity = useAppActivity();
  // §4.5：WP 不新加 interval；仅 activity gating + uiOwned（inactive 时 focus/invalidation 不发请求）
  const policy = uiQueryPolicy(activity.active);
  const settings = useSettings();
  const meta = useQuery({
    queryKey: ["wpMeta"],
    queryFn: () => client.getWpMeta(),
    ...policy,
  });

  const overview = useQuery({
    queryKey: ["wpOverview", range.from, range.to],
    queryFn: () => client.getWpOverview(range.from, range.to),
    placeholderData: keepPreviousData,
    ...policy,
  });
  const keys = useQuery({
    queryKey: ["wpKeys", range.from, range.to],
    queryFn: () => client.getWpKeys(range.from, range.to, 200),
    placeholderData: keepPreviousData,
    ...policy,
  });
  const combos = useQuery({
    queryKey: ["wpCombos", range.from, range.to],
    queryFn: () => client.getWpCombos(range.from, range.to, 200),
    placeholderData: keepPreviousData,
    ...policy,
  });
  const apps = useQuery({
    queryKey: ["wpApps", range.from, range.to],
    queryFn: () => client.getWpApps(range.from, range.to, 200),
    placeholderData: keepPreviousData,
    ...policy,
  });
  const mouse = useQuery({
    queryKey: ["wpMouse", range.from, range.to],
    queryFn: () => client.getWpMouse(range.from, range.to),
    placeholderData: keepPreviousData,
    ...policy,
  });
  // F7：key 绑定 (from, to, limit)，改日期/limit 才会触发查询且缓存不串用；
  // 工厂 key 以 ['wpButtons']/['wpScrolls'] 为前缀，导入完成后的失效逻辑不变
  const buttons = useQuery({
    ...wpMouseQueryOptions("buttons", range, 20, client.getWpMouseButtons),
    placeholderData: keepPreviousData,
    ...policy,
  });
  const scrolls = useQuery({
    ...wpMouseQueryOptions("scrolls", range, 20, client.getWpMouseScrolls),
    placeholderData: keepPreviousData,
    ...policy,
  });

  const [report, setReport] = useState<ImportReport | null>(null);
  const importMut = useMutation({
    mutationFn: (path: string) => client.importWhatpulse(path),
    onSuccess: (r) => {
      setReport(r);
      void qc.invalidateQueries({ queryKey: ["wpMeta"] });
      void qc.invalidateQueries({ queryKey: ["wpOverview"] });
      void qc.invalidateQueries({ queryKey: ["wpKeys"] });
      void qc.invalidateQueries({ queryKey: ["wpCombos"] });
      void qc.invalidateQueries({ queryKey: ["wpApps"] });
      void qc.invalidateQueries({ queryKey: ["wpMouse"] });
      void qc.invalidateQueries({ queryKey: ["wpButtons"] });
      void qc.invalidateQueries({ queryKey: ["wpScrolls"] });
    },
  });

  const dbPath = settings.data?.wpDbPath ?? "";
  // §4.5：历史 placeholder 保留（跨范围显示旧数据），但更新中必须明确可见
  const updating = [overview, keys, combos, apps, mouse, buttons, scrolls].some(
    (q) => q.data !== undefined && q.isFetching,
  );

  if (meta.isPending) return <SkeletonCard rows={6} />;
  if (!meta.data) {
    return (
      <>
        <div className="page-header">
          <div>
            <h1 className="page-title">WhatPulse</h1>
            <p className="page-desc">导入的历史记录，独立保留</p>
          </div>
        </div>
        <EmptyState
          icon={<IconPulse size={36} />}
          title="尚未导入 WhatPulse 数据"
          description="将从 WhatPulse 本地 SQLite 数据库（只读、先复制后打开）导入按键/组合/应用/鼠标历史。默认探测 %LOCALAPPDATA%\\WhatPulse\\whatpulse.db，也可在设置页覆盖路径。"
          action={{
            label: "开始导入",
            onClick: () => importMut.mutate(dbPath),
          }}
        />
        {importMut.isPending ? (
          <p className="chart-readout">正在导入（复制快照并重建 wp_* 表）…</p>
        ) : null}
        {importMut.isError ? (
          <p style={{ color: "var(--color-danger)" }}>
            导入失败：{String(importMut.error)}
          </p>
        ) : null}
        {report ? <ImportReportCard report={report} /> : null}
      </>
    );
  }

  const m = meta.data;
  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">WhatPulse</h1>
          <p className="page-desc">
            导入的历史快照 · {fmtDay(m.dateMin ?? m.importedAt)} —{" "}
            {fmtDay(m.dateMax ?? m.importedAt)}
          </p>
        </div>
        <div
          style={{
            display: "flex",
            gap: "var(--space-2)",
            alignItems: "center",
            flexWrap: "wrap",
          }}
        >
          <span className="chip">
            导入于 {fmtDay(m.importedAt.slice(0, 10))}
          </span>
          <button
            type="button"
            className="btn"
            disabled={importMut.isPending}
            onClick={() => importMut.mutate(dbPath)}
          >
            <IconRefresh size={16} />{" "}
            {importMut.isPending ? "导入中…" : "重新导入"}
          </button>
        </div>
      </div>

      {report ? <ImportReportCard report={report} /> : null}

      <div className="wp-tools">
        <DateRangePicker value={range} onChange={setRange} />
        <span className="chip" title={m.sourcePath}>
          独立历史快照
        </span>
      </div>
      {updating ? (
        <p className="chart-readout" role="status">
          历史数据更新中…
        </p>
      ) : null}

      {overview.isLoading ? (
        <SkeletonCards count={4} />
      ) : (
        <div className="grid-cards wp-metrics">
          {/* §4.6：appsTotal 为范围内应用前台秒数总和（非条目数量），fmtDuration 展示 */}
          <StatCard
            label="WhatPulse 按键"
            value={fmtNum(overview.data?.keysTotal ?? 0)}
            tone="primary"
          />
          <StatCard
            label="组合键"
            value={fmtNum(overview.data?.combosTotal ?? 0)}
          />
          <StatCard
            label="应用前台时长"
            value={fmtDuration(overview.data?.appsTotal ?? 0)}
          />
          <StatCard
            label="鼠标点击"
            value={fmtNum(overview.data?.mouseClicksTotal ?? 0)}
            tone="accent"
          />
        </div>
      )}

      {overview.isLoading ? (
        <SkeletonCard rows={5} height="110px" />
      ) : (
        <TrendChart
          data={overview.data?.days ?? []}
          title="WhatPulse 每日按键（所选范围）"
          colorVar="--chart-2"
          seriesName="WhatPulse"
          height={200}
        />
      )}

      {keys.isLoading ? (
        <SkeletonCard rows={5} />
      ) : (
        <RankedList
          title="WhatPulse 按键逐日高频项"
          rows={(keys.data ?? []).map((r) => ({
            id: `${r.day}:${r.qtKey}`,
            label: `${r.label}（${fmtDay(r.day)}）`,
            value: r.count,
          }))}
          unit="次"
          labelHeader="按键"
          valueHeader="累计次数"
          queryLimit={200}
          fullTable={
            <DataTable
              columns={[
                {
                  key: "day",
                  header: "日期",
                  value: (r) => r.day,
                  render: (r) => fmtDay(r.day),
                },
                { key: "label", header: "按键", value: (r) => r.label },
                {
                  key: "qtKey",
                  header: "Qt 键码",
                  value: (r) => r.qtKey,
                  render: (r) => <span className="mono">{r.qtKey}</span>,
                },
                {
                  key: "count",
                  header: "累计次数",
                  value: (r) => r.count,
                  numeric: true,
                  render: (r) => fmtNum(r.count),
                },
              ]}
              rows={keys.data ?? []}
              rowKey={(r) => `${r.day}:${r.qtKey}`}
              initialSort={{ key: "count", dir: "desc" }}
              caption="WhatPulse 按键逐日明细"
              pageSize={50}
              resetKey={`${range.from}|${range.to}`}
            />
          }
        />
      )}

      {combos.isLoading ? (
        <SkeletonCard rows={4} />
      ) : (
        <RankedList
          title="WhatPulse 组合键逐日高频项"
          rows={(combos.data ?? []).map((r) => ({
            id: `${r.day}:${r.combo}`,
            label: `${r.label}（${fmtDay(r.day)}）`,
            value: r.count,
          }))}
          unit="次"
          labelHeader="组合键"
          valueHeader="累计次数"
          queryLimit={200}
          fullTable={
            <DataTable
              columns={[
                {
                  key: "day",
                  header: "日期",
                  value: (r) => r.day,
                  render: (r) => fmtDay(r.day),
                },
                { key: "label", header: "组合键", value: (r) => r.label },
                {
                  key: "combo",
                  header: "源记录",
                  value: (r) => r.combo,
                  render: (r) => <span className="mono">{r.combo}</span>,
                },
                {
                  key: "count",
                  header: "累计次数",
                  value: (r) => r.count,
                  numeric: true,
                  render: (r) => fmtNum(r.count),
                },
              ]}
              rows={combos.data ?? []}
              rowKey={(r) => `${r.day}:${r.combo}`}
              initialSort={{ key: "count", dir: "desc" }}
              caption="WhatPulse 组合键逐日明细"
              pageSize={50}
              resetKey={`${range.from}|${range.to}`}
            />
          }
        />
      )}

      {apps.isLoading ? (
        <SkeletonCard rows={6} />
      ) : (
        <div className="card">
          <h2 className="card-title">应用明细</h2>
          <p className="card-sub">
            按 (日期, 应用) 记录的前台时长与按键、点击；同名不同路径按 path 区分
          </p>
          <div style={{ marginTop: "var(--space-3)" }}>
            <DataTable
              columns={wpAppColumns}
              rows={apps.data ?? []}
              rowKey={(r) => `${r.day}:${r.path}`}
              initialSort={{ key: "seconds", dir: "desc" }}
              caption="WhatPulse 应用明细"
              pageSize={50}
              resetKey={`${range.from}|${range.to}`}
            />
          </div>
        </div>
      )}

      <div className="grid-2">
        {mouse.isLoading ? (
          <SkeletonCard rows={5} />
        ) : (
          <div className="card">
            <h2 className="card-title">鼠标点击与移动</h2>
            <p className="card-sub">历史移动距离（米）</p>
            <div style={{ marginTop: "var(--space-3)" }}>
              <DataTable
                columns={[
                  {
                    key: "day",
                    header: "日期",
                    value: (r) => r.day,
                    render: (r) => fmtDay(r.day),
                  },
                  {
                    key: "clicks",
                    header: "点击",
                    value: (r) => r.clicks,
                    numeric: true,
                    render: (r) => fmtNum(r.clicks),
                  },
                  {
                    key: "meters",
                    header: "移动距离",
                    value: (r) => r.distanceMeters,
                    numeric: true,
                    render: (r) => `${fmtNum(r.distanceMeters)} m`,
                  },
                ]}
                rows={mouse.data ?? []}
                rowKey={(r) => r.day}
                initialSort={{ key: "day", dir: "desc" }}
                caption="WhatPulse 每日鼠标数据"
                pageSize={50}
                resetKey={`${range.from}|${range.to}`}
              />
            </div>
          </div>
        )}
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: "var(--grid-gap)",
          }}
        >
          <div className="card">
            <h2 className="card-title">鼠标按键分布</h2>
            <div style={{ marginTop: "var(--space-2)" }}>
              <DataTable
                columns={[
                  { key: "label", header: "按键", value: (r) => r.label },
                  {
                    key: "total",
                    header: "次数",
                    value: (r) => r.total,
                    numeric: true,
                    render: (r) => fmtNum(r.total),
                  },
                ]}
                rows={buttons.data ?? []}
                rowKey={(r) => r.label}
                initialSort={{ key: "total", dir: "desc" }}
                caption="WhatPulse 鼠标按键分布"
              />
            </div>
          </div>
          <div className="card">
            <h2 className="card-title">滚轮方向分布</h2>
            <div style={{ marginTop: "var(--space-2)" }}>
              <DataTable
                columns={[
                  { key: "label", header: "方向", value: (r) => r.label },
                  {
                    key: "total",
                    header: "次数",
                    value: (r) => r.total,
                    numeric: true,
                    render: (r) => fmtNum(r.total),
                  },
                ]}
                rows={scrolls.data ?? []}
                rowKey={(r) => r.label}
                initialSort={{ key: "total", dir: "desc" }}
                caption="WhatPulse 滚轮方向分布"
              />
            </div>
          </div>
        </div>
      </div>
    </>
  );
}
