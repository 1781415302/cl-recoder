// 通用可排序数据表格（§4.9 硬规则：表格可排序 + 默认值降序 + aria-sort + tabular-nums/等宽）。
// 仅对"查询返回的行"做展示排序/切片/分页，不做任何聚合（PLAN §2.5）。
// 分页（usability-runtime-v3 §4.8）：pageSize 提供后分页（与 renderLimit 互斥，pageSize 优先）；
// 排序/resetKey 语义变化回第一页，普通轮询行值变动不重置，页数缩小时 clamp；
// 页号属组件私有实现，对外不引入页号状态/回调；全部返回行均可通过翻页访问（不引入虚拟列表）。
import { useMemo, useState, type ReactNode } from "react";

export interface Column<T> {
  key: string;
  header: string;
  /** 排序与默认文本展示所用的值 */
  value: (row: T) => string | number;
  render?: (row: T) => ReactNode;
  /** 数字列：右对齐 + tabular-nums 等宽 */
  numeric?: boolean;
  sortable?: boolean; // 默认 true
}

export type SortDir = "asc" | "desc";

interface DataTableProps<T> {
  columns: Column<T>[];
  rows: T[];
  rowKey: (row: T, index: number) => string;
  /** 默认排序（§4.9：默认值降序） */
  initialSort?: { key: string; dir: SortDir };
  empty?: ReactNode;
  caption?: string;
  /** 大表保护：只渲染前 N 行（排序后），显示提示。展示层切片，不是聚合。 */
  renderLimit?: number;
  /** 分页：每页行数。提供后启用分页并忽略 renderLimit（二者互斥，pageSize 优先）。 */
  pageSize?: number;
  /** 语义重置键（如 `${deviceId}|${range}`）：其变化（设备/范围/排序语义）回第一页；
   *  key 不变的普通轮询行值变动不重置页号。 */
  resetKey?: string;
}

/** 分页内部状态：key 记录其所随的 resetKey（页号属组件私有实现，不外置为 API）。 */
interface PageState {
  key: string;
  page: number;
}

/** resetKey 语义变化 → 回第一页；key 不变（普通轮询行值变动）→ 保持页号。导出仅供测试接缝。 */
export function applyPageReset(
  prev: PageState,
  resetKey: string | undefined,
): PageState {
  const key = resetKey ?? "";
  return prev.key === key ? prev : { key, page: 1 };
}

/** 页号 clamp：收敛到 [1, 页数]（页数 = ceil(rowCount/pageSize)，最小 1）；
 *  未启用分页（pageSize 非正数/未提供）恒为 1。导出仅供测试接缝。 */
export function clampPageIndex(
  page: number,
  rowCount: number,
  pageSize: number | undefined,
): number {
  if (pageSize === undefined || !Number.isFinite(pageSize) || pageSize <= 0)
    return 1;
  const pageCount = Math.max(1, Math.ceil(rowCount / pageSize));
  return Math.min(Math.max(1, page), pageCount);
}

