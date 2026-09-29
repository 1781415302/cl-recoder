// 键盘 / 鼠标 / 手柄三个页面的共享实现（PLAN §3：pages 三页 + DeviceTabs + TopBarChart + DataTable）。
// 数据全部来自 §4.7 命令（get_devices / get_top_keys / get_key_daily），页面不做聚合（PLAN §2.5）。
import { useState } from "react";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import * as client from "../api/client";
import type { DeviceRow, Range } from "../api/types";
import { useDevices } from "../api/queries";
import { DataTable, type Column } from "./DataTable";
import { DateRangePicker } from "./DateRangePicker";
import { DeviceTabs } from "./DeviceTabs";
import { EmptyState } from "./EmptyState";
import { TopBarChart } from "./TopBarChart";
import { SkeletonCard, SkeletonCards } from "./Skeleton";
import { fmtDay, fmtNum, defaultRange, kindLabel } from "../lib/format";
import { IconPlug } from "./icons";

interface DeviceStatsPageProps {
  kind: DeviceRow["kind"];
  title: string;
  description: string;
  /** 图表主色（token 名） */
  colorVar: string;
  /** 鼠标页：附带移动距离卡片 */
  showMouseDistance?: boolean;
}

const INCH_TO_M = 0.0254;

function fmtMeters(inches: number): string {
  const m = inches * INCH_TO_M;
  if (m >= 1000) return `${fmtNum(Math.round(m / 100) / 10)} km`;
  return `${fmtNum(Math.round(m * 10) / 10)} m`;
}

function goSettings() {
  window.location.hash = "settings";
}

