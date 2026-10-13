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
foreach ($k in "File", "ExeDir", "Ancestor", "VolumeRoot") {
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
$a10 = Get-UntrustedWriteAce -Sddl ("O:BAG:SYD:(A;;FA;;;" + $user + ")") -Kind "File"
Assert-True ($null -ne $a10 -and $a10.Sid -eq $user) "A10 未提供 ExpectedUserSid 时普通用户写 ACE 仍不可信（非可信集合）" ("" + $a10.Sid)

Write-Host ""
Write-Host "== A2. 令牌组受检集合（-ExtraSids / Select-TrustWatchedSids）=="
$grp = "S-1-5-21-100-200-300-5555"
$grpDir = "O:BAG:SYD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1301bf;;;" + $grp + ")(A;OICI;0x1200a9;;;BU)"
$h0 = Get-UntrustedWriteAce -Sddl $grpDir -ExpectedUserSid $user -Kind "ExeDir"
Assert-True ($null -ne $h0 -and $h0.Sid -eq $grp) "A2-1 自定义组可写（不在可信集合）→ 不可信" ("" + $h0.Sid)
$h = Get-UntrustedWriteAce -Sddl $grpDir -ExpectedUserSid $user -Kind "ExeDir" -ExtraSids @($user, $grp.ToLowerInvariant())
Assert-True ($null -ne $h -and $h.Sid -eq $grp) "A2-2 ExtraSids 不影响：仍不可信" ("" + $h.Sid)
$puDir = "O:BAG:SYD:PAI(A;OICI;FA;;;SY)(A;OICI;0x1301bf;;;PU)"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl $puDir -ExpectedUserSid $user -Kind "ExeDir")) "A2-3 Power Users 可写 → 不可信（固定受检）"
$adminTok = @($user, "S-1-5-32-544", "S-1-5-18", "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464", "S-1-5-114", "S-1-16-12288", "S-1-5-32-545", "S-1-5-11")
foreach ($k in "File", "ExeDir", "Ancestor", "VolumeRoot") {
  Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $pf -ExpectedUserSid $user -Kind $k -ExtraSids $adminTok)) ("A2-4 提权令牌（含 Administrators 等）下 Program Files 仍可信（" + $k + "）")
}
$sel = Select-TrustWatchedSids @(" s-1-5-32-544 ", "S-1-5-21-1-2-3-4", "s-1-5-21-1-2-3-4", "", "S-1-5-80-1-2", "S-1-5-32-545")
Assert-True ($sel.Count -eq 2 -and $sel[0] -eq "S-1-5-21-1-2-3-4" -and $sel[1] -eq "S-1-5-32-545") "A2-5 过滤特权主体、去空、大写去重" ($sel -join ",")
$owDir = "O:BAG:SYD:PAI(A;OICI;FA;;;SY)(A;;FA;;;OW)"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $owDir -ExpectedUserSid $user -Kind "ExeDir")) "A2-6 OWNER RIGHTS 且 Owner 为管理员 → 不计入"
$owUser = "O:" + $user + "G:SYD:PAI(A;OICI;FA;;;SY)(A;;FA;;;OW)"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl $owUser -ExpectedUserSid $user -Kind "ExeDir")) "A2-7 OWNER RIGHTS 且 Owner 为用户 → 不可信（Owner 规则）"
Assert-True ((Get-TokenWatchedSids) -is [array]) "A2-8 Get-TokenWatchedSids 在任意平台返回数组（非 Windows 为空）"

Write-Host "== A3. Null/空 DACL 与跨账户写 ACE =="
$nullHit = Get-UntrustedWriteAce -Sddl "O:BAG:SYD:NO_ACCESS_CONTROL" -ExpectedUserSid $user -Kind "File"
Assert-True ($null -ne $nullHit -and $nullHit.Reason -eq "Null DACL") "A3-1 Null DACL 不可信" ("" + $nullHit.Reason)
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:" -ExpectedUserSid $user -Kind "File")) "A3-2 空 DACL 可信（拒绝所有人）"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:P" -ExpectedUserSid $user -Kind "ExeDir")) "A3-3 空 DACL (D:P) 可信"
$bad = Get-UntrustedWriteAce -Sddl "not-a-valid-sddl" -ExpectedUserSid $user -Kind "File"
Assert-True ($null -ne $bad -and $bad.Reason -eq "无法解析 SDDL") "A3-4 无法解析 SDDL 不可信" ("" + $bad.Reason)
$other = "S-1-5-21-100-200-300-1002"
$cross = "O:BAG:SYD:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1301bf;;;" + $other + ")"
$cx = Get-UntrustedWriteAce -Sddl $cross -ExpectedUserSid $user -Kind "File"
Assert-True ($null -ne $cx -and $cx.Sid -eq $other) "A3-5 其他用户写 ACE 不可信" ("" + $cx.Sid)
$privOk = "O:BAG:SYD:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;TI)"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $privOk -ExpectedUserSid $user -Kind "File")) "A3-6 仅 BA/SY/TI 写 ACE 可信"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $privOk -ExpectedUserSid $user -Kind "ExeDir")) "A3-7 仅特权写 ACE（ExeDir）可信"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $privOk -ExpectedUserSid $user -Kind "Ancestor")) "A3-8 仅特权写 ACE（Ancestor）可信"

