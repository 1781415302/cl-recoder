// 鼠标/手柄控件图（motion-dpi §4.6 S7）：本地 SVG 参考外观 + 原生 button 点击层。
//
// 边界：组件不查询 API；几何来自 lib/motionPresentation（hit ≥44×44、两两不重叠，测试钉死），
// 组件以固定画布尺寸渲染 + 容器横向滚动（不缩放，保证点击区域不缩水）。计数按 code 直接查找
// （不 SUM、不靠 label 定位、缺行=0）；9/17 码全部可见，未知 code 在「其它输入」保留入口，
// 完整集合由页面明细表呈现。所有控件有字母/箭头标记（不只靠颜色）；ABXY 用低饱和 Xbox 语义色
// （主题令牌 + 低不透明度）。点击/Enter/Space 仅 onSelect(code)，Escape 清除；数据刷新无动画。
import { useMemo } from "react";
import type { TopKeyRow } from "../api/types";
import {
  MOTION_CANVAS,
  motionControlSpecs,
  type MotionControlSpec,
} from "../lib/motionPresentation";
import { fmtCompact, fmtNum } from "../lib/format";
import { Skeleton } from "./Skeleton";

export interface PeripheralControlsStatsProps {
  kind: "mouse" | "gamepad";
  rows: TopKeyRow[];
  selectedCode: number | null;
  onSelect: (code: number | null) => void;
  loading?: boolean;
}

/** ABXY 低饱和 Xbox 语义色（主题令牌；所有按钮另有字母，不只靠颜色） */
const ABXY_FILL: Record<number, string> = {
  1: "var(--color-positive)", // A
  2: "var(--color-danger)", // B
  3: "var(--color-accent)", // Y
  4: "var(--chart-2)", // X
};

function arrowPoints(c: MotionControlSpec): string {
  const { cx, cy, w, h } = c;
  switch (c.dir ?? "up") {
    case "down":
      return `${cx},${cy + h / 2} ${cx - w / 2},${cy - h / 2} ${cx + w / 2},${cy - h / 2}`;
    case "left":
      return `${cx - w / 2},${cy} ${cx + w / 2},${cy - h / 2} ${cx + w / 2},${cy + h / 2}`;
    case "right":
      return `${cx + w / 2},${cy} ${cx - w / 2},${cy - h / 2} ${cx - w / 2},${cy + h / 2}`;
    default:
      return `${cx},${cy - h / 2} ${cx - w / 2},${cy + h / 2} ${cx + w / 2},${cy + h / 2}`;
  }
}

/** 单控件形状 + 标记字母（aria-hidden 装饰层；可读名称由按钮 aria-label 提供） */
function ControlShape({ c }: { c: MotionControlSpec }) {
  const abxy = ABXY_FILL[c.code];
  if (c.shape === "circle") {
    return (
      <g>
        <circle
          cx={c.cx}
          cy={c.cy}
          r={c.w / 2}
          style={abxy ? { fill: abxy, fillOpacity: 0.16, stroke: abxy, strokeOpacity: 0.55 } : undefined}
          className={abxy ? undefined : "mc-shape"}
        />
        <text
          className="mc-label"
          x={c.cx}
          y={c.cy - 2}
          style={{ fontSize: c.shortLabel.length <= 2 ? 14 : 12, fontWeight: c.shortLabel.length <= 2 ? 600 : 400 }}
          dominantBaseline="central"
        >
          {c.shortLabel}
        </text>
      </g>
    );
  }
  if (c.shape === "arrow") {
    return <polygon points={arrowPoints(c)} style={{ fill: "var(--color-primary)", fillOpacity: 0.85 }} />;
  }
  return (
    <g>
      <rect
        x={c.cx - c.w / 2}
        y={c.cy - c.h / 2}
        width={c.w}
        height={c.h}
        rx={c.shape === "pill" ? Math.min(c.w, c.h) / 2 : 10}
        className="mc-shape"
      />
      <text
        className="mc-label"
        x={c.cx}
        y={c.cy}
        style={{ fontSize: c.shortLabel.length <= 2 ? 14 : 11 }}
        dominantBaseline="central"
      >
        {c.shortLabel}
      </text>
    </g>
  );
}

