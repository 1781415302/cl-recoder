# install-collector-task.ps1 —— CL Recoder 采集器自启计划任务安装/修复（usability-runtime-v3 §4.3）
#
# 作用：
#   - 创建（仅任务不存在时，沿用既有方式）：
#       schtasks /Create /TN ClRecoderCollector /TR <采集器绝对路径> /SC ONLOGON /RL HIGHEST /F
#       随后以纯 XML 变换（scripts\collector-task-policy.ps1）设置并回读三项策略，再 /Run；
#   - 已存在：COM 读取原定义与安全描述符（GetSecurityDescriptor 0x7 = OWNER|GROUP|DACL），
#       应用纯 XML 后 RegisterTask 更新（flags = 0x34：TASK_UPDATE | TASK_DONT_ADD_PRINCIPAL_ACE
#       | TASK_IGNORE_REGISTRATION_TRIGGERS），传原 principal/logonType 及原 SDDL，回读确认
#       三项与 owner/group/DACL 等价——不 /Create /F 重建；
#   - -RepairOnly：任务必须存在；不要求重新定位采集器文件；只更新策略，不 /Run，
#       不结束任何进程（修复由用户点击发起，无静默提权）。
#
# 安全约束（§4.3）：
#   - 仅支持当前用户 InteractiveToken + 仅 LogonTrigger 定义；principal / 登录类型 /
#     额外触发器（Registration/Time/Calendar/Boot 等）/ 安全描述符不可得 → 明确冲突
#     （exit 5），只读不修改；更新后回读不符 → exit 6（不宣称安装成功）；
#   - -ExpectedUserSid 由 GUI 后端可信获取传入；提权身份 SID 与预期不一致 → exit 1，
#     不悄悄创建/修改其它账户的任务；手动缺省取当前身份；
#   - 本脚本绝不结束任何进程（无强杀工具调用、无停止任务调用）。
#
# 调用方式：
#   - GUI（§4.7 collector_autostart_enable / collector_autostart_repair）：外层 powershell
#     以 -Verb RunAs 提权执行 `-NoProfile -ExecutionPolicy Bypass -File <本脚本> [参数]`；
#   - 手动：以管理员身份打开 PowerShell 后运行本脚本（无参数即可）。
#
# 定位规则（提权进程的 CWD 是 C:\Windows\System32，必须基于 $PSScriptRoot；仅创建路径需要）：
#   1. -CollectorPath 显式指定（优先）；
#   2. <脚本目录>\..\cl-recoder-collector.exe          —— 安装布局：脚本随 bundle.resources
#      落盘在主程序 exe 同目录的 scripts\ 下（§8-S13）；
#   3. <脚本目录>\..\target\release\cl-recoder-collector.exe —— 开发布局：仓库 scripts\。
#
# 退出码：0 成功；1 采集器未找到 / 权限不足 / 预期 SID 不符；5 定义冲突（只读，未修改）；
#         6 更新后回读校验失败；其它非 0 为 schtasks 错误码。

param(
  [string]$CollectorPath = "",
  # 仅测试用：覆盖任务名（GUI/正常使用一律走默认 ClRecoderCollector）
  [string]$TaskName = "ClRecoderCollector",
  # 修复既有任务：只更新三项策略并回读，不 /Run、不 /Create、不结束任何进程
  [switch]$RepairOnly,
  # GUI 后端可信获取的预期用户 SID；手动缺省取当前身份
  [string]$ExpectedUserSid = ""
)

# 注意：用 Continue——PS 5.1 中 schtasks 失败时 stderr 会变成 ErrorRecord，
# Stop 模式下脚本直接终止、$LASTEXITCODE 传不出来；本脚本所有错误路径都
# 显式 exit，Continue 不影响错误判定。
$ErrorActionPreference = "Continue"
$ExeName = "cl-recoder-collector.exe"
$TaskNs = "http://schemas.microsoft.com/windows/2004/02/mit/task"
# RegisterTask 更新 flags（§4.3 固定）：TASK_UPDATE(0x4) | TASK_DONT_ADD_PRINCIPAL_ACE(0x10)
# | TASK_IGNORE_REGISTRATION_TRIGGERS(0x20) = 0x34——RepairOnly 不会因更新动作触发注册/时间任务
$RegisterFlags = 0x34
# GetSecurityDescriptor 的 SECURITY_INFORMATION：OWNER(0x1) | GROUP(0x2) | DACL(0x4)
$SecInfoOwnerGroupDacl = 0x7
# RegisterTask 的 logonType 参数：TASK_LOGON_INTERACTIVE_TOKEN（与原定义一致，凭据为空）
$LogonInteractiveToken = 3

