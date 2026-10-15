// 热力色带：三段色（低 → 中 → 高）取自 theme.css 的 --heat-low/mid/high（#rrggbb），
// 随浅/深主题变化。纯展示换算：intensity 已由 motionPresentation 归一化到 [0,1]。
export type Rgb = readonly [number, number, number];
export interface HeatPalette {
  low: Rgb;
  mid: Rgb;
  high: Rgb;
}

const FALLBACK: HeatPalette = {
  low: [27, 34, 54],
  mid: [209, 71, 122],
  high: [255, 200, 87],
};

function parseHex(value: string, fallback: Rgb): Rgb {
  const m = /^#([0-9a-fA-F]{6})$/.exec(value.trim());
  if (!m) return fallback;
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** 读取当前主题的热力色带（仅在浏览器内调用；读不到时回落默认深色色带） */
export function readHeatPalette(): HeatPalette {
  if (typeof document === "undefined") return FALLBACK;
  const styles = getComputedStyle(document.documentElement);
  return {
    low: parseHex(styles.getPropertyValue("--heat-low"), FALLBACK.low),
    mid: parseHex(styles.getPropertyValue("--heat-mid"), FALLBACK.mid),
    high: parseHex(styles.getPropertyValue("--heat-high"), FALLBACK.high),
  };
}

/** 强度 → RGB（越界/非有限收敛到 [0,1]） */
export function heatRgb(intensity: number, palette: HeatPalette): Rgb {
  const t = Number.isFinite(intensity) ? Math.min(1, Math.max(0, intensity)) : 0;
  const [from, to, k] = t < 0.5 ? [palette.low, palette.mid, t * 2] : [palette.mid, palette.high, (t - 0.5) * 2];
  return [
    Math.round(from[0] + (to[0] - from[0]) * k),
    Math.round(from[1] + (to[1] - from[1]) * k),
    Math.round(from[2] + (to[2] - from[2]) * k),
  ];
}

export function heatCss(intensity: number, palette: HeatPalette): string {
  const [r, g, b] = heatRgb(intensity, palette);
  return `rgb(${r},${g},${b})`;
}
