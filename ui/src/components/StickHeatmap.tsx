// 摇杆停留热力圆盘（motion-dpi §4.6 S7）：本地 Canvas、25×25 停留量、青绿→明亮青绿单色强度。
//
// 边界：组件不查询 API；数值只经 lib/motionPresentation.heatCells 归一化（仅图形，秒数原值透传），
// 固定空间核（3×3）仅用于渲染平滑，绝不修改 summary.dwellSeconds。圆盘 y 向上：行 0（摇杆前推）
// 画在顶部；灰色中心死区 + 方向直接标签；scaleMaxSeconds 由页面取左右两图共同最大值传入（共同色标）。
// 读数入口：点击选格 + 原生 range 滑块（每图唯一 Tab 入口，绝无 625 个）；Escape 清除选择；
// 选中格显示精确停留时间与占活动时间比例（tooltip 不是唯一读数入口）。数据刷新无动画。
import { useEffect, useMemo, useRef } from "react";
import type { StickMotionSummary } from "../api/types";
import {
  dwellShareText,
  formatCellDwell,
  formatTravelR,
  heatCellColor,
  heatCellIntersectsDisc,
  heatCells,
} from "../lib/motionPresentation";
import { fmtDuration } from "../lib/format";
import { Skeleton } from "./Skeleton";

const GRID = 25;
const CELLS = GRID * GRID;
/** 画布逻辑边长（正方形，CSS 等比缩放，圆盘不被拉成椭圆） */
const DISC_PX = 288;
/** 固定空间核（渲染平滑用，3×3 加权；仅影响绘制，不改数值） */
const KERNEL = [1, 2, 1, 2, 4, 2, 1, 2, 1] as const;

export interface StickHeatmapProps {
  title: string;
  summary: StickMotionSummary;
  selectedBin: number | null;
  scaleMaxSeconds: number;
  onSelect: (bin: number | null) => void;
  loading?: boolean;
}

/** 格中心是否在圆盘内（格单位坐标） */
function insideDisc(row: number, col: number): boolean {
  return heatCellIntersectsDisc(row, col);
}

/** 选中格相对中心的方向文本（"中心" / "上2格·右3格"） */
function binPositionText(bin: number): string {
  const row = Math.floor(bin / GRID);
  const col = bin % GRID;
  const dx = col - (GRID - 1) / 2;
  const dy = row - (GRID - 1) / 2; // dy<0 = 圆盘上方（摇杆前推）
  if (dx === 0 && dy === 0) return "中心";
  const parts: string[] = [];
  if (dy < 0) parts.push(`上${-dy}格`);
  else if (dy > 0) parts.push(`下${dy}格`);
  if (dx < 0) parts.push(`左${-dx}格`);
  else if (dx > 0) parts.push(`右${dx}格`);
  return parts.join("·");
}

