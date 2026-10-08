import { useMemo } from "react";
import type { TopKeyRow } from "../api/types";
import {
  MOTION_CANVAS,
  motionControlSpecs,
  type MotionDirection,
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
function DeviceShell({ kind }: { kind: "mouse" | "gamepad" }) {
  const size = MOTION_CANVAS[kind];
  return (
    <svg
      className="peripheral-svg"
      width={size.width}
      height={size.height}
      viewBox={`0 0 ${size.width} ${size.height}`}
      aria-hidden="true"
    >
      {kind === "mouse" ? (
        <>
          <path
            className="peripheral-shell"
            d="M160 14C77 14 34 64 34 144v126c0 54 42 76 126 76s126-22 126-76V144C286 64 243 14 160 14Z"
          />
          <path className="peripheral-seam" d="M34 184h252M160 14v170" />
          <rect
            className="peripheral-inset"
            x="150"
            y="70"
            width="20"
            height="64"
            rx="10"
          />
          <path className="peripheral-seam" d="M102 315q58 20 116 0" />
        </>
      ) : (
        <>
          <path
            className="peripheral-shell"
            d="M104 65C77 70 63 94 55 131L33 260c-8 43 4 66 27 66 26 0 43-22 68-66l23-31c34 33 91 60 159 60s125-27 159-60l23 31c25 44 42 66 68 66 23 0 35-23 27-66l-22-129c-8-37-22-61-49-66-67-12-131 20-206 20S171 53 104 65Z"
          />
          <path
            className="peripheral-seam"
            d="M77 270q10 23 21 3M543 270q-10 23-21 3M165 117q145 23 290 0"
          />
          <circle className="peripheral-inset" cx="130" cy="178" r="42" />
          <circle className="peripheral-inset" cx="365" cy="258" r="40" />
          <path
            className="peripheral-inset"
            d="M213 162h44v44h44v44h-44v44h-44v-44h-44v-44h44Z"
          />
        </>
      )}
    </svg>
  );
}
export function PeripheralControlsStats({
  kind,
  rows,
  selectedCode,
  onSelect,
  loading = false,
}: PeripheralControlsStatsProps) {
  const specs = motionControlSpecs(kind),
    size = MOTION_CANVAS[kind];
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
      {loading ? (
        <div role="status" aria-label="按键分布加载中">
          <Skeleton h={`${size.height}px`} />
        </div>
      ) : (
        <>
          <div className="peripheral-stage">
            <div
              className="peripheral-board"
              style={{ width: size.width, height: size.height }}
            >
              <DeviceShell kind={kind} />
              {specs.map((spec) => {
                const count = lookup.get(spec.code)?.total ?? 0;
                return (
                  <button
                    type="button"
                    key={spec.code}
                    className={`peripheral-button ${kind === "gamepad" ? "gamepad-button" : "mouse-button"}`}
                    data-code={spec.code}
                    data-shape={spec.shape}
                    style={{
                      left: spec.hit.x,
                      top: spec.hit.y,
                      width: spec.hit.w,
                      height: spec.hit.h,
                    }}
                    aria-pressed={selectedCode === spec.code}
                    aria-label={`${spec.shortLabel}，${fmtNum(count)} 次`}
                    onClick={() => onSelect(spec.code)}
                  >
                    {spec.shape === "arrow" ? (
                      <DirectionMark direction={spec.dir ?? "up"} />
                    ) : (
                      <span className="peripheral-label" aria-hidden="true">
                        {spec.shortLabel}
                      </span>
                    )}
                    <span className="peripheral-count" aria-hidden="true">
                      {fmtCompact(count)}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
          <div className="peripheral-readout" aria-live="polite">
            {selectedCode === null ? (
              <span>点击按键查看完整计数</span>
            ) : (
              <>
                <strong>{selectedLabel ?? `其它输入 ${selectedCode}`}</strong>
                <span className="num">{fmtNum(selected?.total ?? 0)} 次</span>
                <span>所选日期</span>
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
                    onClick={() => onSelect(row.code)}
                  >
                    <span className="dev-other-label">{row.label}</span>
                    <span className="num">{fmtNum(row.total)}</span>
                  </button>
                ))}
              </div>
            </div>
          )}
          {kind === "gamepad" && (
            <p className="chart-hint">Guide 键可能被系统接管，无法采集。</p>
          )}
        </>
      )}
    </section>
  );
}