export function PeripheralControlsStats({ kind, rows, selectedCode, onSelect, loading = false }: PeripheralControlsStatsProps) {
  const specs = motionControlSpecs(kind);
  const canvas = MOTION_CANVAS[kind];

  // code → 行（后端行按 code 聚合，正常无重复；重复时取首个，绝不求和）
  const rowByCode = useMemo(() => {
    const m = new Map<number, TopKeyRow>();
    for (const r of rows) if (!m.has(r.code)) m.set(r.code, r);
    return m;
  }, [rows]);
  // 参考图未绘制的输入：在「其它输入」保留入口（完整行仍由页面明细表呈现）
  const unmapped = useMemo(() => {
    const mapped = new Set(specs.map((c) => c.code));
    return rows.filter((r) => !mapped.has(r.code));
  }, [rows, specs]);

  const title = kind === "mouse" ? "鼠标控件" : "手柄控件";

  function onCardKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      onSelect(null);
    }
  }

  const selectedRow = selectedCode !== null ? rowByCode.get(selectedCode) : undefined;
  const selectedFallback =
    selectedCode !== null ? specs.find((c) => c.code === selectedCode)?.shortLabel : undefined;

  return (
    <div className="card" onKeyDown={onCardKeyDown}>
      <h2 className="card-title">{title}</h2>
      <p className="card-sub">
        {kind === "mouse" ? "左/右/中键、X1/X2 与四向滚轮" : "Xbox 参考外观：扳机/肩键/摇杆/十字/ABXY"} ·
        已记录 {fmtNum(rows.length)} 项
      </p>
      {loading ? (
        <div role="status" aria-label={`${title}加载中`} style={{ marginTop: "var(--space-3)" }}>
          <Skeleton h="44px" />
          <Skeleton h={`${Math.round((canvas.height * 320) / canvas.width)}px`} style={{ marginTop: "var(--space-3)" }} />
        </div>
      ) : (
        <>
          <div className="mc-stage" style={{ marginTop: "var(--space-3)" }}>
            <div className="mc-stage-inner" style={{ width: canvas.width, height: canvas.height }}>
              <svg
                width={canvas.width}
                height={canvas.height}
                viewBox={`0 0 ${canvas.width} ${canvas.height}`}
                aria-hidden="true"
                focusable="false"
                className="mc-svg"
              >
                {kind === "mouse" ? (
                  <g>
                    <path
                      className="mc-body"
                      d="M 160 12 C 78 12 24 66 24 158 L 24 352 C 24 400 82 418 160 418 C 238 418 296 400 296 352 L 296 158 C 296 66 242 12 160 12 Z"
                    />
                    <line x1={24} y1={176} x2={296} y2={176} className="mc-line" />
                    <line x1={160} y1={12} x2={160} y2={176} className="mc-line" />
                    <rect x={152} y={70} width={16} height={56} rx={8} className="mc-detail" />
                  </g>
                ) : (
                  <g>
                    <path
                      className="mc-body"
                      transform="scale(0.78)"
                      d="M 122 64 C 80 68 58 106 52 158 C 46 212 44 266 52 318 C 60 372 76 432 116 460 C 148 482 180 462 190 426 C 198 396 206 440 244 444 C 288 448 332 448 380 448 C 428 448 472 448 516 444 C 554 440 562 396 570 426 C 580 462 612 482 644 460 C 684 432 700 372 708 318 C 716 266 714 212 708 158 C 702 106 680 68 638 64 C 556 56 468 84 380 84 C 292 84 204 56 122 64 Z"
                    />
                    {/* 十字 D-pad 底座（四臂点击区由控件层提供） */}
                    <rect x={263} y={217} width={44} height={132} rx={8} className="mc-detail" />
                    <rect x={219} y={261} width={132} height={44} rx={8} className="mc-detail" />
                  </g>
                )}
                {specs.map((c) => (
                  <ControlShape key={c.code} c={c} />
                ))}
              </svg>
              {specs.map((c) => {
                const total = rowByCode.get(c.code)?.total ?? 0;
                const selected = selectedCode === c.code;
                return (
                  <button
                    key={c.code}
                    type="button"
                    className={`mc-hit${c.shape === "circle" ? " mc-hit-round" : ""}`}
                    style={{ left: c.hit.x, top: c.hit.y, width: c.hit.w, height: c.hit.h }}
                    aria-pressed={selected}
                    aria-label={`${c.shortLabel}，编码 ${c.code}，${fmtNum(total)} 次${selected ? "，已选中" : ""}`}
                    onClick={() => onSelect(c.code)}
                  >
                    <span className="mc-count" aria-hidden="true">{fmtCompact(total)}</span>
                  </button>
                );
              })}
            </div>
          </div>
          <div className="dev-detail" aria-live="polite">
            {selectedCode === null ? (
              <span className="td-muted">点击控件查看精确数值；Escape 清除选择。未知码见「其它输入」与完整明细表。</span>
            ) : selectedRow ? (
              <>
                <span style={{ fontWeight: 600 }}>{selectedRow.label}</span>
                <span className="mono">（编码 {selectedRow.code}）</span>
                <span className="num">{fmtNum(selectedRow.total)} 次</span>
              </>
            ) : (
              <>
                <span style={{ fontWeight: 600 }}>{selectedFallback ?? `编码 ${selectedCode}`}</span>
                <span className="mono">（编码 {selectedCode}）</span>
                <span className="td-muted">所选范围内无记录（0 次）</span>
              </>
            )}
          </div>
          {unmapped.length > 0 ? (
            <div className="dev-other">
              <p className="dev-other-title">其它输入（参考图未绘制）· {unmapped.length} 项</p>
              <div className="dev-other-list">
                {unmapped.map((r) => (
                  <button
                    key={r.code}
                    type="button"
                    className="dev-other-chip"
                    aria-pressed={selectedCode === r.code}
                    onClick={() => onSelect(r.code)}
                  >
                    <span className="dev-other-label">{r.label}</span>
                    <span className="mc-count">{fmtNum(r.total)}</span>
                  </button>
                ))}
              </div>
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}
