# collector-task-policy.tests.ps1 —— 三项任务策略【无 Pester 依赖】测试（usability-runtime-v3 §8.2-S2）
#
# 运行方式（§8.3 双宿主都要跑，退出码 0 为通过）：
#   pwsh      -NoLogo -NoProfile -File scripts/tests/collector-task-policy.tests.ps1
#   powershell -NoLogo -NoProfile -File scripts/tests/collector-task-policy.tests.ps1
#
# 结构：
#   A. 纯矩阵 —— 直接 dot-source ..\collector-task-policy.ps1（纯函数，无副作用）：
#      修改/补齐三项、原文不变、其它节点保留、幂等、深克隆；
#   B. 假任务 runner —— 子进程运行【真实】install-collector-task.ps1，环境注入：
#        * PATH 前置假 schtasks.cmd（记录调用行、可脚本化退出码）；
#        * 函数级 New-Object 代理：Schedule.Service → 假 COM（journal 到文件）；
#          WindowsPrincipal → IsInRole 恒可配（非提权测试进程，不弹 UAC）；
#      断言：三项回读 / flags 0x34 / SDDL 保全 / 额外触发器只读冲突 /
#      RepairOnly 无 Run/Create/Stop / 身份与权限校验 / 创建失败不宣称成功。
#
# 铁律：不创建/修改真实计划任务（假 COM + 假 schtasks）、不弹 UAC、不 taskkill、
#       全部 fixture 用唯一临时目录（pid + 时间戳）。

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# 断言基础设施
# ---------------------------------------------------------------------------
$script:Failures = New-Object System.Collections.Generic.List[string]
$script:PassCount = 0

function Assert-True {
  param([bool]$Condition, [string]$Name, [string]$Detail = "")
  if ($Condition) {
    $script:PassCount++
    Write-Host ("  PASS  " + $Name)
  } else {
    $msg = $Name
    if ($Detail) { $msg = $msg + "  --  " + $Detail }
    $script:Failures.Add($msg) | Out-Null
    Write-Host ("  FAIL  " + $msg)
  }
}

# ---------------------------------------------------------------------------
# 路径与唯一临时目录
# ---------------------------------------------------------------------------
$script:RepoScriptsDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$script:InstallScriptPath = Join-Path $script:RepoScriptsDir "install-collector-task.ps1"
$script:PolicyHelperPath = Join-Path $script:RepoScriptsDir "collector-task-policy.ps1"
$script:ChildHost = (Get-Process -Id $PID).Path
$script:WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) (
  "clrec-task-policy-tests-" + $PID + "-" + [DateTime]::UtcNow.Ticks)
New-Item -ItemType Directory -Path $script:WorkDir -Force | Out-Null
Write-Host ("workdir: " + $script:WorkDir)

# ---------------------------------------------------------------------------
# A. 纯矩阵：Get-CollectorTaskPolicyXml（真机实证形状作为 fixture 之一）
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "== A. 纯策略变换矩阵 =="

. $script:PolicyHelperPath

$script:TaskNs = "http://schemas.microsoft.com/windows/2004/02/mit/task"

function New-TaskXml {
  param(
    [string]$Sid = "S-1-5-21-100-200-300-1001",
    [string]$LogonType = "InteractiveToken",
    [string[]]$Triggers = @("LogonTrigger"),
    [string]$Etl = "PT72H",          # "" = 节点缺失（真机 /Create 产物形状）
    [string]$Disallow = "true",
    [string]$Stop = "true",
    [switch]$WithoutSettings
  )
  $triggerXml = ($Triggers | ForEach-Object { "    <$_><Enabled>true</Enabled></$_>" }) -join "`r`n"
  $settingsXml = ""
  if (-not $WithoutSettings) {
    $etlXml = ""
    if ($Etl -ne "") { $etlXml = "    <ExecutionTimeLimit>$Etl</ExecutionTimeLimit>`r`n" }
    $settingsXml = @"
  <Settings>
    <DisallowStartIfOnBatteries>$Disallow</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>$Stop</StopIfGoingOnBatteries>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
$($etlXml)    <IdleSettings><Duration>PT10M</Duration><WaitTimeout>PT1H</WaitTimeout><StopOnIdleEnd>true</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
  </Settings>
"@
  }
  return @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="$script:TaskNs">
  <RegistrationInfo>
    <Author>LQL\17814</Author>
    <URI>\ClRecoderTestTask</URI>
  </RegistrationInfo>
  <Principals>
    <Principal id="Author">
      <UserId>$Sid</UserId>
      <LogonType>$LogonType</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
$($settingsXml)  <Triggers>
$($triggerXml)
  </Triggers>
  <Actions Context="Author">
    <Exec><Command>"C:\temp\x\cl-recoder-collector.exe"</Command></Exec>
  </Actions>
</Task>
"@
}

