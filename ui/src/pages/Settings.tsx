// 设置页：采集器健康状态（§4.2 按 health 分类显示，不以 running=false 一律"未运行"）+
// 控制（暂停/立即启动）+ 自启任务三项策略只读展示与「修复采集器自启」（§4.3：一次 UAC、
// RepairOnly、对后续启动生效）+ 诊断日志目录（§4.1）+ GUI 自启 + WhatPulse 数据源 +
// 数据导出（§5.5，默认仍 90 天）。查询全部经 uiQueryPolicy 做 activity gating（§4.5）。
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { save } from "@tauri-apps/plugin-dialog";
import * as client from "../api/client";
import { uiQueryPolicy } from "../api/queryPolicy";
import { useUpdateStatus } from "../api/queries";
import { useAppActivity } from "../lib/AppActivityProvider";
import { DateRangePicker } from "../components/DateRangePicker";
import { PageHeader } from "../components/PageHeader";
import { EmptyState } from "../components/EmptyState";
import {
  IconCheck,
  IconDatabase,
  IconDownload,
  IconPause,
  IconPlay,
  IconPlug,
  IconRefresh,
  IconWarn,
} from "../components/icons";
import { defaultRange, fmtNum } from "../lib/format";
import {
  collectorActionBusyState,
  deriveUpdateFlow,
  shouldPollUpdateStatus,
  shouldResumeInstallPolling,
} from "../lib/updateFlow";
import type {
  ExportReport,
  Range,
  UpdateCheck,
  UpdateStatus,
} from "../api/types";

const inTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** 启动/启用类动作的进行中提示（涉及时长承诺与 UAC） */
const PENDING_START = "正在确保采集器运行…（启动最多约 10 秒，请勿连点）";
const PENDING_ENABLE =
  "正在启用自启并启动采集器…（需一次 UAC 授权，请在弹窗中确认）";
const PENDING_REPAIR =
  "正在修复自启任务策略…（需一次 UAC 授权，请在弹窗中确认）";

/** 修复成功提示（§5.2 核心流程逐字）：仅表示定义校验通过，不表示当前实例已重启 */
const REPAIR_OK = "策略已更新，对后续启动生效；当前采集器未重启";

/** 诊断态（unreachable/access_denied/unknown）展示文案：不宣称"未运行"，给重试与日志入口 */
const DIAG_COPY: Record<
  "unreachable" | "access_denied" | "unknown",
  { title: string; desc: string }
> = {
  unreachable: {
    title: "采集器进程存在，但控制管道无响应",
    desc: "点「立即启动」等待其就绪（不会结束或重复拉起进程）；仍失败请查看诊断日志。",
  },
  access_denied: {
    title: "控制管道访问被拒绝",
    desc: "当前用户可能无权访问控制管道；可查看诊断日志，或点「立即启动」重试。",
  },
  unknown: {
    title: "暂时无法确认采集器状态",
    desc: "探测证据不足，不能断定未运行；活动期间会自动重试，也可点「立即启动」。",
  },
};

/** 任务策略执行时限展示：PT0S = 无限制；其余原样展示（XML 值不翻译），null = 未知 */
function fmtExecutionTimeLimit(v: string | null): string {
  return v === null ? "未知" : v === "PT0S" ? "无限制" : v;
}

/** 任务策略布尔项展示：null = 未知（不臆断） */
function fmtPolicyBool(v: boolean | null): string {
  return v === null ? "未知" : v ? "是" : "否";
}

function Row({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="settings-row">
      <div className="settings-row-copy">
        <div className="settings-row-title">{label}</div>
        {hint && <div className="settings-row-hint">{hint}</div>}
      </div>
      <div className="settings-row-actions">{children}</div>
    </div>
  );
}

