// 真实 Xbox 手柄分布图（input-live-trigger §4.7 S8）：17 控件、双摇杆点、LT/RT 行程填充；
// 历史计数不常驻；Guide 可标“不支持实时”但仍可选；摇杆/扳机叠加层 pointer-events:none。
import { useMemo } from "react";
import type { TopKeyRow } from "../api/types";
import {
  MOTION_CANVAS,
  motionControlSpecs,
  peripheralShellSpec,
  stickTipPosition,
  type MotionDirection,
} from "../lib/motionPresentation";
import { fmtNum } from "../lib/format";
import type { DeviceDiagramProps } from "./MouseDeviceDiagram";

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
  unsupported: boolean,
): string {
  const base =
    historyState !== "ready"
      ? `${label}，历史次数暂不可用`
      : `${label}，${fmtNum(total ?? 0)} 次`;
  return unsupported ? `${base}，不支持实时` : base;
}

function historyTitle(
  historyState: DeviceDiagramProps["historyState"],
  label: string,
  total: number | undefined,
  unsupported: boolean,
): string {
  const base =
    historyState !== "ready"
      ? `${label}：历史次数暂不可用`
      : `${label}：${fmtNum(total ?? 0)} 次`;
  return unsupported ? `${base}（不支持实时）` : base;
}

function triggerPct(value: number | null | undefined): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.round(Math.min(1, Math.max(0, value)) * 100);
}

export function GamepadDeviceDiagram({
  rows,
  historyState,
  selectedCode,
  onSelect,
  live,
}: DeviceDiagramProps) {
  const specs = motionControlSpecs("gamepad");
  const size = MOTION_CANVAS.gamepad;
  const shell = peripheralShellSpec("gamepad");
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
  const unsupported = useMemo(
    () => new Set(live?.unsupportedCodes ?? []),
    [live?.unsupportedCodes],
  );
  const analog = live?.analog ?? null;
  const leftPct = triggerPct(analog?.leftTrigger);
  const rightPct = triggerPct(analog?.rightTrigger);

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
          {shell.stickWells.map((well) => (
            <circle
              key={well.code}
              className="peripheral-inset"
              cx={well.cx}
              cy={well.cy}
              r={well.radius + 8}
            />
          ))}
          <path
            className="peripheral-inset"
            d="M213 162h44v44h44v44h-44v44h-44v-44h-44v-44h44Z"
          />
          {/* LT/RT 行程填充（原始 0..1 → 整数百分比；不冒充历史 bin） */}
          {[
            { code: 6, pct: leftPct },
            { code: 8, pct: rightPct },
          ].map(({ code, pct }) => {
            const spec = specs.find((s) => s.code === code);
            if (!spec) return null;
            const fillH = (spec.h * pct) / 100;
            return (
              <g key={`trig-${code}`} pointerEvents="none">
                <rect
                  className="trigger-travel-track"
                  x={spec.cx - spec.w / 2}
                  y={spec.cy - spec.h / 2}
                  width={spec.w}
                  height={spec.h}
                  rx={6}
                />
                <rect
                  className="trigger-travel-fill"
                  x={spec.cx - spec.w / 2}
                  y={spec.cy + spec.h / 2 - fillH}
                  width={spec.w}
                  height={fillH}
                  rx={6}
                />
              </g>
            );
          })}
          {/* 摇杆点与细方向线：仅在 stickWell 内，不挡 LS/RS 点击 */}
          {shell.stickWells.map((well) => {
            const point =
              well.side === "left"
                ? analog?.leftStick
                : analog?.rightStick;
            const tip = stickTipPosition(
              well,
              point?.x ?? 0,
              point?.y ?? 0,
            );
            return (
              <g key={`stick-${well.code}`} pointerEvents="none">
                <line
                  className="stick-tip-line"
                  x1={well.cx}
                  y1={well.cy}
                  x2={tip.x}
                  y2={tip.y}
                />
                <circle
                  className="stick-tip-dot"
                  cx={tip.x}
                  cy={tip.y}
                  r={5}
                />
              </g>
            );
          })}
        </svg>
        {specs.map((spec) => {
          const total = lookup.get(spec.code)?.total;
          const isUnsupported = unsupported.has(spec.code);
          const isLive = held.has(spec.code);
          const isPulse = !isLive && pulse.has(spec.code);
          const selected = selectedCode === spec.code;
          const isTrigger = spec.code === 6 || spec.code === 8;
          const pct =
            spec.code === 6 ? leftPct : spec.code === 8 ? rightPct : null;
          return (
            <button
              type="button"
              key={spec.code}
              className={[
                "peripheral-button",
                "gamepad-button",
                isLive ? "is-live" : "",
                isPulse ? "is-pulse" : "",
                isUnsupported ? "is-unsupported" : "",
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
              aria-label={historyAria(
                historyState,
                spec.shortLabel,
                total,
                isUnsupported,
              )}
              title={historyTitle(
                historyState,
                spec.shortLabel,
                total,
                isUnsupported,
              )}
              onClick={() => onSelect(spec.code)}
            >
              {spec.shape === "arrow" ? (
                <DirectionMark direction={spec.dir ?? "up"} />
              ) : (
                <span className="peripheral-label" aria-hidden="true">
                  {spec.shortLabel}
                  {isUnsupported ? (
                    <span className="peripheral-unsupported">不支持实时</span>
                  ) : null}
                  {isTrigger && analog ? (
                    <span className="trigger-pct" data-trigger-pct={pct}>
                      {pct}%
                    </span>
                  ) : null}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
