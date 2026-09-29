// 鼠标页：按键 + 滚轮 + 移动距离，与手柄完全分开统计（R5）。
import { DeviceStatsPage } from "../components/DeviceStatsPage";

export function Mouse() {
  return (
    <DeviceStatsPage
      kind="mouse"
      title="鼠标"
      description="左/右/中/X1/X2 按键与滚轮四向，按设备型号分开统计（与手柄完全独立）"
      colorVar="--chart-2"
      showMouseDistance
    />
  );
}
