// 应用内更新展示纯函数（§4.8）：状态 → 文案 / 进度 / 可用按钮；组件只渲染。
// 零运行时副作用（无 DOM / 无网络 / 无时钟），由 updateFlow.test.ts 锚定。

import type {
  UpdateBlockReason,
  UpdateCheck,
  UpdateErrorCode,
  UpdatePhase,
  UpdateStatus,
} from "../api/types";

/** 确认面板 4 点（§4.8 逐字） */
export const UPDATE_CONFIRM_POINTS = [
  "将正常停止采集器（期间不统计）",
  "会备份数据库",
  "需要一次管理员授权",
  "完成后自动重新打开",
] as const;

/** 采集器回滚失败时的醒目提示（§4.8 / §4.5 回滚约定） */
export const COLLECTOR_MANUAL_HINT =
  "采集器未能自动恢复，请到设置页点击「立即启动采集器」。";

/** UI 模式（由 deriveUpdateFlow 推导） */
export type UpdateUiMode =
  | "initial"
  | "checking"
  | "up_to_date"
  | "available"
  | "blocked"
  | "confirm"
  | "in_progress"
  | "failed"
  | "launched"
  | "check_error";

export interface UpdateFlowButtons {
  /** 「检查更新」 */
  check: boolean;
  /** 「重新检查」 */
  recheck: boolean;
  /** 「下载并更新」 */
  download: boolean;
  /** 「确认并更新」 */
  confirm: boolean;
  /** 「取消」（确认面板） */
  cancel: boolean;
  /** 「重试」 */
  retry: boolean;
  /** 「前往发布页」 */
  openRelease: boolean;
}

export interface UpdateFlowProgress {
  /** 确定进度：下载阶段有值；其余为 indeterminate */
  kind: "determinate" | "indeterminate";
  value: number;
  max: number;
}

export interface UpdateFlowInput {
  /** 当前应用版本；getAppVersion 失败时为 null（显示「未知」，不传假版本） */
  currentVersion: string | null;
  /** 是否正在检查 */
  checking: boolean;
  /** 检查失败文案（检查阶段报错，不改安装态） */
  checkError: string | null;
  /** 最近一次检查结果；尚未检查为 null */
  check: UpdateCheck | null;
  /** 用户已点「下载并更新」，显示确认面板 */
  confirming: boolean;
  /** 本地已提交 update_install（或后端 phase 非 idle/failed） */
  installSubmitted: boolean;
  /** 最近一次 update_status；未轮询时可为 null */
  status: UpdateStatus | null;
}

export interface UpdateFlowView {
  mode: UpdateUiMode;
  /** 主文案 */
  headline: string;
  /** 次要说明（版本号/发布时间等） */
  detail: string | null;
  /** 更新说明纯文本（仅 available/blocked 有 notes 时） */
  notes: string | null;
  /** 进行中阶段文案 */
  phaseLabel: string | null;
  progress: UpdateFlowProgress | null;
  buttons: UpdateFlowButtons;
  /** 进行中 / launched：全部按钮禁用 */
  allDisabled: boolean;
  /** 同步禁用设置页其它采集器操作（复用 busy） */
  pageBusy: boolean;
  confirmPoints: readonly string[] | null;
  /** collectorRestored===false 时醒目提示 */
  collectorManualHint: string | null;
  blockReasonText: string | null;
  /** 失败时的 message */
  failureMessage: string | null;
  releaseUrl: string | null;
  /** 可供确认/安装的目标版本 */
  targetVersion: string | null;
}

const NO_BUTTONS: UpdateFlowButtons = {
  check: false,
  recheck: false,
  download: false,
  confirm: false,
  cancel: false,
  retry: false,
  openRelease: false,
};