# ---------------------------------------------------------------------------
# 工具函数
# ---------------------------------------------------------------------------

# NTAccount/SID 文本 → SID 字符串（已是 SID 原样返回；解析失败返回 $null）
function ConvertTo-SidString([string]$Id) {
  if (-not $Id) { return $null }
  $trimmed = $Id.Trim()
  if (-not $trimmed) { return $null }
  if ($trimmed -like "S-1-*") { return $trimmed }
  try {
    $nt = New-Object Security.Principal.NTAccount($trimmed)
    return $nt.Translate([Security.Principal.SecurityIdentifier]).Value
  } catch {
    return $null
  }
}

# SDDL 规范化指纹：owner/group/DACL 控制标志原样 + ACE 排序集合（等价判定不靠字符串顺序）
function Get-SddlFingerprint([string]$Sddl) {
  if (-not $Sddl) { return "" }
  $rest = $Sddl.Trim()
  $owner = ""; $group = ""; $daclFlags = ""; $aces = @()
  if ($rest -match "^O:([^GD]*)") { $owner = $Matches[1]; $rest = $rest.Substring($Matches[0].Length) }
  if ($rest -match "^G:([^D]*)") { $group = $Matches[1]; $rest = $rest.Substring($Matches[0].Length) }
  if ($rest -match "^D:([^()]*)") { $daclFlags = $Matches[1]; $rest = $rest.Substring($Matches[0].Length) }
  foreach ($m in [regex]::Matches($rest, "\(([^()]*)\)")) { $aces += $m.Groups[1].Value }
  $sorted = @($aces | Sort-Object) -join "|"
  return "O=$owner;G=$group;D=$daclFlags;ACES=$sorted"
}

# ---------------------------------------------------------------------------
# 1. 权限校验：/RL HIGHEST 的任务必须由管理员令牌创建/更新
# ---------------------------------------------------------------------------
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  Write-Error "需要管理员权限：GUI 会经 UAC 提权运行本脚本；手动运行请以管理员身份打开 PowerShell。"
  exit 1
}

# ---------------------------------------------------------------------------
# 2. 身份校验：提权身份必须与预期用户一致（不悄悄创建/修改其它账户的任务）
# ---------------------------------------------------------------------------
$currentSid = $identity.User.Value
if ($ExpectedUserSid -and $ExpectedUserSid -ne $currentSid) {
  Write-Error ("提权身份 SID（{0}）与预期（{1}）不一致——拒绝操作其它账户的任务" -f $currentSid, $ExpectedUserSid)
  exit 1
}
$expectedSid = if ($ExpectedUserSid) { $ExpectedUserSid } else { $currentSid }

# ---------------------------------------------------------------------------
# 3. 装载纯策略变换 helper + COM 连接 + 任务存在性（只读）
# ---------------------------------------------------------------------------
$policyHelper = Join-Path $PSScriptRoot "collector-task-policy.ps1"
if (-not (Test-Path -LiteralPath $policyHelper -PathType Leaf)) {
  Write-Error ("未找到策略 helper {0}（程序安装可能不完整）" -f $policyHelper)
  exit 1
}
. $policyHelper

