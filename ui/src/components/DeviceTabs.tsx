// 设备标签页（同型号设备分开统计，R3；当前选中高亮 + aria 语义 + 昵称）。
import { useState } from "react";
import type { DeviceRow } from "../api/types";
import * as client from "../api/client";
import { IconEdit } from "./icons";
import { fmtCompact } from "../lib/format";

interface DeviceTabsProps {
  devices: DeviceRow[];
  selectedId: number | null;
  onSelect: (id: number) => void;
  onRenamed?: () => void;
}

export function DeviceTabs({
  devices,
  selectedId,
  onSelect,
  onRenamed,
}: DeviceTabsProps) {
  const [editingId, setEditingId] = useState<number | null>(null);
  const [draft, setDraft] = useState("");

  async function saveNickname(id: number) {
    const nick = draft.trim();
    try {
      await client.setDeviceNickname(id, nick === "" ? null : nick);
      setEditingId(null);
      onRenamed?.();
    } catch {
      // 保留编辑态，便于重试
    }
  }

  return (
    <div role="tablist" aria-label="设备" className="device-selector">
      {devices.map((d) => {
        const selected = d.id === selectedId;
        const label = d.nickname || d.name;
        if (editingId === d.id) {
          return (
            <span
              key={d.id}
              style={{
                display: "inline-flex",
                gap: "var(--space-1)",
                alignItems: "center",
              }}
            >
              <input
                className="input"
                value={draft}
                autoFocus
                maxLength={32}
                aria-label={`为 ${d.name} 设置昵称`}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void saveNickname(d.id);
                  if (e.key === "Escape") setEditingId(null);
                }}
                style={{ width: 140 }}
              />
              <button
                type="button"
                className="btn btn-sm btn-primary"
                onClick={() => void saveNickname(d.id)}
              >
                保存
              </button>
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => setEditingId(null)}
              >
                取消
              </button>
            </span>
          );
        }
        return (
          <span
            key={d.id}
            className="device-tab-group"
            data-selected={selected}
          >
            <button
              type="button"
              role="tab"
              aria-selected={selected}
              className="tab"
              onClick={() => onSelect(d.id)}
              title={`${d.name}${d.nickname ? `（昵称 ${d.nickname}）` : ""}（VID ${d.vid.toString(16).padStart(4, "0").toUpperCase()} / PID ${d.pid.toString(16).padStart(4, "0").toUpperCase()}）双击改昵称`}
              onDoubleClick={() => {
                setEditingId(d.id);
                setDraft(d.nickname ?? "");
              }}
            >
              {label}
              <span className="tab-total num">历史 {fmtCompact(d.total)}</span>
            </button>
            <button
              type="button"
              className="tab-rename"
              aria-label={`重命名 ${d.name}`}
              title="设置昵称"
              onClick={() => {
                setEditingId(d.id);
                setDraft(d.nickname ?? "");
              }}
            >
              <IconEdit size={14} />
            </button>
          </span>
        );
      })}
    </div>
  );
}
