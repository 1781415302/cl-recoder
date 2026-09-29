# check-task.ps1 —— 查询采集器自启计划任务是否存在（PLAN §3/§8-S13）
#
# 输出（stdout，唯一契约）：exists 或 missing
# 退出码：0 = exists；1 = missing（便于脚本判断；stdout 仍以此为准）
#
# 调用方式：
#   powershell -NoProfile -ExecutionPolicy Bypass -File check-task.ps1
#   可选：-TaskName <名称>（默认 ClRecoderCollector）

param(
  [string]$TaskName = "ClRecoderCollector"
)

$ErrorActionPreference = "SilentlyContinue"

$null = schtasks /Query /TN $TaskName 2>$null
if ($LASTEXITCODE -eq 0) {
  Write-Output "exists"
  exit 0
} else {
  Write-Output "missing"
  exit 1
}