function Get-PolicyLeaf {
  param([xml]$Doc, [string]$Leaf)
  $settings = $Doc.DocumentElement.GetElementsByTagName("Settings", $script:TaskNs) | Select-Object -First 1
  if (-not $settings) { return $null }
  $node = $settings.GetElementsByTagName($Leaf, $script:TaskNs) | Select-Object -First 1
  if ($node) { return $node.InnerText } else { return $null }
}

# A1 修改既有三值 + 原 XML 不变 + 其它节点保留
$doc = New-Object System.Xml.XmlDocument
$doc.LoadXml((New-TaskXml))
$before = $doc.OuterXml
$out = Get-CollectorTaskPolicyXml $doc
Assert-True ((Get-PolicyLeaf $out "ExecutionTimeLimit") -eq "PT0S") "A1 ExecutionTimeLimit -> PT0S"
Assert-True ((Get-PolicyLeaf $out "DisallowStartIfOnBatteries") -eq "false") "A1 DisallowStartIfOnBatteries -> false"
Assert-True ((Get-PolicyLeaf $out "StopIfGoingOnBatteries") -eq "false") "A1 StopIfGoingOnBatteries -> false"
Assert-True ($doc.OuterXml -eq $before) "A1 原 [xml] 输入保持不变"
Assert-True ($out.OuterXml.Contains("<Author>LQL\17814</Author>")) "A1 RegistrationInfo 保留"
Assert-True ($out.OuterXml.Contains("<UserId>S-1-5-21-100-200-300-1001</UserId>")) "A1 Principal UserId 保留"
Assert-True ($out.OuterXml.Contains("<LogonType>InteractiveToken</LogonType>")) "A1 LogonType 保留"
Assert-True ($out.OuterXml.Contains("<LogonTrigger>")) "A1 LogonTrigger 保留"
Assert-True ($out.OuterXml.Contains('<Command>"C:\temp\x\cl-recoder-collector.exe"</Command>')) "A1 Actions 保留"
Assert-True ($out.OuterXml.Contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>")) "A1 其它 Settings 节点保留"
Assert-True ($out.OuterXml.Contains("<Duration>PT10M</Duration>")) "A1 IdleSettings 保留"

# A2 缺节点补齐（无 ExecutionTimeLimit；连 Settings 都没有）
$doc2 = New-Object System.Xml.XmlDocument
$doc2.LoadXml((New-TaskXml -Etl ""))
$out2 = Get-CollectorTaskPolicyXml $doc2
Assert-True ((Get-PolicyLeaf $out2 "ExecutionTimeLimit") -eq "PT0S") "A2 缺 ExecutionTimeLimit 节点 → 补齐 PT0S"
Assert-True ((Get-PolicyLeaf $out2 "DisallowStartIfOnBatteries") -eq "false") "A2 既有 Disallow 节点就地改 false"
Assert-True ($doc2.OuterXml.Contains('<ExecutionTimeLimit>PT72H</ExecutionTimeLimit>') -eq $false) "A2 输入确无该节点"

$doc3 = New-Object System.Xml.XmlDocument
$doc3.LoadXml((New-TaskXml -WithoutSettings))
$out3 = Get-CollectorTaskPolicyXml $doc3
Assert-True ((Get-PolicyLeaf $out3 "ExecutionTimeLimit") -eq "PT0S") "A3 无 Settings 节点 → 补齐并写入 PT0S"
Assert-True ((Get-PolicyLeaf $out3 "StopIfGoingOnBatteries") -eq "false") "A3 无 Settings → 三项均补齐"
Assert-True ($out3.OuterXml.Contains("<LogonTrigger>")) "A3 无 Settings 补齐不破坏触发器"

# A4 幂等：重复应用等价
$twice = Get-CollectorTaskPolicyXml $out2
Assert-True ($twice.OuterXml -eq $out2.OuterXml) "A4 重复应用等价（幂等）"
$twice3 = Get-CollectorTaskPolicyXml $out3
Assert-True ($twice3.OuterXml -eq $out3.OuterXml) "A4 补齐后重复应用等价"

# A5 深克隆：返回对象与输入非同一实例
Assert-True (-not [object]::ReferenceEquals($out, $doc)) "A5 返回深克隆（非输入实例）"

# ---------------------------------------------------------------------------
# B. 假任务 runner：真实 install 脚本 + 假 schtasks + 假 COM
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "== B. 假任务 runner（真实 install-collector-task.ps1）=="

$realSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$testTaskName = "ClRecoderTestTask"
$fakeOtherSid = "S-1-5-21-1111111111-2222222222-3333333333-9999"

function New-TaskSddl { param([string]$Sid, [switch]$ExtraAce)
  $sddl = "O:BAG:" + $Sid + "D:(A;ID;0x1f019f;;;BA)(A;ID;0x1f019f;;;SY)(A;;FR;;;" + $Sid + ")"
  if ($ExtraAce) { $sddl = $sddl + "(A;;FR;;;WD)" }
  return $sddl
}
function New-ReorderedSddl { param([string]$Sid)
  # 同一 DACL 的 ACE 重排（等价性不得依赖字符串顺序）
  return "O:BAG:" + $Sid + "D:(A;;FR;;;" + $Sid + ")(A;ID;0x1f019f;;;SY)(A;ID;0x1f019f;;;BA)"
}

# 假 schtasks.cmd：记录原始参数行；按首参数与预设环境变量决定退出码
$fakeBin = Join-Path $script:WorkDir "fakebin"
New-Item -ItemType Directory -Path $fakeBin -Force | Out-Null
$fakeCmd = @'
@echo off
echo %* >> "%CLREC_FAKE_LOG%"
if /i "%~1" == "/Create" if defined CLREC_FAKE_CREATE_EXIT exit /b %CLREC_FAKE_CREATE_EXIT%
if /i "%~1" == "/Run" if defined CLREC_FAKE_RUN_EXIT exit /b %CLREC_FAKE_RUN_EXIT%
exit /b 0
'@
[System.IO.File]::WriteAllText((Join-Path $fakeBin "schtasks.cmd"), $fakeCmd)
$script:FakeSchtasksCmd = $fakeCmd

# driver：定义假 COM/假 principal/journal，然后以子作用域运行真实 install 脚本。
# 约束：全部 ASCII（无编码依赖）；状态 journal 到文件（即便 exit 终止进程也可断言）。
$driverContent = @'
param(
  [Parameter(Mandatory = $true)][string]$ConfigPath,
  [Parameter(Mandatory = $true)][string]$ScriptPath,
  [Parameter(Mandatory = $true)][string]$WorkDir
)
$ErrorActionPreference = "Continue"

$cfg = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json

$callLog = Join-Path $WorkDir "calls.log"
$regXmlPath = Join-Path $WorkDir "registered-task.xml"
$sddlPassedPath = Join-Path $WorkDir "sddl-passed.txt"

$global:FAKE = @{
  CallLog        = $callLog
  RegXmlPath     = $regXmlPath
  SddlPassedPath = $sddlPassedPath
  FakeAdmin      = [bool]$cfg.FakeAdmin
  TaskExists     = [bool]$cfg.TaskExists
  TaskXml        = [string]$cfg.TaskXml
  CreatedTaskXml = [string]$cfg.CreatedTaskXml
  Sddl           = [string]$cfg.Sddl
  SddlReadback   = $cfg.SddlReadback
  GetSddlFails   = [bool]$cfg.GetSddlFails
  TaskLookupFails = [bool]$cfg.TaskLookupFails
  PostTaskXml    = $cfg.PostTaskXml
  RegisterCount  = 0
}

function Write-FakeCall([string]$Line) {
  Add-Content -LiteralPath $global:FAKE.CallLog -Value $Line
}

class FakeWindowsPrincipal {
  hidden [object] $identity
  FakeWindowsPrincipal([object]$id) { $this.identity = $id }
  [bool] IsInRole([object]$role) { return [bool]$global:FAKE.FakeAdmin }
}

class FakeRegisteredTask {
  [string] $Xml
  FakeRegisteredTask([string]$xml) { $this.Xml = $xml }
  [string] GetSecurityDescriptor([int]$flags) {
    Write-FakeCall ("GetSecurityDescriptor flags=" + $flags)
    if ($global:FAKE.GetSddlFails) { throw "simulated GetSecurityDescriptor failure" }
    if ($global:FAKE.RegisterCount -gt 0 -and $global:FAKE.SddlReadback) {
      return [string]$global:FAKE.SddlReadback
    }
    return [string]$global:FAKE.Sddl
  }
}

class FakeTaskFolder {
  [object] GetTask([string]$path) {
    Write-FakeCall ("GetTask " + $path)
    if ($global:FAKE.TaskLookupFails) {
      throw [System.Runtime.InteropServices.COMException]::new("access denied", -2147024891)
    }
    $exists = [bool]$global:FAKE.TaskExists
    if (-not $exists) {
      # 假 schtasks 落笔 /Create 后任务"出现"（模拟创建成功后 COM 可读）
      if ((Test-Path -LiteralPath $global:FAKE.CallLog) -and (Select-String -LiteralPath $global:FAKE.CallLog -Pattern "/Create" -Quiet)) {
        $exists = $true
        $global:FAKE.TaskExists = $true
        if ($global:FAKE.CreatedTaskXml) { $global:FAKE.TaskXml = [string]$global:FAKE.CreatedTaskXml }
      }
    }
    if (-not $exists) { throw [System.Runtime.InteropServices.COMException]::new("task not found (simulated)", -2147024894) }
    return [FakeRegisteredTask]::new([string]$global:FAKE.TaskXml)
  }
  [object] RegisterTask([string]$path, [string]$xml, [int]$flags, [object]$userId, [object]$password, [int]$logonType, [string]$sddl) {
    Write-FakeCall ("RegisterTask path=" + $path + " flags=" + $flags + " logonType=" + $logonType + " sddl=" + $sddl)
    $global:FAKE.RegisterCount = $global:FAKE.RegisterCount + 1
    Set-Content -LiteralPath $global:FAKE.RegXmlPath -Value $xml
    Set-Content -LiteralPath $global:FAKE.SddlPassedPath -Value $sddl
    if ($global:FAKE.PostTaskXml) { $global:FAKE.TaskXml = [string]$global:FAKE.PostTaskXml } else { $global:FAKE.TaskXml = $xml }
    return [FakeRegisteredTask]::new([string]$global:FAKE.TaskXml)
  }
}

class FakeTaskService {
  Connect() { Write-FakeCall "Connect" }
  [object] GetFolder([string]$p) { Write-FakeCall ("GetFolder " + $p); return [FakeTaskFolder]::new() }
}

# 函数优先级高于外部应用/COM 激活：拦截 WindowsPrincipal 与 Schedule.Service，其余透传
function New-Object {
  [CmdletBinding()]
  param(
    [Parameter(Position = 0)][string]$TypeName,
    [object[]]$ArgumentList,
    [string]$ComObject,
    [Parameter(ValueFromRemainingArguments = $true)]$Rest
  )
  if ($ComObject -eq "Schedule.Service") { return [FakeTaskService]::new() }
  # 注意 @($null).Count == 1——必须先判 $null（无参 XmlDocument 调用不得误传构造参数）
  if ($TypeName -like "*WindowsPrincipal") {
    # New-Object Foo($x) 形式：$x 落入 ValueFromRemainingArguments（$Rest）
    $id = $null
    if ($null -ne $ArgumentList -and $ArgumentList.Count -gt 0) { $id = $ArgumentList[0] }
    elseif ($null -ne $Rest -and @($Rest).Count -gt 0) { $id = @($Rest)[0] }
    return [FakeWindowsPrincipal]::new($id)
  }
  # 其余透传：TypeName + ArgumentList（$Rest 吸收的额外定位参数并入 ArgumentList）
  $passArgs = @()
  if ($null -ne $ArgumentList -and $ArgumentList.Count -gt 0) { $passArgs += @($ArgumentList) }
  if ($null -ne $Rest -and @($Rest).Count -gt 0) { $passArgs += @($Rest) }
  if ($passArgs.Count -gt 0) {
    return Microsoft.PowerShell.Utility\New-Object -TypeName $TypeName -ArgumentList $passArgs
  }
  return Microsoft.PowerShell.Utility\New-Object -TypeName $TypeName
}

# PATH 前置假 schtasks；退出码场景化
$fakeBin = Join-Path $WorkDir "fakebin"
$env:PATH = $fakeBin + ";" + $env:PATH
$env:CLREC_FAKE_LOG = $callLog
if ($cfg.CreateExit) { $env:CLREC_FAKE_CREATE_EXIT = [string][int]$cfg.CreateExit }
if ($cfg.RunExit) { $env:CLREC_FAKE_RUN_EXIT = [string][int]$cfg.RunExit }

# 哈希 splat（数组 splat 不把 "-TaskName" 之类元素当参数名，会整体按位置绑定导致错位）
$invokeSplat = @{ TaskName = $cfg.TaskName }
if ($cfg.CollectorPath) { $invokeSplat.CollectorPath = $cfg.CollectorPath }
if ($cfg.RepairOnly) { $invokeSplat.RepairOnly = $true }
if ($cfg.ExpectedUserSid) { $invokeSplat.ExpectedUserSid = $cfg.ExpectedUserSid }

$out = & $ScriptPath @invokeSplat 2>&1
$code = $LASTEXITCODE
if ($null -eq $code) { $code = 1 }
$out | ForEach-Object {
  if ($null -ne $_.InvocationInfo) {
    ("{0}  [{1}:{2}]" -f $_, $_.InvocationInfo.ScriptName, $_.InvocationInfo.ScriptLineNumber)
  } else { "$_" }
} | Set-Content -LiteralPath (Join-Path $WorkDir "script-output.txt")
@{ registerCount = $global:FAKE.RegisterCount } | ConvertTo-Json |
  Set-Content -LiteralPath (Join-Path $WorkDir "fake-state.json")
exit $code
'@
# 必须带 BOM：Windows PowerShell 5.1 对无 BOM 文件按 ANSI 解析，中文注释会破坏语法
$script:DriverPath = Join-Path $script:WorkDir "driver.ps1"
[System.IO.File]::WriteAllText($script:DriverPath, $driverContent, (New-Object System.Text.UTF8Encoding($true)))

function Invoke-Scenario {
  param([string]$Name, [hashtable]$Config, [string]$ScriptPath = $script:InstallScriptPath)
  $dir = Join-Path $script:WorkDir $Name
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
  New-Item -ItemType Directory -Path (Join-Path $dir "fakebin") -Force | Out-Null
  [System.IO.File]::WriteAllText((Join-Path $dir "fakebin\schtasks.cmd"), $script:FakeSchtasksCmd)
  $configPath = Join-Path $dir "config.json"
  $Config | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $configPath -Encoding UTF8
  & $script:ChildHost -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass `
    -File $script:DriverPath -ConfigPath $configPath -ScriptPath $ScriptPath -WorkDir $dir | Out-Null
  $code = $LASTEXITCODE
  $calls = @()
  $callsPath = Join-Path $dir "calls.log"
  if (Test-Path -LiteralPath $callsPath) { $calls = @(Get-Content -LiteralPath $callsPath) }
  $regXmlPath = Join-Path $dir "registered-task.xml"
  $regXml = $null
  if (Test-Path -LiteralPath $regXmlPath) { $regXml = Get-Content -LiteralPath $regXmlPath -Raw }
  $sddlPassed = $null
  $sddlPath = Join-Path $dir "sddl-passed.txt"
  if (Test-Path -LiteralPath $sddlPath) { $sddlPassed = (Get-Content -LiteralPath $sddlPath -Raw).TrimEnd() }
  return @{ Code = $code; Calls = $calls; RegisteredXml = $regXml; SddlPassed = $sddlPassed; Dir = $dir }
}

function Test-HasCall { param($Calls, [string]$Needle)
  return (@($Calls | Where-Object { $_.Contains($Needle) }).Count -gt 0)
}

function Assert-HasPolicyValues { param($Result, [string]$Name)
  Assert-True ($null -ne $Result.RegisteredXml) ($Name + "：RegisterTask 已发生") ($Name + "：registered-task.xml 缺失")
  if ($null -eq $Result.RegisteredXml) { return }
  Assert-True $Result.RegisteredXml.Contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>") ($Name + "：回读 ExecutionTimeLimit=PT0S")
  Assert-True $Result.RegisteredXml.Contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>") ($Name + "：回读 DisallowStartIfOnBatteries=false")
  Assert-True $Result.RegisteredXml.Contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>") ($Name + "：回读 StopIfGoingOnBatteries=false")
}

# B0 源码级：不结束任何进程（§2.1/§4.3）
$installSrc = Get-Content -LiteralPath $script:InstallScriptPath -Raw
Assert-True (-not $installSrc.Contains("taskkill")) "B0 install 脚本不含 taskkill"
Assert-True (-not $installSrc.Contains("Stop-ScheduledTask")) "B0 install 脚本不含 Stop-ScheduledTask"

# B1 创建路径：/Create（内嵌引号配方）→ 三项设置回读 → /Run
$dummyExe = Join-Path $script:WorkDir "dummy-collector.exe"
[System.IO.File]::WriteAllBytes($dummyExe, [byte[]]@(0x4d, 0x5a))
$createdXml = New-TaskXml -Sid $realSid -Etl ""
$r1 = Invoke-Scenario "S1-create" @{
  FakeAdmin      = $true
  TaskExists     = $false
  TaskXml        = ""
  CreatedTaskXml = $createdXml
  Sddl           = (New-TaskSddl $realSid)
  TaskName       = $testTaskName
  CollectorPath  = $dummyExe
}
Assert-True ($r1.Code -eq 0) "B1 创建路径退出码 0" ("实际 " + $r1.Code)
$createLine = @($r1.Calls | Where-Object { $_.Contains("/Create") })
Assert-True ($createLine.Count -eq 1) "B1 恰一次 /Create" (@($r1.Calls) -join " | ")
if ($createLine.Count -eq 1) {
  Assert-True $createLine[0].Contains('/TN "' + $testTaskName + '"') "B1 /TN 传名"
  Assert-True $createLine[0].Contains('/TR "\"') "B1 /TR 内嵌引号配方保持"
  Assert-True $createLine[0].Contains("/RL HIGHEST") "B1 /RL HIGHEST"
  Assert-True $createLine[0].Contains("/SC ONLOGON") "B1 /SC ONLOGON"
  Assert-True $createLine[0].Contains("/F") "B1 /F"
}
Assert-HasPolicyValues $r1 "B1"
$registerLine = @($r1.Calls | Where-Object { $_.Contains("RegisterTask") })
Assert-True ($registerLine.Count -eq 1) "B1 恰一次 RegisterTask"
if ($registerLine.Count -eq 1) {
  Assert-True $registerLine[0].Contains("flags=52") "B1 flags=0x34(52)"
  Assert-True $registerLine[0].Contains("logonType=3") "B1 logonType=InteractiveToken(3)"
  Assert-True $registerLine[0].Contains("sddl=O:BAG:") "B1 传非空原 SDDL"
}
Assert-True ($r1.SddlPassed -eq (New-TaskSddl $realSid)) "B1 传给 RegisterTask 的 SDDL 即原 SDDL" ("实际 " + $r1.SddlPassed)
Assert-True (Test-HasCall $r1.Calls "/Run") "B1 创建后 /Run"

# B2 已存在（非 RepairOnly）：COM 更新不 /Create 重建；回读通过；/Run
$existsXml = New-TaskXml -Sid $realSid
$r2 = Invoke-Scenario "S2-update" @{
  FakeAdmin  = $true
  TaskExists = $true
  TaskXml    = $existsXml
  Sddl       = (New-TaskSddl $realSid)
  TaskName   = $testTaskName
}
Assert-True ($r2.Code -eq 0) "B2 已存在更新退出码 0" ("实际 " + $r2.Code)
Assert-True (-not (Test-HasCall $r2.Calls "/Create")) "B2 已存在绝不 /Create /F 重建"
Assert-HasPolicyValues $r2 "B2"
Assert-True (Test-HasCall $r2.Calls "/Run") "B2 更新后 /Run"
Assert-True ($r2.SddlPassed -eq (New-TaskSddl $realSid)) "B2 原 SDDL 原样传回"

# B2b DACL ACE 重排等价：回读顺序不同 → 仍判定等价 → 退出码 0
$r2b = Invoke-Scenario "S2b-reorder" @{
  FakeAdmin   = $true
  TaskExists  = $true
  TaskXml     = $existsXml
  Sddl        = (New-TaskSddl $realSid)
  SddlReadback = (New-ReorderedSddl $realSid)
  TaskName    = $testTaskName
}
Assert-True ($r2b.Code -eq 0) "B2b ACE 重排后 owner/group/DACL 判等 → 0" ("实际 " + $r2b.Code)

# B3 RepairOnly：只更新策略；无 /Create、无 /Run；RegisterTask 0x34
$r3 = Invoke-Scenario "S3-repair" @{
  FakeAdmin  = $true
  TaskExists = $true
  TaskXml    = $existsXml
  Sddl       = (New-TaskSddl $realSid)
  TaskName   = $testTaskName
  RepairOnly = $true
}
Assert-True ($r3.Code -eq 0) "B3 RepairOnly 退出码 0" ("实际 " + $r3.Code)
Assert-HasPolicyValues $r3 "B3"
$registerLine3 = @($r3.Calls | Where-Object { $_.Contains("RegisterTask") })
Assert-True ($registerLine3.Count -eq 1 -and $registerLine3[0].Contains("flags=52")) "B3 RepairOnly 以 0x34 更新策略"
Assert-True (-not (Test-HasCall $r3.Calls "/Create")) "B3 RepairOnly 无 /Create"
Assert-True (-not (Test-HasCall $r3.Calls "/Run")) "B3 RepairOnly 无 /Run"

# B3b RepairOnly 不要求重新定位采集器：脚本+helper 拷贝到无采集器的临时目录
$copyDir = Join-Path $script:WorkDir "S3b-copy"
New-Item -ItemType Directory -Path $copyDir -Force | Out-Null
Copy-Item -LiteralPath $script:InstallScriptPath -Destination (Join-Path $copyDir "install-collector-task.ps1")
Copy-Item -LiteralPath $script:PolicyHelperPath -Destination (Join-Path $copyDir "collector-task-policy.ps1")
$r3b = Invoke-Scenario "S3b-repair-noexe" @{
  FakeAdmin  = $true
  TaskExists = $true
  TaskXml    = $existsXml
  Sddl       = (New-TaskSddl $realSid)
  TaskName   = $testTaskName
  RepairOnly = $true
} -ScriptPath (Join-Path $copyDir "install-collector-task.ps1")
Assert-True ($r3b.Code -eq 0) "B3b RepairOnly 不定位采集器文件 → 0" ("实际 " + $r3b.Code)
Assert-True (-not (Test-HasCall $r3b.Calls "/Run")) "B3b 无 /Run"

# B4 RepairOnly 但任务不存在：失败且绝不创建
$r4 = Invoke-Scenario "S4-repair-missing" @{
  FakeAdmin  = $true
  TaskExists = $false
  TaskXml    = ""
  Sddl       = (New-TaskSddl $realSid)
  TaskName   = $testTaskName
  RepairOnly = $true
}
Assert-True ($r4.Code -ne 0) "B4 RepairOnly 任务缺失 → 失败" ("实际 " + $r4.Code)
Assert-True (-not (Test-HasCall $r4.Calls "/Create")) "B4 修复绝不 /Create"
Assert-True ($null -eq $r4.RegisteredXml) "B4 无 RegisterTask"

# B5 额外触发器（Time+Logon）：只读报冲突，不修改
$r5 = Invoke-Scenario "S5-extra-trigger" @{
  FakeAdmin  = $true
  TaskExists = $true
  TaskXml    = (New-TaskXml -Sid $realSid -Triggers @("TimeTrigger", "LogonTrigger"))
  Sddl       = (New-TaskSddl $realSid)
  TaskName   = $testTaskName
  RepairOnly = $true
}
Assert-True ($r5.Code -eq 5) "B5 额外触发器 → 明确冲突退出码 5" ("实际 " + $r5.Code)
Assert-True ($null -eq $r5.RegisteredXml) "B5 冲突不修改（无 RegisterTask）"
Assert-True (-not (Test-HasCall $r5.Calls "/Run")) "B5 冲突不 /Run"

# B6 principal 归属其他用户：明确冲突，不覆盖
$r6 = Invoke-Scenario "S6-principal-mismatch" @{
  FakeAdmin  = $true
  TaskExists = $true
  TaskXml    = (New-TaskXml -Sid $fakeOtherSid)
  Sddl       = (New-TaskSddl $fakeOtherSid)
  TaskName   = $testTaskName
  RepairOnly = $true
}
Assert-True ($r6.Code -eq 5) "B6 principal 非当前用户 → 冲突 5" ("实际 " + $r6.Code)
Assert-True ($null -eq $r6.RegisteredXml) "B6 不覆盖其他账户定义"

# B7 登录类型非 InteractiveToken：明确冲突
$r7 = Invoke-Scenario "S7-logon-type" @{
  FakeAdmin  = $true
  TaskExists = $true
  TaskXml    = (New-TaskXml -Sid $realSid -LogonType "Password")
  Sddl       = (New-TaskSddl $realSid)
  TaskName   = $testTaskName
  RepairOnly = $true
}
Assert-True ($r7.Code -eq 5) "B7 LogonType=Password → 冲突 5" ("实际 " + $r7.Code)
Assert-True ($null -eq $r7.RegisteredXml) "B7 冲突不修改"

# B8 ExpectedUserSid 与提权身份不符：失败于任何任务操作之前
$r8 = Invoke-Scenario "S8-sid-mismatch" @{
  FakeAdmin       = $true
  TaskExists      = $true
  TaskXml         = $existsXml
  Sddl            = (New-TaskSddl $realSid)
  TaskName        = $testTaskName
  RepairOnly      = $true
  ExpectedUserSid = "S-1-5-21-9999999999-8888888888-7777777777-1234"
}
Assert-True ($r8.Code -eq 1) "B8 预期 SID 不符 → 1" ("实际 " + $r8.Code)
Assert-True ($r8.Calls.Count -eq 0) "B8 未触碰任何任务操作（无 COM/无 schtasks）"

# B9 安全描述符不可得：只读冲突（不能传空 SDDL 冒充保全）
$r9 = Invoke-Scenario "S9-sddl-fails" @{
  FakeAdmin   = $true
  TaskExists  = $true
  TaskXml     = $existsXml
  Sddl        = (New-TaskSddl $realSid)
  GetSddlFails = $true
  TaskName    = $testTaskName
  RepairOnly  = $true
}
Assert-True ($r9.Code -eq 5) "B9 SDDL 不可得 → 冲突 5" ("实际 " + $r9.Code)
Assert-True ($null -eq $r9.RegisteredXml) "B9 无空 SDDL 冒充保全"

# B10 回读三项不符（模拟系统丢弃 ExecutionTimeLimit）：不宣称安装成功
$droppedEtl = New-TaskXml -Sid $realSid -Etl ""
$r10 = Invoke-Scenario "S10-readback-fail" @{
  FakeAdmin  = $true
  TaskExists = $true
  TaskXml    = $existsXml
  Sddl       = (New-TaskSddl $realSid)
  PostTaskXml = $droppedEtl
  TaskName   = $testTaskName
}
Assert-True ($r10.Code -eq 6) "B10 回读三项不符 → 6（不宣称成功）" ("实际 " + $r10.Code)

# B11 回读 SDDL 不等价（多出 ACE）：不宣称安装成功
$r11 = Invoke-Scenario "S11-sddl-drift" @{
  FakeAdmin   = $true
  TaskExists  = $true
  TaskXml     = $existsXml
  Sddl        = (New-TaskSddl $realSid)
  SddlReadback = (New-TaskSddl $realSid -ExtraAce)
  TaskName    = $testTaskName
}
Assert-True ($r11.Code -eq 6) "B11 回读 DACL 不等价 → 6" ("实际 " + $r11.Code)

# B12 非管理员：失败于任何任务操作之前
$r12 = Invoke-Scenario "S12-not-admin" @{
  FakeAdmin  = $false
  TaskExists = $true
  TaskXml    = $existsXml
  Sddl       = (New-TaskSddl $realSid)
  TaskName   = $testTaskName
}
Assert-True ($r12.Code -eq 1) "B12 非管理员 → 1" ("实际 " + $r12.Code)
Assert-True ($r12.Calls.Count -eq 0) "B12 未触碰任何任务操作"

# B13 创建失败：按 schtasks 退出码失败，不 /Run、不宣称成功
$r13 = Invoke-Scenario "S13-create-fail" @{
  FakeAdmin      = $true
  TaskExists     = $false
  TaskXml        = ""
  CreatedTaskXml = $createdXml
  Sddl           = (New-TaskSddl $realSid)
  TaskName       = $testTaskName
  CollectorPath  = $dummyExe
  CreateExit     = 267009
}
Assert-True ($r13.Code -eq 267009) "B13 /Create 失败按其退出码失败" ("实际 " + $r13.Code)
Assert-True (Test-HasCall $r13.Calls "/Create") "B13 有 /Create 尝试"
Assert-True ($null -eq $r13.RegisteredXml) "B13 失败后不做策略更新"
Assert-True (-not (Test-HasCall $r13.Calls "/Run")) "B13 失败后不 /Run"

$r14 = Invoke-Scenario "S14-lookup-denied" @{
  FakeAdmin = $true
  TaskExists = $true
  TaskLookupFails = $true
  TaskXml = $existsXml
  Sddl = (New-TaskSddl $realSid)
  TaskName = $testTaskName
  CollectorPath = $dummyExe
}
Assert-True ($r14.Code -eq 5) "B14 查询访问拒绝不当作不存在" ("实际 " + $r14.Code)
Assert-True (-not (Test-HasCall $r14.Calls "/Create")) "B14 不覆盖未知任务"
Assert-True ($null -eq $r14.RegisteredXml) "B14 不更新策略"
Assert-True (-not (Test-HasCall $r14.Calls "/Run")) "B14 不启动任务"

# ---------------------------------------------------------------------------
# 收尾
# ---------------------------------------------------------------------------
if ($script:Failures.Count -eq 0) {
  Remove-Item -LiteralPath $script:WorkDir -Recurse -Force -ErrorAction SilentlyContinue
} else {
  Write-Host ("保留现场以便排查: " + $script:WorkDir)
}

Write-Host ""
if ($script:Failures.Count -gt 0) {
  Write-Host ("FAILED: " + $script:Failures.Count + " 项断言失败（通过 " + $script:PassCount + "）")
  $script:Failures | ForEach-Object { Write-Host ("  - " + $_) }
  exit 1
}
Write-Host ("ALL PASS（" + $script:PassCount + " 项断言）")
exit 0
