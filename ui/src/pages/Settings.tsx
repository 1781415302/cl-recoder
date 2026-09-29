// 设置页：采集器控制（状态近实时轮询 / 暂停 / 自启）+ GUI 自启 + WhatPulse 数据源 + 数据导出（§5.5）。
import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { save } from "@tauri-apps/plugin-dialog";
import * as client from "../api/client";
import { useCollectorStatus, useSettings } from "../api/queries";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { IconCheck, IconDatabase, IconDownload, IconPause, IconPlay, IconPlug } from "../components/icons";
import { defaultRange, fmtNum } from "../lib/format";
import type { ExportReport, Range } from "../api/types";

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "var(--space-4)", padding: "var(--space-2) 0", flexWrap: "wrap" }}>
      <div style={{ minWidth: 220 }}>
        <div style={{ fontWeight: 600 }}>{label}</div>
        {hint ? <div style={{ fontSize: "var(--text-caption)", color: "var(--color-text-muted)" }}>{hint}</div> : null}
      </div>
      <div style={{ display: "flex", gap: "var(--space-2)", alignItems: "center", flexWrap: "wrap" }}>{children}</div>
    </div>
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
      const r = await client.exportData(format, scope, range.from, range.to, path);
      setResult(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="card">
      <h2 className="card-title" style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}><IconDownload /> 数据导出</h2>
      <p className="card-sub">CSV 带 BOM（Excel 中文兼容）；JSON 为单文件全量（§4.10）</p>
      <div style={{ marginTop: "var(--space-3)", display: "flex", flexDirection: "column", gap: "var(--space-2)" }}>
        <Row label="格式">
          {(["csv", "json"] as const).map((f) => (
            <button key={f} type="button" className={`btn btn-sm${format === f ? " btn-primary" : ""}`} aria-pressed={format === f} onClick={() => setFormat(f)}>
              {f.toUpperCase()}
            </button>
          ))}
        </Row>
        <Row label="范围数据源" hint="own = 本软件统计；wp = WhatPulse 导入镜像">
          {(["own", "wp"] as const).map((s) => (
            <button key={s} type="button" className={`btn btn-sm${scope === s ? " btn-primary" : ""}`} aria-pressed={scope === s} onClick={() => setScope(s)}>
              {s === "own" ? "本软件" : "WhatPulse"}
            </button>
          ))}
        </Row>
        <Row label="日期范围">
          <DateRangePicker value={range} onChange={setRange} />
        </Row>
        <div>
          <button type="button" className="btn btn-primary" disabled={busy} onClick={() => void doExport()}>
            {busy ? "导出中…" : "选择位置并导出"}
          </button>
          {error ? <span style={{ color: "var(--color-danger)", marginLeft: "var(--space-2)" }}>{error}</span> : null}
        </div>
        {result ? (
          <p className="chart-readout" role="status">
            已导出 {fmtNum(result.rows)} 行 → {result.files.join("、")}
          </p>
        ) : null}
      </div>
    </div>
  );
}

