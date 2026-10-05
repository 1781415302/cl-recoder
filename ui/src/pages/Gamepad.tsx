// 手柄页（motion-dpi §4.6 固定布局，S8）：现有型号 tabs → 左右摇杆热力并排 → 全宽手柄控件图 → 折叠明细。
//
// 数据接线（§4.5）：摇杆运动 useGamepadMotion（今日 1s/历史无 interval）；按钮 TopKeys 与键盘共享页
// 同键同构（statsPolicy：范围含今日 1s）；逐日明细展开才查询。全部查询复用 uiQueryPolicy
// （enabled 取 AND、meta.uiOwned），跨设备/range 不 keepPreviousData。
// 热力共同色标：页面取左右两图 625 格共 1250 值的 max 作为 scaleMaxSeconds 传两图（不每侧独自缩放）；
// selectedBin 在范围/型号变化清空。没有新运动记录（needs_upgrade 或全零）时说明
// "从更新后的采集器开始记录"，旧按钮仍显示——不把零热度画成采集正常的证据。
// 页面不做聚合（PLAN §2.5）；宽度不足时两热力上下排列（flex wrap）。
import { useMemo, useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as client from "../api/client";
import type { KeyDailyRowLabeled, TopKeyRow } from "../api/types";
import { useDevices, useGamepadMotion } from "../api/queries";
import { uiQueryPolicy } from "../api/queryPolicy";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { DeviceTabs } from "../components/DeviceTabs";
import { EmptyState } from "../components/EmptyState";
import { PeripheralControlsStats } from "../components/PeripheralControlsStats";
import { Skeleton, SkeletonCards } from "../components/Skeleton";
import { StickHeatmap } from "../components/StickHeatmap";
import { IconPlug } from "../components/icons";
import { useAppActivity } from "../lib/AppActivityProvider";
import { useStatisticsRange } from "../lib/useStatisticsRange";
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
function DetailCard(props: { title: string; sub: string; open: boolean; onToggle: () => void; children: ReactNode }) {
  return (
    <div className="card">
      <div style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: "var(--space-2)", flexWrap: "wrap" }}>
        <h2 className="card-title">{props.title}</h2>
        <button type="button" className="btn btn-sm" aria-expanded={props.open} onClick={props.onToggle}>
          {props.open ? "收起" : "展开"}
        </button>
      </div>
      <p className="card-sub">{props.sub}</p>
      {props.open ? props.children : null}
    </div>
  );
}

const topColumns: Column<TopKeyRow>[] = [
  { key: "label", header: "手柄按键", value: (r) => r.label, render: (r) => <span style={{ fontWeight: 600 }}>{r.label}</span> },
  { key: "code", header: "编码", value: (r) => r.code, render: (r) => <span className="mono">{String(r.code)}</span> },
  { key: "total", header: "累计次数", value: (r) => r.total, numeric: true, render: (r) => fmtNum(r.total) },
];

const dailyColumns: Column<KeyDailyRowLabeled>[] = [
  { key: "day", header: "日期", value: (r) => r.day, render: (r) => fmtDay(r.day) },
  { key: "label", header: "手柄按键", value: (r) => r.label },
  { key: "code", header: "编码", value: (r) => r.code, render: (r) => <span className="mono">{String(r.code)}</span> },
  { key: "count", header: "次数", value: (r) => r.count, numeric: true, render: (r) => fmtNum(r.count) },
];