export function DeviceStatsPage({ kind, title, description, colorVar, showMouseDistance }: DeviceStatsPageProps) {
  const [range, setRange] = useState<Range>(() => defaultRange(30));
  const qc = useQueryClient();
  const devices = useDevices();
  const kindDevices = (devices.data ?? []).filter((d) => d.kind === kind);
  const [picked, setPicked] = useState<number | null>(null);
  // 派生选中设备：未点选或所选设备已消失时回落到第一个设备（无 effect，首帧即可发起查询）
  const deviceId =
    picked !== null && kindDevices.some((d) => d.id === picked)
      ? picked
      : kindDevices[0]?.id ?? null;

  const topKeys = useQuery({
    queryKey: ["topKeys", deviceId, range.from, range.to],
    queryFn: () => client.getTopKeys(deviceId!, range.from, range.to, 200),
    enabled: deviceId !== null,
    placeholderData: keepPreviousData,
    refetchInterval: 1_000,
    refetchIntervalInBackground: true,
    staleTime: 0,
  });
  const keyDaily = useQuery({
    queryKey: ["keyDaily", deviceId, range.from, range.to],
    queryFn: () => client.getKeyDaily(deviceId!, range.from, range.to),
    enabled: deviceId !== null,
    placeholderData: keepPreviousData,
    refetchInterval: 1_000,
    refetchIntervalInBackground: true,
    staleTime: 0,
  });
  const mouseDist = useQuery({
    queryKey: ["mouseDistance", range.from, range.to],
    queryFn: () => client.getMouseDistance(range.from, range.to),
    enabled: !!showMouseDistance,
    placeholderData: keepPreviousData,
    refetchInterval: 1_000,
    refetchIntervalInBackground: true,
    staleTime: 0,
  });

  const noun = kindLabel(kind);

  const topColumns: Column<{ code: number; total: number; label: string }>[] = [
    { key: "label", header: `${noun}按键`, value: (r) => r.label, render: (r) => <span style={{ fontWeight: 600 }}>{r.label}</span> },
    { key: "code", header: "编码", value: (r) => r.code, render: (r) => <span className="mono">{`0x${r.code.toString(16).toUpperCase()}`}</span> },
    { key: "total", header: "累计次数", value: (r) => r.total, numeric: true, render: (r) => fmtNum(r.total) },
  ];

  const dailyColumns: Column<{ day: string; code: number; count: number; label: string }>[] = [
    { key: "day", header: "日期", value: (r) => r.day, render: (r) => fmtDay(r.day) },
    { key: "label", header: `${noun}按键`, value: (r) => r.label },
    { key: "code", header: "编码", value: (r) => r.code, render: (r) => <span className="mono">{`0x${r.code.toString(16).toUpperCase()}`}</span> },
    { key: "count", header: "次数", value: (r) => r.count, numeric: true, render: (r) => fmtNum(r.count) },
  ];

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">{title}</h1>
          <p className="page-desc">{description}</p>
        </div>
        <DateRangePicker value={range} onChange={setRange} />
      </div>

      {devices.isLoading ? (
        <SkeletonCards count={2} />
      ) : kindDevices.length === 0 ? (
        <EmptyState
          icon={<IconPlug size={36} />}
          title={`还没有${noun}设备的记录`}
          description="采集器启动后会自动识别接入的设备并开始计数。若已连接设备，请确认采集器正在运行（设置页可启动）。"
          action={{ label: "前往设置", onClick: goSettings }}
        />
      ) : (
        <>
          {showMouseDistance ? (
            <div className="card">
              <h2 className="card-title">鼠标移动距离</h2>
              <p className="card-sub">按设备累计相对位移（约 80 counts/inch 折算，与 WhatPulse 近似同口径）</p>
              <div style={{ display: "flex", gap: "var(--space-4)", flexWrap: "wrap", marginTop: "var(--space-2)" }}>
                <div>
                  <div style={{ fontSize: "var(--text-caption)", color: "var(--color-text-muted)" }}>范围内总距离</div>
                  <div className="num" style={{ fontSize: 28, fontWeight: 700 }}>
                    {fmtMeters(mouseDist.data?.total_inches ?? 0)}
                  </div>
                </div>
                <div>
                  <div style={{ fontSize: "var(--text-caption)", color: "var(--color-text-muted)" }}>英寸原值</div>
                  <div className="num" style={{ fontSize: 20, fontWeight: 600 }}>
                    {fmtNum(Math.round(mouseDist.data?.total_inches ?? 0))} in
                  </div>
                </div>
              </div>
            </div>
          ) : null}
          <DeviceTabs
            devices={kindDevices}
            selectedId={deviceId}
            onSelect={setPicked}
            onRenamed={() => void qc.invalidateQueries({ queryKey: ["devices"] })}
          />
          {topKeys.isLoading ? (
            <SkeletonCard rows={6} />
          ) : (
            <TopBarChart
              title={`${noun}按键排行`}
              rows={(topKeys.data ?? []).map((r) => ({ label: r.label, value: r.total }))}
              colorVar={colorVar}
              unit="次"
              labelHeader={noun}
              valueHeader="累计次数"
              fullTable={
                <DataTable
                  columns={topColumns}
                  rows={topKeys.data ?? []}
                  rowKey={(r) => `k${r.code}`}
                  initialSort={{ key: "total", dir: "desc" }}
                  caption={`${noun}按键累计排行（所选范围）`}
                  empty={<EmptyState title="该设备在所选范围内没有按键记录" description="试试扩大日期范围，或确认采集器已开始统计。" />}
                />
              }
            />
          )}
          {keyDaily.isLoading ? (
            <SkeletonCard rows={8} />
          ) : (
            <div className="card">
              <h2 className="card-title">{noun} × 逐日明细</h2>
              <p className="card-sub">每个{noun}按键每天的按下次数（物理按下边沿，自动重复不计）</p>
              <div style={{ marginTop: "var(--space-3)" }}>
                <DataTable
                  columns={dailyColumns}
                  rows={keyDaily.data ?? []}
                  rowKey={(r) => `${r.day}-${r.code}`}
                  initialSort={{ key: "count", dir: "desc" }}
                  renderLimit={100}
                  caption={`${noun}按键逐日明细`}
                  empty={<EmptyState title="所选范围内没有逐日数据" description="扩大日期范围后再试。" />}
                />
              </div>
            </div>
          )}
        </>
      )}
    </>
  );
}
