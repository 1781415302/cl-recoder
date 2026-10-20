import { describe, expect, it } from "vitest";
import { NICKNAME_MAX, normalizeNickname, validateNickname } from "./nickname";

const TOO_LONG = "昵称最多 32 个字符";

describe("NICKNAME_MAX", () => {
  it("为 32（UTF-16 代码单元）", () => {
    expect(NICKNAME_MAX).toBe(32);
  });
});

describe("normalizeNickname / validateNickname（§4.1 示例表）", () => {
  it("去首尾空白：\"  G102  \"", () => {
    expect(normalizeNickname("  G102  ")).toBe("G102");
    expect(validateNickname("  G102  ")).toBeNull();
  });

  it.each([[""], ["   "], ["\u3000"]])("空白输入 %j → null", (input) => {
    expect(normalizeNickname(input)).toBeNull();
    expect(validateNickname(input)).toBeNull();
  });

  it("32 个 a 合法", () => {
    const s = "a".repeat(32);
    expect(normalizeNickname(s)).toBe(s);
    expect(validateNickname(s)).toBeNull();
  });

  it("33 个 a 超长", () => {
    const s = "a".repeat(33);
    expect(normalizeNickname(s)).toBe(s);
    expect(validateNickname(s)).toBe(TOO_LONG);
  });

  it("16 个 😀（32 单元）合法", () => {
    const s = "😀".repeat(16);
    expect(s.length).toBe(32);
    expect(normalizeNickname(s)).toBe(s);
    expect(validateNickname(s)).toBeNull();
  });

  it("17 个 😀 超长", () => {
    const s = "😀".repeat(17);
    expect(normalizeNickname(s)).toBe(s);
    expect(validateNickname(s)).toBe(TOO_LONG);
  });

  it("不转义、不做白名单：\"<b>x</b>\"", () => {
    expect(normalizeNickname("<b>x</b>")).toBe("<b>x</b>");
    expect(validateNickname("<b>x</b>")).toBeNull();
  });

  it("全角空格包裹时被 trim，校验按 trim 后长度", () => {
    expect(normalizeNickname("\u3000G102\u3000")).toBe("G102");
    expect(validateNickname(`\u3000${"a".repeat(32)}\u3000`)).toBeNull();
  });
});
