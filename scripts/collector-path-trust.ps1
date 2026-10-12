# collector-path-trust.ps1 —— 采集器路径可信性与任务 Exec 核对的纯函数（hardening-v4 §4.2.2/§4.2.3）
#
# 由 install-collector-task.ps1 dot-source；纯函数部分（Get-UntrustedWriteAce、
# ConvertTo-NormalizedExecPath、Test-TaskExecMatches）无副作用，可在任意宿主单测
# （scripts/tests/collector-path-trust.tests.ps1）。Test-CollectorPathTrusted 读取真实 ACL（Get-Acl），
# 仅在 Windows 上有意义。
#
# 判据（以“普通用户能否替换以最高权限运行的 exe 或在其目录植入文件”为准）：
#   - 受检主体：Users(S-1-5-32-545/BU)、Authenticated Users(S-1-5-11/AU)、Everyone(S-1-1-0/WD)、
#     INTERACTIVE(S-1-5-4/IU)、以及 -ExpectedUserSid（当前登录用户本人）；
#   - 只看 Allow ACE；忽略仅作用于新建子项的 InheritOnly（IO）ACE；
#   - 对象 Owner 为受检主体 → 不可信（Owner 隐含 WRITE_DAC）；
#   - exe 文件本身：WriteData/AppendData/WriteEA/WriteAttributes/Delete/WRITE_DAC/WRITE_OWNER/
#     GENERIC_WRITE/GENERIC_ALL 任一 → 不可信；
#   - exe 所在目录：上述权限 + DeleteChild（可删除替换子项），且 WriteData(AddFile)/
#     AppendData(AddSubdirectory) 亦计入（可植入同目录 DLL）；
#   - 更上层祖先目录（直到卷根）：只计“可删除/重命名既有子项或改写自身 ACL”的权限——
#     DeleteChild、Delete、WRITE_DAC、WRITE_OWNER、GENERIC_ALL。仅能“新建”子项的
#     AddFile/AddSubdirectory 不能替换既有子目录，不计入（实施偏差说明见 docs/ACCEPTANCE-hardening-v4.md：
#     Windows 默认卷根 C:\ 对 Authenticated Users 有一条非继承的“创建文件夹”ACE，若按方案原文
#     对祖先计入 AppendData，所有路径都会被误拒）。

$script:TrustWatchedAliases = @{
  "BU" = "S-1-5-32-545"
  "AU" = "S-1-5-11"
  "WD" = "S-1-1-0"
  "IU" = "S-1-5-4"
}

# SDDL 访问权限字符串代码 → 掩码（文件/目录与通用权限）
$script:SddlRightCodes = @{
  "GA" = 0x10000000; "GX" = 0x20000000; "GW" = 0x40000000; "GR" = 0x80000000
  "FA" = 0x001F01FF; "FR" = 0x00120089; "FW" = 0x00120116; "FX" = 0x001200A0
  "SD" = 0x00010000; "RC" = 0x00020000; "WD" = 0x00040000; "WO" = 0x00080000
  "CC" = 0x00000001; "DC" = 0x00000002; "LC" = 0x00000004; "SW" = 0x00000008
  "RP" = 0x00000010; "WP" = 0x00000020; "DT" = 0x00000040; "LO" = 0x00000080
  "CR" = 0x00000100
  "KA" = 0x000F003F; "KR" = 0x00020019; "KW" = 0x00020006; "KX" = 0x00020019
}

$script:MaskFile     = 0x2 -bor 0x4 -bor 0x10 -bor 0x100 -bor 0x10000 -bor 0x40000 -bor 0x80000 -bor 0x40000000 -bor 0x10000000
$script:MaskExeDir   = $script:MaskFile -bor 0x40
$script:MaskAncestor = 0x40 -bor 0x10000 -bor 0x40000 -bor 0x80000 -bor 0x10000000

function ConvertFrom-SddlRights([string]$Rights) {
  $r = $Rights.Trim()
  if ($r -match "^0[xX][0-9a-fA-F]+$") { return [int64]([Convert]::ToUInt32($r.Substring(2), 16)) }
  if ($r -match "^[0-9]+$") { return [int64]$r }
  $mask = [int64]0
  for ($i = 0; $i + 2 -le $r.Length; $i += 2) {
    $code = $r.Substring($i, 2).ToUpperInvariant()
    if ($script:SddlRightCodes.ContainsKey($code)) { $mask = $mask -bor [int64]$script:SddlRightCodes[$code] }
  }
  return $mask
}

function ConvertTo-CanonicalSid([string]$Sid) {
  $s = $Sid.Trim()
  $up = $s.ToUpperInvariant()
  if ($script:TrustWatchedAliases.ContainsKey($up)) { return $script:TrustWatchedAliases[$up] }
  return $up
}

