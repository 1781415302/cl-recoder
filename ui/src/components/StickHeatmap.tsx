import { useEffect, useMemo, useRef } from "react";
import type { StickMotionSummary } from "../api/types";
import {
  heatCells,
  heatCellIntersectsDisc,
  formatCellDwell,
  formatTravelR,
  dwellShareText,
  type HeatCell,
} from "../lib/motionPresentation";
import { fmtDuration } from "../lib/format";
import { IconStick } from "./icons";
import { Skeleton } from "./Skeleton";
export interface StickHeatmapProps {
  title: string;
  summary: StickMotionSummary;
  selectedBin: number | null;
  scaleMaxSeconds: number;
  onSelect: (bin: number | null) => void;
  loading?: boolean;
}
const GRID = 25,
  COUNT = 625,
  SIZE = 256;
function positionText(bin: number): string {
  const x = ((bin % GRID) - 12) / 12.5,
    y = (12 - Math.floor(bin / GRID)) / 12.5;
  if (x === 0 && y === 0) return "中心";
  return [
    y === 0 ? "" : `${y > 0 ? "上" : "下"} ${Math.round(Math.abs(y) * 100)}%`,
    x === 0 ? "" : `${x > 0 ? "右" : "左"} ${Math.round(Math.abs(x) * 100)}%`,
  ]
    .filter(Boolean)
    .join(" · ");
}
/** 纹理只插值显示强度；停留时间与选格读数始终使用原始网格。 */
function paintDensity(
  canvas: HTMLCanvasElement,
  cells: readonly HeatCell[],
  selectedBin: number | null,
) {
  const context = canvas.getContext("2d");
  if (!context) return;
  const dpr = window.devicePixelRatio || 1;
  canvas.width = SIZE * dpr;
  canvas.height = SIZE * dpr;
  context.setTransform(dpr, 0, 0, dpr, 0, 0);
  const styles = getComputedStyle(document.documentElement);
  const color = (name: string) => styles.getPropertyValue(name).trim();
  const rgb = (name: string) =>
    color(name)
      .slice(1)
      .match(/.{2}/g)!
      .map((part) => parseInt(part, 16));
  const palette = [rgb("--heat-low"), rgb("--heat-mid"), rgb("--heat-high")];
  const texture = document.createElement("canvas");
  texture.width = texture.height = 125;
  const textureContext = texture.getContext("2d");
  if (!textureContext) return;
  const pixels = textureContext.createImageData(125, 125);
  const intensity = (row: number, col: number) =>
    cells[
      Math.max(0, Math.min(24, row)) * GRID + Math.max(0, Math.min(24, col))
    ].intensity;
  for (let y = 0; y < 125; y++)
    for (let x = 0; x < 125; x++) {
      const gx = (x + 0.5) / 5 - 0.5,
        gy = (y + 0.5) / 5 - 0.5,
        c = Math.floor(gx),
        r = Math.floor(gy),
        fx = gx - c,
        fy = gy - r;
      const top = intensity(r, c) * (1 - fx) + intensity(r, c + 1) * fx;
      const bottom =
        intensity(r + 1, c) * (1 - fx) + intensity(r + 1, c + 1) * fx;
      const t = top * (1 - fy) + bottom * fy,
        index = t < 0.5 ? 0 : 1,
        mix = t < 0.5 ? t * 2 : (t - 0.5) * 2,
        offset = (y * 125 + x) * 4;
      for (let channel = 0; channel < 3; channel++)
        pixels.data[offset + channel] = Math.round(
          palette[index][channel] * (1 - mix) +
            palette[index + 1][channel] * mix,
        );
      pixels.data[offset + 3] = 255;
    }
  textureContext.putImageData(pixels, 0, 0);
  const center = SIZE / 2;
  context.clearRect(0, 0, SIZE, SIZE);
  context.save();
  context.beginPath();
  context.arc(center, center, center - 1, 0, Math.PI * 2);
  context.clip();
  context.drawImage(texture, 0, 0, SIZE, SIZE);
  context.strokeStyle = color("--color-border");
  context.lineWidth = 0.8;
  for (const radius of [center * 0.2, center * 0.5, center * 0.8]) {
    context.beginPath();
    context.arc(center, center, radius, 0, Math.PI * 2);
    context.stroke();
  }
  context.setLineDash([3, 5]);
  context.beginPath();
  context.moveTo(0, center);
  context.lineTo(SIZE, center);
  context.moveTo(center, 0);
  context.lineTo(center, SIZE);
  context.stroke();
  context.setLineDash([]);
  if (
    selectedBin !== null &&
    heatCellIntersectsDisc(Math.floor(selectedBin / GRID), selectedBin % GRID)
  ) {
    const x = (((selectedBin % GRID) + 0.5) * SIZE) / GRID,
      y = ((Math.floor(selectedBin / GRID) + 0.5) * SIZE) / GRID;
    context.beginPath();
    context.arc(x, y, 6, 0, Math.PI * 2);
    context.strokeStyle = "white";
    context.lineWidth = 4;
    context.stroke();
    context.strokeStyle = color("--color-primary");
    context.lineWidth = 2;
    context.stroke();
  }
  context.restore();
  context.beginPath();
  context.arc(center, center, center - 1, 0, Math.PI * 2);
  context.strokeStyle = color("--color-border");
  context.lineWidth = 1;
  context.stroke();
}
export function StickHeatmap({
  title,
  summary,
  selectedBin,
  scaleMaxSeconds,
  onSelect,
  loading = false,
}: StickHeatmapProps) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const cells = useMemo(
    () => heatCells(summary.dwellSeconds, scaleMaxSeconds),
    [summary.dwellSeconds, scaleMaxSeconds],
  );
  const cell = selectedBin !== null ? cells[selectedBin] : undefined;
  const hasData = cells.some((c) => c.seconds > 0);
  useEffect(() => {
    if (canvas.current) paintDensity(canvas.current, cells, selectedBin);
  }, [cells, selectedBin, loading]);
  return (
    <section className="card heat-card">
      <div className="heat-heading">
        <h2 className="card-title">{title}</h2>
        <IconStick size={20} />
      </div>
      {loading ? (
        <Skeleton h="256px" />
      ) : (
        <>
          <div className="heat-disc-wrap">
            <canvas
              ref={canvas}
              className="heat-disc"
              width={SIZE}
              height={SIZE}
              aria-hidden="true"
              onClick={(e) => {
                const rect = e.currentTarget.getBoundingClientRect(),
                  x = (e.clientX - rect.left) / rect.width,
                  y = (e.clientY - rect.top) / rect.height;
                const col = Math.floor(x * GRID),
                  row = Math.floor(y * GRID);
                if (
                  Math.hypot(x * 2 - 1, y * 2 - 1) > 1 ||
                  col < 0 ||
                  col >= GRID ||
                  row < 0 ||
                  row >= GRID
                ) {
                  onSelect(null);
                  return;
                }
                onSelect(row * GRID + col);
              }}
            />
            <span className="heat-dir heat-dir-n" aria-hidden="true">
              上
            </span>
            <span className="heat-dir heat-dir-s" aria-hidden="true">
              下
            </span>
            <span className="heat-dir heat-dir-w" aria-hidden="true">
              左
            </span>
            <span className="heat-dir heat-dir-e" aria-hidden="true">
              右
            </span>
          </div>
          <div
            className="heat-scale"
            aria-label={`停留色标：0 到 ${formatCellDwell(scaleMaxSeconds)}`}
          >
            <span className="heat-scale-bar" aria-hidden="true" />
            <div className="heat-scale-values">
              <span>0 秒</span>
              <span>{formatCellDwell(scaleMaxSeconds * 0.25)}</span>
              <span>{formatCellDwell(scaleMaxSeconds)}</span>
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
              <span
                className="heat-stats-label"
                title="1 R 为从摇杆中心到满幅边缘的行程"
              >
                累计行程
              </span>
              <strong className="heat-stats-value">
                {formatTravelR(summary.travelR)}
              </strong>
            </div>
          </div>
          <input
            type="range"
            className="heat-slider"
            min={0}
            max={COUNT - 1}
            step={1}
            value={selectedBin ?? 312}
            aria-label={`${title}：用方向键选择位置，Escape 清除`}
            aria-valuetext={
              cell
                ? `${positionText(cell.bin)}，停留 ${formatCellDwell(cell.seconds)}`
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
                <strong>{positionText(cell.bin)}</strong>
                <span>{formatCellDwell(cell.seconds)}</span>
                <span>
                  占活动 {dwellShareText(cell.seconds, summary.activeSeconds)}
                </span>
              </>
            ) : (
              <span>
                {hasData
                  ? "点选位置，或用方向键查看停留时间"
                  : "所选日期没有摇杆停留记录"}
              </span>
            )}
          </div>
        </>
      )}
    </section>
  );
}
