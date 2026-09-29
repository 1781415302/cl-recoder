// 通用可排序数据表格（§4.9 硬规则：表格可排序 + 默认值降序 + aria-sort + tabular-nums/等宽）。
// 仅对"查询返回的行"做展示排序/切片，不做任何聚合（PLAN §2.5）。
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
}

export function DataTable<T>(props: DataTableProps<T>) {
  const { columns, rows, rowKey, initialSort, empty, caption, renderLimit } = props;
  const [sort, setSort] = useState<{ key: string; dir: SortDir } | null>(initialSort ?? null);

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

  const shown = renderLimit !== undefined ? sorted.slice(0, renderLimit) : sorted;

  function toggle(key: string) {
    setSort((prev) => {
      if (prev?.key !== key) return { key, dir: "desc" }; // 首次点击 = 值降序（§4.9）
      if (prev.dir === "desc") return { key, dir: "asc" };
      return null; // 第三次取消排序（回到后端原始顺序）
    });
  }

  if (rows.length === 0 && empty) return <>{empty}</>;

  return (
    <div>
      <div className="table-wrap">
        <table className="table">
          {caption ? <caption style={{ position: "absolute", width: 1, height: 1, overflow: "hidden", clip: "rect(0 0 0 0)" }}>{caption}</caption> : null}
          <thead>
            <tr>
              {columns.map((col) => {
                const sortable = col.sortable !== false;
                const active = sort?.key === col.key;
                const ariaSort = active ? (sort.dir === "asc" ? "ascending" : "descending") : undefined;
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
                        <span aria-hidden="true">{active ? (sort?.dir === "asc" ? "▲" : "▼") : "↕"}</span>
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
      {renderLimit !== undefined && sorted.length > shown.length ? (
        <p className="chart-hint">
          仅显示前 {shown.length} 行，共 {sorted.length} 行（点击表头可排序查看）
        </p>
      ) : null}
    </div>
  );
}
