// 真实 Xbox 手柄分布图（input-live-trigger §4.7 S8）：绘制式手柄、17 控件、双摇杆点、LT/RT 行程填充；
// 历史计数不常驻；Guide 可标“不支持实时”但仍可选；摇杆/扳机叠加层 pointer-events:none。
import { useId, useMemo, useState } from "react";
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
import { SkinGroup, totalsByCode, useControlHeat } from "./diagramSkin";

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

const ABXY_CLASS: Record<number, string> = {
  1: "skin--abxy skin--a",
  2: "skin--abxy skin--b",
  3: "skin--abxy skin--y",
  4: "skin--abxy skin--x",
};
/** 十字底座：以上/左臂中心推出中心点与十字多边形（厚 38、臂长 60，描边圆角） */
function dpadGeometry(
  up: { cx: number },
  left: { cy: number },
): { dpad: { x: number; y: number }; dpadCross: string } {
  const x = up.cx;
  const y = left.cy;
  const t = 19;
  const r = 60;
  const p = [
    [x - t, y - r], [x + t, y - r], [x + t, y - t], [x + r, y - t],
    [x + r, y + t], [x + t, y + t], [x + t, y + r], [x - t, y + r],
    [x - t, y + t], [x - r, y + t], [x - r, y - t], [x - t, y - t],
  ];
  return {
    dpad: { x, y },
    dpadCross: `M${p.map(([px, py]) => `${px} ${py}`).join("L")}Z`,
  };
}

