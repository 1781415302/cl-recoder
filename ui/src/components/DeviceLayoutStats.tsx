// 键盘参考布局卡（ANSI-104 四区）。几何数据来自 lib/deviceLayouts.ts 纯模板；
// 本组件只做 code 直接查找计数（不 SUM，缺行=0）、每区一个 Tab 入口的 roving 焦点导航
// （方向键 focus 与 selectedCode 独立）与受控选择：点击/Enter/Space 仅 onSelect(code)，
// 不重放输入、不写统计、不查询 API。
// 热力着色为可选的纯显示层：强度 = sqrt(次数 / 榜首次数)，只改键帽底色，不改任何数值；
// 数字缩写展示，详情/aria 为精确 fmtNum。
import { useId, useMemo, useState, type ReactNode } from "react";
import type { DeviceLiveView, TopKeyRow } from "../api/types";
import {
  getDeviceLayout,
  getUnmappedRows,
  nextControlId,
  type DeviceKind,
  type DeviceZoneSpec,
  type NavigationKey,
} from "../lib/deviceLayouts";
import { fmtCompact, fmtNum } from "../lib/format";
import { heatRgb, readHeatPalette } from "../lib/heatPalette";
import { useResolvedTheme } from "../lib/theme";

/** 单控件最小 44×44 显示单元：1u = 44px，从不缩放字号；卡内不足时布局区横向滚动 */
const UNIT_PX = 44;

const NAV_KEYS: readonly string[] = [
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "Home",
  "End",
];

export interface DeviceLayoutStatsProps {
  kind: DeviceKind;
  rows: readonly TopKeyRow[];
  title: string;
  loading?: boolean;
  /** 受控选择：组件用该值画选中态；Escape 清除经 onSelect(null) 回传页面 */
  selectedCode: number | null;
  onSelect: (code: number | null) => void;
  live?: DeviceLiveView | null;
  liveToolbar?: ReactNode;
  historyState?: "loading" | "ready" | "error";
}

function fmtHex(code: number): string {
  return `0x${code.toString(16).toUpperCase()}`;
}

