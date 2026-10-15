import { describe, expect, it } from "vitest";
import type { UpdateCheck, UpdateErrorCode, UpdatePhase, UpdateStatus } from "../api/types";
import {
  COLLECTOR_MANUAL_HINT,
  UPDATE_CONFIRM_POINTS,
  blockReasonText,
  collectorActionBusyState,
  deriveUpdateFlow,
  errorCodeText,
  formatBytes,
  phaseLabel,
  shouldPollUpdateStatus,
  shouldResumeInstallPolling,
} from "./updateFlow";

const baseCheck = (over: Partial<UpdateCheck> = {}): UpdateCheck => ({
  currentVersion: "0.4.0",
  latestVersion: "0.5.0",
  hasUpdate: true,
  releaseUrl: "https://github.com/1781415302/cl-recoder/releases/tag/v0.5.0",
  publishedAt: "2026-10-01T12:00:00Z",
  notes: "修复\n<img onerror=alert(1) src=x>",
  assetSize: 12_500_000,
  installable: true,
  blockReason: null,
  ...over,
});

const idleStatus = (): UpdateStatus => ({
  phase: "idle",
  receivedBytes: 0,
  totalBytes: null,
  errorCode: null,
  message: null,
  collectorRestored: null,
});

describe("formatBytes", () => {
  it("formats B/KB/MB/GB", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1500)).toBe("1.5 KB");
    expect(formatBytes(12_500_000)).toBe("12.5 MB");
    expect(formatBytes(2_000_000_000)).toBe("2.00 GB");
  });
  it("guards non-finite", () => {
    expect(formatBytes(Number.NaN)).toBe("0 B");
    expect(formatBytes(-3)).toBe("0 B");
  });
});

describe("blockReasonText", () => {
  it("covers every reason", () => {
    for (const r of [
      "portable_layout",
      "asset_missing",
      "digest_missing",
      "asset_too_large",
    ] as const) {
      const t = blockReasonText(r);
      expect(t.length).toBeGreaterThan(4);
      expect(t).toMatch(/发布页|手动/);
    }
  });
});

describe("phaseLabel", () => {
  it("covers every phase", () => {
    const phases: UpdatePhase[] = [
      "idle",
      "downloading",
      "verifying",
      "stopping_collector",
      "backing_up",
      "launching",
      "launched",
      "failed",
    ];
    for (const p of phases) {
      const label = phaseLabel(p);
      if (p === "idle") expect(label).toBe("");
      else expect(label.length).toBeGreaterThan(0);
    }
    expect(phaseLabel("launched")).toContain("即将关闭");
    expect(phaseLabel("downloading")).toContain("下载");
  });
});

describe("errorCodeText", () => {
  it("covers every error code", () => {
    const codes: UpdateErrorCode[] = [
      "busy",
      "not_installed_layout",
      "version_changed",
      "curl_missing",
      "network",
      "http_status",
      "size_mismatch",
      "digest_mismatch",
      "disk",
      "collector_stop_failed",
      "backup_failed",
      "uac_cancelled",
      "launch_failed",
    ];
    for (const c of codes) {
      expect(errorCodeText(c).length).toBeGreaterThan(2);
    }
  });
});

