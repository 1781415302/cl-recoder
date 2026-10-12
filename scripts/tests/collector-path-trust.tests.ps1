# collector-path-trust.tests.ps1 —— 路径可信性与 Exec 核对纯函数【无 Pester 依赖】测试（hardening-v4 §4.2）
#
# 运行方式（退出码 0 为通过；纯函数，无系统副作用，Linux pwsh 亦可运行）：
#   pwsh       -NoLogo -NoProfile -File scripts/tests/collector-path-trust.tests.ps1
#   powershell -NoLogo -NoProfile -File scripts/tests/collector-path-trust.tests.ps1

$ErrorActionPreference = "Stop"
$script:Failures = New-Object System.Collections.Generic.List[string]
$script:PassCount = 0
function Assert-True {
  param([bool]$Condition, [string]$Name, [string]$Detail = "")
  if ($Condition) { $script:PassCount++; Write-Host ("  PASS  " + $Name) }
  else {
    $msg = $Name; if ($Detail) { $msg = $msg + "  --  " + $Detail }
    $script:Failures.Add($msg) | Out-Null; Write-Host ("  FAIL  " + $msg)
  }
}

. (Join-Path $PSScriptRoot (Join-Path ".." "collector-path-trust.ps1"))

$user = "S-1-5-21-100-200-300-1001"

Write-Host "== A. Get-UntrustedWriteAce（SDDL 夹具）=="
# Program Files 下安装目录典型 ACL（TrustedInstaller/SYSTEM/Administrators 完全控制，Users 读取执行）
$pf = "O:S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464G:S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464D:PAI(A;;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;OICIIO;GA;;;CO)(A;;0x1301bf;;;SY)(A;OICIIO;GA;;;SY)(A;;0x1301bf;;;BA)(A;OICIIO;GA;;;BA)(A;;0x1200a9;;;BU)(A;OICIIO;GXGR;;;BU)(A;;0x1200a9;;;AC)"
foreach ($k in "File", "ExeDir", "Ancestor") {
  Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $pf -ExpectedUserSid $user -Kind $k)) ("A1 Program Files 默认 ACL 可信（" + $k + "）")
}
# %LOCALAPPDATA% 典型 ACL：用户本人完全控制、Owner 为用户
$lad = "O:" + $user + "G:" + $user + "D:P(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;" + $user + ")"
$hit = Get-UntrustedWriteAce -Sddl $lad -ExpectedUserSid $user -Kind "File"
Assert-True ($null -ne $hit) "A2 LOCALAPPDATA 典型 ACL 不可信"
Assert-True ($null -ne $hit -and $hit.Sid -eq $user) "A2 命中主体为当前用户"
$ladOwnerAdmin = "O:BAG:SYD:P(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;" + $user + ")"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl $ladOwnerAdmin -ExpectedUserSid $user -Kind "ExeDir")) "A2b Owner 非用户但用户有 FA → 不可信"
# 卷根 C:\：AU 仅 InheritOnly 的修改 + 本目录“创建文件夹”(AD/0x4)
$root = "O:S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464G:S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464D:PAI(A;OICI;FA;;;BA)(A;OICI;FA;;;SY)(A;OICI;0x1200a9;;;BU)(A;OICIIO;SDGXGWGR;;;AU)(A;;LC;;;AU)"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $root -ExpectedUserSid $user -Kind "Ancestor")) "A3 卷根仅 InheritOnly 修改 + 创建文件夹 → 作为祖先可信"
$onlyIo = "O:BAG:SYD:(A;OICIIO;SDGXGWGR;;;AU)(A;;FA;;;BA)"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $onlyIo -ExpectedUserSid $user -Kind "ExeDir")) "A3b 仅 InheritOnly 的 AU ACE 被忽略"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;LC;;;AU)" -Kind "ExeDir")) "A3c exe 所在目录 AU 可创建子项 → 不可信（可植入 DLL）"
# Everyone Modify
$wd = "O:BAG:SYD:(A;;FA;;;BA)(A;;0x1301bf;;;WD)"
$hitWd = Get-UntrustedWriteAce -Sddl $wd -ExpectedUserSid $user -Kind "File"
Assert-True ($null -ne $hitWd -and $hitWd.Sid -eq "S-1-1-0") "A4 Everyone Modify → 不可信"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x1301bf;;;S-1-1-0)" -Kind "File")) "A4b 完整 SID 写法同样命中"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BUG:SYD:(A;;FA;;;BA)" -Kind "File")) "A5 Owner 为 Users → 不可信"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(D;;FA;;;BU)(A;;FA;;;BA)" -Kind "File")) "A6 Deny ACE 不计入"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x40;;;IU)" -Kind "ExeDir")) "A7 目录 DeleteChild → 不可信"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x40;;;IU)" -Kind "File")) "A7b 文件对象不计 DeleteChild 位"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x10000;;;AU)" -Kind "Ancestor")) "A8 祖先 Delete → 不可信"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x6;;;AU)" -Kind "Ancestor")) "A8b 祖先仅 AddFile/AddSubdirectory → 不计入"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;GW;;;BU)" -Kind "File")) "A9 GENERIC_WRITE → 不可信"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl ("O:BAG:SYD:(A;;FA;;;" + $user + ")") -Kind "File")) "A10 未提供 ExpectedUserSid 时不检查该用户"

Write-Host ""
Write-Host "== B. Test-TaskExecMatches =="
$exp = "C:\Program Files\CL Recoder\cl-recoder-collector.exe"
Assert-True (Test-TaskExecMatches -Command ('"' + $exp + '"') -Arguments "" -ExpectedPath $exp) "B1 带引号一致"
Assert-True (Test-TaskExecMatches -Command $exp -Arguments $null -ExpectedPath $exp) "B2 不带引号一致"
Assert-True (Test-TaskExecMatches -Command "c:\program files\cl recoder\CL-RECODER-COLLECTOR.EXE" -ExpectedPath $exp) "B3 大小写不敏感"
$env:CLREC_TEST_PF = "C:\Program Files"
Assert-True (Test-TaskExecMatches -Command '"%CLREC_TEST_PF%\CL Recoder\cl-recoder-collector.exe"' -ExpectedPath $exp) "B4 环境变量展开"
Remove-Item Env:\CLREC_TEST_PF
Assert-True (-not (Test-TaskExecMatches -Command $exp -Arguments "--evil" -ExpectedPath $exp)) "B5 有参数 → 不一致"
Assert-True (Test-TaskExecMatches -Command $exp -Arguments "   " -ExpectedPath $exp) "B5b 空白参数视为无参数"
Assert-True (-not (Test-TaskExecMatches -Command "C:\Windows\notepad.exe" -ExpectedPath $exp)) "B6 不同程序 → 不一致"
Assert-True (-not (Test-TaskExecMatches -Command "" -ExpectedPath $exp)) "B7 空 Command → 不一致"
Assert-True (-not (Test-TaskExecMatches -Command "C:\Users\u\AppData\Local\CL Recoder\cl-recoder-collector.exe" -ExpectedPath $exp)) "B8 旧按用户安装路径 → 不一致"

Write-Host ""
if ($script:Failures.Count -gt 0) {
  Write-Host ("FAILED: " + $script:Failures.Count + " 项断言失败（通过 " + $script:PassCount + "）")
  $script:Failures | ForEach-Object { Write-Host ("  - " + $_) }
  exit 1
}
Write-Host ("ALL PASS（" + $script:PassCount + " 项断言）")
exit 0
