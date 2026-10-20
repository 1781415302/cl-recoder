// 型号昵称行内编辑：查看态显示当前名称 + 铅笔，编辑态输入框 + 保存/取消。
// 只负责写入昵称；刷新由父组件经 onSaved 注入（不依赖 React Query）。
import { useEffect, useRef, useState } from "react";
import * as client from "../api/client";
import { IconEdit } from "./icons";
import { NICKNAME_MAX, normalizeNickname, validateNickname } from "../lib/nickname";

export interface DeviceNicknameEditorProps {
  /** devices.id（型号 id） */
  deviceId: number;
  /** 设备原名（devices.name），用于 placeholder / aria / 「原名」展示 */
  name: string;
  /** 当前昵称；null = 未设置 */
  nickname: string | null;
  /** 写入成功后调用；返回 Promise 时编辑器等待其完成再退出编辑态。其失败/拒绝被忽略（写入已成功） */
  onSaved: () => void | Promise<void>;
}

export function DeviceNicknameEditor({ deviceId, name, nickname, onSaved }: DeviceNicknameEditorProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 卸载保护：卸载后不再 setState
  const mountedRef = useRef(true);
  // 同步防重复提交（state 更新是异步的）
  const savingRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  function startEdit() {
    // 草稿仅在进入编辑态时初始化，轮询刷新 props 不会覆盖
    setDraft(nickname ?? "");
    setError(null);
    setEditing(true);
  }

  function cancelEdit() {
    if (savingRef.current) return;
    setEditing(false);
    setError(null);
  }

  async function save() {
    if (savingRef.current) return;
    const invalid = validateNickname(draft);
    if (invalid) {
      setError(invalid);
      return;
    }
    savingRef.current = true;
    setSaving(true);
    try {
      await client.setDeviceNickname(deviceId, normalizeNickname(draft));
    } catch (e) {
      // 保留编辑态，便于重试
      savingRef.current = false;
      if (mountedRef.current) {
        setSaving(false);
        setError(e instanceof Error ? e.message : String(e));
      }
      return;
    }
    try {
      await onSaved();
    } catch {
      // 写入已成功，刷新失败忽略
    }
    savingRef.current = false;
    if (!mountedRef.current) return;
    setSaving(false);
    setError(null);
    setEditing(false);
  }

  if (editing) {
    return (
      <div className="nick-editor">
        <input
          className="input input-sm"
          value={draft}
          autoFocus
          disabled={saving}
          maxLength={NICKNAME_MAX}
          aria-label={`为 ${name} 设置昵称`}
          placeholder={name}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              // 输入法组合中的回车用于选词，不提交
              if (e.nativeEvent.isComposing) return;
              void save();
            } else if (e.key === "Escape") {
              cancelEdit();
            }
          }}
        />
        <button type="button" className="btn btn-sm btn-primary" disabled={saving} onClick={() => void save()}>
          保存
        </button>
        <button type="button" className="btn btn-sm" disabled={saving} onClick={cancelEdit}>
          取消
        </button>
        {error ? (
          <span className="field-error" role="alert">
            {error}
          </span>
        ) : null}
      </div>
    );
  }

  return (
    <div className="nick-editor">
      <span className="nick-editor-label">名称</span>
      <strong className="nick-editor-value">{nickname ?? name}</strong>
      <button
        type="button"
        className="nick-editor-edit"
        aria-label={`重命名 ${name}`}
        title="设置昵称"
        onClick={startEdit}
      >
        <IconEdit size={14} />
      </button>
      <p className="hint nick-editor-hint">
        {nickname ? `原名：${name}。` : ""}同型号的多只鼠标共用此名称。
      </p>
    </div>
  );
}