Write-Host "== A4. 条件 ACE 嵌套括号不得截断后续写 ACE =="
# 复现 1：条件 ACE 在前，其后有 Everyone 写权限 —— 必须拒绝（不得当成 Empty）
$condFirst = "O:BAG:SYD:PAI(XA;;FR;;;WD;(Member_of {SID(BU)}))(A;;FW;;;WD)(A;;FA;;;BA)"
$d1 = Get-SddlDaclInfo $condFirst
Assert-True ($d1.Kind -eq "Aces") "A4-1a 条件 ACE 在前时 Kind=Aces（非 Empty）" ("" + $d1.Kind)
$split1 = Split-SddlAceList $d1.Text
Assert-True ($null -ne $split1 -and $split1.Count -eq 3) "A4-1b 切出 3 条 ACE" ("" + $(if ($split1) { $split1.Count } else { "null" }))
$h1 = Get-UntrustedWriteAce -Sddl $condFirst -ExpectedUserSid $user -Kind "File"
Assert-True ($null -ne $h1) "A4-1c 条件 ACE 在前 + Everyone 写 → 不可信" ("" + $h1.Reason)
# 复现 2：BA Allow 在前，条件 ACE 居中，Everyone 写在后 —— 必须拒绝（不得只看见 BA）
$condMid = "O:BAG:SYD:PAI(A;;FA;;;BA)(XA;;FR;;;WD;(Member_of {SID(BU)}))(A;;FW;;;WD)"
$d2 = Get-SddlDaclInfo $condMid
Assert-True ($d2.Kind -eq "Aces") "A4-2a 条件 ACE 居中时 Kind=Aces" ("" + $d2.Kind)
$split2 = Split-SddlAceList $d2.Text
Assert-True ($null -ne $split2 -and $split2.Count -eq 3) "A4-2b 切出 3 条 ACE" ("" + $(if ($split2) { $split2.Count } else { "null" }))
$h2 = Get-UntrustedWriteAce -Sddl $condMid -ExpectedUserSid $user -Kind "File"
Assert-True ($null -ne $h2) "A4-2c BA + 条件 ACE + Everyone 写 → 不可信" ("" + $h2.Reason)
# 仅条件 ACE、无后续写：仍因 XA 保守拒绝
$xaOnly = "O:BAG:SYD:PAI(A;;FA;;;BA)(XA;;FR;;;WD;(Member_of {SID(BU)}))"
$h3 = Get-UntrustedWriteAce -Sddl $xaOnly -ExpectedUserSid $user -Kind "File"
Assert-True ($null -ne $h3 -and $h3.Reason -match "条件/未知 ACE") "A4-3 单独 XA 保守拒绝" ("" + $h3.Reason)
# 嵌套切分在仅有简单 ACE 时与旧行为一致（Program Files 夹具仍可信）
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $pf -ExpectedUserSid $user -Kind "File")) "A4-4 Program Files 夹具在嵌套切分下仍可信"

Write-Host "== A5. 卷根 VolumeRoot 与普通祖先掩码分离 =="
# 复现：卷根 AU Allow 0x1301BF（含自身 DELETE，不含 DELETE_CHILD/WRITE_DAC/WRITE_OWNER）
$volAu = "O:BAG:SYD:PAI(A;OICI;FA;;;BA)(A;OICI;FA;;;SY)(A;OICI;0x1200a9;;;BU)(A;;0x1301bf;;;AU)"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl $volAu -ExpectedUserSid $user -Kind "VolumeRoot")) "A5-1 卷根仅 AU 0x1301BF → VolumeRoot 可信"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl $volAu -ExpectedUserSid $user -Kind "Ancestor")) "A5-2 同一 SDDL 作为普通祖先仍因 DELETE 不可信"
Assert-True ($null -eq (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x10000;;;AU)(A;;FA;;;BA)" -Kind "VolumeRoot")) "A5-3 卷根仅自身 DELETE → 可信"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x10000;;;AU)" -Kind "Ancestor")) "A5-4 普通祖先 DELETE → 不可信（回归）"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x40;;;AU)(A;;FA;;;BA)" -Kind "VolumeRoot")) "A5-5 卷根 DELETE_CHILD → 不可信"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x40000;;;AU)(A;;FA;;;BA)" -Kind "VolumeRoot")) "A5-6 卷根 WRITE_DAC → 不可信"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;0x80000;;;AU)(A;;FA;;;BA)" -Kind "VolumeRoot")) "A5-7 卷根 WRITE_OWNER → 不可信"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;GA;;;AU)(A;;FA;;;BA)" -Kind "VolumeRoot")) "A5-8 卷根 GENERIC_ALL → 不可信"
Assert-True ($null -ne (Get-UntrustedWriteAce -Sddl "O:BAG:SYD:(A;;FA;;;AU)" -Kind "VolumeRoot")) "A5-9 卷根 FA（含危险位）→ 不可信"

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
