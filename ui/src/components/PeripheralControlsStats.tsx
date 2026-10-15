// 鼠标/手柄按键分布卡：外壳与控件委托 Mouse/GamepadDeviceDiagram；
// 保留详情/其它输入；只消费 props，历史 loading 不遮住实时图。
import { useMemo, useState, type ReactNode } from "react";
import type { DeviceLiveView, TopKeyRow } from "../api/types";
import { motionControlSpecs } from "../lib/motionPresentation";
import { fmtNum } from "../lib/format";
import { MouseDeviceDiagram } from "./MouseDeviceDiagram";
import { GamepadDeviceDiagram } from "./GamepadDeviceDiagram";
import { totalsByCode, useControlHeat } from "./diagramSkin";

export interface PeripheralControlsStatsProps {
  kind: "mouse" | "gamepad";
  rows: readonly TopKeyRow[];
  selectedCode: number | null;
  onSelect: (code: number | null) => void;
  /** 旧调用兼容：未传 historyState 时由 loading 推导 */
  loading?: boolean;
  live?: DeviceLiveView | null;
  liveToolbar?: ReactNode;
  historyState?: "loading" | "ready" | "error";
}

export function PeripheralControlsStats({
  kind,
  rows,
  selectedCode,
  onSelect,
  loading = false,
  live = null,
  liveToolbar,
  historyState: historyStateProp,
}: PeripheralControlsStatsProps) {
  const historyState = historyStateProp ?? (loading ? "loading" : "ready");
  const specs = motionControlSpecs(kind);
  const lookup = useMemo(() => {
    const entries = new Map<number, TopKeyRow>();
    for (const row of rows) if (!entries.has(row.code)) entries.set(row.code, row);
    return entries;
  }, [rows]);
  const others = rows.filter((row) => !specs.some((spec) => spec.code === row.code));
  const selected = selectedCode === null ? null : lookup.get(selectedCode);
  const selectedLabel =
    selected?.label ?? specs.find((spec) => spec.code === selectedCode)?.shortLabel;
  const countReady = historyState === "ready";
  const Diagram = kind === "mouse" ? MouseDeviceDiagram : GamepadDeviceDiagram;
  const [heatOn, setHeatOn] = useState(true);
  const totals = useMemo(() => totalsByCode(rows), [rows]);
  const codes = useMemo(() => specs.map((spec) => spec.code), [specs]);
  const heatVars = useControlHeat(codes, totals, heatOn && countReady);
  const dist = useMemo(() => {
    const items = specs.map((spec, order) => ({
      code: spec.code,
      label: spec.shortLabel,
      total: totals.get(spec.code) ?? 0,
      order,
    }));
    const sum = items.reduce((acc, item) => acc + item.total, 0);
    const max = items.reduce((acc, item) => Math.max(acc, item.total), 0);
    const sorted = countReady
      ? [...items].sort((a, b) => b.total - a.total || a.order - b.order)
      : items;
    return { items: sorted, sum, max, top: countReady && max > 0 ? sorted[0] : null };
  }, [specs, totals, countReady]);

  return (
    <section
      className="card card--stage peripheral-card"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          onSelect(null);
        }
      }}
    >
      <div className="card-head">
        <div>
          <h2 className="card-title">按键分布</h2>
          <p className="card-sub">
            {kind === "mouse"
              ? "按下与滚动方向分别记录"
              : "Xbox 布局 · 肩键、扳机与各按键分别记录"}
          </p>
        </div>
        <div className="card-tools">
          <label className="switch switch--sm">
            <input
              type="checkbox"
              role="switch"
              checked={heatOn}
              onChange={(e) => setHeatOn(e.currentTarget.checked)}
            />
            <span className="switch-track" aria-hidden="true" />
            <span className="switch-label">频次热力</span>
          </label>
          <span className="chip">{specs.length} 类控件</span>
        </div>
      </div>
      {liveToolbar}
      <div className={`dist dist--${kind}`}>
        <Diagram
          rows={rows}
          historyState={historyState}
          selectedCode={selectedCode}
          onSelect={onSelect}
          live={live}
          heat={heatOn}
        />
          <div className="dist-summary">
            <div className="dist-summary-main">
              <span className="dist-kicker">所选日期合计</span>
              <strong className="dist-total num">
                {countReady ? fmtNum(dist.sum) : "—"}
                <small> 次</small>
              </strong>
              <span className="dist-top muted">
                {!countReady
                  ? historyState === "error"
                    ? "历史次数暂不可用"
                    : "历史次数加载中…"
                  : dist.top
                    ? `最常用 ${dist.top.label} · ${Math.round((dist.top.total / dist.sum) * 100)}%`
                    : "所选日期暂无记录"}
              </span>
            </div>
            {heatOn && countReady && dist.max > 0 ? (
              <div className="heat-legend" aria-hidden="true">
                <span>少</span>
                <span className="heat-legend-bar" />
                <span>多</span>
              </div>
            ) : null}
          </div>
          <ol className="dist-list" aria-label="按键使用排行">
            {dist.items.map((item) => {
              const share =
                countReady && dist.sum > 0 ? (item.total / dist.sum) * 100 : 0;
              const fill = countReady && dist.max > 0 ? (item.total / dist.max) * 100 : 0;
              const vars = heatVars.get(item.code);
              return (
                <li key={item.code}>
                  <button
                    type="button"
                    className="dist-row"
                    data-code={item.code}
                    data-zero={countReady && item.total === 0 ? "true" : undefined}
                    aria-pressed={selectedCode === item.code}
                    aria-label={
                      countReady
                        ? `${item.label}，${fmtNum(item.total)} 次，占 ${share.toFixed(share < 10 ? 1 : 0)}%`
                        : `${item.label}，历史次数暂不可用`
                    }
                    onClick={() => onSelect(selectedCode === item.code ? null : item.code)}
                  >
                    <span className="dist-name">{item.label}</span>
                    <span className="dist-bar" aria-hidden="true">
                      <span
                        className="dist-bar-fill"
                        style={{ width: `${fill}%`, ...vars }}
                      />
                    </span>
                    <span className="dist-count num">
                      {countReady ? fmtNum(item.total) : "—"}
                    </span>
                    <span className="dist-share num">
                      {countReady && dist.sum > 0 ? `${share.toFixed(share < 10 ? 1 : 0)}%` : "—"}
                    </span>
                  </button>
                </li>
              );
            })}
          </ol>
        <div className="readout-bar" aria-live="polite">
          {selectedCode === null ? (
            <span className="muted">点击按键查看完整计数</span>
          ) : (
            <>
              <strong>{selectedLabel ?? `其它输入 ${selectedCode}`}</strong>
              {countReady ? (
                <span className="readout-num num">{fmtNum(selected?.total ?? 0)} 次</span>
              ) : (
                <span className="readout-num num">—</span>
              )}
              <span className="muted">{countReady ? "所选日期" : "历史次数暂不可用"}</span>
            </>
          )}
        </div>
      </div>
      {others.length > 0 ? (
        <div className="others">
          <p className="others-title">其它输入</p>
          <div className="others-list">
            {others.map((row) => (
              <button
                className="other-chip"
                key={row.code}
                type="button"
                aria-pressed={selectedCode === row.code}
                aria-label={
                  countReady
                    ? `${row.label}，${fmtNum(row.total)} 次`
                    : `${row.label}，历史次数暂不可用`
                }
                onClick={() => onSelect(row.code)}
              >
                <span className="other-chip-label">{row.label}</span>
                <span className="num">{countReady ? fmtNum(row.total) : "—"}</span>
              </button>
            ))}
          </div>
        </div>
      ) : null}
      {kind === "gamepad" ? <p className="hint">Guide 键可能被系统接管，无法采集。</p> : null}
    </section>
  );
}