describe("deriveUpdateFlow", () => {
  it("initial → 检查更新", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: null,
      confirming: false,
      installSubmitted: false,
      status: null,
    });
    expect(v.mode).toBe("initial");
    expect(v.headline).toBe("当前版本 v0.4.0");
    expect(v.buttons.check).toBe(true);
    expect(v.allDisabled).toBe(false);
    expect(v.pageBusy).toBe(false);
  });

  it("checking disables buttons", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: true,
      checkError: null,
      check: null,
      confirming: false,
      installSubmitted: false,
      status: null,
    });
    expect(v.mode).toBe("checking");
    expect(v.headline).toBe("正在检查…");
    expect(v.allDisabled).toBe(true);
  });

  it("up_to_date", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: baseCheck({ hasUpdate: false, latestVersion: "0.4.0", installable: false }),
      confirming: false,
      installSubmitted: false,
      status: idleStatus(),
    });
    expect(v.mode).toBe("up_to_date");
    expect(v.headline).toBe("已是最新版本");
    expect(v.buttons.recheck).toBe(true);
  });

  it("available shows notes as data (caller must render as text)", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: baseCheck(),
      confirming: false,
      installSubmitted: false,
      status: idleStatus(),
    });
    expect(v.mode).toBe("available");
    expect(v.buttons.download).toBe(true);
    expect(v.buttons.openRelease).toBe(true);
    expect(v.notes).toContain("<img onerror=");
  });

  it("blocked shows blockReason text", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: baseCheck({
        installable: false,
        blockReason: "portable_layout",
      }),
      confirming: false,
      installSubmitted: false,
      status: idleStatus(),
    });
    expect(v.mode).toBe("blocked");
    expect(v.blockReasonText).toBe(blockReasonText("portable_layout"));
    expect(v.buttons.openRelease).toBe(true);
    expect(v.buttons.download).toBe(false);
  });

  it("confirm panel lists 4 points", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: baseCheck(),
      confirming: true,
      installSubmitted: false,
      status: idleStatus(),
    });
    expect(v.mode).toBe("confirm");
    expect(v.confirmPoints).toEqual([...UPDATE_CONFIRM_POINTS]);
    expect(v.confirmPoints).toHaveLength(4);
    expect(v.buttons.confirm).toBe(true);
    expect(v.buttons.cancel).toBe(true);
  });

  it("in_progress downloading has determinate progress", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: baseCheck(),
      confirming: false,
      installSubmitted: true,
      status: {
        phase: "downloading",
        receivedBytes: 2_500_000,
        totalBytes: 10_000_000,
        errorCode: null,
        message: null,
        collectorRestored: null,
      },
    });
    expect(v.mode).toBe("in_progress");
    expect(v.headline).toContain("下载");
    expect(v.phaseLabel).toBeNull();
    expect(v.progress?.kind).toBe("determinate");
    expect(v.progress?.value).toBe(2_500_000);
    expect(v.progress?.max).toBe(10_000_000);
    expect(v.allDisabled).toBe(true);
    expect(v.pageBusy).toBe(true);
  });

  it("non-download phases use indeterminate progress", () => {
    for (const phase of [
      "verifying",
      "stopping_collector",
      "backing_up",
      "launching",
    ] as const) {
      const v = deriveUpdateFlow({
        currentVersion: "0.4.0",
        checking: false,
        checkError: null,
        check: baseCheck(),
        confirming: false,
        installSubmitted: true,
        status: {
          phase,
          receivedBytes: 10,
          totalBytes: 10,
          errorCode: null,
          message: null,
          collectorRestored: null,
        },
      });
      expect(v.mode).toBe("in_progress");
      expect(v.progress?.kind).toBe("indeterminate");
      expect(v.headline).toBe(phaseLabel(phase));
      expect(v.phaseLabel).toBeNull();
    }
  });

  it("launched disables everything", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: baseCheck(),
      confirming: false,
      installSubmitted: true,
      status: {
        phase: "launched",
        receivedBytes: 10,
        totalBytes: 10,
        errorCode: null,
        message: null,
        collectorRestored: null,
      },
    });
    expect(v.mode).toBe("launched");
    expect(v.headline).toContain("即将关闭");
    expect(v.allDisabled).toBe(true);
    expect(v.pageBusy).toBe(true);
  });

  it("failed shows message and collectorManualHint when restored===false", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: baseCheck(),
      confirming: false,
      installSubmitted: true,
      status: {
        phase: "failed",
        receivedBytes: 0,
        totalBytes: null,
        errorCode: "uac_cancelled",
        message: "管理员授权已取消。",
        collectorRestored: false,
      },
    });
    expect(v.mode).toBe("failed");
    expect(v.failureMessage).toContain("管理员授权");
    expect(v.collectorManualHint).toBe(COLLECTOR_MANUAL_HINT);
    expect(v.buttons.retry).toBe(true);
    expect(v.buttons.openRelease).toBe(true);
  });

  it("failed without collectorManualHint when restored!==false", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: null,
      check: baseCheck(),
      confirming: false,
      installSubmitted: true,
      status: {
        phase: "failed",
        receivedBytes: 0,
        totalBytes: null,
        errorCode: "network",
        message: null,
        collectorRestored: true,
      },
    });
    expect(v.collectorManualHint).toBeNull();
    expect(v.failureMessage).toBe(errorCodeText("network"));
  });

  it("check_error keeps recheck", () => {
    const v = deriveUpdateFlow({
      currentVersion: "0.4.0",
      checking: false,
      checkError: "网络错误",
      check: null,
      confirming: false,
      installSubmitted: false,
      status: null,
    });
    expect(v.mode).toBe("check_error");
    expect(v.buttons.recheck).toBe(true);
  });

  it("null currentVersion shows 未知 without fake version", () => {
    const v = deriveUpdateFlow({
      currentVersion: null,
      checking: false,
      checkError: null,
      check: null,
      confirming: false,
      installSubmitted: false,
      status: null,
    });
    expect(v.mode).toBe("initial");
    expect(v.headline).toBe("当前版本 未知");
    expect(v.headline).not.toContain("0.4.0");
  });
});

