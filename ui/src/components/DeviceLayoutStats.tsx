// 设备参考物理布局卡（usability-runtime-v3 §4.8 U4）：键盘 ANSI-104 四区 / 鼠标中性 9 控件 / 手柄 Xbox 17 控件。
// 几何数据来自 lib/deviceLayouts.ts 纯模板；本组件只做 code 直接查找计数（不 SUM，缺行=0）、
// 每区一个 Tab 入口的 roving 焦点导航（方向键 focus 与 selectedCode 独立）与受控选择：
// 点击/Enter/Space 仅 onSelect(code) 供页面展示精确详情，不重放输入、不写统计、不查询 API。
// 所有填充保持中性（不按频率染色），仅选择/焦点强调用主色；数字缩写展示，详情/aria 为精确 fmtNum。
import { useId, useMemo, useState } from "react";
import type { TopKeyRow } from "../api/types";
import {
  getDeviceLayout,
  getUnmappedRows,
  nextControlId,
  type DeviceKind,
  type DeviceZoneSpec,
  type NavigationKey,
} from "../lib/deviceLayouts";
import { fmtCompact, fmtNum } from "../lib/format";

/** 单控件最小 44×44 显示单元（§4.8）：1u = 44px，从不缩放字号；卡内不足时布局区横向滚动 */
const UNIT_PX = 44;

const NAV_KEYS: readonly string[] = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"];

export interface DeviceLayoutStatsProps {
  kind: DeviceKind;
  rows: TopKeyRow[];
  title: string;
  loading?: boolean;
  /** 受控选择：组件用该值画选中态；Escape 清除经 onSelect(null) 回传页面 */
  selectedCode: number | null;
  onSelect: (code: number | null) => void;
}

function fmtHex(code: number): string {
  return `0x${code.toString(16).toUpperCase()}`;
}

/** 中性 SVG 轮廓（装饰背景，aria-hidden；本地 SVG，不依赖型号外观识别） */
function MouseOutline({ width, height }: { width: number; height: number }) {
  return (
    <svg className="dev-outline" width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden="true" focusable="false">
      <path
        d="M110 10 C 42 10 14 62 14 150 L 14 268 C 14 306 44 320 110 320 C 176 320 206 306 206 268 L 206 150 C 206 62 178 10 110 10 Z M 110 10 L 110 96 M 14 96 L 206 96"
        style={{ fill: "var(--color-bg)", stroke: "var(--color-divider)" }}
      />
      <rect x={98} y={26} width={24} height={96} rx={12} style={{ fill: "var(--color-muted)", stroke: "var(--color-divider)" }} />
    </svg>
  );
}

function GamepadOutline({ width, height }: { width: number; height: number }) {
  return (
    <svg className="dev-outline" width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden="true" focusable="false">
      <path
        d="M 128 64 C 64 64 28 116 28 176 C 28 244 66 266 98 246 C 128 227 148 216 180 216 L 260 216 C 292 216 312 227 342 246 C 374 266 412 244 412 176 C 412 116 376 64 312 64 Z"
        style={{ fill: "var(--color-bg)", stroke: "var(--color-divider)" }}
      />
    </svg>
  );
}