export function GamepadDeviceDiagram({
  rows,
  historyState,
  selectedCode,
  onSelect,
  live,
  heat = true,
}: DeviceDiagramProps) {
  const specs = motionControlSpecs("gamepad");
  const size = MOTION_CANVAS.gamepad;
  const shell = peripheralShellSpec("gamepad");
  const uid = useId().replace(/[^a-zA-Z0-9]/g, "");
  const sheenId = `${uid}-sheen`;
  const bodyId = `${uid}-body`;
  const [hover, setHover] = useState<number | null>(null);
  const lookup = useMemo(() => {
    const entries = new Map<number, TopKeyRow>();
    for (const row of rows)
      if (!entries.has(row.code)) entries.set(row.code, row);
    return entries;
  }, [rows]);
  const totals = useMemo(() => totalsByCode(rows), [rows]);
  const codes = useMemo(() => specs.map((s) => s.code), [specs]);
  const heatVars = useControlHeat(codes, totals, heat && historyState === "ready");
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

  function skin(code: number) {
    const isLive = held.has(code);
    return {
      code,
      vars: heatVars.get(code),
      sheenId,
      state: {
        selected: selectedCode === code,
        live: isLive,
        pulse: !isLive && pulse.has(code),
        hover: hover === code,
        unsupported: unsupported.has(code),
      },
    };
  }
  const spec = (code: number) => specs.find((s) => s.code === code)!;
  const guide = spec(11);
  const { dpad, dpadCross } = dpadGeometry(spec(14), spec(16));

  return (
    <div
      className="peripheral-stage peripheral-stage--gamepad"
      data-heat={heat ? "on" : "off"}
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
          <defs>
            <linearGradient id={sheenId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stopColor="#fff" stopOpacity="0.55" />
              <stop offset="0.55" stopColor="#fff" stopOpacity="0" />
              <stop offset="1" stopColor="#3a2a1c" stopOpacity="0.08" />
            </linearGradient>
            <linearGradient id={bodyId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stopColor="var(--device-top)" />
              <stop offset="1" stopColor="var(--device-bottom)" />
            </linearGradient>
          </defs>
          <path className="device-shadow" d={shell.bodyPath} transform="translate(0 7)" />
          {/* 扳机在机身后面，向外微倾 */}
          {[6, 8].map((code) => {
            const s = spec(code);
            const tilt = code === 6 ? -7 : 7;
            return (
              <SkinGroup key={code} {...skin(code)} extra="skin--trigger">
                {(cls, fill) => (
                  <rect
                    className={cls}
                    x={s.cx - s.w / 2}
                    y={s.cy - s.h / 2}
                    width={s.w}
                    height={s.h}
                    rx={14}
                    fill={fill}
                    transform={`rotate(${tilt} ${s.cx} ${s.cy + 10})`}
                  />
                )}
              </SkinGroup>
            );
          })}
          <path className="device-body" d={shell.bodyPath} fill={`url(#${bodyId})`} />
          {/* 机身上表面：略内缩的浅色面板，做出厚度感 */}
          <path
            className="device-face"
            d={shell.bodyPath}
            transform="translate(310 224) scale(0.93 0.9) translate(-310 -224)"
          />
          <path className="device-outline" d={shell.bodyPath} />
          {/* 肩键压在机身上沿 */}
          {[5, 7].map((code) => {
            const s = spec(code);
            const tilt = code === 5 ? -7 : 7;
            return (
              <SkinGroup key={code} {...skin(code)} extra="skin--bumper">
                {(cls, fill) => (
                  <rect
                    className={cls}
                    x={s.cx - s.w / 2}
                    y={s.cy - s.h / 2}
                    width={s.w}
                    height={s.h}
                    rx={14}
                    fill={fill}
                    transform={`rotate(${tilt} ${s.cx} ${s.cy})`}
                  />
                )}
              </SkinGroup>
            );
          })}
          {/* 摇杆井 + 摇杆帽 */}
          {shell.stickWells.map((well) => (
            <g key={`well-${well.code}`}>
              <circle className="peripheral-inset" cx={well.cx} cy={well.cy} r={well.radius + 10} />
              <circle className="device-well" cx={well.cx} cy={well.cy} r={well.radius + 3} />
            </g>
          ))}
          {shell.stickWells.map((well) => (
            <SkinGroup key={`cap-${well.code}`} {...skin(well.code)} extra="skin--stick">
              {(cls, fill) => (
                <circle className={cls} cx={well.cx} cy={well.cy} r={well.radius - 3} fill={fill} />
              )}
            </SkinGroup>
          ))}
          {shell.stickWells.map((well) => (
            <circle
              key={`groove-${well.code}`}
              className="device-groove device-groove--round"
              cx={well.cx}
              cy={well.cy}
              r={well.radius - 14}
            />
          ))}
          {/* View / Menu / Guide */}
          <circle className="peripheral-inset" cx={guide.cx} cy={guide.cy} r={25} />
          {[9, 10, 11].map((code) => {
            const s = spec(code);
            const r = code === 11 ? 19 : 16;
            return (
              <SkinGroup key={code} {...skin(code)} extra="skin--round">
                {(cls, fill) => (
                  <circle className={cls} cx={s.cx} cy={s.cy} r={r} fill={fill} />
                )}
              </SkinGroup>
            );
          })}
          {/* 十字键：整体十字底座 + 四个方向臂 */}
          <path className="device-dpad-base" d={dpadCross} />
          <circle className="device-dpad-hub" cx={dpad.x} cy={dpad.y} r={16} />
          <circle className="device-groove device-groove--round" cx={dpad.x} cy={dpad.y} r={8} />
          {[14, 15, 16, 17].map((code) => {
            const s = spec(code);
            return (
              <SkinGroup key={code} {...skin(code)} extra="skin--arm">
                {(cls, fill) => (
                  <rect
                    className={cls}
                    x={s.cx - 17}
                    y={s.cy - 17}
                    width={34}
                    height={34}
                    rx={9}
                    fill={fill}
                  />
                )}
              </SkinGroup>
            );
          })}
          {/* ABXY */}
          {[3, 4, 2, 1].map((code) => {
            const s = spec(code);
            return (
              <SkinGroup key={code} {...skin(code)} extra={ABXY_CLASS[code]}>
                {(cls, fill) => (
                  <circle className={cls} cx={s.cx} cy={s.cy} r={20} fill={fill} />
                )}
              </SkinGroup>
            );
          })}
        </svg>
        <svg
          className="peripheral-live-overlay"
          width={size.width}
          height={size.height}
          viewBox={`0 0 ${size.width} ${size.height}`}
          aria-hidden="true"
        >
          {/* LT/RT 行程填充（原始 0..1 → 整数百分比；不冒充历史 bin） */}
          {[
            { code: 6, pct: leftPct },
            { code: 8, pct: rightPct },
          ].map(({ code, pct }) => {
            const s = spec(code);
            const fillH = (s.h * pct) / 100;
            return (
              <g
                key={`trig-${code}`}
                pointerEvents="none"
                transform={`rotate(${code === 6 ? -7 : 7} ${s.cx} ${s.cy + 10})`}
              >
                <rect
                  className="trigger-travel-track"
                  x={s.cx - s.w / 2}
                  y={s.cy - s.h / 2}
                  width={s.w}
                  height={s.h}
                  rx={13}
                />
                <rect
                  className="trigger-travel-fill"
                  x={s.cx - s.w / 2}
                  y={s.cy + s.h / 2 - fillH}
                  width={s.w}
                  height={fillH}
                  rx={fillH < 13 ? 0 : 13}
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
        {specs.map((s) => {
          const total = lookup.get(s.code)?.total;
          const isUnsupported = unsupported.has(s.code);
          const isLive = held.has(s.code);
          const isPulse = !isLive && pulse.has(s.code);
          const selected = selectedCode === s.code;
          const isTrigger = s.code === 6 || s.code === 8;
          const pct = s.code === 6 ? leftPct : s.code === 8 ? rightPct : null;
          return (
            <button
              type="button"
              key={s.code}
              className={[
                "peripheral-button",
                "gamepad-button",
                isLive ? "is-live" : "",
                isPulse ? "is-pulse" : "",
                isUnsupported ? "is-unsupported" : "",
              ]
                .filter(Boolean)
                .join(" ")}
              data-code={s.code}
              data-shape={s.shape}
              data-heat={heatVars.has(s.code) ? "on" : undefined}
              style={{
                left: s.hit.x,
                top: s.hit.y,
                width: s.hit.w,
                height: s.hit.h,
                ...heatVars.get(s.code),
              }}
              aria-pressed={selected}
              aria-label={historyAria(
                historyState,
                s.shortLabel,
                total,
                isUnsupported,
              )}
              title={historyTitle(
                historyState,
                s.shortLabel,
                total,
                isUnsupported,
              )}
              onClick={() => onSelect(s.code)}
              onPointerEnter={() => setHover(s.code)}
              onPointerLeave={() => setHover((c) => (c === s.code ? null : c))}
              onFocus={() => setHover(s.code)}
              onBlur={() => setHover((c) => (c === s.code ? null : c))}
            >
              {s.shape === "arrow" ? (
                <DirectionMark direction={s.dir ?? "up"} />
              ) : (
                <span className="peripheral-label" aria-hidden="true">
                  {s.shortLabel}
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
