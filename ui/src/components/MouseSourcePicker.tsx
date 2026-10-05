// 鼠标来源选择（motion-dpi §4.6 S7）：型号名＋来源序号的物理来源项 + 「按型号汇总」的型号历史项。
//
// 边界：纯选择列表，不查询 API、不改选择状态（选择属页面，轮询不跳源）；不暴露来源路径/键；
// 来源序号 = 同型号内按来源 id 升序的稳定次序（同型号两只鼠标显示为 来源 1/来源 2）；
// 型号历史项独立列出，仅指向型号级旧记录查询，不与来源项混排。
import { useId, useMemo } from "react";
import type { MouseSourceRow } from "../api/types";
import type { MouseModelRow, MouseSelection } from "../lib/motionPresentation";

export interface MouseSourcePickerProps {
  sources: MouseSourceRow[];
  models: MouseModelRow[];
  selection: MouseSelection | null;
  onSelect: (selection: MouseSelection) => void;
}

function modelNameOf(model: MouseModelRow): string {
  return model.nickname ?? model.name;
}

function sourceFallbackName(s: MouseSourceRow): string {
  return s.nickname ?? s.name;
}

function dpiChipText(s: MouseSourceRow): string {
  if (s.dpiOrigin === "auto" && s.autoDpi !== null) return `自动 ${s.autoDpi}`;
  if (s.dpiOrigin === "manual" && s.manualDpi !== null) return `手动 ${s.manualDpi}`;
  return "未配置";
}

export function MouseSourcePicker({ sources, models, selection, onSelect }: MouseSourcePickerProps) {
  const selectId = useId();
  // 按型号分组（型号名不暴露路径；型号缺失时回退来源自身名称），组内按 id 升序给稳定序号
  const groups = useMemo(() => {
    const byDevice = new Map<number, MouseSourceRow[]>();
    for (const s of [...sources].sort((a, b) => a.id - b.id)) {
      const list = byDevice.get(s.deviceId);
      if (list) list.push(s);
      else byDevice.set(s.deviceId, [s]);
    }
    return [...byDevice.entries()].map(([deviceId, list]) => {
      const model = models.find((m) => m.id === deviceId);
      return { deviceId, label: model ? modelNameOf(model) : sourceFallbackName(list[0]), list };
    });
  }, [sources, models]);

  const empty = groups.length === 0 && models.length === 0;

  return (
    <div className="card">
      <label className="field-label" htmlFor={selectId}>鼠标来源</label>
      {empty ? (
        <p className="chart-hint" style={{ marginTop: "var(--space-3)" }}>暂无鼠标来源与型号记录。</p>
      ) : (
        <select
          id={selectId}
          className="input"
          style={{ width: "100%", minHeight: 44, marginTop: "var(--space-2)" }}
          value={selection ? `${selection.kind}:${selection.id}` : ""}
          onChange={(e) => {
            const [kind, rawId] = e.currentTarget.value.split(":");
            const id = Number(rawId);
            if ((kind === "source" || kind === "model") && Number.isSafeInteger(id) && id > 0) onSelect({ kind, id });
          }}
        >
          <option value="" disabled>选择鼠标</option>
          {groups.map((g) => (
            <optgroup key={g.deviceId} label={g.label}>
              {g.list.map((s, i) => (
                <option key={s.id} value={`source:${s.id}`}>
                  {g.label} · 来源 {i + 1} · {s.connected ? "已连接" : "离线"} · {dpiChipText(s)}
                </option>
              ))}
            </optgroup>
          ))}
          {models.length > 0 ? (
            <optgroup label="型号历史（按钮汇总）">
              {models.map((m) => <option key={m.id} value={`model:${m.id}`}>{modelNameOf(m)} · 型号历史</option>)}
            </optgroup>
          ) : null}
        </select>
      )}
    </div>
  );
}