export function DeviceLayoutStats({ kind, rows, title, loading = false, selectedCode, onSelect }: DeviceLayoutStatsProps) {
  const layout = getDeviceLayout(kind);
  const idPrefix = useId();
  // 区内 roving 焦点：每区恰好一个 tabIndex=0 的控件作为 Tab 入口；
  // 焦点状态与 selectedCode 完全独立（方向键移动焦点不改选择）。
  const [focusByZone, setFocusByZone] = useState<Record<string, string>>({});

  // code → 行（后端行按 code 聚合，正常无重复；重复时取首个，绝不求和）。
  const rowByCode = useMemo(() => {
    const m = new Map<number, TopKeyRow>();
    for (const r of rows) if (!m.has(r.code)) m.set(r.code, r);
    return m;
  }, [rows]);
  // 没有绘制位置的输入：在“其它输入”保留（完整行仍由页面完整表呈现，不能只显示布局控件）。
  const unmapped = useMemo(() => getUnmappedRows(kind, rows), [kind, rows]);

  const totalOf = (code: number): number => rowByCode.get(code)?.total ?? 0;

  const subtitle =
    kind === "keyboard"
      ? `ANSI-104 参考布局 · 已加载 ${fmtNum(rows.length)} 项。其它键位见下方列表。`
      : kind === "mouse"
        ? `鼠标按键与滚动方向 · 已加载 ${fmtNum(rows.length)} 项。`
        : `Xbox 参考布局 · 已加载 ${fmtNum(rows.length)} 项。Guide 可能无法采集。`;

  function onCardKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      onSelect(null); // Escape 清选择（§4.8）
    }
  }

  function onZoneKeyDown(zone: DeviceZoneSpec, e: React.KeyboardEvent<HTMLDivElement>) {
    if (!NAV_KEYS.includes(e.key)) return; // Enter/Space 交给按钮原生激活
    e.preventDefault();
    const zoneControls = layout.controls.filter((c) => c.zone === zone.id);
    const currentId = focusByZone[zone.id] ?? zoneControls[0].id;
    const nextId = nextControlId(layout, currentId, e.key as NavigationKey);
    setFocusByZone((prev) => ({ ...prev, [zone.id]: nextId }));
    document.getElementById(`${idPrefix}-${nextId}`)?.focus();
  }

  const selectedRow = selectedCode !== null ? rowByCode.get(selectedCode) : undefined;
  const selectedFallback =
    selectedCode !== null ? layout.controls.find((c) => c.code === selectedCode)?.shortLabel : undefined;

  return (
    <div className="card" onKeyDown={onCardKeyDown}>
      <h2 className="card-title">{title}</h2>
      <p className="card-sub">{subtitle}</p>
      {loading ? (
        <div role="status" aria-label={`${title}加载中`} style={{ marginTop: "var(--space-3)" }}>
          <div className="skeleton" style={{ height: 44 }} />
          <div className="skeleton" style={{ height: 220, marginTop: "var(--space-3)" }} />
        </div>
      ) : (
        <>
          <div className="dev-zones" style={{ marginTop: "var(--space-3)" }}>
            {layout.zones.map((zone) => {
              const zoneControls = layout.controls.filter((c) => c.zone === zone.id);
              const tabStopId = focusByZone[zone.id] ?? zoneControls[0].id;
              return (
                <section key={zone.id} className="dev-zone" aria-label={zone.label}>
                  <div className="dev-zone-label">{zone.label}</div>
                  <div className="dev-zone-scroll">
                    <div
                      className="dev-zone-grid"
                      role="group"
                      aria-label={`${zone.label}：方向键区内移动，Home/End 首末，Enter/Space 选择，Escape 清除选择`}
                      style={{ width: zone.width * UNIT_PX, height: zone.height * UNIT_PX }}
                      onKeyDown={(e) => onZoneKeyDown(zone, e)}
                    >
                      {kind === "mouse" ? <MouseOutline width={zone.width * UNIT_PX} height={zone.height * UNIT_PX} /> : null}
                      {kind === "gamepad" ? <GamepadOutline width={zone.width * UNIT_PX} height={zone.height * UNIT_PX} /> : null}
                      {zoneControls.map((c) => {
                        const total = totalOf(c.code);
                        const selected = selectedCode === c.code;
                        return (
                          <button
                            key={c.id}
                            id={`${idPrefix}-${c.id}`}
                            type="button"
                            className="dev-key"
                            style={{
                              left: c.x * UNIT_PX,
                              top: c.y * UNIT_PX,
                              width: c.width * UNIT_PX,
                              height: c.height * UNIT_PX,
                            }}
                            tabIndex={tabStopId === c.id ? 0 : -1}
                            aria-pressed={selected}
                            aria-label={`${c.shortLabel}，编码 ${fmtHex(c.code)}，${fmtNum(total)} 次${selected ? "，已选中" : ""}`}
                            onClick={() => onSelect(c.code)}
                            onFocus={() => setFocusByZone((prev) => ({ ...prev, [zone.id]: c.id }))}
                          >
                            <span className="dev-key-name" aria-hidden="true">{c.shortLabel}</span>
                            <span className="dev-key-num" aria-hidden="true">{fmtCompact(total)}</span>
                          </button>
                        );
                      })}
                    </div>
                  </div>
                </section>
              );
            })}
          </div>
          <div className="dev-detail" aria-live="polite">
            {selectedCode === null ? (
              <span className="td-muted">点击布局控件或 Tab + 方向键选择，查看精确数值；Escape 清除选择。</span>
            ) : selectedRow ? (
              <>
                <span style={{ fontWeight: 600 }}>{selectedRow.label}</span>
                <span className="mono">（编码 {fmtHex(selectedRow.code)}）</span>
                <span className="num">{fmtNum(selectedRow.total)} 次</span>
              </>
            ) : (
              <>
                <span style={{ fontWeight: 600 }}>{selectedFallback ?? fmtHex(selectedCode)}</span>
                <span className="mono">（编码 {fmtHex(selectedCode)}）</span>
                <span className="td-muted">所选范围内无记录（0 次）</span>
              </>
            )}
          </div>
          {unmapped.length > 0 ? (
            <div className="dev-other">
              <p className="dev-other-title">其它输入（参考布局未绘制）· {unmapped.length} 项</p>
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
                    <span className="dev-key-num">{fmtNum(r.total)}</span>
                  </button>
                ))}
              </div>
            </div>
          ) : null}
          {kind === "keyboard" ? (
            <p className="chart-hint">键位以参考布局展示；较宽区域可左右滚动查看。</p>
          ) : null}
        </>
      )}
    </div>
  );
}
