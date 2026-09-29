// 键盘页：按设备型号统计按键次数（R4：每键 × 每天，scan code 为准）。
import { DeviceStatsPage } from "../components/DeviceStatsPage";

export function Keyboard() {
  return (
    <DeviceStatsPage
      kind="keyboard"
      title="键盘"
      description="按设备型号统计每个键的按下次数（物理按下边沿，自动重复不计；跨布局稳定，以 scan code 为准）"
      colorVar="--chart-1"
    />
  );
}
