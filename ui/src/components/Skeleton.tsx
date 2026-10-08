// 加载骨架屏（§4.9：加载用骨架屏）。
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
      <Skeleton w="40%" h="18px" />
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: "var(--space-3)",
          marginTop: "var(--space-4)",
        }}
      >
        {Array.from({ length: rows }, (_, i) => (
          <Skeleton key={i} h={height ?? "14px"} w={`${92 - i * 9}%`} />
        ))}
      </div>
    </div>
  );
}

/** KPI 骨架行 */
export function SkeletonCards({ count = 3 }: { count?: number }) {
  return (
    <div className="grid-cards" role="status" aria-label="加载中">
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className="card">
          <Skeleton w="36%" h="12px" />
          <Skeleton w="60%" h="28px" style={{ marginTop: "var(--space-3)" }} />
        </div>
      ))}
    </div>
  );
}
