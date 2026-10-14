// 鼠标/手柄按键分布卡（input-live-trigger §4.7 S8）：外壳与控件委托 U9/U10；
// 保留详情/其它输入；只消费 props，历史 loading 不遮住实时图。
import { useMemo, type ReactNode } from "react";
import type { DeviceLiveView, TopKeyRow } from "../api/types";
import { motionControlSpecs } from "../lib/motionPresentation";
import { fmtNum } from "../lib/format";
import { MouseDeviceDiagram } from "./MouseDeviceDiagram";
import { GamepadDeviceDiagram } from "./GamepadDeviceDiagram";

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
  const historyState =
    historyStateProp ?? (loading ? "loading" : "ready");
  const specs = motionControlSpecs(kind);
  const lookup = useMemo(() => {
    const entries = new Map<number, TopKeyRow>();
    for (const row of rows)
      if (!entries.has(row.code)) entries.set(row.code, row);
    return entries;
  }, [rows]);
  const others = rows.filter(
    (row) => !specs.some((spec) => spec.code === row.code),
  );
  const selected = selectedCode === null ? null : lookup.get(selectedCode);
  const selectedLabel =
    selected?.label ??
    specs.find((spec) => spec.code === selectedCode)?.shortLabel;
  const countReady = historyState === "ready";
  const Diagram = kind === "mouse" ? MouseDeviceDiagram : GamepadDeviceDiagram;

  return (
    <section
      className="card peripheral-card"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          onSelect(null);
        }
      }}
    >
      <div className="section-heading">
        <div>
          <h2 className="card-title">按键分布</h2>
          <p className="card-sub">
            {kind === "mouse"
              ? "按下与滚动方向分别记录"
              : "Xbox 布局 · 肩键、扳机与各按键分别记录"}
          </p>
        </div>
        <span className="chip">{specs.length} 类控件</span>
      </div>
      {liveToolbar}
      <Diagram
        rows={rows}
        historyState={historyState}
        selectedCode={selectedCode}
        onSelect={onSelect}
        live={live}
      />
      <div className="peripheral-readout" aria-live="polite">
        {selectedCode === null ? (
          <span>点击按键查看完整计数</span>
        ) : (
          <>
            <strong>{selectedLabel ?? `其它输入 ${selectedCode}`}</strong>
            {countReady ? (
              <span className="num">{fmtNum(selected?.total ?? 0)} 次</span>
            ) : (
              <span className="num">—</span>
            )}
            <span>{countReady ? "所选日期" : "历史次数暂不可用"}</span>
          </>
        )}
      </div>
      {others.length > 0 && (
        <div>
          <p className="dev-other-title">其它输入</p>
          <div className="dev-other-list">
            {others.map((row) => (
              <button
                className="dev-other-chip"
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
                <span className="dev-other-label">{row.label}</span>
                <span className="num">
                  {countReady ? fmtNum(row.total) : "—"}
                </span>
              </button>
            ))}
          </div>
        </div>
      )}
      {kind === "gamepad" && (
        <p className="chart-hint">Guide 键可能被系统接管，无法采集。</p>
      )}
    </section>
  );
}
