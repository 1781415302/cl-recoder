# install-collector-task.ps1 —— CL Recoder 采集器自启计划任务安装（PLAN §3/§5.1/§8-S13）
#
# 作用（与 PLAN 逐字对齐）：
#   schtasks /Create /TN ClRecoderCollector /TR <采集器绝对路径> /SC ONLOGON /RL HIGHEST /F
#
# 调用方式：
#   - GUI（正式路径，§4.7 collector_autostart_enable）：外层 powershell 以 -Verb RunAs
#     提权执行 `-NoProfile -ExecutionPolicy Bypass -File <本脚本>`，无任何附加参数——
#     采集器路径由本脚本自行定位；
#   - 手动：以管理员身份打开 PowerShell 后运行本脚本（无参数即可）。
#
# 定位规则（提权进程的 CWD 是 C:\Windows\System32，必须基于 $PSScriptRoot）：
#   1. -CollectorPath 显式指定（优先）；
#   2. <脚本目录>\..\cl-recoder-collector.exe          —— 安装布局：脚本随 bundle.resources
#      落盘在主程序 exe 同目录的 scripts\ 下（§8-S13）；
#   3. <脚本目录>\..\target\release\cl-recoder-collector.exe —— 开发布局：仓库 scripts\。
#
# 退出码：0 成功；1 采集器未找到 / 权限不足；其他非 0 为 schtasks 错误码。

param(
  [string]$CollectorPath = "",
  # 仅测试用：覆盖任务名（GUI/正常使用一律走默认 ClRecoderCollector）
  [string]$TaskName = "ClRecoderCollector"
)

# 注意：用 Continue——PS 5.1 中 schtasks 失败时 stderr 会变成 ErrorRecord，
# Stop 模式下脚本直接终止、$LASTEXITCODE 传不出来；本脚本所有错误路径都
# 显式 exit，Continue 不影响错误判定。
$ErrorActionPreference = "Continue"
$ExeName = "cl-recoder-collector.exe"

# ---------------------------------------------------------------------------
# 1. 定位采集器
# ---------------------------------------------------------------------------
$candidates = @()
if ($CollectorPath -ne "") { $candidates += $CollectorPath }
if ($PSScriptRoot) {
  $candidates += (Join-Path $PSScriptRoot (Join-Path ".." $ExeName))
  $candidates += (Join-Path $PSScriptRoot (Join-Path ".." (Join-Path "target" (Join-Path "release" $ExeName))))
}

$exe = $null
foreach ($c in $candidates) {
  if (Test-Path -LiteralPath $c -PathType Leaf) {
    $exe = (Resolve-Path -LiteralPath $c).Path
    break
  }
}
if (-not $exe) {
  Write-Error ("未找到采集器 {0}（查找过：{1}）" -f $ExeName, ($candidates -join "；"))
  exit 1
}

# ---------------------------------------------------------------------------
# 2. 权限校验：/RL HIGHEST 的任务必须由管理员令牌创建
# ---------------------------------------------------------------------------
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  Write-Error "需要管理员权限：GUI 会经 UAC 提权运行本脚本；手动运行请以管理员身份打开 PowerShell。"
  exit 1
}

# ---------------------------------------------------------------------------
# 3. 创建任务
#    /TR 的值必须以"内嵌引号"到达 schtasks（微软官方文档配方 /TR "\"C:\...\""）：
#    含空格的路径（安装目录默认为 %LOCALAPPDATA%\CL Recoder）若值内不含引号，
#    会被 schtasks 拆成 Command + Arguments，任务路径断裂（§8-S13，已实测复现）。
#    PS 5.1 直接传 '"path"' 时 CRT argv 解析会剥掉引号、schtasks 收到裸路径——
#    故用 --% 停止解析符：PS 原样传递后续文本并展开 %环境变量%，schtasks 的
#    argv 解码把 \" 还原为字面引号，最终 /TR 值恰为 "C:\...\cl-recoder-collector.exe"。
# ---------------------------------------------------------------------------
$env:CLREC_COLLECTOR_EXE = $exe
$env:CLREC_TASK_NAME = $TaskName
schtasks --% /Create /TN "%CLREC_TASK_NAME%" /TR "\"%CLREC_COLLECTOR_EXE%\"" /SC ONLOGON /RL HIGHEST /F
$createExit = $LASTEXITCODE
Remove-Item Env:\CLREC_COLLECTOR_EXE -ErrorAction SilentlyContinue
Remove-Item Env:\CLREC_TASK_NAME -ErrorAction SilentlyContinue
if ($createExit -ne 0) { exit $createExit }

# 任务已创建：立即拉起采集器（本脚本已在管理员上下文，schtasks /Run 以任务的
# HIGHEST 权限启动，无需再弹 UAC）。失败不视为安装失败——GUI 侧还会 wait_collector。
schtasks /Run /TN "$TaskName" | Out-Null

Write-Host ("已创建计划任务 {0}（用户登录时自启，最高权限）" -f $TaskName)
Write-Host ("  采集器：{0}" -f $exe)
exit 0
