// 展示格式化工具。仅对"查询返回的单行"做展示格式化（PLAN §2.5：聚合一律由 SQL 完成）。
// 数字千分位 / 缩写（1.2K）/ 日期本地化 —— §4.9 组件硬规则。

/** 千分位：48213 → "48,213" */
export function fmtNum(n: number): string {
  return new Intl.NumberFormat("zh-Hans-CN").format(n);
}

/** 缩写（§4.9 示例 1.2K）：9800 → "9.8K"，图表轴/KPI 辅助用 */
export function fmtCompact(n: number): string {
  return new Intl.NumberFormat("en-US", {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(n);
}

/** 秒 → "3 小时 24 分" / "12 分 30 秒" / "45 秒" */
export function fmtDuration(totalSeconds: number): string {
  const s = Math.max(0, Math.round(totalSeconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分 ${sec} 秒`;
  return `${sec} 秒`;
}

/** "YYYY-MM-DD" → 本地 Date（避免 UTC 解析偏移一天） */
export function parseDay(day: string): Date {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y ?? 1970, (m ?? 1) - 1, d ?? 1);
}

/** 本地日期 → "YYYY-MM-DD" */
export function toDay(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

/** 今日（本地时区）"YYYY-MM-DD" */
export function todayDay(): string {
  return toDay(new Date());
}

/** n 天前（含今天为第 0 天）"YYYY-MM-DD" */
export function daysAgo(n: number): string {
  const d = new Date();
  d.setDate(d.getDate() - n);
  return toDay(d);
}

/** 表格用：日期本地化 "2026/09/28"（§4.9 日期本地化） */
export function fmtDay(day: string): string {
  return parseDay(day).toLocaleDateString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  });
}

/** 图表轴用：短日期 "9/28" */
export function fmtDayShort(day: string): string {
  return parseDay(day).toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" });
}

/** 默认查询范围：近 n 天（含今天） */
export function defaultRange(days: number): { from: string; to: string } {
  return { from: daysAgo(days - 1), to: todayDay() };
}

/** 设备种类中文标签（kind 是开放枚举，未知值原样展示） */
export function kindLabel(kind: string): string {
  switch (kind) {
    case "keyboard":
      return "键盘";
    case "mouse":
      return "鼠标";
    case "gamepad":
      return "手柄";
    default:
      return kind;
  }
}
