// 手动 DPI 编辑侧栏（motion-dpi §4.6 S7）：本地受控表单，不查询 API（保存经 props.onSave 交页面 mutation）。
//
// 行为（§4.5/§4.6）：自动 DPI 有效时只读展示（后端同样拒绝，双保险）；探测失败/离线可编辑已存手动值；
// 非物理来源（虚拟/未知桶）禁配置。仅十进制整数 1..100000（lib.parseManualDpi 校验，错误内联展示）；
// 保存失败保留编辑内容并展示错误；成功后编辑框回写规范值。pending 时禁用输入与按钮；
// 切换来源重置草稿（渲染期对齐，同 DataTable resetKey 惯例），进行中的保存不污染新来源草稿。
import { useEffect, useId, useRef, useState } from "react";
import type { DpiOrigin, DpiProbeStatus, MouseSourceRow } from "../api/types";
import { MANUAL_DPI_MAX, MANUAL_DPI_MIN, parseManualDpi } from "../lib/motionPresentation";

export interface MouseDpiEditorProps {
  source: MouseSourceRow;
  pending: boolean;
  onSave: (dpi: number | null) => Promise<void>;
}

const ORIGIN_LABEL: Record<DpiOrigin, string> = {
  auto: "自动值",
  manual: "手动值",
  unknown: "未配置",
};

const PROBE_LABEL: Record<DpiProbeStatus, string> = {
  pending: "进行中",
  available: "可用",
  unsupported: "设备不支持",
  ambiguous: "结果不唯一",
  unavailable: "不可用",
  disconnected: "来源离线",
};

function initialText(source: MouseSourceRow): string {
  return source.manualDpi !== null ? String(source.manualDpi) : "";
}

function fmtUntil(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
}

export function MouseDpiEditor({ source, pending, onSave }: MouseDpiEditorProps) {
  const inputId = useId();
  const [text, setText] = useState(() => initialText(source));
  const [error, setError] = useState<string | null>(null);
  // 草稿归属：切换来源时渲染期对齐重置（草稿不跨来源携带）
  const [syncedId, setSyncedId] = useState(source.id);
  // 进行中的保存完成时刻的来源 id（异步守卫：不把旧来源的结果写进新来源草稿）
  const currentIdRef = useRef(source.id);
  useEffect(() => {
    currentIdRef.current = source.id;
  }, [source.id]);

  if (syncedId !== source.id) {
    setSyncedId(source.id);
    setText(initialText(source));
    setError(null);
  }

  const autoActive = source.dpiOrigin === "auto" && source.autoDpi !== null;

  async function save(dpi: number | null) {
    const forId = source.id;
    setError(null);
    try {
      await onSave(dpi);
      if (currentIdRef.current === forId) setText(dpi === null ? "" : String(dpi));
    } catch (e) {
      // 失败保留编辑内容，仅展示错误（§4.6）
      if (currentIdRef.current === forId) setError(e instanceof Error ? e.message : String(e));
    }
  }

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const parsed = parseManualDpi(text);
    if (!parsed.ok) {
      setError(parsed.message);
      return;
    }
    void save(parsed.dpi);
  }

  const displayName = source.nickname ?? source.name;

  return (
    <div className="card">
      <h2 className="card-title">DPI 设置</h2>
      <p className="card-sub">
        {displayName}（来源 #{source.id}）{" "}
        <span className={`chip${source.connected ? " chip-positive" : ""}`}>{source.connected ? "已连接" : "离线"}</span>
      </p>
      <div className="dpi-current" style={{ marginTop: "var(--space-3)" }}>
        <span className="td-muted">当前换算 DPI</span>
        {source.effectiveDpi !== null ? (
          <span className="dpi-value">{source.effectiveDpi}</span>
        ) : (
          <span className="td-muted">未配置</span>
        )}
        <span className="chip">{ORIGIN_LABEL[source.dpiOrigin]}</span>
      </div>
      {!source.physical ? (
        <p className="chart-hint" role="note" style={{ marginTop: "var(--space-3)" }}>
          虚拟/未知来源不支持配置或换算 DPI。
        </p>
      ) : autoActive ? (
        <div className="dpi-readonly" role="note" style={{ marginTop: "var(--space-3)" }}>
          <div>
            自动 DPI 生效中：<span className="dpi-value">{source.autoDpi}</span>
            {source.autoValidUntil ? <>（有效期至 {fmtUntil(source.autoValidUntil)}）</> : null}
            ，只读；失效或断连后可修改手动值。
          </div>
          {source.manualDpi !== null ? (
            <div style={{ marginTop: "var(--space-1)" }}>
              已保存手动值 <span className="dpi-value">{source.manualDpi}</span>，将在自动值失效后使用。
            </div>
          ) : null}
        </div>
      ) : (
        <form className="dpi-editor" style={{ marginTop: "var(--space-3)" }} noValidate onSubmit={onSubmit}>
          <label className="field-label" htmlFor={inputId}>
            手动 DPI（{MANUAL_DPI_MIN}–{MANUAL_DPI_MAX}，仅十进制整数）
          </label>
          <input
            id={inputId}
            className="input"
            inputMode="numeric"
            autoComplete="off"
            placeholder="例如 1600"
            value={text}
            disabled={pending}
            onChange={(e) => {
              setText(e.currentTarget.value);
              setError(null);
            }}
          />
          {error ? (
            <p className="dpi-error" role="alert">
              {error}
            </p>
          ) : null}
          <div className="dpi-actions">
            <button type="submit" className="btn btn-primary" disabled={pending}>
              {pending ? "保存中…" : "保存"}
            </button>
            <button
              type="button"
              className="btn"
              disabled={pending || source.manualDpi === null}
              onClick={() => void save(null)}
            >
              清除手动值
            </button>
          </div>
          <p className="chart-hint">仅影响之后的采集，不回算历史。自动探测：{PROBE_LABEL[source.probeStatus]}。</p>
        </form>
      )}
    </div>
  );
}