export function DeviceLayoutStats({
  kind,
  rows,
  title,
  loading = false,
  selectedCode,
  onSelect,
  live = null,
  liveToolbar,
  historyState: historyStateProp,
}: DeviceLayoutStatsProps) {
  const historyState = historyStateProp ?? (loading ? "loading" : "ready");
  const countReady = historyState === "ready";
  const held = useMemo(() => new Set(live?.heldCodes ?? []), [live?.heldCodes]);
  const pulse = useMemo(() => new Set(live?.pulseCodes ?? []), [live?.pulseCodes]);
  const layout = getDeviceLayout(kind);
  const idPrefix = useId();
  const theme = useResolvedTheme();
  const [heatOn, setHeatOn] = useState(false);
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

  // 热力：仅在开启且历史就绪时计算（palette 随主题重读）
  const heat = useMemo(() => {
    if (!heatOn || !countReady) return null;
    let max = 0;
    for (const c of layout.controls) {
      const t = rowByCode.get(c.code)?.total ?? 0;
      if (t > max) max = t;
    }
    return { max, palette: readHeatPalette() };
    // theme 变化需重新读取 CSS 令牌
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [heatOn, countReady, layout, rowByCode, theme]);

  const totalOf = (code: number): number | undefined =>
    countReady ? (rowByCode.get(code)?.total ?? 0) : undefined;

  function capStyle(total: number | undefined): Record<string, string> {
    if (!heat || total === undefined || heat.max <= 0) return {};
    const intensity = total > 0 ? Math.sqrt(total / heat.max) : 0;
    const [r, g, b] = heatRgb(intensity, heat.palette);
    const lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
    return {
      "--cap": `rgb(${r},${g},${b})`,
      "--cap-ink": lum > 0.58 ? "#15171c" : "#ffffff",
    };
  }

  function onCardKeyDown(e: React.KeyboardEvent<HTMLElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      onSelect(null); // Escape 清选择
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
    selectedCode !== null
      ? layout.controls.find((c) => c.code === selectedCode)?.shortLabel
      : undefined;

  const subtitle = `ANSI-104 参考布局 · 已加载 ${fmtNum(rows.length)} 项。其它键位见下方。`;

  return (
    <section className="card card--stage" onKeyDown={onCardKeyDown}>
      <div className="card-head">
        <div>
          <h2 className="card-title">{title}</h2>
          <p className="card-sub">{subtitle}</p>
        </div>
        <label className="switch switch--sm">
          <input
            type="checkbox"
            role="switch"
            checked={heatOn}
            onChange={(e) => setHeatOn(e.currentTarget.checked)}
          />
          <span className="switch-track" aria-hidden="true" />
          <span className="switch-label">频次热力</span>
        </label>
      </div>
      {liveToolbar}
      {!countReady ? (
        <p className="hint" role="status">
          {historyState === "error" ? "历史次数暂不可用" : "历史次数加载中…"}
        </p>
      ) : null}
      <div className="dev-zones">
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
                  {zoneControls.map((c) => {
                    const total = totalOf(c.code);
                    const selected = selectedCode === c.code;
                    const isLive = held.has(c.code);
                    const isPulse = !isLive && pulse.has(c.code);
                    const ariaCount =
                      total === undefined ? "历史次数暂不可用" : `${fmtNum(total)} 次`;
                    return (
                      <button
                        key={c.id}
                        id={`${idPrefix}-${c.id}`}
                        type="button"
                        className={["dev-key", isLive ? "is-live" : "", isPulse ? "is-pulse" : ""]
                          .filter(Boolean)
                          .join(" ")}
                        style={{
                          left: c.x * UNIT_PX,
                          top: c.y * UNIT_PX,
                          width: c.width * UNIT_PX,
                          height: c.height * UNIT_PX,
                          ...capStyle(total),
                        }}
                        tabIndex={tabStopId === c.id ? 0 : -1}
                        aria-pressed={selected}
                        aria-label={`${c.shortLabel}，编码 ${fmtHex(c.code)}，${ariaCount}${selected ? "，已选中" : ""}`}
                        title={
                          total === undefined
                            ? `${c.shortLabel}：历史次数暂不可用`
                            : `${c.shortLabel}：${fmtNum(total)} 次`
                        }
                        onClick={() => onSelect(c.code)}
                        onFocus={() => setFocusByZone((prev) => ({ ...prev, [zone.id]: c.id }))}
                      >
                        <span className="dev-key-name" aria-hidden="true">
                          {c.shortLabel}
                        </span>
                        <span className="dev-key-num" aria-hidden="true">
                          {total === undefined ? "—" : fmtCompact(total)}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </div>
            </section>
          );
        })}
      </div>
      <div className="readout-bar" aria-live="polite">
        {selectedCode === null ? (
          <span className="muted">点击按键查看使用次数；也可用方向键选择</span>
        ) : !countReady ? (
          <>
            <strong>{selectedRow?.label ?? selectedFallback ?? fmtHex(selectedCode)}</strong>
            <span className="mono muted">编码 {fmtHex(selectedCode)}</span>
            <span className="muted">历史次数暂不可用</span>
          </>
        ) : selectedRow ? (
          <>
            <strong>{selectedRow.label}</strong>
            <span className="mono muted">编码 {fmtHex(selectedRow.code)}</span>
            <span className="readout-num num">{fmtNum(selectedRow.total)} 次</span>
          </>
        ) : (
          <>
            <strong>{selectedFallback ?? fmtHex(selectedCode)}</strong>
            <span className="mono muted">编码 {fmtHex(selectedCode)}</span>
            <span className="muted">所选范围内无记录（0 次）</span>
          </>
        )}
      </div>
      {unmapped.length > 0 ? (
        <div className="others">
          <p className="others-title">其它输入（参考布局未绘制）· {unmapped.length} 项</p>
          <div className="others-list">
            {unmapped.map((r) => (
              <button
                key={r.code}
                type="button"
                className="other-chip"
                aria-pressed={selectedCode === r.code}
                aria-label={
                  countReady
                    ? `${r.label}，${fmtNum(r.total)} 次`
                    : `${r.label}，历史次数暂不可用`
                }
                onClick={() => onSelect(r.code)}
              >
                <span className="other-chip-label">{r.label}</span>
                <span className="num">{countReady ? fmtNum(r.total) : "—"}</span>
              </button>
            ))}
          </div>
        </div>
      ) : null}
      <p className="hint">键位以参考布局展示；较宽区域可左右滚动查看。</p>
    </section>
  );
}
