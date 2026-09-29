// 组合键页：仅修饰键组合（Ctrl/Shift/Alt/Win + 非修饰键），× 每天（R7）。
import { useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import * as client from "../api/client";
import { DataTable, type Column } from "../components/DataTable";
import { DateRangePicker } from "../components/DateRangePicker";
import { EmptyState } from "../components/EmptyState";
import { SkeletonCard } from "../components/Skeleton";
import { TopBarChart } from "../components/TopBarChart";
import { defaultRange, fmtNum } from "../lib/format";
import type { ComboRowLabeled } from "../api/types";

const columns: Column<ComboRowLabeled>[] = [
  { key: "label", header: "组合键", value: (r) => r.label, render: (r) => <span style={{ fontWeight: 600 }}>{r.label}</span> },
  { key: "code", header: "编码", value: (r) => r.code, render: (r) => <span className="mono">{`0x${r.code.toString(16).toUpperCase()}`}</span> },
  { key: "total", header: "累计次数", value: (r) => r.total, numeric: true, render: (r) => fmtNum(r.total) },
];

export function Combos() {
  const [range, setRange] = useState(() => defaultRange(30));
  const { data, isLoading } = useQuery({
    queryKey: ["combos", range.from, range.to],
    queryFn: () => client.getCombos(range.from, range.to, 200),
    placeholderData: keepPreviousData,
  });

  return (
    <>
      <div className="page-header">
        <div>
          <h1 className="page-title">组合键</h1>
          <p className="page-desc">修饰键组合（Ctrl/Shift/Alt/Win + 非修饰键）；纯修饰键按下不计入组合</p>
        </div>
        <DateRangePicker value={range} onChange={setRange} />
      </div>

      {isLoading ? (
        <SkeletonCard rows={6} />
      ) : (
        <TopBarChart
          title="组合键排行"
          rows={(data ?? []).map((r) => ({ label: r.label, value: r.total }))}
          colorVar="--chart-5"
          unit="次"
          labelHeader="组合键"
          valueHeader="累计次数"
          fullTable={
            <DataTable
              columns={columns}
              rows={data ?? []}
              rowKey={(r) => `${r.mods}-${r.code}`}
              initialSort={{ key: "total", dir: "desc" }}
              caption="组合键排行（所选范围）"
              empty={
                <EmptyState
                  title="还没有组合键记录"
                  description="使用带修饰键的快捷键（如 Ctrl+C）时会自动记录。"
                  action={{ label: "前往设置", onClick: () => { window.location.hash = "settings"; } }}
                />
              }
            />
          }
        />
      )}
    </>
  );
}
