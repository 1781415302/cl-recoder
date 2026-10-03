// 手柄页：独立于鼠标的手柄按键统计（R6；XInput 手柄单行合并为已声明降级，§9.3）。
import { DeviceStatsPage } from "../components/DeviceStatsPage";

export function Gamepad() {
  return (
    <DeviceStatsPage
      kind="gamepad"
      title="手柄"
      description="XInput/Xbox 系手柄按键计数（扳机上穿 0.33 计 1 次）；非 XInput 手柄（如部分 PS/Switch）暂不统计"
      colorVar="--chart-3"
    />
  );
}
