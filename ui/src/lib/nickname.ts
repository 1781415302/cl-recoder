/** 昵称最大长度，单位 UTF-16 代码单元（即 String.length），与 <input maxLength> 同口径 */
export const NICKNAME_MAX = 32;

/** 超长时的固定提示文案 */
const TOO_LONG_MESSAGE = "昵称最多 32 个字符";

/** 去首尾空白（含全角空格）；结果为空串 → null。用于提交给后端 */
export function normalizeNickname(draft: string): string | null {
  const nick = draft.trim();
  return nick === "" ? null : nick;
}

/** 校验草稿：对 trim 后的值，length > NICKNAME_MAX 返回固定文案，否则 null */
export function validateNickname(draft: string): string | null {
  return draft.trim().length > NICKNAME_MAX ? TOO_LONG_MESSAGE : null;
}
