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
  if (!items.length) return null;
  return (
    <dl className="motion-summary">
      {items.map((item) => (
        <div className="motion-summary-item" key={item.id}>
          <dt className="motion-summary-label">{item.label}</dt>
          <dd className="motion-summary-value">{item.value}</dd>
          {item.hint && <dd className="motion-summary-hint">{item.hint}</dd>}
        </div>
      ))}
    </dl>
  );
}
