// 键盘页共享实现（motion-dpi S8 收窄：鼠标/手柄旧支路与旧 80 换算展示已移入
// pages/Mouse.tsx、pages/Gamepad.tsx 的新版页面；本组件仅服务键盘页，键盘合同逐字保留）。
// 数据全部来自 §4.7 命令（get_devices / get_top_keys / get_key_daily），
// 页面不做聚合（PLAN §2.5）。
// §4.5：默认今日（useStatisticsRange）；统计 interval 仅范围含当前 today 时启用；
// keyDaily 仅"逐日明细"展开且 active 时查询；跨设备/范围不用 keepPreviousData（先显示加载态）。
// §4.6/§4.8：TopKeys limit=65536（u16 完整值域，缺席已计数键不画成 0）；物理布局计数
// （DeviceLayoutStats，只做 code 查找）+ 完整表分页（全部返回行可访问）；
// 设备标签次数为全历史累计（DeviceRow.total 不改），紧邻 DeviceTabs 固定说明。
// input-live-trigger S9：分布图经 LiveDeviceControls 接线；移出 devices 加载门，
// 日期变化只改历史查询，不重启实时；historyState 明确传入。
import { useCallback, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as client from "../api/client";
import type { LiveDeviceKey, LiveTarget } from "../api/types";
import { useDevices } from "../api/queries";
import { uiQueryPolicy } from "../api/queryPolicy";
import { DataTable, type Column } from "./DataTable";
import { DateRangePicker } from "./DateRangePicker";
import { DeviceTabs } from "./DeviceTabs";
import { EmptyState } from "./EmptyState";
import { LiveDeviceControls } from "./LiveDeviceControls";
import { SkeletonCard, SkeletonCards } from "./Skeleton";
import { fmtDay, fmtNum, kindLabel } from "../lib/format";
import { sameDevice } from "../lib/liveInputPresentation";
import { useAppActivity } from "../lib/AppActivityProvider";
import { useStatisticsRange } from "../lib/useStatisticsRange";
import { IconPlug } from "./icons";

interface DeviceStatsPageProps {
  /** 键盘页专用（鼠标/手柄已改用各自 motion 页面，见 pages/Mouse.tsx、pages/Gamepad.tsx） */
  kind: "keyboard";
  title: string;
  description: string;
  /** 图表主色（token 名；保留页面入参兼容——布局/排行填充为中性，本组件不再使用） */
  colorVar: string;
}

/** §4.5：设备输入专用 limit——u16 完整值域，返回全部已计数键（不做 Top-N 截断） */
const TOP_KEYS_LIMIT = 65_536;

function goSettings() {
  window.location.hash = "settings";
}

function toLiveKey(d: {
  kind: "keyboard" | "mouse" | "gamepad";
  vid: number;
  pid: number;
  name: string;
}): LiveDeviceKey {
  return { kind: d.kind, vid: d.vid, pid: d.pid, name: d.name };
}

export function DeviceStatsPage({
  kind,
  title,
  description,
}: DeviceStatsPageProps) {
  const { range, onChange } = useStatisticsRange();
  const activity = useAppActivity();
  // §4.5：统计 interval 仅范围包含当前 today 时传入（历史固定范围不轮询）
  const includesToday =
    range.from <= activity.today && activity.today <= range.to;
  const statsPolicy = uiQueryPolicy(
    activity.active,
    includesToday ? 1_000 : undefined,
  );
  const qc = useQueryClient();
  const devices = useDevices();
  const kindDevices = (devices.data ?? []).filter((d) => d.kind === kind);
  const [picked, setPicked] = useState<number | null>(null);
  // 无历史元数据时由 live 四字段低频绑定；不伪造 DB id
  const [liveHint, setLiveHint] = useState<LiveDeviceKey | null>(null);
  const onLiveDeviceResolved = useCallback((device: LiveDeviceKey) => {
    setLiveHint(device);
  }, []);

  // 派生选中设备：用户点选 > live 四字段匹配 > 第一个设备
  const deviceId = useMemo(() => {
    if (picked !== null && kindDevices.some((d) => d.id === picked))
      return picked;
    if (liveHint !== null) {
      const match = kindDevices.find((d) =>
        sameDevice(toLiveKey(d), liveHint),
      );
      if (match) return match.id;
    }
    return kindDevices[0]?.id ?? null;
  }, [picked, kindDevices, liveHint]);

  const selectedRow =
    deviceId !== null
      ? (kindDevices.find((d) => d.id === deviceId) ?? null)
      : null;

  // 稳定实时目标：不依赖 range；无历史型号时 device=null 仍启动 kind 实时
  const liveTarget: LiveTarget = useMemo(
    () => ({
      kind: "keyboard",
      device: selectedRow !== null ? toLiveKey(selectedRow) : null,
    }),
    [selectedRow],
  );

  // 设备/范围语义键：变化时清空布局选择并回完整表/逐日表第一页（§4.8 resetKey 语义）
  const deviceRangeKey = `${deviceId ?? "none"}|${range.from}|${range.to}`;
  // selectedCode 受控值：设备/范围语义变化即清（渲染期派生，不用 effect）
  const [selection, setSelection] = useState<{
    key: string;
    code: number | null;
  }>(() => ({
    key: deviceRangeKey,
    code: null,
  }));
  if (selection.key !== deviceRangeKey) {
    setSelection({ key: deviceRangeKey, code: null });
  }
  // §4.5：逐日明细按需——用户展开才发起查询
  const [dailyOpen, setDailyOpen] = useState(false);

  const topKeys = useQuery({
    queryKey: ["topKeys", deviceId, range.from, range.to, TOP_KEYS_LIMIT],
    queryFn: () =>
      client.getTopKeys(deviceId!, range.from, range.to, TOP_KEYS_LIMIT),
    ...statsPolicy,
    enabled: deviceId !== null && statsPolicy.enabled,
  });
  const keyDaily = useQuery({
    queryKey: ["keyDaily", deviceId, range.from, range.to],
    queryFn: () => client.getKeyDaily(deviceId!, range.from, range.to),
    ...statsPolicy,
    enabled: deviceId !== null && dailyOpen && statsPolicy.enabled,
  });

  // 活动初始化完成前查询被 gating（无凭据不臆断"无数据"），按加载态呈现
  const devicesLoading = devices.isLoading || !activity.ready;
  const topKeysLoading = topKeys.isLoading || !activity.ready;
  const noun = kindLabel(kind);

  // 新接线必须明确传入 historyState；loading/error 不当 0
  const historyState: "loading" | "ready" | "error" = devices.isError
    ? "error"
    : topKeys.isError
      ? "error"
      : devicesLoading || (deviceId !== null && topKeysLoading)
        ? "loading"
        : "ready";

  const topColumns: Column<{ code: number; total: number; label: string }>[] = [
    {
      key: "label",
      header: `${noun}按键`,
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

  const dailyColumns: Column<{
    day: string;
    code: number;
    count: number;
    label: string;
  }>[] = [
    {
      key: "day",
      header: "日期",
      value: (r) => r.day,
      render: (r) => fmtDay(r.day),
    },
    { key: "label", header: `${noun}按键`, value: (r) => r.label },
    {
      key: "code",
      header: "编码",
      value: (r) => r.code,
      render: (r) => (
        <span className="mono">{`0x${r.code.toString(16).toUpperCase()}`}</span>
      ),
    },
    {
      key: "count",
      header: "次数",
      value: (r) => r.count,
      numeric: true,
      render: (r) => fmtNum(r.count),
    },
  ];

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">{title}</h1>
          <p className="page-desc">{description}</p>
        </div>
        <DateRangePicker value={range} onChange={onChange} />
      </div>

      {/* 历史型号 tabs 就绪时放分布图前；分布图本身移出 devices 加载门 */}
      {!devicesLoading && kindDevices.length > 0 ? (
        <DeviceTabs
          devices={kindDevices}
          selectedId={deviceId}
          onSelect={setPicked}
          onRenamed={() =>
            void qc.invalidateQueries({ queryKey: ["devices"] })
          }
        />
      ) : null}

      <LiveDeviceControls
        kind="keyboard"
        target={liveTarget}
        rows={topKeys.data ?? []}
        historyState={historyState}
        selectedCode={selection.code}
        onSelect={(code) =>
          setSelection((prev) => ({ key: prev.key, code }))
        }
        onLiveDeviceResolved={onLiveDeviceResolved}
      />

      {devicesLoading ? (
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
          {topKeysLoading ? null : (
            <details className="card overview-details">
              <summary className="card-title">{noun}完整列表</summary>
              <p className="card-sub">全部按键记录，可排序查看</p>
              <div style={{ marginTop: "var(--space-3)" }}>
                <DataTable
                  columns={topColumns}
                  rows={topKeys.data ?? []}
                  rowKey={(r) => `k${r.code}`}
                  initialSort={{ key: "total", dir: "desc" }}
                  caption={`${noun}按键累计排行（所选范围）`}
                  pageSize={50}
                  resetKey={deviceRangeKey}
                  empty={
                    <EmptyState
                      title="该设备在所选范围内没有按键记录"
                      description="试试扩大日期范围，或确认采集器已开始统计。"
                    />
                  }
                />
              </div>
            </details>
          )}
          <div className="card">
            <div
              style={{
                display: "flex",
                alignItems: "baseline",
                justifyContent: "space-between",
                gap: "var(--space-2)",
                flexWrap: "wrap",
              }}
            >
              <h2 className="card-title">{noun} × 逐日明细</h2>
              <button
                type="button"
                className="btn btn-sm"
                aria-expanded={dailyOpen}
                onClick={() => setDailyOpen((open) => !open)}
              >
                {dailyOpen ? "收起逐日明细" : "展开逐日明细"}
              </button>
            </div>
            <p className="card-sub">每日按键使用次数</p>
            {dailyOpen ? (
              keyDaily.isLoading || !activity.ready ? (
                <SkeletonCard rows={8} />
              ) : (
                <div style={{ marginTop: "var(--space-3)" }}>
                  <DataTable
                    columns={dailyColumns}
                    rows={keyDaily.data ?? []}
                    rowKey={(r) => `${r.day}-${r.code}`}
                    initialSort={{ key: "count", dir: "desc" }}
                    caption={`${noun}按键逐日明细`}
                    pageSize={50}
                    resetKey={deviceRangeKey}
                    empty={
                      <EmptyState
                        title="所选范围内没有逐日数据"
                        description="扩大日期范围后再试。"
                      />
                    }
                  />
                </div>
              )
            ) : null}
          </div>
        </>
      )}
    </>
  );
}