export function StickHeatmap({ title, summary, selectedBin, scaleMaxSeconds, onSelect, loading = false }: StickHeatmapProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // 纯函数换算：恰 625 格（非 625 属上游合同错误，heatCells 抛错——不补造数据）；不修改 summary.dwellSeconds
  const cells = useMemo(
    () => heatCells(summary.dwellSeconds, scaleMaxSeconds),
    [summary.dwellSeconds, scaleMaxSeconds],
  );
  const hasDwell = useMemo(() => cells.some((c) => c.intensity > 0), [cells]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    const dpr = typeof window !== "undefined" && window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
    canvas.width = DISC_PX * dpr;
    canvas.height = DISC_PX * dpr;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, DISC_PX, DISC_PX);

    const styles = getComputedStyle(document.documentElement);
    const divider = styles.getPropertyValue("--color-divider").trim() || "#E5E7EB";
    const dead = styles.getPropertyValue("--color-muted").trim() || "#E8F1F4";
    const primary = styles.getPropertyValue("--color-primary").trim() || "#0D9488";

    const cell = DISC_PX / GRID;
    const center = DISC_PX / 2;
    // 圆盘裁剪：格子被圆边裁齐（不出现方角）
    ctx.save();
    ctx.beginPath();
    ctx.arc(center, center, center - 1, 0, Math.PI * 2);
    ctx.clip();

    // 固定空间核平滑（仅渲染）：对 intensity 做 3×3 加权平均（圆盘内权重归一）
    for (let row = 0; row < GRID; row++) {
      for (let col = 0; col < GRID; col++) {
        if (!insideDisc(row, col)) continue;
        let acc = 0;
        let wsum = 0;
        for (let dr = -1; dr <= 1; dr++) {
          for (let dc = -1; dc <= 1; dc++) {
            const r = row + dr;
            const c = col + dc;
            if (r < 0 || r >= GRID || c < 0 || c >= GRID || !insideDisc(r, c)) continue;
            const w = KERNEL[(dr + 1) * 3 + (dc + 1)];
            acc += w * cells[r * GRID + c].intensity;
            wsum += w;
          }
        }
        const t = wsum > 0 ? acc / wsum : 0;
        if (t > 0.004) {
          ctx.fillStyle = heatCellColor(t);
          ctx.fillRect(col * cell - 0.5, row * cell - 0.5, cell + 1, cell + 1);
        }
      }
    }

    // 灰色中心死区（半透明标记，不遮死读数；精确值见读出行）
    ctx.beginPath();
    ctx.arc(center, center, cell * 2.2, 0, Math.PI * 2);
    ctx.globalAlpha = 0.6;
    ctx.fillStyle = dead;
    ctx.fill();
    ctx.globalAlpha = 1;

    // 选中格描边（圆盘内才描）
    if (selectedBin !== null && Number.isInteger(selectedBin) && selectedBin >= 0 && selectedBin < CELLS) {
      const row = Math.floor(selectedBin / GRID);
      const col = selectedBin % GRID;
      if (insideDisc(row, col)) {
        ctx.strokeStyle = primary;
        ctx.lineWidth = 2;
        ctx.strokeRect(col * cell + 1, row * cell + 1, cell - 2, cell - 2);
      }
    }
    ctx.restore();

    // 圆盘外圈
    ctx.beginPath();
    ctx.arc(center, center, center - 1, 0, Math.PI * 2);
    ctx.strokeStyle = divider;
    ctx.lineWidth = 1;
    ctx.stroke();
  }, [cells, selectedBin, loading]);

  if (loading) {
    return (
      <div className="card" role="status" aria-label={`${title}加载中`}>
        <h2 className="card-title">{title}</h2>
        <div style={{ marginTop: "var(--space-3)" }}>
          <Skeleton h="20px" w="60%" />
          <Skeleton h={`${DISC_PX}px`} style={{ marginTop: "var(--space-3)" }} />
        </div>
      </div>
    );
  }

  function onDiscClick(e: React.MouseEvent<HTMLCanvasElement>) {
    const rect = e.currentTarget.getBoundingClientRect();
    if (rect.width <= 0) return;
    const cellPx = rect.width / GRID;
    const x = (e.clientX - rect.left) / rect.width * 2 - 1;
    const y = (e.clientY - rect.top) / rect.height * 2 - 1;
    if (Math.hypot(x, y) > 1) {
      onSelect(null);
      return;
    }
    const col = Math.floor((e.clientX - rect.left) / cellPx);
    const row = Math.floor((e.clientY - rect.top) / cellPx);
    if (col < 0 || col >= GRID || row < 0 || row >= GRID || !insideDisc(row, col)) {
      onSelect(null); // 圆盘外点击 = 清除选择
      return;
    }
    onSelect(row * GRID + col);
  }

  const selectedCell = selectedBin !== null && selectedBin >= 0 && selectedBin < CELLS ? cells[selectedBin] : undefined;

  return (
    <div className="card">
      <h2 className="card-title">{title}</h2>
      <div className="heat-meta">
        <span>
          活动时长 <span className="num">{fmtDuration(summary.activeSeconds)}</span>
        </span>
        <span>
          累计行程 <span className="num">{formatTravelR(summary.travelR)}</span>
        </span>
      </div>
      <div className="heat-disc-wrap">
        <canvas
          ref={canvasRef}
          width={DISC_PX}
          height={DISC_PX}
          className="heat-disc"
          aria-hidden="true"
          onClick={onDiscClick}
        />
        <span className="heat-dir heat-dir-n" aria-hidden="true">上</span>
        <span className="heat-dir heat-dir-s" aria-hidden="true">下</span>
        <span className="heat-dir heat-dir-w" aria-hidden="true">左</span>
        <span className="heat-dir heat-dir-e" aria-hidden="true">右</span>
      </div>
      <input
        type="range"
        className="heat-slider"
        min={0}
        max={CELLS - 1}
        step={1}
        value={selectedBin ?? 0}
        aria-label={`${title}：选择热力格 0–${CELLS - 1}（12＝正上，312＝中心，612＝正下；Enter/方向键选格，Escape 清除）`}
        aria-valuetext={
          selectedCell
            ? `第 ${selectedCell.bin} 格（${binPositionText(selectedCell.bin)}），停留 ${formatCellDwell(selectedCell.seconds)}，占活动 ${dwellShareText(selectedCell.seconds, summary.activeSeconds)}`
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
      <div className="dev-detail" aria-live="polite">
        {selectedCell ? (
          <>
            <span style={{ fontWeight: 600 }}>第 {selectedCell.bin} 格（{binPositionText(selectedCell.bin)}）</span>
            <span>停留 <span className="num">{formatCellDwell(selectedCell.seconds)}</span></span>
            <span>占活动 <span className="num">{dwellShareText(selectedCell.seconds, summary.activeSeconds)}</span></span>
          </>
        ) : hasDwell ? (
          <span className="td-muted">点击圆盘或用滑块选择格子查看精确停留；Escape 清除选择。</span>
        ) : (
          <span className="td-muted">暂无摇杆停留数据。</span>
        )}
      </div>
    </div>
  );
}