/** 阻断原因中文说明（§4.8） */
export function blockReasonText(reason: UpdateBlockReason): string {
  switch (reason) {
    case "portable_layout":
      return "当前为便携版布局，无法自动安装，请前往发布页手动下载。";
    case "asset_missing":
      return "发布资产缺失或名称不符，无法自动安装，请前往发布页手动下载。";
    case "digest_missing":
      return "发布资产缺少校验信息，无法自动安装，请前往发布页手动下载。";
    case "asset_too_large":
      return "安装包过大，无法自动安装，请前往发布页手动下载。";
    default: {
      const _exhaustive: never = reason;
      return _exhaustive;
    }
  }
}

/** 流水线阶段文案（§4.8） */
export function phaseLabel(phase: UpdatePhase): string {
  switch (phase) {
    case "idle":
      return "";
    case "downloading":
      return "正在下载…";
    case "verifying":
      return "正在校验…";
    case "stopping_collector":
      return "正在停止采集器…";
    case "backing_up":
      return "正在备份数据库…";
    case "launching":
      return "正在请求管理员授权…";
    case "launched":
      return "正在交给安装程序，应用即将关闭…";
    case "failed":
      return "更新失败";
    default: {
      const _exhaustive: never = phase;
      return _exhaustive;
    }
  }
}

/** 错误码兜底文案（后端 message 优先；测试覆盖各码有稳定中文） */
export function errorCodeText(code: UpdateErrorCode): string {
  switch (code) {
    case "busy":
      return "已有更新任务在进行中。";
    case "not_installed_layout":
      return "当前不是安装版布局，无法自动更新。";
    case "version_changed":
      return "发布版本已变化，请重新检查更新。";
    case "curl_missing":
      return "系统缺少 curl.exe，请前往发布页手动下载。";
    case "network":
      return "网络错误，请稍后重试。";
    case "http_status":
      return "下载失败（HTTP 状态异常），请稍后重试。";
    case "size_mismatch":
      return "安装包大小不符，已取消更新。";
    case "digest_mismatch":
      return "安装包校验失败，已取消更新。";
    case "disk":
      return "磁盘空间不足或写入失败。";
    case "collector_stop_failed":
      return "无法停止采集器，已取消更新。";
    case "backup_failed":
      return "备份数据库失败，已取消更新。";
    case "uac_cancelled":
      return "管理员授权已取消。";
    case "launch_failed":
      return "启动安装程序失败。";
    default: {
      const _exhaustive: never = code;
      return _exhaustive;
    }
  }
}

/** 字节格式化：B / KB / MB / GB（十进制 1000；展示用） */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "0 B";
  if (n < 1000) return `${Math.floor(n)} B`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)} KB`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(1)} MB`;
  return `${(n / 1_000_000_000).toFixed(2)} GB`;
}

function fmtPublishedAt(iso: string | null): string | null {
  if (!iso) return null;
  // 仅展示日期时间前 19 位（YYYY-MM-DDTHH:MM:SS → 空格分隔），避免时区库依赖
  const s = iso.replace("T", " ").slice(0, 19);
  return s.length > 0 ? `发布时间 ${s}` : null;
}

function buttons(partial: Partial<UpdateFlowButtons>): UpdateFlowButtons {
  return { ...NO_BUTTONS, ...partial };
}

/**
 * 由本地 UI 状态 + 后端 UpdateCheck/UpdateStatus 推导展示模型。
 * 优先级：launched / in_progress / failed / confirm / checking / check_error / check 结果 / initial。
 */
