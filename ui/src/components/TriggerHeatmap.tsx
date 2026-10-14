// LT/RT 历史行程热力条（input-live-trigger §4.7 S8）：101 格横条，仅历史；
// 不接收实时位置 props；LT/RT 共同 scaleMaxSeconds 由页面传入。
import { useMemo } from "react";
import type { TriggerHeatSummary } from "../api/types";
import {
  triggerHeatCells,
  formatCellDwell,
  dwellShareText,
  heatCellColor,
} from "../lib/motionPresentation";
import { fmtDuration } from "../lib/format";
import { Skeleton } from "./Skeleton";

export interface TriggerHeatmapProps {
  title: string;
  summary: TriggerHeatSummary;
  scaleMaxSeconds: number;
  selectedBin: number | null;
  onSelect: (bin: number | null) => void;
  loading?: boolean;
}

const BIN_COUNT = 101;

export function TriggerHeatmap({
  title,
  summary,
  scaleMaxSeconds,
  selectedBin,
  onSelect,
  loading = false,
}: TriggerHeatmapProps) {
  const cells = useMemo(
    () => triggerHeatCells(summary.dwellSeconds, scaleMaxSeconds),
    [summary.dwellSeconds, scaleMaxSeconds],
  );
  const cell = selectedBin !== null ? cells[selectedBin] : undefined;
  const hasData = cells.some((c) => c.seconds > 0);

  return (
    <section className="card heat-card trigger-heat-card">
      <div className="heat-heading">
        <h2 className="card-title">{title}</h2>
        <span className="chip">
          {summary.side === "left" ? "LT" : "RT"} · 0%–100%
        </span>
      </div>
      {loading ? (
        <Skeleton h="120px" />
      ) : (
        <>
          <div
            className="trigger-heat-bar"
            role="img"
            aria-label={`${title}：行程 0% 到 100% 的停留热力`}
          >
            {cells.map((c) => (
              <button
                key={c.bin}
                type="button"
                className={[
                  "trigger-heat-cell",
                  selectedBin === c.bin ? "is-selected" : "",
                ]
                  .filter(Boolean)
                  .join(" ")}
                style={{ background: heatCellColor(c.intensity) }}
                aria-pressed={selectedBin === c.bin}
                aria-label={`行程 ${c.percent}%，停留 ${formatCellDwell(c.seconds)}`}
                title={`行程 ${c.percent}% · ${formatCellDwell(c.seconds)}`}
                onClick={() =>
                  onSelect(selectedBin === c.bin ? null : c.bin)
                }
              />
            ))}
          </div>
          <div
            className="heat-scale"
            aria-label={`停留色标：0 到 ${formatCellDwell(scaleMaxSeconds)}`}
          >
            <span className="heat-scale-bar" aria-hidden="true" />
            <div className="heat-scale-values">
              <span>0%</span>
              <span>50%</span>
              <span>100%</span>
            </div>
          </div>
          <div className="heat-stats">
            <div>
              <span className="heat-stats-label">活动时长</span>
              <strong className="heat-stats-value">
                {fmtDuration(summary.activeSeconds)}
              </strong>
            </div>
            <div>
              <span className="heat-stats-label">色标上限</span>
              <strong className="heat-stats-value">
                {formatCellDwell(scaleMaxSeconds)}
              </strong>
            </div>
          </div>
          <input
            type="range"
            className="heat-slider"
            min={0}
            max={BIN_COUNT - 1}
            step={1}
            value={selectedBin ?? 0}
            aria-label={`${title}：用方向键选择行程，Escape 清除`}
            aria-valuetext={
              cell
                ? `行程 ${cell.percent}%，停留 ${formatCellDwell(cell.seconds)}`
                : "未选择"
            }
            onChange={(e) => onSelect(Number(e.currentTarget.value))}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                onSelect(null);
              }
            }}
          />
          <div className="heat-readout" aria-live="polite">
            {cell ? (
              <>
                <strong>行程 {cell.percent}%</strong>
                <span>{formatCellDwell(cell.seconds)}</span>
                <span>
                  占活动{" "}
                  {dwellShareText(cell.seconds, summary.activeSeconds)}
                </span>
              </>
            ) : (
              <span>
                {hasData
                  ? "点选行程，或用方向键查看停留时间"
                  : "从新版开始记录"}
              </span>
            )}
          </div>
        </>
      )}
    </section>
  );
}
