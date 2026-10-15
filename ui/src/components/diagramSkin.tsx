// 鼠标/手柄示意图共用的“皮肤”工具：热力着色（按使用次数）与可叠加状态环的图形组。
// 纯展示；不引入 API 层。次数只用于颜色，不在图上常驻显示。
import { useMemo, type CSSProperties, type ReactNode } from "react";
import type { TopKeyRow } from "../api/types";
import { heatRgb, readHeatPalette } from "../lib/heatPalette";
import { useResolvedTheme } from "../lib/theme";

export type SkinVars = CSSProperties & Record<string, string>;

/** code → total（同 code 取首行，与图/列表保持一致） */
export function totalsByCode(rows: readonly TopKeyRow[]): Map<number, number> {
  const out = new Map<number, number>();
  for (const row of rows) if (!out.has(row.code)) out.set(row.code, row.total);
  return out;
}

/**
 * 热力着色：每个控件一组 CSS 变量 --skin / --skin-ink / --skin-t。
 * intensity = sqrt(total / max)；total=0 或未启用/未就绪时不着色（回落到 --key）。
 */
export function useControlHeat(
  codes: readonly number[],
  totals: ReadonlyMap<number, number>,
  enabled: boolean,
): ReadonlyMap<number, SkinVars> {
  const theme = useResolvedTheme();
  return useMemo(() => {
    const out = new Map<number, SkinVars>();
    if (!enabled) return out;
    let max = 0;
    for (const code of codes) max = Math.max(max, totals.get(code) ?? 0);
    if (max <= 0) return out;
    const palette = readHeatPalette();
    for (const code of codes) {
      const total = totals.get(code) ?? 0;
      if (total <= 0) continue;
      const t = Math.sqrt(total / max);
      const [r, g, b] = heatRgb(t, palette);
      const lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
      out.set(code, {
        "--skin": `rgb(${r},${g},${b})`,
        "--skin-ink": lum > 0.6 ? "#2a1d14" : "#ffffff",
        "--skin-t": t.toFixed(3),
      });
    }
    return out;
    // theme 变化时重读色带
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [codes, totals, enabled, theme]);
}

export interface SkinState {
  selected?: boolean;
  live?: boolean;
  pulse?: boolean;
  hover?: boolean;
  unsupported?: boolean;
  focus?: boolean; // 键盘焦点（:focus-visible）
}

function stateClass(state: SkinState): string {
  return [
    state.selected ? "is-selected" : "",
    state.live ? "is-live" : "",
    state.pulse ? "is-pulse" : "",
    state.hover ? "is-hover" : "",
    state.unsupported ? "is-unsupported" : "",
    state.focus ? "is-focus" : "",
  ]
    .filter(Boolean)
    .join(" ");
}

/**
 * 一个可着色图形：状态环（选中/实时/脉冲/焦点）+ 本体 + 高光层。
 * children(cls) 返回同一几何的 SVG 元素，cls 分别为 skin-ring / skin / skin-sheen。
 */
export function SkinGroup({
  code,
  state,
  vars,
  sheenId,
  extra,
  children,
}: {
  code: number;
  state: SkinState;
  vars?: SkinVars;
  /** 高光渐变 id（由所属图的 useId 生成） */
  sheenId: string;
  /** 附加类名（如 skin--abxy-a） */
  extra?: string;
  children: (cls: string, fill?: string) => ReactNode;
}) {
  const states = stateClass(state);
  const hasRing = state.selected || state.live || state.pulse || state.focus;
  return (
    <g
      className={["skin-group", extra ?? "", states].filter(Boolean).join(" ")}
      data-skin={code}
      style={vars}
    >
      {hasRing ? children(`skin-ring ${states}`) : null}
      {children(`skin ${states}`)}
      {children("skin-sheen", `url(#${sheenId})`)}
    </g>
  );
}