export function Settings() {
  const qc = useQueryClient();
  const status = useCollectorStatus();
  const settings = useSettings();
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [wpPathDraft, setWpPathDraft] = useState<string | null>(null);

  function after(promise: Promise<unknown>, okMsg: string) {
    setBusy(true);
    setNotice("处理中，请稍候…（启动最多约 10 秒，请勿连点）");
    promise.then(
      () => {
        setBusy(false);
        setNotice(okMsg);
        void qc.invalidateQueries();
      },
      (e: unknown) => {
        setBusy(false);
        setNotice(`操作失败：${String(e)}`);
      },
    );
  }

  const st = status.data;
  const s = settings.data;
  const wpPath = wpPathDraft ?? s?.wpDbPath ?? "";
  const taskOn = st?.taskExists ?? false;

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">设置</h1>
          <p className="page-desc">采集器控制、自启管理、数据源与导出（状态近实时刷新）</p>
        </div>
      </div>

      {notice ? <p className="chart-readout" role="status">{notice}</p> : null}

      {status.isLoading || !st ? (
        <EmptyState title="正在读取采集器状态…" description="通过控制管道探测（500ms 超时）。" />
      ) : st.running ? (
        <div className="card">
          <h2 className="card-title" style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}><IconPlug /> 采集器</h2>
          <p className="card-sub">
            {st.paused ? "已暂停（不记录输入）" : "运行中"} · 启动于 {st.startedAt?.replace("T", " ").slice(0, 19)} ·
            最近事件 {st.lastEventAt ? st.lastEventAt.replace("T", " ").slice(0, 19) : "无"} ·
            自启任务{taskOn ? "已配置" : "未配置"}
          </p>
          <div style={{ marginTop: "var(--space-3)", display: "flex", gap: "var(--space-2)", flexWrap: "wrap" }}>
            <button
              type="button"
              className={st.paused ? "btn btn-primary" : "btn"}
              disabled={busy || status.isFetching}
              onClick={() => after(client.setCollectorPaused(!st.paused), st.paused ? "已恢复统计" : "已暂停统计")}
            >
              {st.paused ? <><IconPlay size={16} /> 恢复统计</> : <><IconPause size={16} /> 暂停统计</>}
            </button>
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() => after(client.collectorStartNow(), "采集器已就绪")}
            >
              {busy ? "启动中…" : "立即启动（确保运行）"}
            </button>
          </div>
        </div>
      ) : (
        <EmptyState
          icon={<IconPlug size={36} />}
          title="采集器未运行"
          description={
            taskOn
              ? "自启任务已配置，但采集器当前未运行。点「立即启动」拉起（请等待完成，勿连点）。"
              : "统计需要提权采集进程（计划任务 ClRecoderCollector，开机自启）。先启用自启任务，会自动启动采集器。"
          }
          action={
            taskOn
              ? { label: busy ? "启动中…" : "立即启动采集器", onClick: () => { if (!busy) after(client.collectorStartNow(), "采集器已就绪"); } }
              : { label: busy ? "处理中…" : "启用采集器自启（需一次 UAC）", onClick: () => { if (!busy) after(client.collectorAutostartEnable(), "已启用自启并启动采集器"); } }
          }
          secondary={
            taskOn
              ? undefined
              : { label: "仅立即启动（不装自启）", onClick: () => { if (!busy) after(client.collectorStartNow(), "采集器已就绪"); } }
          }
        />
      )}

      <div className="grid-2">
        <div className="card">
          <h2 className="card-title">自启管理</h2>
          <div style={{ marginTop: "var(--space-2)" }}>
            <Row
              label="采集器自启（计划任务）"
              hint={taskOn ? "已配置：登录时自启 + 最高权限" : "未配置：需一次 UAC 授权"}
            >
              <button
                type="button"
                className={`btn${taskOn ? "" : " btn-primary"}`}
                disabled={taskOn || busy}
                onClick={() => after(client.collectorAutostartEnable(), "已启用自启并启动采集器")}
              >
                {taskOn ? <><IconCheck size={14} /> 已启用</> : busy ? "处理中…" : "启用"}
              </button>
              <button
                type="button"
                className="btn btn-danger"
                disabled={!taskOn || busy}
                onClick={() => after(client.collectorAutostartDisable(), "已禁用采集器自启任务")}
              >
                禁用
              </button>
            </Row>
            <Row label="GUI 开机自启" hint="HKCU Run（tauri-plugin-autostart）">
              <button
                type="button"
                className={`btn btn-sm${s?.guiAutostart ? " btn-primary" : ""}`}
                aria-pressed={s?.guiAutostart ?? false}
                disabled={!s}
                onClick={() => after(client.setSettings({ guiAutostart: !s?.guiAutostart }), "已保存 GUI 自启设置")}
              >
                {s?.guiAutostart ? <><IconCheck size={14} /> 已开启</> : "已关闭"}
              </button>
            </Row>
          </div>
        </div>

        <div className="card">
          <h2 className="card-title" style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}><IconDatabase /> WhatPulse 数据源</h2>
          <p className="card-sub">仅只读导入（先复制后打开），绝不写入 WhatPulse 目录</p>
          <div style={{ marginTop: "var(--space-2)", display: "flex", gap: "var(--space-2)" }}>
            <input
              className="input"
              style={{ flex: 1, fontFamily: "var(--font-mono)", fontSize: "var(--text-caption)" }}
              value={wpPath}
              onChange={(e) => setWpPathDraft(e.target.value)}
              aria-label="WhatPulse 数据库路径"
              placeholder="%LOCALAPPDATA%\WhatPulse\whatpulse.db"
            />
            <button
              type="button"
              className="btn"
              disabled={!wpPathDraft}
              onClick={() => after(client.setSettings({ wpDbPath: wpPathDraft || null }), "已保存 WhatPulse 路径")}
            >
              保存
            </button>
          </div>
        </div>
      </div>

      <ExportCard />

      <div className="card">
        <h2 className="card-title">关于</h2>
        <p className="card-sub">
          数据位置：%LOCALAPPDATA%\ClRecoder\stats.db（SQLite WAL，永久保留；删除该文件即清空全部统计）。
          锁屏与 UAC 安全桌面期间的输入属于系统级隔离，不会统计。
        </p>
      </div>
    </>
  );
}
