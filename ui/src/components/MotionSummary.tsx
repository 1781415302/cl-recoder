// 运动概览指标条（motion-dpi §4.6 S7）：label/value/hint 纯展示（值已由页面格式化，本组件不做换算）。
// 无状态、无查询；空 items 不渲染。用于鼠标页 3 项概览等场景。
export interface MotionSummaryItem {
  id: string;
  label: string;
  value: string;
  hint?: string;
}

export interface MotionSummaryProps {
  items: readonly MotionSummaryItem[];
}

export function MotionSummary({ items }: MotionSummaryProps) {
  if (items.length === 0) return null;
  return (
    <dl className="motion-summary">
      {items.map((item) => (
        <div key={item.id} className="motion-summary-item">
          <dt className="motion-summary-label">{item.label}</dt>
          <dd className="motion-summary-value">{item.value}</dd>
          {item.hint ? <dd className="motion-summary-hint">{item.hint}</dd> : null}
        </div>
      ))}
    </dl>
  );
}
