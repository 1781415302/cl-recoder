// 设备切换（同型号设备分开统计）：胶囊式单选 + 昵称编辑（双击或铅笔图标）。
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

const hex4 = (n: number) => n.toString(16).padStart(4, "0").toUpperCase();

export function DeviceTabs({ devices, selectedId, onSelect, onRenamed }: DeviceTabsProps) {
  const [editingId, setEditingId] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function saveNickname(id: number) {
    const nick = draft.trim();
    try {
      await client.setDeviceNickname(id, nick === "" ? null : nick);
      setEditingId(null);
      setError(null);
      onRenamed?.();
    } catch (e) {
      // 保留编辑态，便于重试
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <div className="devtabs" role="tablist" aria-label="设备">
      {devices.map((d) => {
        const selected = d.id === selectedId;
        const label = d.nickname || d.name;
        if (editingId === d.id) {
          return (
            <span key={d.id} className="devtab-edit">
              <input
                className="input input-sm"
                value={draft}
                autoFocus
                maxLength={32}
                aria-label={`为 ${d.name} 设置昵称`}
                placeholder={d.name}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void saveNickname(d.id);
                  if (e.key === "Escape") setEditingId(null);
                }}
              />
              <button type="button" className="btn btn-sm btn-primary" onClick={() => void saveNickname(d.id)}>
                保存
              </button>
              <button type="button" className="btn btn-sm" onClick={() => setEditingId(null)}>
                取消
              </button>
              {error ? (
                <span className="field-error" role="alert">
                  {error}
                </span>
              ) : null}
            </span>
          );
        }
        return (
          <span key={d.id} className="devtab" data-selected={selected}>
            <button
              type="button"
              role="tab"
              aria-selected={selected}
              className="devtab-main"
              onClick={() => onSelect(d.id)}
              title={`${d.name}${d.nickname ? `（昵称 ${d.nickname}）` : ""}（VID ${hex4(d.vid)} / PID ${hex4(d.pid)}）双击改昵称`}
              onDoubleClick={() => {
                setEditingId(d.id);
                setDraft(d.nickname ?? "");
              }}
            >
              <span className="devtab-name">{label}</span>
              <span className="devtab-total num">历史 {fmtCompact(d.total)}</span>
            </button>
            <button
              type="button"
              className="devtab-rename"
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