describe("shouldResumeInstallPolling", () => {
  it("resumes when phase is not idle", () => {
    expect(shouldResumeInstallPolling(null)).toBe(false);
    expect(shouldResumeInstallPolling(undefined)).toBe(false);
    expect(shouldResumeInstallPolling(idleStatus())).toBe(false);
    expect(
      shouldResumeInstallPolling({ ...idleStatus(), phase: "downloading" }),
    ).toBe(true);
    expect(
      shouldResumeInstallPolling({ ...idleStatus(), phase: "verifying" }),
    ).toBe(true);
    expect(
      shouldResumeInstallPolling({ ...idleStatus(), phase: "failed" }),
    ).toBe(true);
    expect(
      shouldResumeInstallPolling({ ...idleStatus(), phase: "launched" }),
    ).toBe(true);
  });
});

describe("collectorActionBusyState", () => {
  it("disables on updateBusy but keeps idle label", () => {
    expect(collectorActionBusyState(false, false)).toEqual({
      disabled: false,
      showBusyLabel: false,
    });
    expect(collectorActionBusyState(true, false)).toEqual({
      disabled: true,
      showBusyLabel: true,
    });
    // 更新占用：按钮禁用，文案不变（不显示「启动中…」）
    expect(collectorActionBusyState(false, true)).toEqual({
      disabled: true,
      showBusyLabel: false,
    });
    expect(collectorActionBusyState(true, true)).toEqual({
      disabled: true,
      showBusyLabel: true,
    });
  });
});

describe("shouldPollUpdateStatus", () => {
  it("polls after submit until failed", () => {
    expect(shouldPollUpdateStatus(false, null)).toBe(false);
    expect(shouldPollUpdateStatus(true, null)).toBe(true);
    expect(shouldPollUpdateStatus(true, idleStatus())).toBe(true);
    expect(
      shouldPollUpdateStatus(true, {
        ...idleStatus(),
        phase: "downloading",
      }),
    ).toBe(true);
    expect(
      shouldPollUpdateStatus(true, {
        ...idleStatus(),
        phase: "failed",
      }),
    ).toBe(false);
    expect(
      shouldPollUpdateStatus(true, {
        ...idleStatus(),
        phase: "launched",
      }),
    ).toBe(true);
  });
});
