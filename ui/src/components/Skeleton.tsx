// 加载骨架屏。
interface SkeletonProps {
  w?: string;
  h?: string;
  style?: React.CSSProperties;
}

export function Skeleton({ w = "100%", h = "14px", style }: SkeletonProps) {
  return (
    <div
      className="skeleton"
      style={{ width: w, height: h, ...style }}
      aria-hidden="true"
    />
  );
}

/** 卡片级骨架：标题 + 若干行 */
export function SkeletonCard({
  rows = 4,
  height,
}: {
  rows?: number;
  height?: string;
}) {
  return (
    <div className="card" role="status" aria-label="加载中">
      <Skeleton w="34%" h="16px" />
      <div className="stack" style={{ marginTop: 16 }}>
        {Array.from({ length: rows }, (_, i) => (
          <Skeleton key={i} h={height ?? "14px"} w={`${94 - i * 9}%`} />
        ))}
      </div>
    </div>
  );
}

/** KPI 骨架行 */
export function SkeletonCards({ count = 3 }: { count?: number }) {
  return (
    <div className="kpi-grid" role="status" aria-label="加载中">
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className="card">
          <Skeleton w="36%" h="12px" />
          <Skeleton w="62%" h="34px" style={{ marginTop: 14 }} />
        </div>
      ))}
    </div>
  );
}
