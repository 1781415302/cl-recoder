// 手柄页：独立于鼠标的手柄按键统计（R6；XInput 手柄单行合并为已声明降级，§9.3）。
import { DeviceStatsPage } from "../components/DeviceStatsPage";

export function Gamepad() {
  return (
    <DeviceStatsPage
      kind="gamepad"
      title="手柄"
      description="手柄按键计数（触发器以上穿 0.33 计 1 次，回落后才可再计）；XInput 手柄按固定名单行合并"
      colorVar="--chart-3"
    />
  );
}