export function Gamepad() {
  const { range, onChange } = useStatisticsRange();
  const activity = useAppActivity();
  const qc = useQueryClient();
  // §4.5：按钮 TopKeys 与键盘共享页同构——统计 interval 仅范围含当前 today 时传入（历史不轮询）
  const includesToday = range.from <= activity.today && activity.today <= range.to;
  const statsPolicy = uiQueryPolicy(activity.active, includesToday ? 1_000 : undefined);

  const devices = useDevices();
  const gamepads = (devices.data ?? []).filter((d) => d.kind === "gamepad");
  const [picked, setPicked] = useState<number | null>(null);
  // 派生选中设备：未点选或所选设备已消失时回落到第一个设备（无 effect，首帧即可发起查询）
  const deviceId =
    picked !== null && gamepads.some((d) => d.id === picked)
      ? picked
      : gamepads[0]?.id ?? null;

  const motion = useGamepadMotion(deviceId, range);
  const topKeys = useQuery({
    queryKey: ["topKeys", deviceId, range.from, range.to, TOP_KEYS_LIMIT],
    queryFn: () => client.getTopKeys(deviceId!, range.from, range.to, TOP_KEYS_LIMIT),
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
  const [binState, setBinState] = useState<{ key: string; left: number | null; right: number | null }>(() => ({
    key: deviceRangeKey,
    left: null,
    right: null,
  }));
  const [codeState, setCodeState] = useState<{ key: string; code: number | null }>(() => ({
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

  const needsUpgrade = motion.data !== undefined && motion.data.availability === "needs_upgrade";
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

  const onSelectCode = (code: number | null) => setCodeState((prev) => ({ key: prev.key, code }));

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">手柄</h1>
          <p className="page-desc">
            摇杆停留热力与按键计数（XInput/Xbox 系；扳机上穿 0.33 计 1 次；非 XInput 手柄如部分 PS/Switch 暂不统计）
          </p>
        </div>
        <DateRangePicker value={range} onChange={onChange} />
      </div>

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
          <DeviceTabs
            devices={gamepads}
            selectedId={deviceId}
            onSelect={setPicked}
            onRenamed={() => void qc.invalidateQueries({ queryKey: ["devices"] })}
          />
          {/* DeviceRow.total 仍为全历史口径，固定说明紧邻设备标签（不改 DeviceTabs） */}
          <p className="chart-hint">设备标签中的次数为全历史累计，不受日期筛选影响；下方按所选日期统计。</p>

          {motion.isError ? (
            <div className="card" role="alert">
              <h2 className="card-title">摇杆运动读取失败</h2>
              <p className="card-sub">{motion.error !== null ? errMsg(motion.error) : "未知错误"}</p>
            </div>
          ) : needsUpgrade ? (
            <div className="card" role="note">
              <h2 className="card-title">从更新后的采集器开始记录</h2>
              <p className="card-sub">
                当前数据库仍是旧 schema：启动/更新采集器后，这里将显示左右摇杆停留热力；下方按键统计不受影响。
              </p>
            </div>
          ) : motion.data === undefined || !activity.ready ? (
            <div style={{ display: "flex", gap: "var(--space-4)", alignItems: "stretch", flexWrap: "wrap" }}>
              <div style={{ flex: "1 1 360px", minWidth: 0 }}>
                <StickSkeleton title="左摇杆停留热力" />
              </div>
              <div style={{ flex: "1 1 360px", minWidth: 0 }}>
                <StickSkeleton title="右摇杆停留热力" />
              </div>
            </div>
          ) : (
            <>
              {/* 左右摇杆并排卡片（宽度不足时上下排列）；scaleMaxSeconds 为两侧共同色标 */}
              <div style={{ display: "flex", gap: "var(--space-4)", alignItems: "stretch", flexWrap: "wrap" }}>
                <div style={{ flex: "1 1 360px", minWidth: 0 }}>
                  <StickHeatmap
                    title="左摇杆停留热力"
                    summary={motion.data.left}
                    selectedBin={binState.left}
                    scaleMaxSeconds={scaleMaxSeconds}
                    onSelect={(bin) => setBinState((prev) => ({ ...prev, left: bin }))}
                  />
                </div>
                <div style={{ flex: "1 1 360px", minWidth: 0 }}>
                  <StickHeatmap
                    title="右摇杆停留热力"
                    summary={motion.data.right}
                    selectedBin={binState.right}
                    scaleMaxSeconds={scaleMaxSeconds}
                    onSelect={(bin) => setBinState((prev) => ({ ...prev, right: bin }))}
                  />
                </div>
              </div>
              {noMotionRecord ? (
                <p className="chart-hint">暂无摇杆运动记录——从更新后的采集器开始记录；下方按键统计不受影响。</p>
              ) : null}
            </>
          )}

          {/* 全宽手柄控件图 */}
          <PeripheralControlsStats
            kind="gamepad"
            rows={topKeys.data ?? []}
            selectedCode={codeState.code}
            onSelect={onSelectCode}
            loading={topKeysLoading}
          />

          {/* 折叠明细：完整列表（无额外查询）+ 逐日明细（展开才查询） */}
          <DetailCard
            title="手柄按键完整列表"
            sub="覆盖全部已计数编码（请求覆盖完整值域，无 Top-N 截断）；可排序、分页查看"
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
                  empty={<EmptyState title="该手柄在所选范围内没有按键记录" description="试试扩大日期范围，或确认采集器已开始统计。" />}
                />
              </div>
            )}
          </DetailCard>
          <DetailCard
            title="手柄 × 逐日明细"
            sub="每个手柄按键每天的按下次数（扳机上穿 0.33 计 1 次）；展开后按需查询"
            open={dailyOpen}
            onToggle={() => setDailyOpen((open) => !open)}
          >
            {keyDaily.isError ? (
              <p className="card-sub" role="alert" style={{ marginTop: "var(--space-3)" }}>
                逐日明细读取失败：{keyDaily.error !== null ? errMsg(keyDaily.error) : "未知错误"}
              </p>
            ) : keyDaily.data === undefined || !activity.ready ? (
              <div role="status" aria-label="逐日明细加载中" style={{ marginTop: "var(--space-3)" }}>
                <Skeleton h="14px" w="92%" />
                <Skeleton h="14px" w="83%" style={{ marginTop: "var(--space-3)" }} />
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
                  empty={<EmptyState title="所选范围内没有逐日数据" description="扩大日期范围后再试。" />}
                />
              </div>
            )}
          </DetailCard>
        </>
      )}
    </>
  );
}
