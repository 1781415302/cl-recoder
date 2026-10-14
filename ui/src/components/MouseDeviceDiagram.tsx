// 真实鼠标分布图（input-live-trigger §4.7 S8）：一体外壳 + 九控件；
// 历史计数只进详情/tooltip，不常驻按钮；实时 held/pulse 只消费 live props。
import { useMemo } from "react";
import type { DeviceLiveView, TopKeyRow } from "../api/types";
import {
  MOTION_CANVAS,
  motionControlSpecs,
  peripheralShellSpec,
  type MotionDirection,
} from "../lib/motionPresentation";
import { fmtNum } from "../lib/format";

export interface DeviceDiagramProps {
  rows: readonly TopKeyRow[];
  historyState: "loading" | "ready" | "error";
  selectedCode: number | null;
  onSelect: (code: number | null) => void;
  live: DeviceLiveView | null;
}

function DirectionMark({ direction }: { direction: MotionDirection }) {
  const rotation = { up: 0, right: 90, down: 180, left: 270 }[direction];
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      aria-hidden="true"
    >
      <path d="m3 10 5-5 5 5" transform={`rotate(${rotation} 8 8)`} />
    </svg>
  );
}

function historyAria(
  historyState: DeviceDiagramProps["historyState"],
  label: string,
  total: number | undefined,
): string {
  if (historyState !== "ready") return `${label}，历史次数暂不可用`;
  return `${label}，${fmtNum(total ?? 0)} 次`;
}

function historyTitle(
  historyState: DeviceDiagramProps["historyState"],
  label: string,
  total: number | undefined,
): string {
  if (historyState !== "ready") return `${label}：历史次数暂不可用`;
  return `${label}：${fmtNum(total ?? 0)} 次`;
}

export function MouseDeviceDiagram({
  rows,
  historyState,
  selectedCode,
  onSelect,
  live,
}: DeviceDiagramProps) {
  const specs = motionControlSpecs("mouse");
  const size = MOTION_CANVAS.mouse;
  const shell = peripheralShellSpec("mouse");
  const lookup = useMemo(() => {
    const entries = new Map<number, TopKeyRow>();
    for (const row of rows)
      if (!entries.has(row.code)) entries.set(row.code, row);
    return entries;
  }, [rows]);
  const held = useMemo(() => new Set(live?.heldCodes ?? []), [live?.heldCodes]);
  const pulse = useMemo(
    () => new Set(live?.pulseCodes ?? []),
    [live?.pulseCodes],
  );

  return (
    <div
      className="peripheral-stage"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          onSelect(null);
        }
      }}
    >
      <div
        className="peripheral-board"
        style={{ width: size.width, height: size.height }}
      >
        <svg
          className="peripheral-svg"
          width={size.width}
          height={size.height}
          viewBox={`0 0 ${size.width} ${size.height}`}
          aria-hidden="true"
        >
          <path className="peripheral-shell" d={shell.bodyPath} />
          {shell.seamPaths.map((d) => (
            <path key={d} className="peripheral-seam" d={d} />
          ))}
          <rect
            className="peripheral-inset"
            x="150"
            y="70"
            width="20"
            height="64"
            rx="10"
          />
        </svg>
        {specs.map((spec) => {
          const total = lookup.get(spec.code)?.total;
          const isLive = held.has(spec.code);
          const isPulse = !isLive && pulse.has(spec.code);
          const selected = selectedCode === spec.code;
          return (
            <button
              type="button"
              key={spec.code}
              className={[
                "peripheral-button",
                "mouse-button",
                isLive ? "is-live" : "",
                isPulse ? "is-pulse" : "",
              ]
                .filter(Boolean)
                .join(" ")}
              data-code={spec.code}
              data-shape={spec.shape}
              style={{
                left: spec.hit.x,
                top: spec.hit.y,
                width: spec.hit.w,
                height: spec.hit.h,
              }}
              aria-pressed={selected}
              aria-label={historyAria(historyState, spec.shortLabel, total)}
              title={historyTitle(historyState, spec.shortLabel, total)}
              onClick={() => onSelect(spec.code)}
            >
              {spec.shape === "arrow" ? (
                <DirectionMark direction={spec.dir ?? "up"} />
              ) : (
                <span className="peripheral-label" aria-hidden="true">
                  {spec.shortLabel}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