# 纯函数：给定对象 SDDL，返回首个不可信条目 @{ Sid; Reason } 或 $null。
#   -Kind File | ExeDir | Ancestor（决定计入的权限集合）
function Get-UntrustedWriteAce {
  param(
    [Parameter(Mandatory = $true)][string]$Sddl,
    [string]$ExpectedUserSid = "",
    [ValidateSet("File", "ExeDir", "Ancestor")][string]$Kind = "File"
  )
  $watched = @($script:TrustWatchedAliases.Values)
  if ($ExpectedUserSid) { $watched += $ExpectedUserSid.Trim().ToUpperInvariant() }
  $mask = switch ($Kind) { "File" { $script:MaskFile } "ExeDir" { $script:MaskExeDir } default { $script:MaskAncestor } }

  if ($Sddl -match "O:([^:]+?)(?=G:|D:|S:|$)") {
    $owner = ConvertTo-CanonicalSid $Matches[1]
    if ($watched -contains $owner) { return @{ Sid = $owner; Reason = "Owner" } }
  }
  $dacl = ""
  if ($Sddl -match "D:([^()]*)((\([^()]*\))*)") { $dacl = $Matches[2] }
  foreach ($m in [regex]::Matches($dacl, "\(([^()]*)\)")) {
    $parts = $m.Groups[1].Value.Split(";")
    if ($parts.Count -lt 6) { continue }
    $aceType = $parts[0].Trim().ToUpperInvariant()
    $flags = $parts[1].Trim().ToUpperInvariant()
    if ($aceType -ne "A") { continue }
    $isInheritOnly = $false
    for ($i = 0; $i + 2 -le $flags.Length; $i += 2) { if ($flags.Substring($i, 2) -eq "IO") { $isInheritOnly = $true } }
    if ($isInheritOnly) { continue }
    $sid = ConvertTo-CanonicalSid $parts[5]
    if (-not ($watched -contains $sid)) { continue }
    $rights = ConvertFrom-SddlRights $parts[2]
    if (($rights -band $mask) -ne 0) {
      return @{ Sid = $sid; Reason = ("Allow 0x{0:X}" -f $rights) }
    }
  }
  return $null
}

# 纯函数：任务 Exec Command 规范化——去首尾空白与引号、展开环境变量；Windows 有根路径再 GetFullPath。
function ConvertTo-NormalizedExecPath([string]$Command) {
  if ($null -eq $Command) { return "" }
  $c = $Command.Trim()
  if ($c.Length -ge 2 -and $c.StartsWith('"') -and $c.EndsWith('"')) { $c = $c.Substring(1, $c.Length - 2) }
  $c = [Environment]::ExpandEnvironmentVariables($c.Trim())
  if ($c -match "^[A-Za-z]:\\" -or $c.StartsWith("\\")) {
    if ([IO.Path]::DirectorySeparatorChar -eq "\") {
      try { $c = [IO.Path]::GetFullPath($c) } catch { }
    }
  }
  return $c
}

# 纯函数：Exec 与期望路径是否一致（OrdinalIgnoreCase；Arguments 必须缺失或空白）。
function Test-TaskExecMatches {
  param([string]$Command, [string]$Arguments, [Parameter(Mandatory = $true)][string]$ExpectedPath)
  if ($Arguments -and $Arguments.Trim()) { return $false }
  $actual = ConvertTo-NormalizedExecPath $Command
  $expected = ConvertTo-NormalizedExecPath $ExpectedPath
  if (-not $actual -or -not $expected) { return $false }
  return [string]::Equals($actual, $expected, [StringComparison]::OrdinalIgnoreCase)
}

# 读取真实 ACL：exe、其目录、逐级祖先直到卷根。返回首个不可信对象 @{ Path; Sid; Reason } 或 $null。
function Test-CollectorPathTrusted {
  param([Parameter(Mandatory = $true)][string]$ExePath, [string]$ExpectedUserSid = "")
  $items = @(@{ Path = $ExePath; Kind = "File" })
  $dir = Split-Path -Parent $ExePath
  $kind = "ExeDir"
  while ($dir) {
    $items += @{ Path = $dir; Kind = $kind }
    $kind = "Ancestor"
    $parent = Split-Path -Parent $dir
    if (-not $parent -or $parent -eq $dir) { break }
    $dir = $parent
  }
  foreach ($it in $items) {
    $sddl = $null
    try { $sddl = (Get-Acl -LiteralPath $it.Path -ErrorAction Stop).Sddl } catch { $sddl = $null }
    if (-not $sddl) {
      return @{ Path = $it.Path; Sid = ""; Reason = "无法读取 ACL" }
    }
    $hit = Get-UntrustedWriteAce -Sddl $sddl -ExpectedUserSid $ExpectedUserSid -Kind $it.Kind
    if ($hit) { return @{ Path = $it.Path; Sid = $hit.Sid; Reason = $hit.Reason } }
  }
  return $null
}
