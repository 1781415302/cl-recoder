export interface MotionSummaryItem {
  id: string;
  label: string;
  value: string;
  hint?: string;
}

export interface MotionSummaryProps {
  items: readonly MotionSummaryItem[];
}

/** 指标条：并排的标签 / 大数字 / 说明 */
export function MotionSummary({ items }: MotionSummaryProps) {
  if (!items.length) return null;
  return (
    <dl className="metrics">
      {items.map((item) => (
        <div className="metric" key={item.id}>
          <dt className="metric-label">{item.label}</dt>
          <dd className="metric-value num">{item.value}</dd>
          {item.hint ? <dd className="metric-hint">{item.hint}</dd> : null}
        </div>
      ))}
    </dl>
  );
}
