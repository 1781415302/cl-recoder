// WhatPulse 页：只读导入的历史数据，独立命名空间展示，绝不与本软件数据混合（R11/设计原则 4）。
import { useState } from "react";
import { useMutation, useQuery, useQueryClient, keepPreviousData } from "@tanstack/react-query";
import * as client from "../api/client";
import { useSettings } from "../api/queries";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { SkeletonCard, SkeletonCards } from "../components/Skeleton";
import { StatCard } from "../components/StatCard";
import { TopBarChart } from "../components/TopBarChart";
import { TrendChart } from "../components/TrendChart";
import { IconImport, IconPulse, IconRefresh, IconWarn } from "../components/icons";
import { defaultRange, fmtDay, fmtDuration, fmtNum } from "../lib/format";
import type { ImportReport, WpAppRow } from "../api/types";

function ImportReportCard({ report }: { report: ImportReport }) {
  return (
    <div className="card" role="status">
      <h2 className="card-title" style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
        <IconImport /> 导入{report.ok ? "成功" : "失败"}
      </h2>
      <p className="card-sub">
        按键 {fmtNum(report.keys)} · 组合 {fmtNum(report.combos)} · 应用 {fmtNum(report.apps)} ·
        鼠标天数 {fmtNum(report.mouseDays)} · 范围 {report.dateMin ?? "?"} ~ {report.dateMax ?? "?"} ·
        耗时 {fmtNum(report.durationMs)} ms
      </p>
      {report.warnings.length > 0 ? (
        <ul style={{ margin: "var(--space-2) 0 0", paddingLeft: "var(--space-4)", color: "var(--color-text-muted)", fontSize: "var(--text-caption)" }}>
          {report.warnings.map((w, i) => (
            <li key={i} style={{ display: "flex", gap: "var(--space-1)", alignItems: "baseline" }}>
              <IconWarn size={14} /> {w}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

const wpAppColumns: Column<WpAppRow>[] = [
  { key: "day", header: "日期", value: (r) => r.day, render: (r) => fmtDay(r.day) },
  { key: "name", header: "应用", value: (r) => r.name },
  { key: "seconds", header: "时长", value: (r) => r.seconds, numeric: true, render: (r) => fmtDuration(r.seconds) },
  { key: "keys", header: "按键", value: (r) => r.keys, numeric: true, render: (r) => fmtNum(r.keys) },
  { key: "clicks", header: "点击", value: (r) => r.clicks, numeric: true, render: (r) => fmtNum(r.clicks) },
];

export function WhatPulse() {
  const [range, setRange] = useState(() => defaultRange(90));
  const qc = useQueryClient();
  const settings = useSettings();
  const meta = useQuery({ queryKey: ["wpMeta"], queryFn: () => client.getWpMeta() });

  const overview = useQuery({
    queryKey: ["wpOverview", range.from, range.to],
    queryFn: () => client.getWpOverview(range.from, range.to),
    placeholderData: keepPreviousData,
  });
  const keys = useQuery({
    queryKey: ["wpKeys", range.from, range.to],
    queryFn: () => client.getWpKeys(range.from, range.to, 200),
    placeholderData: keepPreviousData,
  });
  const combos = useQuery({
    queryKey: ["wpCombos", range.from, range.to],
    queryFn: () => client.getWpCombos(range.from, range.to, 200),
    placeholderData: keepPreviousData,
  });
  const apps = useQuery({
    queryKey: ["wpApps", range.from, range.to],
    queryFn: () => client.getWpApps(range.from, range.to, 200),
    placeholderData: keepPreviousData,
  });
  const mouse = useQuery({
    queryKey: ["wpMouse", range.from, range.to],
    queryFn: () => client.getWpMouse(range.from, range.to),
    placeholderData: keepPreviousData,
  });
  const buttons = useQuery({ queryKey: ["wpButtons"], queryFn: () => client.getWpMouseButtons(range.from, range.to, 20) });
  const scrolls = useQuery({ queryKey: ["wpScrolls"], queryFn: () => client.getWpMouseScrolls(range.from, range.to, 20) });

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

  const dbPath = settings.data?.wpDbPath ?? "C:\\Users\\17814\\AppData\\Local\\WhatPulse\\whatpulse.db";

  if (meta.isLoading) return <SkeletonCard rows={6} />;
  if (!meta.data) {
    return (
      <>
        <div className="page-header">
          <div>
            <h1 className="page-title">WhatPulse</h1>
            <p className="page-desc">只读导入的历史统计，独立命名空间展示，不与本软件数据混合</p>
          </div>
        </div>
        <EmptyState
          icon={<IconPulse size={36} />}
          title="尚未导入 WhatPulse 数据"
          description={`将从 WhatPulse 本地 SQLite 数据库（只读、先复制后打开）导入按键/组合/应用/鼠标历史。默认路径：${dbPath}`}
          action={{ label: "开始导入", onClick: () => importMut.mutate(dbPath) }}
        />
        {importMut.isPending ? <p className="chart-readout">正在导入（复制快照并重建 wp_* 表）…</p> : null}
        {importMut.isError ? <p style={{ color: "var(--color-danger)" }}>导入失败：{String(importMut.error)}</p> : null}
      </>
    );
  }

  const m = meta.data;
  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">WhatPulse</h1>
          <p className="page-desc">导入快照：{fmtDay(m.dateMin ?? m.importedAt)} ~ {fmtDay(m.dateMax ?? m.importedAt)} · 来源 {m.sourcePath}</p>
        </div>
        <div style={{ display: "flex", gap: "var(--space-2)", alignItems: "center", flexWrap: "wrap" }}>
          <span className="chip">导入于 {fmtDay(m.importedAt.slice(0, 10))}</span>
          <button
            type="button"
            className="btn"
            disabled={importMut.isPending}
            onClick={() => importMut.mutate(dbPath)}
          >
            <IconRefresh size={16} /> {importMut.isPending ? "导入中…" : "重新导入"}
          </button>
        </div>
      </div>

      {report ? <ImportReportCard report={report} /> : null}

      <DateRangePicker value={range} onChange={setRange} />

      {overview.isLoading ? (
        <SkeletonCards count={4} />
      ) : (
        <div className="grid-cards">
          <StatCard label="WhatPulse 按键" value={fmtNum(overview.data?.keysTotal ?? 0)} tone="primary" />
          <StatCard label="组合键" value={fmtNum(overview.data?.combosTotal ?? 0)} />
          <StatCard label="应用条目" value={fmtNum(overview.data?.appsTotal ?? 0)} />
          <StatCard label="鼠标点击" value={fmtNum(overview.data?.mouseClicksTotal ?? 0)} tone="accent" />
        </div>
      )}

      {overview.isLoading ? <SkeletonCard rows={5} height="110px" /> : (
        <TrendChart
          data={overview.data?.days ?? []}
          title="WhatPulse 每日按键（所选范围）"
          colorVar="--chart-2"
          seriesName="WhatPulse"
          height={200}
        />
      )}

      {keys.isLoading ? <SkeletonCard rows={5} /> : (
        <TopBarChart
          title="WhatPulse 按键排行"
          rows={(keys.data ?? []).map((r) => ({ label: r.label, value: r.count }))}
          colorVar="--chart-2"
          unit="次"
          labelHeader="按键"
          valueHeader="累计次数"
          fullTable={
            <DataTable
              columns={[
                { key: "label", header: "按键", value: (r) => r.label },
                { key: "count", header: "累计次数", value: (r) => r.count, numeric: true, render: (r) => fmtNum(r.count) },
              ]}
              rows={keys.data ?? []}
              rowKey={(r) => `${r.day}-${r.label}`}
              initialSort={{ key: "count", dir: "desc" }}
              caption="WhatPulse 按键排行"
            />
          }
        />
      )}

      {combos.isLoading ? <SkeletonCard rows={4} /> : (
        <TopBarChart
          title="WhatPulse 组合键排行"
          rows={(combos.data ?? []).map((r) => ({ label: r.label, value: r.count }))}
          colorVar="--chart-5"
          unit="次"
          labelHeader="组合键"
          valueHeader="累计次数"
          fullTable={
            <DataTable
              columns={[
                { key: "label", header: "组合键", value: (r) => r.label },
                { key: "combo", header: "源记录", value: (r) => r.combo, render: (r) => <span className="mono">{r.combo}</span> },
                { key: "count", header: "累计次数", value: (r) => r.count, numeric: true, render: (r) => fmtNum(r.count) },
              ]}
              rows={combos.data ?? []}
              rowKey={(r) => `${r.day}-${r.combo}`}
              initialSort={{ key: "count", dir: "desc" }}
              caption="WhatPulse 组合键排行"
            />
          }
        />
      )}

      {apps.isLoading ? <SkeletonCard rows={6} /> : (
        <div className="card">
          <h2 className="card-title">应用明细</h2>
          <p className="card-sub">按 (日期, 应用) 记录的前台时长与按键、点击</p>
          <div style={{ marginTop: "var(--space-3)" }}>
            <DataTable columns={wpAppColumns} rows={apps.data ?? []} rowKey={(r) => `${r.day}-${r.name}`}
              initialSort={{ key: "seconds", dir: "desc" }} renderLimit={100} caption="WhatPulse 应用明细" />
          </div>
        </div>
      )}

      <div className="grid-2">
        {mouse.isLoading ? <SkeletonCard rows={5} /> : (
          <div className="card">
            <h2 className="card-title">鼠标点击与移动</h2>
            <p className="card-sub">距离由源英寸 × 0.0254 换算为米（GUI 层换算）</p>
            <div style={{ marginTop: "var(--space-3)" }}>
              <DataTable
                columns={[
                  { key: "day", header: "日期", value: (r) => r.day, render: (r) => fmtDay(r.day) },
                  { key: "clicks", header: "点击", value: (r) => r.clicks, numeric: true, render: (r) => fmtNum(r.clicks) },
                  { key: "meters", header: "移动距离", value: (r) => r.distanceMeters, numeric: true, render: (r) => `${fmtNum(r.distanceMeters)} m` },
                ]}
                rows={mouse.data ?? []}
                rowKey={(r) => r.day}
                initialSort={{ key: "day", dir: "desc" }}
                renderLimit={100}
                caption="WhatPulse 每日鼠标数据"
              />
            </div>
          </div>
        )}
        <div style={{ display: "flex", flexDirection: "column", gap: "var(--grid-gap)" }}>
          <div className="card">
            <h2 className="card-title">鼠标按键分布</h2>
            <div style={{ marginTop: "var(--space-2)" }}>
              <DataTable
                columns={[
                  { key: "label", header: "按键", value: (r) => r.label },
                  { key: "total", header: "次数", value: (r) => r.total, numeric: true, render: (r) => fmtNum(r.total) },
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
                  { key: "total", header: "次数", value: (r) => r.total, numeric: true, render: (r) => fmtNum(r.total) },
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
