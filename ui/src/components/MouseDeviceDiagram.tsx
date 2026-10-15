// 真实鼠标分布图（input-live-trigger §4.7 S8）：绘制式鼠标（按键/滚轮/侧键）+ 九控件；
// 历史计数只进详情/tooltip，不常驻按钮；实时 held/pulse 只消费 live props。
import { useId, useMemo, useState } from "react";
import type { DeviceLiveView, TopKeyRow } from "../api/types";
import {
  MOTION_CANVAS,
  motionControlSpecs,
  peripheralShellSpec,
  type MotionDirection,
} from "../lib/motionPresentation";
import { fmtNum } from "../lib/format";
import { SkinGroup, totalsByCode, useControlHeat } from "./diagramSkin";

export interface DeviceDiagramProps {
  rows: readonly TopKeyRow[];
  historyState: "loading" | "ready" | "error";
  selectedCode: number | null;
  onSelect: (code: number | null) => void;
  live: DeviceLiveView | null;
  /** 按使用次数给控件着色（默认开启） */
  heat?: boolean;
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

/* 两侧键帽留出 44px 滚轮通道；同一轮廓用于绘制与按钮命中。 */
const LEFT_REGION = "M0 0H138V170Q138 184 124 184H0Z";
const RIGHT_REGION = "M320 0H182V170Q182 184 196 184H320Z";
const BODY_PATH =
  "M160 10C110 10 70 34 56 96C46 140 34 200 34 262C34 322 84 350 160 350C236 350 286 322 286 262C286 200 274 140 264 96C250 34 210 10 160 10Z";

export function MouseDeviceDiagram({
  rows,
  historyState,
  selectedCode,
  onSelect,
  live,
  heat = true,
}: DeviceDiagramProps) {
  const specs = motionControlSpecs("mouse");
  const size = MOTION_CANVAS.mouse;
  // 外壳几何仍由 motionPresentation 提供（测试与可达性依赖）；此处绘制用更贴近手形的轮廓
  const shell = peripheralShellSpec("mouse");
  const uid = useId().replace(/[^a-zA-Z0-9]/g, "");
  const clipId = `${uid}-clip`;
  const leftHitId = `${uid}-left-hit`;
  const rightHitId = `${uid}-right-hit`;
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
      },
    };
  }

  return (
    <div
      className="peripheral-stage peripheral-stage--mouse"
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
          data-shell-seams={shell.seamPaths.length}
        >
          <defs>
            <clipPath id={clipId}>
              <path d={BODY_PATH} />
            </clipPath>
            <clipPath id={leftHitId}>
              <path d={LEFT_REGION} clipPath={`url(#${clipId})`} />
            </clipPath>
            <clipPath id={rightHitId}>
              <path d={RIGHT_REGION} clipPath={`url(#${clipId})`} />
            </clipPath>
            <linearGradient id={sheenId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stopColor="#fff" stopOpacity="0.55" />
              <stop offset="0.55" stopColor="#fff" stopOpacity="0" />
              <stop offset="1" stopColor="#3a2a1c" stopOpacity="0.07" />
            </linearGradient>
            <linearGradient id={bodyId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stopColor="var(--device-top)" />
              <stop offset="1" stopColor="var(--device-bottom)" />
            </linearGradient>
          </defs>
          {/* 投影 + 机身 */}
          <path className="device-shadow" d={BODY_PATH} transform="translate(0 7)" />
          <path className="device-body" d={BODY_PATH} fill={`url(#${bodyId})`} />
          {/* 左右键：机身轮廓裁切 */}
          <g clipPath={`url(#${clipId})`}>
            <SkinGroup {...skin(1)} extra="skin--mouse-key">
              {(cls, fill) => <path className={cls} d={LEFT_REGION} fill={fill} />}
            </SkinGroup>
            <SkinGroup {...skin(2)} extra="skin--mouse-key">
              {(cls, fill) => <path className={cls} d={RIGHT_REGION} fill={fill} />}
            </SkinGroup>
          </g>
          <path className="device-outline" d={BODY_PATH} />
          {/* 手掌区细节 */}
          <path className="device-groove" d="M92 300Q160 330 228 300" />
          <circle className="device-logo" cx="160" cy="278" r="15" />
          <circle className="device-logo-dot" cx="160" cy="278" r="4" />
          {/* 滚轮槽 + 滚轮 */}
          <rect className="peripheral-inset" x="141" y="72" width="38" height="60" rx="19" />
          <SkinGroup {...skin(3)} extra="skin--wheel">
            {(cls, fill) => (
              <rect className={cls} x="145" y="77" width="30" height="50" rx="15" fill={fill} />
            )}
          </SkinGroup>
          {/* 滚轮方向 */}
          <SkinGroup {...skin(6)} extra="skin--chip">
            {(cls, fill) => (
              <rect className={cls} x="146" y="37" width="28" height="22" rx="11" fill={fill} />
            )}
          </SkinGroup>
          <SkinGroup {...skin(7)} extra="skin--chip">
            {(cls, fill) => (
              <rect className={cls} x="146" y="139" width="28" height="22" rx="11" fill={fill} />
            )}
          </SkinGroup>
          <SkinGroup {...skin(8)} extra="skin--chip">
            {(cls, fill) => (
              <rect className={cls} x="96" y="201" width="24" height="28" rx="12" fill={fill} />
            )}
          </SkinGroup>
          <SkinGroup {...skin(9)} extra="skin--chip">
            {(cls, fill) => (
              <rect className={cls} x="200" y="201" width="24" height="28" rx="12" fill={fill} />
            )}
          </SkinGroup>
          {/* 侧键 */}
          <SkinGroup {...skin(4)} extra="skin--side">
            {(cls, fill) => (
              <rect className={cls} x="25" y="208" width="46" height="36" rx="14" fill={fill} />
            )}
          </SkinGroup>
          <SkinGroup {...skin(5)} extra="skin--side">
            {(cls, fill) => (
              <rect className={cls} x="25" y="260" width="46" height="36" rx="14" fill={fill} />
            )}
          </SkinGroup>
        </svg>
        {specs.map((spec) => {
          const mainButton = spec.code === 1 || spec.code === 2;
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
              data-heat={heatVars.has(spec.code) ? "on" : undefined}
              style={{
                left: mainButton ? 0 : spec.hit.x,
                top: mainButton ? 0 : spec.hit.y,
                width: mainButton ? size.width : spec.hit.w,
                height: mainButton ? size.height : spec.hit.h,
                clipPath: mainButton
                  ? `url(#${spec.code === 1 ? leftHitId : rightHitId})`
                  : undefined,
                ...heatVars.get(spec.code),
              }}
              aria-pressed={selected}
              aria-label={historyAria(historyState, spec.shortLabel, total)}
              title={historyTitle(historyState, spec.shortLabel, total)}
              onClick={() => onSelect(spec.code)}
              onPointerEnter={() => setHover(spec.code)}
              onPointerLeave={() => setHover((c) => (c === spec.code ? null : c))}
              onFocus={() => setHover(spec.code)}
              onBlur={() => setHover((c) => (c === spec.code ? null : c))}
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