export function DataTable<T>(props: DataTableProps<T>) {
  const {
    columns,
    rows,
    rowKey,
    initialSort,
    empty,
    caption,
    renderLimit,
    pageSize,
    resetKey,
  } = props;
  const [sort, setSort] = useState<{ key: string; dir: SortDir } | null>(
    initialSort ?? null,
  );
  const [pageState, setPageState] = useState<PageState>(() => ({
    key: resetKey ?? "",
    page: 1,
  }));
  // resetKey 语义变化：渲染期对齐派生页状态回第一页（React 推荐模式，避免 effect 双渲染）；
  // key 不变的普通轮询行值变动不触发（不重置页号）。
  if (pageState.key !== (resetKey ?? "")) {
    setPageState(applyPageReset(pageState, resetKey));
  }

  const sorted = useMemo(() => {
    if (!sort) return rows;
    const col = columns.find((c) => c.key === sort.key);
    if (!col) return rows;
    const copy = [...rows];
    copy.sort((a, b) => {
      const va = col.value(a);
      const vb = col.value(b);
      const cmp =
        typeof va === "number" && typeof vb === "number"
          ? va - vb
          : String(va).localeCompare(String(vb), "zh-CN");
      return sort.dir === "asc" ? cmp : -cmp;
    });
    return copy;
  }, [rows, columns, sort]);

  const pageSizeNum =
    pageSize !== undefined && Number.isFinite(pageSize) && pageSize > 0
      ? pageSize
      : undefined;
  const paginated = pageSizeNum !== undefined;
  const pageCount =
    pageSizeNum !== undefined
      ? Math.max(1, Math.ceil(sorted.length / pageSizeNum))
      : 1;
  const page = clampPageIndex(pageState.page, sorted.length, pageSizeNum);
  const shown =
    pageSizeNum !== undefined
      ? sorted.slice((page - 1) * pageSizeNum, page * pageSizeNum)
      : renderLimit !== undefined
        ? sorted.slice(0, renderLimit)
        : sorted;

  function toggle(key: string) {
    setSort((prev) => {
      if (prev?.key !== key) return { key, dir: "desc" }; // 首次点击 = 值降序（§4.9）
      if (prev.dir === "desc") return { key, dir: "asc" };
      return null; // 第三次取消排序（回到后端原始顺序）
    });
    setPageState((prev) => ({ key: prev.key, page: 1 })); // 排序变化回第一页（§4.8）
  }

  function goto(page2: number) {
    setPageState((prev) => ({
      key: prev.key,
      page: clampPageIndex(page2, sorted.length, pageSizeNum),
    }));
  }

  if (rows.length === 0 && empty) return <>{empty}</>;

  return (
    <div>
      <div className="table-wrap">
        <table className="table">
          {caption ? (
            <caption
              style={{
                position: "absolute",
                width: 1,
                height: 1,
                overflow: "hidden",
                clip: "rect(0 0 0 0)",
              }}
            >
              {caption}
            </caption>
          ) : null}
          <thead>
            <tr>
              {columns.map((col) => {
                const sortable = col.sortable !== false;
                const active = sort?.key === col.key;
                const ariaSort = active
                  ? sort.dir === "asc"
                    ? "ascending"
                    : "descending"
                  : undefined;
                return (
                  <th
                    key={col.key}
                    scope="col"
                    aria-sort={ariaSort}
                    style={{ textAlign: col.numeric ? "right" : "left" }}
                  >
                    {sortable ? (
                      <button
                        type="button"
                        className="th-sort"
                        aria-pressed={active}
                        onClick={() => toggle(col.key)}
                        title={`按「${col.header}」排序`}
                      >
                        {col.header}
                        <svg
                          aria-hidden="true"
                          width="12"
                          height="14"
                          viewBox="0 0 12 14"
                          fill="none"
                          stroke="currentColor"
                          strokeWidth="1.3"
                        >
                          <path
                            d="m3 5 3-3 3 3"
                            opacity={!active || sort?.dir === "asc" ? 1 : 0.25}
                          />
                          <path
                            d="m3 9 3 3 3-3"
                            opacity={!active || sort?.dir === "desc" ? 1 : 0.25}
                          />
                        </svg>
                      </button>
                    ) : (
                      col.header
                    )}
                  </th>
                );
              })}
            </tr>
          </thead>
          <tbody>
            {shown.map((row, i) => (
              <tr key={rowKey(row, i)}>
                {columns.map((col) => (
                  <td
                    key={col.key}
                    className={col.numeric ? "num" : undefined}
                    style={{
                      textAlign: col.numeric ? "right" : "left",
                      whiteSpace: col.numeric ? "nowrap" : undefined,
                    }}
                  >
                    {col.render ? col.render(row) : String(col.value(row))}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {paginated ? (
        <nav className="pager" aria-label="表格分页">
          <span className="chart-hint">
            第 {page} / {pageCount} 页 · 每页 {pageSizeNum} 行 · 共{" "}
            {sorted.length} 行
          </span>
          <div style={{ display: "flex", gap: "var(--space-2)" }}>
            <button
              type="button"
              className="btn btn-sm"
              disabled={page <= 1}
              onClick={() => goto(page - 1)}
            >
              上一页
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={page >= pageCount}
              onClick={() => goto(page + 1)}
            >
              下一页
            </button>
          </div>
        </nav>
      ) : renderLimit !== undefined && sorted.length > shown.length ? (
        <p className="chart-hint">
          仅显示前 {shown.length} 行，共 {sorted.length}{" "}
          行（点击表头可排序查看）
        </p>
      ) : null}
    </div>
  );
}
