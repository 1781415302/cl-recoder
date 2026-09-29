# uninstall-collector-task.ps1 —— 删除 CL Recoder 采集器自启计划任务（PLAN §3/§8-S13）
#
# 对应 install-collector-task.ps1 创建的 schtasks 任务；GUI 的
# collector_autostart_disable（§4.7）与本仓库卸载清理都会调用。
#
# 幂等：任务不存在视为成功（重复停用/从未安装过均不算错误）。
#
# 调用方式：
#   powershell -NoProfile -ExecutionPolicy Bypass -File uninstall-collector-task.ps1
#   （GUI 经 UAC 提权调用；删除当前用户自己的任务通常无需提权，提权亦无害）
#
# 退出码：0 成功（或任务本就不存在）；其他非 0 为 schtasks 错误码。

param(
  # 仅测试用：覆盖任务名（GUI/正常使用一律走默认 ClRecoderCollector）
  [string]$TaskName = "ClRecoderCollector"
)

# 注意：必须用 Continue——PS 5.1 里 2>$null 会把 schtasks 的 stderr 变成 ErrorRecord，
# Stop 模式下会直接终止脚本，幂等"任务不存在"分支就永远到不了（已实测复现）。
$ErrorActionPreference = "Continue"

$null = schtasks /Query /TN $TaskName 2>$null
if ($LASTEXITCODE -ne 0) {
  Write-Host ("计划任务 {0} 不存在——无需删除" -f $TaskName)
  exit 0
}

schtasks /Delete /TN $TaskName /F
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host ("已删除计划任务 {0}" -f $TaskName)
exit 0
