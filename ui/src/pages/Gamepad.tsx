// 手柄页（motion-dpi §4.6 固定布局，S8 + input-live-trigger S9）：型号 tabs →
// 分布图优先（LiveDeviceControls）→ 摇杆热力 → 扳机历史热力 → 折叠明细。
//
// 数据接线（§4.5）：摇杆运动 useGamepadMotion（今日 1s/历史无 interval）；按钮 TopKeys 与键盘共享页
// 同键同构（statsPolicy：范围含今日 1s）；逐日明细展开才查询。全部查询复用 uiQueryPolicy
// （enabled 取 AND、meta.uiOwned），跨设备/range 不 keepPreviousData。
// 热力共同色标：页面取左右两图 625 格共 1250 值的 max 作为 scaleMaxSeconds 传两图（不每侧独自缩放）；
// selectedBin 在范围/型号变化清空。没有新运动记录（needs_upgrade 或全零）时说明
// "从更新后的采集器开始记录"，旧按钮仍显示——不把零热度画成采集正常的证据。
// 页面不做聚合（PLAN §2.5）；宽度不足时两热力上下排列（flex wrap）。
import { useCallback, useMemo, useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as client from "../api/client";
import type {
  KeyDailyRowLabeled,
  LiveDeviceKey,
  LiveTarget,
  TopKeyRow,
} from "../api/types";
import { useDevices, useGamepadMotion } from "../api/queries";
import { uiQueryPolicy } from "../api/queryPolicy";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { DeviceTabs } from "../components/DeviceTabs";
import { EmptyState } from "../components/EmptyState";
import { LiveDeviceControls } from "../components/LiveDeviceControls";
import { Skeleton, SkeletonCards } from "../components/Skeleton";
import { StickHeatmap } from "../components/StickHeatmap";
import { TriggerHeatmap } from "../components/TriggerHeatmap";
import { IconPlug } from "../components/icons";
import { useAppActivity } from "../lib/AppActivityProvider";
import { useStatisticsRange } from "../lib/useStatisticsRange";
import { sameDevice } from "../lib/liveInputPresentation";
import { fmtDay, fmtNum } from "../lib/format";

/** §4.5：设备输入专用 limit——u16 完整值域（与键盘共享页同键同参，缓存互通） */
const TOP_KEYS_LIMIT = 65_536;

function goSettings() {
  window.location.hash = "settings";
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** 单摇杆骨架卡（StickHeatmap 需 summary 才渲染；加载期用骨架占位，画布高度与热力卡一致） */
function StickSkeleton({ title }: { title: string }) {
  return (
    <div className="card" role="status" aria-label={`${title}加载中`}>
      <h2 className="card-title">{title}</h2>
      <div style={{ marginTop: "var(--space-3)" }}>
        <Skeleton w="60%" h="20px" />
        <Skeleton h="288px" style={{ marginTop: "var(--space-3)" }} />
      </div>
    </div>
  );
}

/** 折叠明细卡（§4.6：表格默认折叠，不删除访问入口；逐日明细的展开同时是查询门控） */
function DetailCard(props: {
  title: string;
  sub: string;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <div className="card detail-card">
      <div className="detail-card-heading">
        <h2 className="card-title">{props.title}</h2>
        <button
          type="button"
          className="btn btn-sm"
          aria-expanded={props.open}
          onClick={props.onToggle}
        >
          {props.open ? "收起" : "展开"}
        </button>
      </div>
      <p className="card-sub">{props.sub}</p>
      {props.open ? props.children : null}
    </div>
  );
}

const topColumns: Column<TopKeyRow>[] = [
  {
    key: "label",
    header: "手柄按键",
    value: (r) => r.label,
    render: (r) => <span style={{ fontWeight: 600 }}>{r.label}</span>,
  },
  {
    key: "code",
    header: "编码",
    value: (r) => r.code,
    render: (r) => <span className="mono">{String(r.code)}</span>,
  },
  {
    key: "total",
    header: "累计次数",
    value: (r) => r.total,
    numeric: true,
    render: (r) => fmtNum(r.total),
  },
];

const dailyColumns: Column<KeyDailyRowLabeled>[] = [
  {
    key: "day",
    header: "日期",
    value: (r) => r.day,
    render: (r) => fmtDay(r.day),
  },
  { key: "label", header: "手柄按键", value: (r) => r.label },
  {
    key: "code",
    header: "编码",
    value: (r) => r.code,
    render: (r) => <span className="mono">{String(r.code)}</span>,
  },
  {
    key: "count",
    header: "次数",
    value: (r) => r.count,
    numeric: true,
    render: (r) => fmtNum(r.count),
  },
];

export function Gamepad() {
  const { range, onChange } = useStatisticsRange();
  const activity = useAppActivity();
  const qc = useQueryClient();
  // §4.5：按钮 TopKeys 与键盘共享页同构——统计 interval 仅范围含当前 today 时传入（历史不轮询）
  const includesToday =
    range.from <= activity.today && activity.today <= range.to;
  const statsPolicy = uiQueryPolicy(
    activity.active,
    includesToday ? 1_000 : undefined,
  );

  const devices = useDevices();
  const gamepads = (devices.data ?? []).filter((d) => d.kind === "gamepad");
  const [picked, setPicked] = useState<number | null>(null);
  // 无历史元数据时由 live 四字段低频绑定；不伪造 DB id
  const [liveHint, setLiveHint] = useState<LiveDeviceKey | null>(null);
  const onLiveDeviceResolved = useCallback((device: LiveDeviceKey) => {
    setLiveHint(device);
  }, []);

  // 派生选中设备：用户点选 > live 四字段匹配 > 第一个设备
  const deviceId = useMemo(() => {
    if (picked !== null && gamepads.some((d) => d.id === picked)) return picked;
    if (liveHint !== null) {
      const match = gamepads.find(
        (d) =>
          sameDevice(
            { kind: d.kind, vid: d.vid, pid: d.pid, name: d.name },
            liveHint,
          ),
      );
      if (match) return match.id;
    }
    return gamepads[0]?.id ?? null;
  }, [picked, gamepads, liveHint]);

  const motion = useGamepadMotion(deviceId, range);
  const topKeys = useQuery({
    queryKey: ["topKeys", deviceId, range.from, range.to, TOP_KEYS_LIMIT],
    queryFn: () =>
      client.getTopKeys(deviceId!, range.from, range.to, TOP_KEYS_LIMIT),
    ...statsPolicy,
    enabled: deviceId !== null && statsPolicy.enabled,
  });

  // 逐日明细：展开才查询（§4.5；与键盘共享页同构的 statsPolicy）
  const [dailyOpen, setDailyOpen] = useState(false);
  const keyDaily = useQuery({
    queryKey: ["keyDaily", deviceId, range.from, range.to],
    queryFn: () => client.getKeyDaily(deviceId!, range.from, range.to),
    ...statsPolicy,
    enabled: deviceId !== null && dailyOpen && statsPolicy.enabled,
  });

  // 设备/范围语义键：selectedBin/selectedCode 变化即清（渲染期派生，同键盘共享页惯例）
  const deviceRangeKey = `${deviceId ?? "none"}|${range.from}|${range.to}`;
  const [binState, setBinState] = useState<{
    key: string;
    left: number | null;
    right: number | null;
  }>(() => ({
    key: deviceRangeKey,
    left: null,
    right: null,
  }));
  const [codeState, setCodeState] = useState<{
    key: string;
    code: number | null;
  }>(() => ({
    key: deviceRangeKey,
    code: null,
  }));
  if (binState.key !== deviceRangeKey) {
    setBinState({ key: deviceRangeKey, left: null, right: null });
  }
  if (codeState.key !== deviceRangeKey) {
    setCodeState({ key: deviceRangeKey, code: null });
  }

  // 折叠明细状态（§4.6：表格默认折叠，不删除访问入口）
  const [listOpen, setListOpen] = useState(false);

  // §4.6：共同色标——左右两图 625 格共 1250 值取 max，两图同一 scaleMaxSeconds（不每侧独自缩放）
  const scaleMaxSeconds = useMemo(() => {
    const data = motion.data;
    if (data === undefined || data.availability !== "ready") return 0;
    let max = 0;
    for (const v of data.left.dwellSeconds) {
      if (Number.isFinite(v) && v > max) max = v;
    }
    for (const v of data.right.dwellSeconds) {
      if (Number.isFinite(v) && v > max) max = v;
    }
    return max;
  }, [motion.data]);

  const needsUpgrade =
    motion.data !== undefined && motion.data.availability === "needs_upgrade";
  // 无新运动记录（ready 但活动/行程全零）：如实说明，不把零热度画成采集正常的证据
  const noMotionRecord =
    motion.data !== undefined &&
    motion.data.availability === "ready" &&
    motion.data.left.activeSeconds === 0 &&
    motion.data.right.activeSeconds === 0 &&
    motion.data.left.travelR === 0 &&
    motion.data.right.travelR === 0;
  const topKeysLoading = topKeys.isLoading || !activity.ready;
  const devicesLoading = devices.isLoading || !activity.ready;

  const onSelectCode = (code: number | null) =>
    setCodeState((prev) => ({ key: prev.key, code }));

  // 扳机热力选中 bin（与摇杆 selectedBin 独立；设备/范围变化随 deviceRangeKey 清）
  const [trigBinState, setTrigBinState] = useState<{
    key: string;
    left: number | null;
    right: number | null;
  }>(() => ({ key: deviceRangeKey, left: null, right: null }));
  if (trigBinState.key !== deviceRangeKey) {
    setTrigBinState({ key: deviceRangeKey, left: null, right: null });
  }

  const selectedRow =
    deviceId !== null
      ? (gamepads.find((d) => d.id === deviceId) ?? null)
      : null;

  const liveTarget: LiveTarget = useMemo(
    () => ({
      kind: "gamepad",
      device:
        selectedRow !== null
          ? {
              kind: "gamepad" as const,
              vid: selectedRow.vid,
              pid: selectedRow.pid,
              name: selectedRow.name,
            }
          : null,
      sourceId: null, // 连接选择归 LiveDeviceControls
    }),
    [selectedRow],
  );

  const historyState: "loading" | "ready" | "error" = devices.isError
    ? "error"
    : topKeys.isError
      ? "error"
      : devicesLoading || (deviceId !== null && topKeysLoading)
        ? "loading"
        : "ready";

  // LT/RT 共同色标（与摇杆 scale 独立）
  const triggerScaleMaxSeconds = useMemo(() => {
    const trig = motion.data?.triggers;
    if (trig === undefined || trig.availability !== "ready") return 0;
    let max = 0;
    for (const v of trig.left.dwellSeconds) {
      if (Number.isFinite(v) && v > max) max = v;
    }
    for (const v of trig.right.dwellSeconds) {
      if (Number.isFinite(v) && v > max) max = v;
    }
    return max;
  }, [motion.data]);

  const triggersNeedsUpgrade =
    motion.data !== undefined &&
    motion.data.triggers.availability === "needs_upgrade";
  const triggersReady =
    motion.data !== undefined &&
    motion.data.triggers.availability === "ready";
  const noTriggerRecord =
    triggersReady &&
    motion.data!.triggers.left.activeSeconds === 0 &&
    motion.data!.triggers.right.activeSeconds === 0;

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">手柄</h1>
          <p className="page-desc">
            摇杆停留分布、运动行程与按键使用 · XInput / Xbox
          </p>
        </div>
        <DateRangePicker value={range} onChange={onChange} />
      </div>

      {!devicesLoading && gamepads.length > 0 ? (
        <DeviceTabs
          devices={gamepads}
          selectedId={deviceId}
          onSelect={setPicked}
          onRenamed={() =>
            void qc.invalidateQueries({ queryKey: ["devices"] })
          }
        />
      ) : null}

      {/* 分布图优先：移出 devices 加载门，连接选择在 LiveDeviceControls */}
      <LiveDeviceControls
        kind="gamepad"
        target={liveTarget}
        rows={topKeys.data ?? []}
        historyState={historyState}
        selectedCode={codeState.code}
        onSelect={onSelectCode}
        onLiveDeviceResolved={onLiveDeviceResolved}
      />

      {devicesLoading ? (
        <SkeletonCards count={2} />
      ) : gamepads.length === 0 ? (
        <EmptyState
          icon={<IconPlug size={36} />}
          title="还没有手柄设备的记录"
          description="采集器启动后会自动识别接入的设备并开始计数。若已连接手柄，请确认采集器正在运行（设置页可启动）。"
          action={{ label: "前往设置", onClick: goSettings }}
        />
      ) : (
        <>
          {motion.isError ? (
            <div className="card" role="alert">
              <h2 className="card-title">摇杆运动读取失败</h2>
              <p className="card-sub">
                {motion.error !== null ? errMsg(motion.error) : "未知错误"}
              </p>
            </div>
          ) : needsUpgrade ? (
            <div className="card" role="note">
              <h2 className="card-title">从更新后的采集器开始记录</h2>
              <p className="card-sub">
                请启动或更新采集器以启用摇杆统计。现有按键统计仍可查看。
              </p>
            </div>
          ) : motion.data === undefined || !activity.ready ? (
            <div className="stick-grid">
              <div style={{ minWidth: 0 }}>
                <StickSkeleton title="左摇杆停留热力" />
              </div>
              <div style={{ minWidth: 0 }}>
                <StickSkeleton title="右摇杆停留热力" />
              </div>
            </div>
          ) : (
            <>
              <div className="stick-grid">
                <div style={{ minWidth: 0 }}>
                  <StickHeatmap
                    title="左摇杆停留热力"
                    summary={motion.data.left}
                    selectedBin={binState.left}
                    scaleMaxSeconds={scaleMaxSeconds}
                    onSelect={(bin) =>
                      setBinState((prev) => ({ ...prev, left: bin }))
                    }
                  />
                </div>
                <div style={{ minWidth: 0 }}>
                  <StickHeatmap
                    title="右摇杆停留热力"
                    summary={motion.data.right}
                    selectedBin={binState.right}
                    scaleMaxSeconds={scaleMaxSeconds}
                    onSelect={(bin) =>
                      setBinState((prev) => ({ ...prev, right: bin }))
                    }
                  />
                </div>
              </div>
              <p className="chart-hint">
                颜色越暖，停留越久；左右使用相同色标。1 R
                为从中心到满幅边缘的行程。
              </p>
              {noMotionRecord ? (
                <p className="chart-hint">
                  暂无摇杆运动记录——从更新后的采集器开始记录；下方按键统计不受影响。
                </p>
              ) : null}
            </>
          )}

          {/* 历史扳机热力（仅历史；与实时扳机填充独立；LT/RT 共同色标） */}
          {motion.isError ? null : triggersNeedsUpgrade ? (
            <div className="card" role="note">
              <h2 className="card-title">扳机热力从新版开始记录</h2>
              <p className="card-sub">
                请启动或更新采集器以启用扳机停留热力。现有按键与摇杆统计仍可查看。
              </p>
            </div>
          ) : motion.data === undefined || !activity.ready ? (
            <div className="stick-grid">
              <div style={{ minWidth: 0 }}>
                <div className="card" role="status" aria-label="左扳机热力加载中">
                  <h2 className="card-title">左扳机停留热力</h2>
                  <Skeleton h="120px" style={{ marginTop: "var(--space-3)" }} />
                </div>
              </div>
              <div style={{ minWidth: 0 }}>
                <div className="card" role="status" aria-label="右扳机热力加载中">
                  <h2 className="card-title">右扳机停留热力</h2>
                  <Skeleton h="120px" style={{ marginTop: "var(--space-3)" }} />
                </div>
              </div>
            </div>
          ) : triggersReady ? (
            <>
              <div className="stick-grid">
                <div style={{ minWidth: 0 }}>
                  <TriggerHeatmap
                    title="左扳机停留热力"
                    summary={motion.data.triggers.left}
                    scaleMaxSeconds={triggerScaleMaxSeconds}
                    selectedBin={trigBinState.left}
                    onSelect={(bin) =>
                      setTrigBinState((prev) => ({ ...prev, left: bin }))
                    }
                  />
                </div>
                <div style={{ minWidth: 0 }}>
                  <TriggerHeatmap
                    title="右扳机停留热力"
                    summary={motion.data.triggers.right}
                    scaleMaxSeconds={triggerScaleMaxSeconds}
                    selectedBin={trigBinState.right}
                    onSelect={(bin) =>
                      setTrigBinState((prev) => ({ ...prev, right: bin }))
                    }
                  />
                </div>
              </div>
              <p className="chart-hint">
                扳机行程 0%–100%；左右使用相同色标。不显示实时游标。
              </p>
              {noTriggerRecord ? (
                <p className="chart-hint">从新版开始记录</p>
              ) : null}
            </>
          ) : null}

          <DetailCard
            title="手柄按键完整列表"
            sub="全部按键记录，可排序查看"
            open={listOpen}
            onToggle={() => setListOpen((open) => !open)}
          >
            {topKeysLoading ? (
              <SkeletonCards count={1} />
            ) : (
              <div style={{ marginTop: "var(--space-3)" }}>
                <DataTable
                  columns={topColumns}
                  rows={topKeys.data ?? []}
                  rowKey={(r) => `k${r.code}`}
                  initialSort={{ key: "total", dir: "desc" }}
                  caption="手柄按键累计排行（所选范围）"
                  pageSize={50}
                  resetKey={deviceRangeKey}
                  empty={
                    <EmptyState
                      title="该手柄在所选范围内没有按键记录"
                      description="试试扩大日期范围，或确认采集器已开始统计。"
                    />
                  }
                />
              </div>
            )}
          </DetailCard>
          <DetailCard
            title="手柄 × 逐日明细"
            sub="每日按键次数 · 扳机超过约三分之一行程计一次"
            open={dailyOpen}
            onToggle={() => setDailyOpen((open) => !open)}
          >
            {keyDaily.isError ? (
              <p
                className="card-sub"
                role="alert"
                style={{ marginTop: "var(--space-3)" }}
              >
                逐日明细读取失败：
                {keyDaily.error !== null ? errMsg(keyDaily.error) : "未知错误"}
              </p>
            ) : keyDaily.data === undefined || !activity.ready ? (
              <div
                role="status"
                aria-label="逐日明细加载中"
                style={{ marginTop: "var(--space-3)" }}
              >
                <Skeleton h="14px" w="92%" />
                <Skeleton
                  h="14px"
                  w="83%"
                  style={{ marginTop: "var(--space-3)" }}
                />
              </div>
            ) : (
              <div style={{ marginTop: "var(--space-3)" }}>
                <DataTable
                  columns={dailyColumns}
                  rows={keyDaily.data}
                  rowKey={(r) => `${r.day}-${r.code}`}
                  initialSort={{ key: "count", dir: "desc" }}
                  caption="手柄按键逐日明细"
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
            )}
          </DetailCard>
        </>
      )}
    </>
  );
}