/** 软件更新卡片（§4.8）：状态由 updateFlow 纯函数给出，本组件只渲染与发命令 */
function UpdateCard({
  onPageBusy,
}: {
  onPageBusy: (busy: boolean) => void;
}) {
  const [currentVersion, setCurrentVersion] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [check, setCheck] = useState<UpdateCheck | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [installSubmitted, setInstallSubmitted] = useState(false);
  const [lastStatus, setLastStatus] = useState<UpdateStatus | null>(null);

  useEffect(() => {
    let cancelled = false;
    void client.getAppVersion().then(
      (v) => {
        if (!cancelled) setCurrentVersion(v);
      },
      () => {
        // 失败时显示「未知」，不传假版本（P1-2a）
        if (!cancelled) setCurrentVersion(null);
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  // 挂载时拉一次 status：非 idle 则恢复 installSubmitted 与轮询（P1-2b）
  useEffect(() => {
    let cancelled = false;
    void client.updateStatus().then(
      (st) => {
        if (cancelled) return;
        setLastStatus(st);
        if (shouldResumeInstallPolling(st)) {
          setInstallSubmitted(true);
        }
      },
      () => {
        /* 忽略：无后端/失败时保持 idle */
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  const poll = shouldPollUpdateStatus(installSubmitted, lastStatus);
  const statusQuery = useUpdateStatus(poll);
  useEffect(() => {
    if (statusQuery.data) setLastStatus(statusQuery.data);
  }, [statusQuery.data]);
  // 未在轮询时仍展示最后一次状态（失败结果）
  const status = statusQuery.data ?? lastStatus;

  const view = deriveUpdateFlow({
    currentVersion,
    checking,
    checkError,
    check,
    confirming,
    installSubmitted,
    status,
  });

  useEffect(() => {
    onPageBusy(view.pageBusy);
  }, [view.pageBusy, onPageBusy]);

  async function doCheck() {
    setChecking(true);
    setCheckError(null);
    setConfirming(false);
    try {
      const result = await client.updateCheck();
      setCheck(result);
    } catch (e: unknown) {
      setCheckError(String(e));
    } finally {
      setChecking(false);
    }
  }

  async function doConfirmInstall() {
    if (!check?.latestVersion) return;
    setConfirming(false);
    setLastStatus(null);
    setInstallSubmitted(true);
    try {
      await client.updateInstall(check.latestVersion);
    } catch (e: unknown) {
      setInstallSubmitted(false);
      setCheckError(String(e));
    }
  }

  function doRetry() {
    setInstallSubmitted(false);
    setLastStatus(null);
    setConfirming(false);
    setCheckError(null);
    void doCheck();
  }

  return (
    <section className="card settings-section" id="settings-update">
      <div className="card-head">
        <div>
          <h2 className="card-title">软件更新</h2>
          <p className="card-sub">检查 GitHub Release 并安装（仅安装版）</p>
        </div>
      </div>
      <div className="update-card" aria-live="polite">
        <div className="update-headline">{view.headline}</div>
        {view.detail ? (
          <div className="update-detail">{view.detail}</div>
        ) : null}
        {view.notes !== null ? (
          <pre className="update-notes">{view.notes}</pre>
        ) : null}
        {view.confirmPoints ? (
          <div className="update-confirm">
            <div className="update-confirm-title">确认更新前请了解：</div>
            <ol className="update-confirm-list">
              {view.confirmPoints.map((p) => (
                <li key={p}>{p}</li>
              ))}
            </ol>
          </div>
        ) : null}
        {view.phaseLabel && view.mode === "in_progress" ? (
          <div className="update-phase">{view.phaseLabel}</div>
        ) : null}
        {view.progress ? (
          <progress
            className="update-progress"
            value={
              view.progress.kind === "determinate"
                ? view.progress.value
                : undefined
            }
            max={
              view.progress.kind === "determinate" ? view.progress.max : undefined
            }
          />
        ) : null}
        {view.failureMessage ? (
          <div className="update-failure" role="alert">
            {view.failureMessage}
          </div>
        ) : null}
        {view.collectorManualHint ? (
          <div className="update-manual-hint" role="alert">
            {view.collectorManualHint}
          </div>
        ) : null}
        <div className="btn-row update-actions">
          {view.buttons.check ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={view.allDisabled}
              onClick={() => void doCheck()}
            >
              检查更新
            </button>
          ) : null}
          {view.buttons.recheck ? (
            <button
              type="button"
              className="btn"
              disabled={view.allDisabled}
              onClick={() => void doCheck()}
            >
              重新检查
            </button>
          ) : null}
          {view.buttons.download ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={view.allDisabled}
              onClick={() => setConfirming(true)}
            >
              下载并更新
            </button>
          ) : null}
          {view.buttons.confirm ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={view.allDisabled}
              onClick={() => void doConfirmInstall()}
            >
              确认并更新
            </button>
          ) : null}
          {view.buttons.cancel ? (
            <button
              type="button"
              className="btn"
              disabled={view.allDisabled}
              onClick={() => setConfirming(false)}
            >
              取消
            </button>
          ) : null}
          {view.buttons.retry ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={view.allDisabled}
              onClick={() => doRetry()}
            >
              重试
            </button>
          ) : null}
          {view.buttons.openRelease && view.releaseUrl ? (
            <button
              type="button"
              className="btn"
              disabled={view.allDisabled}
              onClick={() => void client.openReleaseUrl(view.releaseUrl!)}
            >
              前往发布页
            </button>
          ) : null}
        </div>
      </div>
    </section>
  );
}

function ExportCard() {
  const [range, setRange] = useState<Range>(() => defaultRange(90));
  const [format, setFormat] = useState<"csv" | "json">("csv");
  const [scope, setScope] = useState<"own" | "wp">("own");
  const [result, setResult] = useState<ExportReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function doExport() {
    setBusy(true);
    setError(null);
    try {
      let path = `clrecoder-export-${Date.now()}`;
      if (inTauri) {
        const picked = await save({
          defaultPath: `clrecoder_${scope}_${range.from}_${range.to}.${format}`,
          filters: [{ name: format.toUpperCase(), extensions: [format] }],
        });
        if (!picked) return; // 用户取消
        path = picked;
      }
      const r = await client.exportData(
        format,
        scope,
        range.from,
        range.to,
        path,
      );
      setResult(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="card settings-section" id="settings-export">
      <div className="card-head">
        <div>
          <h2 className="card-title with-icon">
            <IconDownload /> 数据导出
          </h2>
          <p className="card-sub">CSV 可用 Excel 打开；JSON 包含完整导出数据</p>
        </div>
      </div>
      <div className="settings-rows">
        <Row label="格式">
          <div className="seg" role="group" aria-label="导出格式">
            {(["csv", "json"] as const).map((f) => (
              <button
                key={f}
                type="button"
                className="seg-btn"
                aria-pressed={format === f}
                onClick={() => setFormat(f)}
              >
                {f.toUpperCase()}
              </button>
            ))}
          </div>
        </Row>
        <Row label="范围数据源" hint="本软件统计或导入的历史记录">
          <div className="seg" role="group" aria-label="数据源">
            {(["own", "wp"] as const).map((s) => (
              <button
                key={s}
                type="button"
                className="seg-btn"
                aria-pressed={scope === s}
                onClick={() => setScope(s)}
              >
                {s === "own" ? "本软件" : "WhatPulse"}
              </button>
            ))}
          </div>
        </Row>
        <Row label="日期范围">
          <DateRangePicker value={range} onChange={setRange} />
        </Row>
      </div>
      <div className="card-foot-actions">
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy}
          onClick={() => void doExport()}
        >
          {busy ? "导出中…" : "选择位置并导出"}
        </button>
        {error ? (
          <span className="field-error" role="alert">
            {error}
          </span>
        ) : null}
      </div>
      {result ? (
        <p className="readout" role="status">
          已导出 {fmtNum(result.rows)} 行 → {result.files.join("、")}
        </p>
      ) : null}
    </section>
  );
}

/** 诊断日志卡片（§4.1）：目录展示与打开；打开按钮不依赖 diagnosticsInfo 查询结果，
 * 读取失败时仍可打开/重试（入口不被 disabled 数据查询挡住）。 */
function DiagnosticsCard({
  busy,
  onNotice,
}: {
  busy: boolean;
  onNotice: (msg: string) => void;
}) {
  const activity = useAppActivity();
  const diagnostics = useQuery({
    queryKey: ["diagnosticsInfo"],
    queryFn: () => client.getDiagnosticsInfo(),
    ...uiQueryPolicy(activity.active),
  });
  const info = diagnostics.data;

  return (
    <section className="card settings-section" id="settings-diagnostics">
      <div className="card-head">
        <div>
          <h2 className="card-title with-icon">
            <IconWarn /> 诊断日志
          </h2>
          <p className="card-sub">遇到问题时，可打开日志查看详情</p>
        </div>
      </div>
      <div className="settings-rows">
        <Row
          label="日志目录"
          hint={
            info
              ? info.guiLoggingAvailable
                ? "GUI 日志可写；collector 日志由采集进程写入同一目录"
                : "GUI 日志当前不可用（stderr 降级）"
              : undefined
          }
        >
          <span className="mono path-text">
            {info
              ? (info.logDirectory ?? "未知")
              : diagnostics.isError
                ? "读取失败"
                : "读取中…"}
          </span>
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() => {
              void client.openDiagnosticsDirectory().then(
                () => onNotice("已打开日志目录"),
                (e: unknown) => onNotice(`操作失败：${String(e)}`),
              );
            }}
          >
            打开日志目录
          </button>
        </Row>
      </div>
      {info?.lastError ? (
        <p className="field-error">最近日志错误：{info.lastError}</p>
      ) : null}
      {info ? (
        <p className="hint">
          两角色日志文件合计上限约{" "}
          {fmtNum(Math.round(info.maxTotalBytes / 1024 / 1024))} MB
        </p>
      ) : null}
    </section>
  );
}

export function Settings() {
  const qc = useQueryClient();
  const activity = useAppActivity();
  // §4.5：全部查询 activity gating。状态与 Sidebar 共用 ["collectorStatus"] 缓存——
  // Sidebar 常驻并负责 500ms 轮询，本页不再另起定时器；设置/策略/日志不加 interval。
  const status = useQuery({
    queryKey: ["collectorStatus"],
    queryFn: () => client.collectorStatus(),
    ...uiQueryPolicy(activity.active),
  });
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: () => client.getSettings(),
    ...uiQueryPolicy(activity.active),
  });
  const taskPolicy = useQuery({
    queryKey: ["collectorTaskPolicy"],
    queryFn: () => client.getCollectorTaskPolicy(),
    ...uiQueryPolicy(activity.active),
  });
  // hardening-v4 §4.2.1：旧安装迁移提示（只读；仅 Settings 活动时查询，不加 interval）
  const legacyHints = useQuery({
    queryKey: ["legacyInstallHints"],
    queryFn: () => client.getLegacyInstallHints(),
    ...uiQueryPolicy(activity.active),
  });
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [wpPathDraft, setWpPathDraft] = useState<string | null>(null);
  /** 更新进行中时同步禁用采集器操作按钮（§4.8）；文案只看 busy（P1-2c） */
  const controlsBusy = busy || updateBusy;
  const actionBusy = collectorActionBusyState(busy, updateBusy);

  function after(
    promise: Promise<unknown>,
    okMsg: string,
    opts: { pending?: string; invalidate?: readonly unknown[][] } = {},
  ) {
    setBusy(true);
    setNotice(opts.pending ?? "处理中，请稍候…");
    promise.then(
      () => {
        setBusy(false);
        setNotice(okMsg);
        if (opts.invalidate) {
          for (const key of opts.invalidate)
            void qc.invalidateQueries({ queryKey: key });
        } else {
          void qc.invalidateQueries();
        }
      },
      (e: unknown) => {
        setBusy(false);
        setNotice(`操作失败：${String(e)}`);
      },
    );
  }

  const st = status.data;
  const s = settings.data;
  const policy = taskPolicy.data;
  const health = st?.health ?? "unknown";
  const taskOn = st?.taskExists ?? false;
  // 修复按钮的可用性：任务"存在"需有证据（status 或 policy 任一探到任务即可），
  // 且三项策略未确认合规；absent/unknown 不臆断，也不误报。
  const taskPresent = st
    ? st.taskEvidence === "present"
    : policy?.evidence === "present";
  const policyCompliant =
    policy?.evidence === "present" && policy.compliant === true;
  const showRepair = taskPresent && !policyCompliant;
  // 任务三项策略（§4.3 只读）展示状态：仅 evidence=present 时展示字段值，其余占位不臆断
  const policyState: "loading" | "error" | "absent" | "unknown" | "present" =
    taskPolicy.isPending
      ? "loading"
      : taskPolicy.isError
        ? "error"
        : policy === undefined
          ? "loading"
          : policy.evidence === "absent"
            ? "absent"
            : policy.evidence === "unknown"
              ? "unknown"
              : "present";
  const policyReady = policyState === "present";
  const policySummary =
    policyState === "loading"
      ? "读取中…"
      : policyState === "error"
        ? `读取失败：${String(taskPolicy.error)}`
        : policyState === "absent"
          ? "未检测到自启任务"
          : policyState === "unknown"
            ? "无法确认（证据不足）"
            : policy!.compliant === true
              ? "已符合推荐策略"
              : policy!.compliant === false
                ? "与推荐策略不一致，可修复"
                : "已读取任务定义，合规状态未知";
  const policyTone =
    policyState === "present" && policy!.compliant === true
      ? "ok"
      : policyState === "present" && policy!.compliant === false
        ? "bad"
        : "muted";

  function repairPolicy() {
    // §5.2：一次 UAC、RepairOnly、只改三项并回读；结果刷新 taskPolicy 与 task 缓存，
    // 不进入启动流程。成功仅表示定义校验通过，当前实例不重启。
    after(client.collectorAutostartRepair(), REPAIR_OK, {
      pending: PENDING_REPAIR,
      invalidate: [
        ["collectorTaskPolicy"],
        ["collectorStatus"],
        ["legacyInstallHints"],
      ],
    });
  }

  const wpPath = wpPathDraft ?? s?.wpDbPath ?? "";

  const collectorHead = (
    <h2 className="card-title with-icon">
      <IconPlug /> 采集器
    </h2>
  );

  return (
    <>
      <PageHeader title="设置" description="管理采集、启动方式与本地数据" />
      <div className="page">
        <div className="settings-layout">
          <nav className="settings-index" aria-label="设置分区">
            {[
              ["settings-collector", "采集器"],
              ["settings-autostart", "启动与运行"],
              ["settings-update", "软件更新"],
              ["settings-source", "历史数据源"],
              ["settings-diagnostics", "诊断日志"],
              ["settings-export", "数据导出"],
              ["settings-about", "关于"],
            ].map(([id, label]) => (
              <button
                type="button"
                key={id}
                onClick={() =>
                  document.getElementById(id)?.scrollIntoView({
                    behavior: window.matchMedia(
                      "(prefers-reduced-motion: reduce)",
                    ).matches
                      ? "auto"
                      : "smooth",
                    block: "start",
                  })
                }
              >
                {label}
              </button>
            ))}
          </nav>
          <div className="settings-content">
            {notice ? (
              <p className="readout notice" role="status">
                {notice}
              </p>
            ) : null}

            <section id="settings-collector" className="settings-section">
              {status.isError && !st ? (
                // 检测失败：重试入口直接 refetch，不依赖查询是否被 gating 禁用（§5.2）
                <EmptyState
                  icon={<IconRefresh size={36} />}
                  title="采集器状态检测失败"
                  description={`检测采集器健康状态时出错：${String(status.error)}。可重新检测；下方的日志入口不受影响。`}
                  action={{
                    label: "重新检测",
                    onClick: () => void status.refetch(),
                  }}
                />
              ) : !st ? (
                <EmptyState
                  title="正在读取采集器状态…"
                  description="请稍候。"
                />
              ) : health === "running" || health === "paused" ? (
                <div className="card">
                  <div className="card-head">
                    <div>
                      {collectorHead}
                      <p className="card-sub">
                        启动于 {st.startedAt?.replace("T", " ").slice(0, 19)} ·
                        最近事件{" "}
                        {st.lastEventAt
                          ? st.lastEventAt.replace("T", " ").slice(0, 19)
                          : "无"}{" "}
                        · 自启任务
                        {st.taskEvidence === "present"
                          ? "已配置"
                          : st.taskEvidence === "absent"
                            ? "未配置"
                            : "状态未知"}
                      </p>
                    </div>
                    <span
                      className={`chip ${health === "paused" ? "chip--warn" : "chip--ok"}`}
                    >
                      {health === "paused" ? "已暂停（不记录输入）" : "运行中"}
                    </span>
                  </div>
                  <div className="btn-row">
                    <button
                      type="button"
                      className={st.paused ? "btn btn-primary" : "btn"}
                      disabled={controlsBusy || status.isFetching}
                      onClick={() =>
                        after(
                          client.setCollectorPaused(!st.paused),
                          st.paused ? "已恢复统计" : "已暂停统计",
                        )
                      }
                    >
                      {st.paused ? (
                        <>
                          <IconPlay size={16} /> 恢复统计
                        </>
                      ) : (
                        <>
                          <IconPause size={16} /> 暂停统计
                        </>
                      )}
                    </button>
                    <button
                      type="button"
                      className="btn"
                      disabled={controlsBusy}
                      onClick={() =>
                        after(client.collectorStartNow(), "采集器已就绪", {
                          pending: PENDING_START,
                        })
                      }
                    >
                      {actionBusy.showBusyLabel ? "启动中…" : "检查并启动"}
                    </button>
                  </div>
                </div>
              ) : health === "not_running" ? (
                <EmptyState
                  icon={<IconPlug size={36} />}
                  title="采集器未运行"
                  description={
                    st.taskEvidence === "present"
                      ? "自启任务已配置，但采集器当前未运行。点「立即启动」拉起（请等待完成，勿连点）。"
                      : st.taskEvidence === "unknown"
                        ? "暂时无法确认自启任务是否已配置（证据不足，不臆断）。可点「立即启动」直接拉起采集器，或在下方「自启管理」查看任务与策略。"
                        : "统计需要提权采集进程（计划任务 ClRecoderCollector，开机自启）。先启用自启任务，会自动启动采集器。"
                  }
                  action={
                    st.taskEvidence === "absent"
                      ? {
                          label: actionBusy.showBusyLabel
                            ? "处理中…"
                            : "启用采集器自启（需一次 UAC）",
                          onClick: () => {
                            if (!controlsBusy)
                              after(
                                client.collectorAutostartEnable(),
                                "已启用自启并启动采集器",
                                { pending: PENDING_ENABLE },
                              );
                          },
                        }
                      : {
                          label: actionBusy.showBusyLabel ? "启动中…" : "立即启动采集器",
                          onClick: () => {
                            if (!controlsBusy)
                              after(
                                client.collectorStartNow(),
                                "采集器已就绪",
                                { pending: PENDING_START },
                              );
                          },
                        }
                  }
                  secondary={
                    st.taskEvidence === "present"
                      ? undefined
                      : st.taskEvidence === "unknown"
                        ? {
                            label: actionBusy.showBusyLabel
                              ? "处理中…"
                              : "启用采集器自启（需一次 UAC）",
                            onClick: () => {
                              if (!controlsBusy)
                                after(
                                  client.collectorAutostartEnable(),
                                  "已启用自启并启动采集器",
                                  { pending: PENDING_ENABLE },
                                );
                            },
                          }
                        : {
                            label: "仅立即启动（不装自启）",
                            onClick: () => {
                              if (!controlsBusy)
                                after(
                                  client.collectorStartNow(),
                                  "采集器已就绪",
                                  { pending: PENDING_START },
                                );
                            },
                          }
                  }
                />
              ) : (
                // unreachable / access_denied / unknown：诊断卡片 + 重试与日志入口（不杀进程、不臆断未运行）
                <div className="card">
                  <div className="card-head">
                    <div>
                      {collectorHead}
                      <p className="card-sub">
                        {DIAG_COPY[health].title} · {DIAG_COPY[health].desc}
                      </p>
                    </div>
                    <span className="chip chip--warn">需要诊断</span>
                  </div>
                  {st.diagnosticMessage ? (
                    <p className="field-error">
                      诊断信息：{st.diagnosticMessage}
                      {st.diagnosticCode
                        ? `（诊断码 ${st.diagnosticCode}）`
                        : ""}
                    </p>
                  ) : null}
                  <div className="btn-row">
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={controlsBusy}
                      onClick={() =>
                        after(client.collectorStartNow(), "采集器已就绪", {
                          pending: PENDING_START,
                        })
                      }
                    >
                      {actionBusy.showBusyLabel ? "启动中…" : "检查并启动"}
                    </button>
                    <button
                      type="button"
                      className="btn"
                      disabled={controlsBusy}
                      onClick={() =>
                        void client.openDiagnosticsDirectory().then(
                          () => setNotice("已打开日志目录"),
                          (e: unknown) => setNotice(`操作失败：${String(e)}`),
                        )
                      }
                    >
                      打开日志目录
                    </button>
                  </div>
                </div>
              )}
            </section>

            <section className="card settings-section" id="settings-autostart">
              <div className="card-head">
                <div>
                  <h2 className="card-title">启动与运行</h2>
                  <p className="card-sub">自启任务、运行策略与界面启动方式</p>
                </div>
              </div>
              {(legacyHints.data ?? []).map((h) => (
                <div
                  key={h.code}
                  className="app-banner app-banner--warning"
                  role="status"
                >
                  {h.message}
                </div>
              ))}
              <div className="settings-rows">
                <Row
                  label="采集器自启（计划任务）"
                  hint={
                    !st
                      ? "状态检测中…"
                      : st.taskEvidence === "present"
                        ? "已配置：登录时自启 + 最高权限（不影响当前已运行的采集器）"
                        : st.taskEvidence === "absent"
                          ? "未配置：需一次 UAC 授权"
                          : "无法确认是否已配置（证据不足）"
                  }
                >
                  <button
                    type="button"
                    className={`btn${taskOn ? "" : " btn-primary"}`}
                    disabled={taskOn || controlsBusy}
                    onClick={() =>
                      after(
                        client.collectorAutostartEnable(),
                        "已启用自启并启动采集器",
                        { pending: PENDING_ENABLE },
                      )
                    }
                  >
                    {taskOn ? (
                      <>
                        <IconCheck size={14} /> 已启用
                      </>
                    ) : busy ? (
                      "处理中…"
                    ) : (
                      "启用"
                    )}
                  </button>
                  <button
                    type="button"
                    className="btn btn-danger"
                    disabled={!taskOn || controlsBusy}
                    onClick={() =>
                      after(
                        client.collectorAutostartDisable(),
                        "已禁用采集器自启任务",
                      )
                    }
                  >
                    禁用
                  </button>
                </Row>
                {/* §4.3 任务三项策略只读展示 + RepairOnly 修复（一次 UAC；不 /Run、不结束进程） */}
                <Row
                  label="任务策略（只读）"
                  hint="保持持续采集；修复对后续启动生效"
                >
                  <span className="policy-state" data-tone={policyTone}>
                    {policySummary}
                  </span>
                  {showRepair ? (
                    <button
                      type="button"
                      className="btn btn-sm btn-primary"
                      disabled={controlsBusy}
                      onClick={repairPolicy}
                    >
                      {actionBusy.showBusyLabel ? "处理中…" : "修复采集器自启（需一次 UAC）"}
                    </button>
                  ) : null}
                </Row>
                <Row label="执行时限" hint="建议不限制运行时长">
                  <span className="value-text">
                    {policyReady
                      ? fmtExecutionTimeLimit(policy!.executionTimeLimit)
                      : "—"}
                  </span>
                </Row>
                <Row
                  label="电池供电时禁止启动"
                  hint="建议关闭，电池供电时照常采集"
                >
                  <span className="value-text">
                    {policyReady
                      ? fmtPolicyBool(policy!.disallowStartIfOnBatteries)
                      : "—"}
                  </span>
                </Row>
                <Row
                  label="切换到电池时停止"
                  hint="建议关闭，切换供电时继续采集"
                >
                  <span className="value-text">
                    {policyReady
                      ? fmtPolicyBool(policy!.stopIfGoingOnBatteries)
                      : "—"}
                  </span>
                </Row>
                <Row label="界面开机自启" hint="登录 Windows 后打开界面">
                  <label className="switch">
                    <input
                      type="checkbox"
                      role="switch"
                      checked={s?.guiAutostart ?? false}
                      disabled={!s}
                      onChange={() =>
                        after(
                          client.setSettings({
                            guiAutostart: !s?.guiAutostart,
                          }),
                          "已保存 GUI 自启设置",
                        )
                      }
                    />
                    <span className="switch-track" aria-hidden="true" />
                    <span className="switch-label">
                      {s?.guiAutostart ? "已开启" : "已关闭"}
                    </span>
                  </label>
                </Row>
              </div>
            </section>

            <section className="card settings-section" id="settings-source">
              <div className="card-head">
                <div>
                  <h2 className="card-title with-icon">
                    <IconDatabase /> WhatPulse 数据源
                  </h2>
                  <p className="card-sub">导入历史快照，不修改原始数据库</p>
                </div>
              </div>
              <div className="path-edit">
                <input
                  className="input mono"
                  value={wpPath}
                  onChange={(e) => setWpPathDraft(e.target.value)}
                  aria-label="WhatPulse 数据库路径"
                  placeholder="%LOCALAPPDATA%\WhatPulse\whatpulse.db"
                />
                <button
                  type="button"
                  className="btn"
                  disabled={!wpPathDraft}
                  onClick={() =>
                    after(
                      client.setSettings({ wpDbPath: wpPathDraft || null }),
                      "已保存 WhatPulse 路径",
                    )
                  }
                >
                  保存
                </button>
              </div>
            </section>

            <UpdateCard onPageBusy={setUpdateBusy} />

            <DiagnosticsCard busy={controlsBusy} onNotice={setNotice} />

            <ExportCard />

            <section className="card settings-section" id="settings-about">
              <div className="card-head">
                <div>
                  <h2 className="card-title">关于</h2>
                </div>
              </div>
              <p className="about-text">
                统计永久保存在本机：%LOCALAPPDATA%\ClRecoder\stats.db。删除该文件会清空统计。
                锁屏与 UAC 安全桌面期间的输入属于系统级隔离，不会统计。
              </p>
            </section>
          </div>
        </div>
      </div>
    </>
  );
}
