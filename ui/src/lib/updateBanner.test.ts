import { describe, expect, it } from "vitest";
import type { UpdateApplied } from "../api/types";
import { formatUpdateBanner } from "./updateBanner";

function applied(partial: Partial<UpdateApplied> & Pick<UpdateApplied, "outcome">): UpdateApplied {
  return {
    fromVersion: "0.4.0",
    toVersion: "0.5.0",
    collectorWasRunning: false,
    collectorRestored: true,
    ...partial,
  };
}

describe("formatUpdateBanner", () => {
  it("applied keeps existing copy", () => {
    const v = formatUpdateBanner(applied({ outcome: "applied" }));
    expect(v.message).toBe("已更新到 v0.5.0。");
    expect(v.showGoSettings).toBe(false);
  });

  it("applied + collector stopped shows settings hint", () => {
    const v = formatUpdateBanner(
      applied({
        outcome: "applied",
        collectorWasRunning: true,
        collectorRestored: false,
      }),
    );
    expect(v.message).toContain("已更新到 v0.5.0。");
    expect(v.message).toContain("采集器在更新时被停止，请到设置页启动。");
    expect(v.showGoSettings).toBe(true);
  });

  it("not_applied shows current still fromVersion", () => {
    const v = formatUpdateBanner(applied({ outcome: "not_applied" }));
    expect(v.message).toBe("更新未完成，当前仍是 v0.4.0。");
    expect(v.showGoSettings).toBe(false);
  });

  it("not_applied + collector not restored appends stop hint", () => {
    const v = formatUpdateBanner(
      applied({
        outcome: "not_applied",
        collectorWasRunning: true,
        collectorRestored: false,
      }),
    );
    expect(v.message).toBe(
      "更新未完成，当前仍是 v0.4.0。采集器已停止，请到设置页启动。",
    );
    expect(v.showGoSettings).toBe(true);
  });
});
