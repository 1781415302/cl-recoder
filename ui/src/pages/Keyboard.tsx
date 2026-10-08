// 键盘页：按设备型号统计按键次数（R4：每键 × 每天，scan code 为准）。
import { DeviceStatsPage } from "../components/DeviceStatsPage";

export function Keyboard() {
  return (
    <DeviceStatsPage
      kind="keyboard"
      title="键盘"
      description="每个按键的使用次数 · 长按自动重复不计"
      colorVar="--chart-1"
    />
  );
}
