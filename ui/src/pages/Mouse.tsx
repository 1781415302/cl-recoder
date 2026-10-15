// 鼠标页（motion-dpi §4.6 固定布局，S8 + input-live-trigger S9）：来源选择 → 3 项概览 →
// 单份 LiveDeviceControls（sourceKey 匹配）+ DPI 编辑侧栏 → 按需明细。分布图移出 sources 加载门。
//
// 数据接线（§4.5）：来源 useMouseSources（活动 2s）、运动 useMouseMotion（今日 1s/历史无 interval）；
// 按钮 TopKeys 与键盘共享页同键同构（statsPolicy：范围含今日 1s）；legacy 单独折叠、展开才查询、
// 无 interval。全部查询复用 uiQueryPolicy（enabled 取 AND、meta.uiOwned），跨 source/range
// 不 keepPreviousData。DPI 配置写成功才使 mouseSources 失效；失败由 MouseDpiEditor 保留输入并展示错误。
// needs_upgrade（旧 schema）在来源选择旁引导"启动/更新采集器后可用"，不阻挡原按钮页——此时仍经
// useDevices 取鼠标型号，型号历史项照常提供按钮/逐日/旧历史（不伪造 source）。
// 初始选择：第一个已连接物理来源 > 已保存来源 > 已有型号；用户选择后不因轮询跳源。
// 页面不做聚合（PLAN §2.5）：运动逐日表复用 summary.days，不额外查询。
import { useCallback, useMemo, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import * as client from "../api/client";
import type {
  KeyDailyRowLabeled,
  LiveDeviceKey,
  LiveTarget,
  MouseMotionDay,
  MouseMotionSummary,
  MouseSourceRow,
  TopKeyRow,
} from "../api/types";
import { useDevices, useMouseMotion, useMouseSources } from "../api/queries";
import { uiQueryPolicy } from "../api/queryPolicy";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { PageHeader } from "../components/PageHeader";
import { EmptyState } from "../components/EmptyState";
import { MouseDpiEditor } from "../components/MouseDpiEditor";
import {
  MotionSummary,
  type MotionSummaryItem,
} from "../components/MotionSummary";
import { MouseSourcePicker } from "../components/MouseSourcePicker";
import { LiveDeviceControls } from "../components/LiveDeviceControls";
import { SkeletonCard, SkeletonCards } from "../components/Skeleton";
import { IconPlug } from "../components/icons";
import { useAppActivity } from "../lib/AppActivityProvider";
import { useStatisticsRange } from "../lib/useStatisticsRange";
import {
  formatMouseDistance,
  type MouseModelRow,
  type MouseSelection,
} from "../lib/motionPresentation";
import { fmtDay, fmtNum } from "../lib/format";

/** §4.5：设备输入专用 limit——u16 完整值域（与键盘共享页同键同参，缓存互通） */
const TOP_KEYS_LIMIT = 65_536;

function goSettings() {
  window.location.hash = "settings";
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** 覆盖率 → 最多 1 位小数的百分比 */
function coverageText(coverage: number): string {
  return `${new Intl.NumberFormat("zh-Hans-CN", { maximumFractionDigits: 1 }).format(coverage * 100)}%`;
}

/** 初始/回落选择（§4.5）：第一个已连接物理来源 > 已保存来源（列表首位）> 已有型号；
 *  用户已选且仍存在时原样保留（轮询不跳源），已选项消失（如 schema 翻转）才按优先级回落。 */
function resolveSelection(
  picked: MouseSelection | null,
  sourceRows: readonly MouseSourceRow[],
  models: readonly MouseModelRow[],
): MouseSelection | null {
  if (picked !== null) {
    if (picked.kind === "source" && sourceRows.some((s) => s.id === picked.id))
      return picked;
    if (picked.kind === "model" && models.some((m) => m.id === picked.id))
      return picked;
  }
  const connected = sourceRows.find((s) => s.physical && s.connected);
  if (connected) return { kind: "source", id: connected.id };
  if (sourceRows.length > 0) return { kind: "source", id: sourceRows[0].id };
  if (models.length > 0) return { kind: "model", id: models[0].id };
  return null;
}

/** 3 项概览（§4.6 固定布局）：估算距离 / 当前 DPI / 原始移动量。
 *  meters=null 显示"未配置/暂无可换算数据"（不用 0 米冒充）；部分覆盖显示"仅含已配置部分"与覆盖率。 */
function overviewItems(
  summary: MouseMotionSummary,
  source: MouseSourceRow,
): MotionSummaryItem[] {
  const partial =
    summary.coverage !== null && summary.coverage < 1
      ? `仅含已配置部分，覆盖率 ${coverageText(summary.coverage)}`
      : undefined;
  return [
    {
      id: "meters",
      label: "估算距离",
      value: formatMouseDistance(summary.meters),
      hint:
        summary.meters !== null
          ? partial
          : summary.rawCounts > 0
            ? "未配置 DPI，暂无可换算数据"
            : "暂无可换算数据",
    },
    {
      id: "dpi",
      label: "当前 DPI",
      value:
        source.effectiveDpi !== null ? String(source.effectiveDpi) : "未配置",
      hint:
        source.dpiOrigin === "auto"
          ? "自动探测生效中"
          : source.dpiOrigin === "manual"
            ? "手动配置"
            : undefined,
    },
    {
      id: "raw",
      label: "原始移动量",
      value: fmtNum(summary.rawCounts),
      hint:
        summary.unconfiguredCounts > 0
          ? `其中 ${fmtNum(summary.unconfiguredCounts)} 未配置 DPI，未计入距离`
          : undefined,
    },
  ];
}

/** needs_upgrade 引导（§4.5）：组件旁提供"启动/更新采集器后可用"，不阻挡原按钮页 */
function UpgradeNote() {
  return (
    <div className="card" role="note">
      <h2 className="card-title">启动/更新采集器后可用</h2>
      <p className="card-sub">
        请启动或更新采集器以启用鼠标运动与 DPI 设置。现有按钮统计仍可查看。
      </p>
    </div>
  );
}

/** 折叠明细卡（§4.6：表格默认折叠，不删除访问入口；逐日/旧历史的展开同时是查询门控） */
function DetailCard(props: {
  title: string;
  sub: string;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <section className="card detail-card">
      <div className="card-head">
        <div>
          <h2 className="card-title">{props.title}</h2>
          <p className="card-sub">{props.sub}</p>
        </div>
        <button
          type="button"
          className="btn btn-sm"
          aria-expanded={props.open}
          onClick={props.onToggle}
        >
          {props.open ? "收起" : "展开"}
        </button>
      </div>
      {props.open ? props.children : null}
    </section>
  );
}

const topColumns: Column<TopKeyRow>[] = [
  {
    key: "label",
    header: "鼠标按键",
    value: (r) => r.label,
    render: (r) => <strong>{r.label}</strong>,
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

const motionDayColumns: Column<MouseMotionDay>[] = [
  {
    key: "day",
    header: "日期",
    value: (r) => r.day,
    render: (r) => fmtDay(r.day),
  },
  {
    key: "rawCounts",
    header: "原始计数",
    value: (r) => r.rawCounts,
    numeric: true,
    render: (r) => fmtNum(r.rawCounts),
  },
  {
    key: "meters",
    header: "估算距离",
    value: (r) => r.meters ?? -1,
    numeric: true,
    render: (r) => formatMouseDistance(r.meters),
  },
  {
    key: "unconfiguredCounts",
    header: "未配置计数",
    value: (r) => r.unconfiguredCounts,
    numeric: true,
    render: (r) => fmtNum(r.unconfiguredCounts),
  },
];

const dailyColumns: Column<KeyDailyRowLabeled>[] = [
  {
    key: "day",
    header: "日期",
    value: (r) => r.day,
    render: (r) => fmtDay(r.day),
  },
  { key: "label", header: "鼠标按键", value: (r) => r.label },
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

export function Mouse() {
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
  const sources = useMouseSources();
  // §4.5：needs_upgrade 时 sources 为空列表；仅 ready 数据参与来源选择与换算
  const sourceRows =
    sources.data?.availability === "ready" ? sources.data.sources : [];
  const mouseModels: MouseModelRow[] = useMemo(
    () =>
      (devices.data ?? [])
        .filter((d) => d.kind === "mouse")
        .map((d) => ({ id: d.id, name: d.name, nickname: d.nickname })),
    [devices.data],
  );

  // 选择（§4.5）：用户点选优先、轮询不跳源；未点选时等来源列表首次解析完成再按优先级派生
  //（避免先落型号、来源到达后又跳源的中间态）
  const [picked, setPicked] = useState<MouseSelection | null>(null);
  const pickerSelection = sources.isPending
    ? null
    : resolveSelection(picked, sourceRows, mouseModels);
  const selectedSource =
    pickerSelection?.kind === "source"
      ? (sourceRows.find((s) => s.id === pickerSelection.id) ?? null)
      : null;
  // 按钮/旧历史按型号（deviceId）查询：来源项用其 deviceId，型号历史项用型号 id
  const deviceId =
    selectedSource !== null
      ? selectedSource.deviceId
      : pickerSelection?.kind === "model"
        ? pickerSelection.id
        : null;

  const motion = useMouseMotion(
    selectedSource !== null ? selectedSource.id : null,
    range,
  );
  const topKeys = useQuery({
    queryKey: ["topKeys", deviceId, range.from, range.to, TOP_KEYS_LIMIT],
    queryFn: () =>
      client.getTopKeys(deviceId!, range.from, range.to, TOP_KEYS_LIMIT),
    ...statsPolicy,
    enabled: deviceId !== null && statsPolicy.enabled,
  });

  // 型号历史逐日明细：展开才查询（§4.5；仅型号历史视图，与键盘共享页同构的 statsPolicy）
  const [modelDailyOpen, setModelDailyOpen] = useState(false);
  const modelKeyDaily = useQuery({
    queryKey: ["keyDaily", deviceId, range.from, range.to],
    queryFn: () => client.getKeyDaily(deviceId!, range.from, range.to),
    ...statsPolicy,
    enabled:
      deviceId !== null &&
      modelDailyOpen &&
      selectedSource === null &&
      statsPolicy.enabled,
  });

  // 旧历史：单独折叠、展开才查询、无 interval（§4.5；不混入新来源主指标）
  const [legacyOpen, setLegacyOpen] = useState(false);
  const legacy = useQuery({
    queryKey: ["mouseLegacy", deviceId, range.from, range.to],
    queryFn: () => client.getMouseLegacy(deviceId!, range.from, range.to),
    ...uiQueryPolicy(activity.active),
    enabled: deviceId !== null && legacyOpen && activity.active,
  });

  // DPI 配置（§4.5）：写成功才使 mouseSources 失效；失败抛给编辑器保留输入并展示错误
  const dpiSave = useMutation({
    mutationFn: (input: { sourceId: number; dpi: number | null }) =>
      client.setMouseDpi(input.sourceId, input.dpi),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["mouseSources"] });
    },
  });
  function saveDpi(dpi: number | null): Promise<void> {
    if (selectedSource === null)
      return Promise.reject(new Error("未选择鼠标来源"));
    return dpiSave
      .mutateAsync({ sourceId: selectedSource.id, dpi })
      .then(() => undefined);
  }

  // 控件图选中编码：设备（型号）/范围语义变化即清（渲染期派生，同键盘共享页惯例）
  const deviceRangeKey = `${deviceId ?? "none"}|${range.from}|${range.to}`;
  const [codeState, setCodeState] = useState<{
    key: string;
    code: number | null;
  }>(() => ({
    key: deviceRangeKey,
    code: null,
  }));
  if (codeState.key !== deviceRangeKey) {
    setCodeState({ key: deviceRangeKey, code: null });
  }

  // 明细折叠状态（§4.6：表格默认折叠，不删除访问入口）
  const [listOpen, setListOpen] = useState(false);
  const [daysOpen, setDaysOpen] = useState(false);

  const needsUpgrade =
    sources.data?.availability === "needs_upgrade" ||
    (motion.data !== undefined && motion.data.availability === "needs_upgrade");
  const topKeysLoading = topKeys.isLoading || !activity.ready;
  const bootLoading = !activity.ready || devices.isLoading || sources.isPending;
  const motionReadyData =
    motion.data?.availability === "ready" ? motion.data : null;
  const selectedModel =
    pickerSelection?.kind === "model"
      ? (mouseModels.find((m) => m.id === pickerSelection.id) ?? null)
      : null;

  const onSelectCode = (code: number | null) =>
    setCodeState((prev) => ({ key: prev.key, code }));

  // 无历史元数据时由 live 四字段低频绑定；不伪造 DB id
  const [liveHint, setLiveHint] = useState<LiveDeviceKey | null>(null);
  const onLiveDeviceResolved = useCallback((device: LiveDeviceKey) => {
    setLiveHint(device);
  }, []);

  // 型号身份从 devices 按 deviceId 取四字段（非 nickname）；物理源按 sourceKey
  const liveDevice: LiveDeviceKey | null = useMemo(() => {
    const all = devices.data ?? [];
    if (selectedSource !== null) {
      const model = all.find((d) => d.id === selectedSource.deviceId);
      if (model)
        return {
          kind: "mouse" as const,
          vid: model.vid,
          pid: model.pid,
          name: model.name,
        };
    }
    if (selectedModel !== null) {
      const model = all.find((d) => d.id === selectedModel.id);
      if (model)
        return {
          kind: "mouse" as const,
          vid: model.vid,
          pid: model.pid,
          name: model.name,
        };
    }
    return null;
  }, [devices.data, selectedSource, selectedModel]);

  const liveTarget: LiveTarget = useMemo(
    () => ({
      kind: "mouse",
      device: liveDevice,
      sourceKey: selectedSource?.sourceKey ?? null,
    }),
    [liveDevice, selectedSource],
  );

  const historyState: "loading" | "ready" | "error" =
    devices.isError || sources.isError || topKeys.isError
      ? "error"
      : bootLoading || (deviceId !== null && topKeysLoading)
        ? "loading"
        : "ready";

  // liveHint 供无历史元数据时的低频绑定（当前 target 已由 picker 派生；保留回调合同）
  void liveHint;

  const liveDiagram = (
    <LiveDeviceControls
      kind="mouse"
      target={liveTarget}
      rows={topKeys.data ?? []}
      historyState={historyState}
      selectedCode={codeState.code}
      onSelect={onSelectCode}
      onLiveDeviceResolved={onLiveDeviceResolved}
    />
  );

  const fullListDetail = (
    <DetailCard
      title="鼠标按键完整列表"
      sub="全部按键记录，可排序查看"
      open={listOpen}
      onToggle={() => setListOpen((open) => !open)}
    >
      {topKeysLoading ? (
        <SkeletonCard rows={6} />
      ) : (
        <div className="card-body">
          <DataTable
            columns={topColumns}
            rows={topKeys.data ?? []}
            rowKey={(r) => `k${r.code}`}
            initialSort={{ key: "total", dir: "desc" }}
            caption="鼠标按键累计排行（所选范围，按型号汇总）"
            pageSize={50}
            resetKey={deviceRangeKey}
            empty={
              <EmptyState
                title="该型号在所选范围内没有按键记录"
                description="试试扩大日期范围，或确认采集器已开始统计。"
              />
            }
          />
        </div>
      )}
    </DetailCard>
  );

  const legacyDetail = (
    <DetailCard
      title="旧历史（旧算法移动量）"
      sub="未经 DPI 校准的历史移动量，不计入距离"
      open={legacyOpen}
      onToggle={() => setLegacyOpen((open) => !open)}
    >
      {legacy.isError ? (
        <p className="card-sub card-body" role="alert">
          旧历史读取失败：
          {legacy.error !== null ? errMsg(legacy.error) : "未知错误"}
        </p>
      ) : legacy.data === undefined || !activity.ready ? (
        <SkeletonCard rows={2} />
      ) : (
        <dl className="metrics card-body">
          <div className="metric">
            <dt className="metric-label">原始移动量（旧算法 · 按型号）</dt>
            <dd className="metric-value num">
              {fmtNum(legacy.data.rawCounts)}
            </dd>
            <dd className="metric-hint">
              质量：legacy_uncalibrated（未校准口径，仅还原原始累计）
            </dd>
          </div>
        </dl>
      )}
    </DetailCard>
  );

  const modelDailyDetail = (
    <DetailCard
      title="鼠标 × 逐日明细"
      sub="每个鼠标按键每天的按下次数（物理按下边沿，自动重复不计）；展开后按需查询"
      open={modelDailyOpen}
      onToggle={() => setModelDailyOpen((open) => !open)}
    >
      {modelKeyDaily.isError ? (
        <p className="card-sub card-body" role="alert">
          逐日明细读取失败：
          {modelKeyDaily.error !== null
            ? errMsg(modelKeyDaily.error)
            : "未知错误"}
        </p>
      ) : modelKeyDaily.data === undefined || !activity.ready ? (
        <SkeletonCard rows={8} />
      ) : (
        <div className="card-body">
          <DataTable
            columns={dailyColumns}
            rows={modelKeyDaily.data}
            rowKey={(r) => `${r.day}-${r.code}`}
            initialSort={{ key: "count", dir: "desc" }}
            caption="鼠标按键逐日明细（按型号汇总）"
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
  );

  return (
    <>
      <PageHeader
        title="鼠标"
        description="查看鼠标移动距离、DPI 与按键统计"
        actions={<DateRangePicker value={range} onChange={onChange} />}
      />
      <div className="page">
        {/* 来源选择：加载中骨架；空列表 EmptyState；就绪后 picker（不阻挡下方实时图） */}
        {bootLoading ? (
          <SkeletonCards count={1} />
        ) : sourceRows.length === 0 && mouseModels.length === 0 ? (
          <EmptyState
            icon={<IconPlug size={36} />}
            title="还没有鼠标设备的记录"
            description="采集器启动后会自动识别接入的设备并开始计数。若已连接鼠标，请确认采集器正在运行（设置页可启动）。"
            action={{ label: "前往设置", onClick: goSettings }}
          />
        ) : (
          <div className="picker-row">
            <div className="picker-main">
              <MouseSourcePicker
                sources={sourceRows}
                models={mouseModels}
                selection={pickerSelection}
                onSelect={setPicked}
              />
            </div>
            {sources.isError ? (
              <div className="card picker-aside" role="alert">
                <h2 className="card-title">鼠标来源读取失败</h2>
                <p className="card-sub">
                  {sources.error !== null ? errMsg(sources.error) : "未知错误"}
                </p>
              </div>
            ) : needsUpgrade ? (
              <div className="picker-aside">
                <UpgradeNote />
              </div>
            ) : null}
          </div>
        )}

        {selectedSource !== null ? (
          motion.isError ? (
            <div className="card" role="alert">
              <h2 className="card-title">运动数据读取失败</h2>
              <p className="card-sub">
                {motion.error !== null ? errMsg(motion.error) : "未知错误"}
              </p>
            </div>
          ) : motion.data === undefined || !activity.ready ? (
            <SkeletonCard rows={2} height="110px" />
          ) : motion.data.availability === "needs_upgrade" ? (
            <UpgradeNote />
          ) : (
            <MotionSummary items={overviewItems(motion.data, selectedSource)} />
          )
        ) : !bootLoading &&
          (sourceRows.length > 0 || mouseModels.length > 0) ? (
          <div className="card" role="note">
            <h2 className="card-title">
              {selectedModel !== null
                ? `${selectedModel.nickname ?? selectedModel.name}（按型号汇总）`
                : "型号历史（按型号汇总）"}
            </h2>
            <p className="card-sub">
              按钮计数与旧历史按型号汇总；运动与 DPI
              按物理来源记录——在上方「运动来源」选择具体来源即可查看。
            </p>
            {needsUpgrade ? (
              <p className="hint">
                按来源记录的运动与 DPI：启动/更新采集器后可用。
              </p>
            ) : null}
          </div>
        ) : null}

        {/* 每页只挂载一份分布图（原两处控件图合并）；DPI 侧栏保留 */}
        <div className="mouse-workspace">
          <div className="cell">
            {liveDiagram}
            <p className="hint">
              按钮计数按型号汇总（同型号来源共享同一型号的按键记录）。
            </p>
          </div>
          {selectedSource !== null ? (
            <div className="cell">
              <MouseDpiEditor
                source={selectedSource}
                pending={dpiSave.isPending}
                onSave={saveDpi}
              />
            </div>
          ) : null}
        </div>

        {!bootLoading && (sourceRows.length > 0 || mouseModels.length > 0) ? (
          selectedSource !== null ? (
            <>
              {fullListDetail}
              {motionReadyData !== null ? (
                <DetailCard
                  title="运动逐日明细"
                  sub="所选来源每天的移动量与距离；未配置 DPI 的部分保留原始量"
                  open={daysOpen}
                  onToggle={() => setDaysOpen((open) => !open)}
                >
                  <div className="card-body">
                    <DataTable
                      columns={motionDayColumns}
                      rows={motionReadyData.days}
                      rowKey={(r) => r.day}
                      initialSort={{ key: "rawCounts", dir: "desc" }}
                      caption="鼠标运动逐日明细（所选来源）"
                      pageSize={50}
                      resetKey={deviceRangeKey}
                      empty={
                        <EmptyState
                          title="所选范围内没有运动记录"
                          description="移动鼠标或扩大日期范围后再试。"
                        />
                      }
                    />
                  </div>
                </DetailCard>
              ) : null}
              {legacyDetail}
            </>
          ) : (
            <>
              {fullListDetail}
              {modelDailyDetail}
              {legacyDetail}
            </>
          )
        ) : null}
      </div>
    </>
  );
}