$service = New-Object -ComObject Schedule.Service
$service.Connect()
$rootFolder = $service.GetFolder("\")

$existingTask = $null
try { $existingTask = $rootFolder.GetTask($TaskName) } catch {
  $failure = $_.Exception
  $confirmedAbsent = $false
  while ($null -ne $failure) {
    if ($failure.HResult -eq -2147024894) { $confirmedAbsent = $true } # ERROR_FILE_NOT_FOUND
    $failure = $failure.InnerException
  }
  if (-not $confirmedAbsent) {
    Write-Error ("无法确认任务 {0} 是否存在：{1}；未修改任务" -f $TaskName, $_.Exception.Message)
    exit 5
  }
}

if ($RepairOnly -and -not $existingTask) {
  Write-Error ("计划任务 {0} 不存在——修复需要任务已安装（请先执行「启用采集器自启」创建）" -f $TaskName)
  exit 1
}

$exe = $null

# ---------------------------------------------------------------------------
# 4. 创建路径（仅任务不存在时；沿用既有 schtasks 方式；RepairOnly 永不进入）
# ---------------------------------------------------------------------------
if (-not $existingTask) {
  $candidates = @()
  if ($CollectorPath -ne "") { $candidates += $CollectorPath }
  if ($PSScriptRoot) {
    $candidates += (Join-Path $PSScriptRoot (Join-Path ".." $ExeName))
    $candidates += (Join-Path $PSScriptRoot (Join-Path ".." (Join-Path "target" (Join-Path "release" $ExeName))))
  }
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

  # /TR 的值必须以"内嵌引号"到达 schtasks（微软官方文档配方 /TR "\"C:\...\""）：
  # 含空格的路径若值内不含引号，会被 schtasks 拆成 Command + Arguments（§8-S13 实测）。
  # PS 5.1 直接传 '"path"' 时 CRT argv 解析会剥掉引号——故用 --% 停止解析符。
  $env:CLREC_COLLECTOR_EXE = $exe
  $env:CLREC_TASK_NAME = $TaskName
  schtasks --% /Create /TN "%CLREC_TASK_NAME%" /TR "\"%CLREC_COLLECTOR_EXE%\"" /SC ONLOGON /RL HIGHEST /F
  $createExit = $LASTEXITCODE
  Remove-Item Env:\CLREC_COLLECTOR_EXE -ErrorAction SilentlyContinue
  Remove-Item Env:\CLREC_TASK_NAME -ErrorAction SilentlyContinue
  if ($createExit -ne 0) {
    Write-Error ("schtasks /Create 失败（退出码 {0}）——安装未成功" -f $createExit)
    exit $createExit
  }

  try { $existingTask = $rootFolder.GetTask($TaskName) } catch { $existingTask = $null }
  if (-not $existingTask) {
    Write-Error ("schtasks /Create 已返回，但任务 {0} 无法读取——安装未成功" -f $TaskName)
    exit 6
  }
}

# ---------------------------------------------------------------------------
# 5. 定义兼容性校验（只读）：仅当前用户 InteractiveToken + 仅 LogonTrigger + SDDL 可得；
#    不满足 → 明确冲突（exit 5），只读不修改——不能覆盖其他账户的定义
# ---------------------------------------------------------------------------
$taskXml = $existingTask.Xml   # IRegisteredTask.Xml（PS 5.1 下 Definition.Xml 返回 null，禁用）
if (-not $taskXml) {
  Write-Error ("无法读取任务 {0} 的定义——无法取得必要权限信息，只读退出" -f $TaskName)
  exit 5
}
$defDoc = New-Object System.Xml.XmlDocument
try { $defDoc.LoadXml($taskXml) } catch {
  Write-Error ("任务 {0} 定义 XML 解析失败：{1}" -f $TaskName, $_.Exception.Message)
  exit 5
}

$principalEl = $defDoc.GetElementsByTagName("Principal", $TaskNs) | Select-Object -First 1
if (-not $principalEl) {
  Write-Error ("任务 {0} 无 Principal 定义——仅支持当前用户交互令牌任务，只读报冲突，未修改" -f $TaskName)
  exit 5
}
$logonType = ""
$userIdText = ""
foreach ($child in $principalEl.ChildNodes) {
  if ($child.LocalName -eq "LogonType") { $logonType = $child.InnerText.Trim() }
  if ($child.LocalName -eq "UserId") { $userIdText = $child.InnerText.Trim() }
}
if ($logonType -ne "InteractiveToken" -or -not $userIdText) {
  $shown = $logonType
  if (-not $shown) { $shown = "<缺失>" }
  Write-Error ("任务 {0} 登录类型为 {1}——仅支持当前用户 InteractiveToken 定义，只读报冲突，未修改" -f $TaskName, $shown)
  exit 5
}
$principalSid = ConvertTo-SidString $userIdText
if (-not $principalSid -or $principalSid -ne $expectedSid) {
  Write-Error ("任务 {0} 归属用户（{1}）与当前用户（{2}）不一致——不能覆盖其他账户的定义，只读退出" -f $TaskName, $userIdText, $expectedSid)
  exit 5
}

$triggersEl = $defDoc.GetElementsByTagName("Triggers", $TaskNs) | Select-Object -First 1
$triggerNames = @()
if ($triggersEl) {
  $triggerNames = @($triggersEl.ChildNodes | ForEach-Object { $_.LocalName })
}
$nonLogon = @($triggerNames | Where-Object { $_ -ne "LogonTrigger" })
if ($triggerNames.Count -lt 1 -or $nonLogon.Count -gt 0) {
  Write-Error ("任务 {0} 的触发器为 [{1}]——本项目仅支持登录触发（LogonTrigger）定义，只读报冲突，未修改" -f $TaskName, ($triggerNames -join ", "))
  exit 5
}

$originalSddl = $null
try { $originalSddl = $existingTask.GetSecurityDescriptor($SecInfoOwnerGroupDacl) } catch { $originalSddl = $null }
if (-not $originalSddl) {
  Write-Error ("无法读取任务 {0} 的安全描述符——不能传空 SDDL 冒充保全，只读报冲突，未修改" -f $TaskName)
  exit 5
}

# ---------------------------------------------------------------------------
# 6. 应用纯 XML 三项策略 → RegisterTask（0x34 + 原 SDDL）→ 回读
# ---------------------------------------------------------------------------
$policyDoc = Get-CollectorTaskPolicyXml $defDoc
$updatedTask = $null
try {
  $updatedTask = $rootFolder.RegisterTask($TaskName, $policyDoc.OuterXml, $RegisterFlags, $null, $null, $LogonInteractiveToken, $originalSddl)
} catch {
  Write-Error ("更新任务 {0} 失败：{1}" -f $TaskName, $_.Exception.Message)
  exit 6
}

$readbackOk = $false
try {
  $rbXml = New-Object System.Xml.XmlDocument
  $rbXml.LoadXml($updatedTask.Xml)
  $rbSettings = $rbXml.GetElementsByTagName("Settings", $TaskNs) | Select-Object -First 1
  $rbEtl = $null; $rbDisallow = $null; $rbStop = $null
  if ($rbSettings) {
    foreach ($child in $rbSettings.ChildNodes) {
      switch ($child.LocalName) {
        "ExecutionTimeLimit" { $rbEtl = $child.InnerText.Trim() }
        "DisallowStartIfOnBatteries" { $rbDisallow = $child.InnerText.Trim() }
        "StopIfGoingOnBatteries" { $rbStop = $child.InnerText.Trim() }
      }
    }
  }
  $rbSddl = $updatedTask.GetSecurityDescriptor($SecInfoOwnerGroupDacl)
  $sddlOk = $false
  if ($rbSddl) { $sddlOk = ((Get-SddlFingerprint $originalSddl) -eq (Get-SddlFingerprint $rbSddl)) }
  $readbackOk = (($rbEtl -eq "PT0S") -and ($rbDisallow -eq "false") -and ($rbStop -eq "false") -and $sddlOk)
} catch {
  $readbackOk = $false
}
if (-not $readbackOk) {
  Write-Error ("任务 {0} 更新后回读校验未通过（三项策略或 owner/group/DACL 与预期不符）——不宣称安装成功" -f $TaskName)
  exit 6
}

# ---------------------------------------------------------------------------
# 7. 收尾：RepairOnly 到此为止（不 /Run、不结束任何进程）；否则 /Run 尽力拉起
#    （失败不视为安装失败——任务定义已更新并回读，GUI 侧另行等待管道就绪）
# ---------------------------------------------------------------------------
if (-not $RepairOnly) {
  schtasks /Run /TN "$TaskName" | Out-Null
  if ($LASTEXITCODE -ne 0) {
    Write-Warning ("计划任务 {0} 已更新但立即启动失败（退出码 {1}）——GUI 侧会另行等待/引导" -f $TaskName, $LASTEXITCODE)
  }
}

if ($RepairOnly) {
  Write-Host ("已修复计划任务 {0} 的三项策略（无执行时限、允许电池启动、切换电池不停止）——对后续启动生效，当前实例未重启" -f $TaskName)
} else {
  Write-Host ("计划任务 {0} 就绪（用户登录时自启，最高权限，无执行时限，允许电池启动）" -f $TaskName)
  if ($exe) { Write-Host ("  采集器：{0}" -f $exe) }
}
exit 0