export function deriveUpdateFlow(input: UpdateFlowInput): UpdateFlowView {
  const {
    currentVersion,
    checking,
    checkError,
    check,
    confirming,
    installSubmitted,
    status,
  } = input;

  const releaseUrl = check?.releaseUrl ?? null;
  const targetVersion = check?.latestVersion ?? null;
  const verLabel =
    currentVersion !== null ? `当前版本 v${currentVersion}` : "当前版本 未知";

  const phase = status?.phase ?? "idle";
  const isLaunched = installSubmitted && phase === "launched";
  const isFailed = installSubmitted && phase === "failed";
  // 已提交且未失败/未 launched：含刚提交时尚 idle、以及下载中各阶段
  const inProgress = installSubmitted && !isFailed && !isLaunched;

  if (isLaunched) {
    return {
      mode: "launched",
      headline: phaseLabel("launched"),
      detail: null,
      notes: null,
      phaseLabel: null,
      progress: { kind: "indeterminate", value: 0, max: 1 },
      buttons: NO_BUTTONS,
      allDisabled: true,
      pageBusy: true,
      confirmPoints: null,
      collectorManualHint: null,
      blockReasonText: null,
      failureMessage: null,
      releaseUrl,
      targetVersion,
    };
  }

  if (inProgress) {
    // downloading…launching（不含 launched；刚提交时 phase 可能仍 idle）
    const label =
      phase === "idle" ? "正在准备更新…" : phaseLabel(phase);
    let progress: UpdateFlowProgress;
    if (phase === "downloading") {
      const total = status?.totalBytes ?? null;
      const received = status?.receivedBytes ?? 0;
      if (total !== null && total > 0) {
        progress = {
          kind: "determinate",
          value: Math.min(received, total),
          max: total,
        };
      } else {
        progress = { kind: "indeterminate", value: 0, max: 1 };
      }
    } else {
      progress = { kind: "indeterminate", value: 0, max: 1 };
    }
    const detail =
      phase === "downloading" && status
        ? status.totalBytes !== null
          ? `${formatBytes(status.receivedBytes)} / ${formatBytes(status.totalBytes)}`
          : formatBytes(status.receivedBytes)
        : null;
    return {
      mode: "in_progress",
      headline: label || "正在更新…",
      detail,
      notes: null,
      // 文案只放 headline，避免与 phaseLabel 重复（P1-2d）
      phaseLabel: null,
      progress,
      buttons: NO_BUTTONS,
      allDisabled: true,
      pageBusy: true,
      confirmPoints: null,
      collectorManualHint: null,
      blockReasonText: null,
      failureMessage: null,
      releaseUrl,
      targetVersion,
    };
  }

  if (isFailed) {
    const msg =
      status?.message ??
      (status?.errorCode ? errorCodeText(status.errorCode) : "更新失败");
    const needManual = status?.collectorRestored === false;
    return {
      mode: "failed",
      headline: "更新失败",
      detail: null,
      notes: null,
      phaseLabel: null,
      progress: null,
      buttons: buttons({ retry: true, openRelease: Boolean(releaseUrl) }),
      allDisabled: false,
      pageBusy: false,
      confirmPoints: null,
      collectorManualHint: needManual ? COLLECTOR_MANUAL_HINT : null,
      blockReasonText: null,
      failureMessage: msg,
      releaseUrl,
      targetVersion,
    };
  }

  if (confirming && check?.installable) {
    return {
      mode: "confirm",
      headline: `即将更新到 v${check.latestVersion}`,
      detail: null,
      notes: null,
      phaseLabel: null,
      progress: null,
      buttons: buttons({ confirm: true, cancel: true }),
      allDisabled: false,
      pageBusy: false,
      confirmPoints: UPDATE_CONFIRM_POINTS,
      collectorManualHint: null,
      blockReasonText: null,
      failureMessage: null,
      releaseUrl,
      targetVersion: check.latestVersion,
    };
  }

  if (checking) {
    return {
      mode: "checking",
      headline: "正在检查…",
      detail: verLabel,
      notes: null,
      phaseLabel: null,
      progress: null,
      buttons: NO_BUTTONS,
      allDisabled: true,
      pageBusy: false,
      confirmPoints: null,
      collectorManualHint: null,
      blockReasonText: null,
      failureMessage: null,
      releaseUrl,
      targetVersion,
    };
  }

  if (checkError) {
    return {
      mode: "check_error",
      headline: "检查更新失败",
      detail: checkError,
      notes: null,
      phaseLabel: null,
      progress: null,
      buttons: buttons({ recheck: true }),
      allDisabled: false,
      pageBusy: false,
      confirmPoints: null,
      collectorManualHint: null,
      blockReasonText: null,
      failureMessage: checkError,
      releaseUrl,
      targetVersion,
    };
  }

  if (check) {
    if (!check.hasUpdate) {
      return {
        mode: "up_to_date",
        headline: "已是最新版本",
        detail: verLabel,
        notes: null,
        phaseLabel: null,
        progress: null,
        buttons: buttons({ recheck: true }),
        allDisabled: false,
        pageBusy: false,
        confirmPoints: null,
        collectorManualHint: null,
        blockReasonText: null,
        failureMessage: null,
        releaseUrl: check.releaseUrl,
        targetVersion: check.latestVersion,
      };
    }

    if (check.installable) {
      const pub = fmtPublishedAt(check.publishedAt);
      const size =
        check.assetSize !== null ? `安装包 ${formatBytes(check.assetSize)}` : null;
      const detailParts = [`新版本 v${check.latestVersion}`, pub, size].filter(
        Boolean,
      ) as string[];
      return {
        mode: "available",
        headline: `发现新版本 v${check.latestVersion}`,
        detail: detailParts.join(" · "),
        notes: check.notes,
        phaseLabel: null,
        progress: null,
        buttons: buttons({ download: true, openRelease: true }),
        allDisabled: false,
        pageBusy: false,
        confirmPoints: null,
        collectorManualHint: null,
        blockReasonText: null,
        failureMessage: null,
        releaseUrl: check.releaseUrl,
        targetVersion: check.latestVersion,
      };
    }

    // 有更新但被阻断
    const reason = check.blockReason;
    const reasonText = reason ? blockReasonText(reason) : "无法自动安装。";
    return {
      mode: "blocked",
      headline: `发现新版本 v${check.latestVersion}（无法自动安装）`,
      detail: reasonText,
      notes: check.notes,
      phaseLabel: null,
      progress: null,
      buttons: buttons({ openRelease: true, recheck: true }),
      allDisabled: false,
      pageBusy: false,
      confirmPoints: null,
      collectorManualHint: null,
      blockReasonText: reasonText,
      failureMessage: null,
      releaseUrl: check.releaseUrl,
      targetVersion: check.latestVersion,
    };
  }

  return {
    mode: "initial",
    headline: verLabel,
    detail: null,
    notes: null,
    phaseLabel: null,
    progress: null,
    buttons: buttons({ check: true }),
    allDisabled: false,
    pageBusy: false,
    confirmPoints: null,
    collectorManualHint: null,
    blockReasonText: null,
    failureMessage: null,
    releaseUrl: null,
    targetVersion: null,
  };
}

/**
 * 挂载时是否应根据后端 status 恢复 installSubmitted（P1-2b）。
 * phase 非 idle（含 downloading…failed/launched）→ 恢复轮询/展示。
 */
export function shouldResumeInstallPolling(
  status: UpdateStatus | null | undefined,
): boolean {
  if (!status) return false;
  return status.phase !== "idle";
}

/**
 * 采集器操作按钮：disabled 受 actionBusy|updateBusy；进行中文案只看 actionBusy（P1-2c）。
 * 更新占用期间按钮禁用，但文案不得变成「启动中…」。
 */
export function collectorActionBusyState(
  actionBusy: boolean,
  updateBusy: boolean,
): { disabled: boolean; showBusyLabel: boolean } {
  return {
    disabled: actionBusy || updateBusy,
    showBusyLabel: actionBusy,
  };
}

/** 是否应轮询 update_status（§4.5：本地已提交安装后轮询，直到 failed；含刚提交时的 idle） */
export function shouldPollUpdateStatus(
  installSubmitted: boolean,
  status: UpdateStatus | null,
): boolean {
  if (!installSubmitted) return false;
  if (!status) return true;
  return status.phase !== "failed";
}
