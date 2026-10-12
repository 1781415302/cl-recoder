# collector-path-trust.ps1 —— 采集器路径可信性与任务 Exec 核对的纯函数（hardening-v4 §4.2.2/§4.2.3）
#
# 由 install-collector-task.ps1 dot-source；纯函数部分（Get-UntrustedWriteAce、
# ConvertTo-NormalizedExecPath、Test-TaskExecMatches）无副作用，可在任意宿主单测
# （scripts/tests/collector-path-trust.tests.ps1）。Test-CollectorPathTrusted 读取真实 ACL（Get-Acl），
# 仅在 Windows 上有意义。
#
# 判据（以“非特权主体能否替换以最高权限运行的 exe 或在其目录植入文件”为准）：
#   - 写权限 Allow ACE：主体不在显式可信集合中 → 不可信。可信集合仅为
#     Administrators(S-1-5-32-544/BA)、SYSTEM(S-1-5-18/SY)、LOCAL SERVICE(S-1-5-19/LS)、
#     NETWORK SERVICE(S-1-5-20/NS)、以及 NT SERVICE 前缀 S-1-5-80-*（含 TrustedInstaller）。
#     不要求 ACE 主体属于当前令牌；其他用户/自定义组的写权限一律拒绝。
#   - Null DACL（SDDL 含 D:NO_ACCESS_CONTROL，或解析后无 DACL 段）= 人人完全控制 → 不可信；
#     空 DACL（D: 仅有控制标志、无 ACE）= 拒绝所有人 → 可信；无法解析的 SDDL → 不可信。
#   - OWNER RIGHTS(S-1-3-4/OW)：其 ACE 只约束 Owner；Owner 非可信主体时已直接判为不可信，
#     故 OW ACE 一律跳过（不重复判定）。
#   - Deny ACE 一律忽略（保守：可能误拒，不会因 Deny 而放行）。
#   - 条件 ACE（XA/XD）及无法可靠分类的 ACE：不求值条件表达式，一律视为不可信；
#     ACE 列表用嵌套括号感知切分，避免条件表达式内的括号截断后续普通 Allow ACE。
#   - Windows 上 Get-UntrustedWriteAce 优先走 RawSecurityDescriptor；Linux 单测走 SDDL 字符串路径。
#   - 只看 Allow ACE；忽略仅作用于新建子项的 InheritOnly（IO）ACE。
#   - 对象 Owner 不在可信集合 → 不可信（Owner 隐含 WRITE_DAC）。
#   - exe 文件本身：WriteData/AppendData/WriteEA/WriteAttributes/Delete/WRITE_DAC/WRITE_OWNER/
#     GENERIC_WRITE/GENERIC_ALL 任一 → 不可信；
#   - exe 所在目录：上述权限 + DeleteChild，且 WriteData(AddFile)/AppendData(AddSubdirectory) 亦计入；
#   - 更上层祖先目录：只计 DeleteChild、Delete、WRITE_DAC、WRITE_OWNER、GENERIC_ALL
#     （不计 AddFile/AddSubdirectory；见 docs/ACCEPTANCE-hardening-v4.md 关于 C:\ 默认 ACE）；
#   - 卷根（如 C:\）：与普通祖先分开，不计对象自身 DELETE（0x10000）。机器上 C:\ 常给
#     Authenticated Users Allow 0x1301BF，与祖先掩码交集仅 DELETE，不能据此证明可替换
#     Program Files 下的安装路径；仍检查 DELETE_CHILD、WRITE_DAC、WRITE_OWNER、GENERIC_ALL。
#   - -ExpectedUserSid / -ExtraSids：保留参数兼容；写 ACE 判定已改为“非可信即拒绝”，
#     不再依赖令牌组成员列表（Select-TrustWatchedSids / Get-TokenWatchedSids 仍供安装脚本与测试使用）。

# 写 ACE / Owner 的显式可信主体（精确 SID）与前缀
$script:TrustTrustedSids = @(
  "S-1-5-32-544",  # Administrators
  "S-1-5-18",      # SYSTEM
  "S-1-5-19",      # LOCAL SERVICE
  "S-1-5-20",      # NETWORK SERVICE
  "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"  # TrustedInstaller
)
$script:TrustTrustedPrefixes = @("S-1-5-80-")  # NT SERVICE\*

# 兼容：过滤令牌 SID 时去掉特权主体（供 Get-TokenWatchedSids / 旧测试）
$script:TrustPrivilegedSids = @("S-1-5-32-544", "S-1-5-18", "S-1-5-19", "S-1-5-20", "S-1-5-114")
$script:TrustPrivilegedPrefixes = @("S-1-5-80-", "S-1-16-")

# 纯函数：过滤令牌 SID 列表，去掉特权主体与空值，大写去重。
function Select-TrustWatchedSids([string[]]$Sids) {
  $out = @()
  foreach ($s in @($Sids)) {
    if (-not $s) { continue }
    $u = $s.Trim().ToUpperInvariant()
    if (-not $u) { continue }
    if ($script:TrustPrivilegedSids -contains $u) { continue }
    $skip = $false
    foreach ($p in $script:TrustPrivilegedPrefixes) { if ($u.StartsWith($p)) { $skip = $true } }
    if ($skip) { continue }
    if (-not ($out -contains $u)) { $out += $u }
  }
  return ,$out
}

# 读取当前进程令牌的用户 SID 与组 SID（仅 Windows；非 Windows 或失败返回空数组）。
function Get-TokenWatchedSids {
  $raw = @()
  try {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    if ($id.User) { $raw += $id.User.Value }
    foreach ($g in $id.Groups) { try { $raw += $g.Value } catch { } }
  } catch { return ,@() }
  return (Select-TrustWatchedSids $raw)
}

$script:TrustWatchedAliases = @{
  "BA" = "S-1-5-32-544"
  "SY" = "S-1-5-18"
  "LS" = "S-1-5-19"
  "NS" = "S-1-5-20"
  "TI" = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
  "PU" = "S-1-5-32-547"
  "BU" = "S-1-5-32-545"
  "AU" = "S-1-5-11"
  "WD" = "S-1-1-0"
  "IU" = "S-1-5-4"
  "OW" = "S-1-3-4"
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
# 卷根：与 MaskAncestor 相同但不含对象自身 DELETE（0x10000）
$script:MaskVolumeRoot = 0x40 -bor 0x40000 -bor 0x80000 -bor 0x10000000

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

# 写 ACE / Owner 是否属于显式可信集合。
function Test-IsTrustedWriteSid([string]$Sid) {
  $u = ConvertTo-CanonicalSid $Sid
  if ($script:TrustTrustedSids -contains $u) { return $true }
  foreach ($p in $script:TrustTrustedPrefixes) { if ($u.StartsWith($p)) { return $true } }
  return $false
}

# 纯函数：在括号深度 0 处截取直到 $Marker（不含），用于剥离 DACL 后的 SACL（S:）。
function Get-SddlTextUntilTopLevelMarker([string]$Text, [string]$Marker) {
  if (-not $Text) { return "" }
  $mLen = $Marker.Length
  if ($mLen -le 0) { return $Text }
  $depth = 0
  for ($i = 0; $i -le $Text.Length - $mLen; $i++) {
    $ch = $Text[$i]
    if ($ch -eq [char]'(') { $depth++ }
    elseif ($ch -eq [char]')') { if ($depth -gt 0) { $depth-- } }
    elseif ($depth -eq 0 -and $Text.Substring($i, $mLen) -eq $Marker) {
      return $Text.Substring(0, $i)
    }
  }
  return $Text
}

# 纯函数：按嵌套括号切分 ACE 列表。返回 ACE 字符串数组（含外层括号）；
# 无法平衡或出现括号外杂讯时返回 $null（调用方应保守拒绝）。
function Split-SddlAceList([string]$AceText) {
  if ($null -eq $AceText) { return ,@() }
  $aces = New-Object System.Collections.Generic.List[string]
  $n = $AceText.Length
  $i = 0
  while ($i -lt $n) {
    while ($i -lt $n -and [char]::IsWhiteSpace($AceText[$i])) { $i++ }
    if ($i -ge $n) { break }
    if ($AceText[$i] -ne [char]'(') { return $null }
    $depth = 0
    $start = $i
    for (; $i -lt $n; $i++) {
      $ch = $AceText[$i]
      if ($ch -eq [char]'(') { $depth++ }
      elseif ($ch -eq [char]')') {
        $depth--
        if ($depth -eq 0) {
          $i++
          [void]$aces.Add($AceText.Substring($start, $i - $start))
          break
        }
      }
    }
    if ($depth -ne 0) { return $null }
  }
  return ,$aces.ToArray()
}

# 纯函数：在括号深度 0 处按分号切分 ACE 字段（条件表达式内的分号不切开）。
function Split-SddlAceFields([string]$AceInner) {
  $fields = New-Object System.Collections.Generic.List[string]
  $depth = 0
  $start = 0
  for ($i = 0; $i -lt $AceInner.Length; $i++) {
    $ch = $AceInner[$i]
    if ($ch -eq [char]'(') { $depth++ }
    elseif ($ch -eq [char]')') { if ($depth -gt 0) { $depth-- } }
    elseif ($ch -eq [char]';' -and $depth -eq 0) {
      [void]$fields.Add($AceInner.Substring($start, $i - $start))
      $start = $i + 1
    }
  }
  [void]$fields.Add($AceInner.Substring($start))
  return ,$fields.ToArray()
}

# 纯函数：解析 SDDL 的 DACL 形态（不依赖 Windows ACL API，可在 Linux pwsh 单测）。
# 使用嵌套括号感知切分，不会因条件 ACE 内的括号而截断后续 ACE。
# 返回 Kind = Null | Empty | Aces | Invalid；Aces 时 Text 为完整 ACE 括号串。
function Get-SddlDaclInfo([string]$Sddl) {
  if (-not $Sddl -or -not $Sddl.Trim()) { return @{ Kind = "Invalid"; Text = "" } }
  if ($Sddl -match "(?i)D:NO_ACCESS_CONTROL") { return @{ Kind = "Null"; Text = "" } }
  $dm = [regex]::Match($Sddl, "D:")
  if (-not $dm.Success) {
    if ($Sddl -match "(?i)(^|:)(O|G|S):") { return @{ Kind = "Null"; Text = "" } }
    return @{ Kind = "Invalid"; Text = "" }
  }
  $afterD = $Sddl.Substring($dm.Index + 2)
  $daclBody = Get-SddlTextUntilTopLevelMarker $afterD "S:"
  $i = 0
  while ($i -lt $daclBody.Length -and $daclBody[$i] -ne [char]'(') { $i++ }
  $aceText = if ($i -lt $daclBody.Length) { $daclBody.Substring($i) } else { "" }
  if (-not $aceText -or -not $aceText.Trim()) { return @{ Kind = "Empty"; Text = "" } }
  # 若 ACE 列表无法完整切分（括号不平衡），视为无效而非 Empty
  $split = Split-SddlAceList $aceText
  if ($null -eq $split) { return @{ Kind = "Invalid"; Text = "" } }
  return @{ Kind = "Aces"; Text = $aceText }
}

# Windows 原生解析接缝（仅 Windows；Linux / 不可用时返回 $null 表示未处理）。
# 返回 @{ Handled = $true; Hit = <不可信条目或 $null> } 或 $null（回退到 SDDL 字符串路径）。
function Get-UntrustedWriteAceFromRawSd {
  param([string]$Sddl, [int64]$Mask)
  try {
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor($Sddl)
  } catch {
    return $null
  }
  if ($null -eq $sd.DiscretionaryAcl) {
    return @{ Handled = $true; Hit = @{ Sid = "S-1-1-0"; Reason = "Null DACL" } }
  }
  if ($sd.Owner -and -not (Test-IsTrustedWriteSid $sd.Owner.Value)) {
    return @{ Handled = $true; Hit = @{ Sid = $sd.Owner.Value.ToUpperInvariant(); Reason = "Owner" } }
  }
  foreach ($ace in $sd.DiscretionaryAcl) {
    $aceType = $ace.AceType.ToString()
    # AccessAllowed = 简单允许；其余（含回调/条件/对象 ACE）保守拒绝
    if ($aceType -ne "AccessAllowed") {
      if ($aceType -eq "AccessDenied") { continue }
      $sid = try { $ace.SecurityIdentifier.Value.ToUpperInvariant() } catch { "" }
      return @{ Handled = $true; Hit = @{ Sid = $sid; Reason = ("条件/未知 ACE (" + $aceType + ")") } }
    }
    $flags = $ace.AceFlags.ToString()
    if ($flags -match "InheritOnly") { continue }
    $sid = $ace.SecurityIdentifier.Value.ToUpperInvariant()
    if ($sid -eq "S-1-3-4") { continue }
    if (Test-IsTrustedWriteSid $sid) { continue }
    $rights = [int64]$ace.AccessMask
    if (($rights -band $Mask) -ne 0) {
      return @{ Handled = $true; Hit = @{ Sid = $sid; Reason = ("Allow 0x{0:X}" -f $rights) } }
    }
  }
  return @{ Handled = $true; Hit = $null }
}

# 纯函数：给定对象 SDDL，返回首个不可信条目 @{ Sid; Reason } 或 $null。
#   -Kind File | ExeDir | Ancestor | VolumeRoot（决定计入的权限集合）
#   -ExpectedUserSid / -ExtraSids：保留兼容；判定以“非可信写主体即拒绝”为准。
#   Windows 上优先走 RawSecurityDescriptor；不可用时用嵌套括号感知的 SDDL 字符串路径（Linux 单测）。
function Get-UntrustedWriteAce {
  param(
    [Parameter(Mandatory = $true)][string]$Sddl,
    [string]$ExpectedUserSid = "",
    [ValidateSet("File", "ExeDir", "Ancestor", "VolumeRoot")][string]$Kind = "File",
    [string[]]$ExtraSids = @()
  )
  $null = $ExpectedUserSid; $null = $ExtraSids
  $mask = switch ($Kind) {
    "File" { $script:MaskFile }
    "ExeDir" { $script:MaskExeDir }
    "VolumeRoot" { $script:MaskVolumeRoot }
    default { $script:MaskAncestor }
  }

  $native = Get-UntrustedWriteAceFromRawSd -Sddl $Sddl -Mask $mask
  if ($null -ne $native -and $native.Handled) { return $native.Hit }

  $dacl = Get-SddlDaclInfo $Sddl
  if ($dacl.Kind -eq "Invalid") { return @{ Sid = ""; Reason = "无法解析 SDDL" } }
  if ($dacl.Kind -eq "Null") { return @{ Sid = "S-1-1-0"; Reason = "Null DACL" } }

  if ($Sddl -match "O:([^:]+?)(?=G:|D:|S:|$)") {
    $owner = ConvertTo-CanonicalSid $Matches[1]
    if (-not (Test-IsTrustedWriteSid $owner)) { return @{ Sid = $owner; Reason = "Owner" } }
  }

  if ($dacl.Kind -eq "Empty") { return $null }

  $aceList = Split-SddlAceList $dacl.Text
  if ($null -eq $aceList) { return @{ Sid = ""; Reason = "无法解析 SDDL" } }

  foreach ($ace in $aceList) {
    if ($ace.Length -lt 2 -or $ace[0] -ne [char]'(' -or $ace[$ace.Length - 1] -ne [char]')') {
      return @{ Sid = ""; Reason = "ACE 结构异常" }
    }
    $inner = $ace.Substring(1, $ace.Length - 2)
    $parts = Split-SddlAceFields $inner
    if ($parts.Count -lt 6) { return @{ Sid = ""; Reason = "ACE 结构异常" } }
    $aceType = $parts[0].Trim().ToUpperInvariant()
    $flags = $parts[1].Trim().ToUpperInvariant()
    # 简单 Allow：按写掩码 + 可信白名单检查
    if ($aceType -eq "A") {
      $isInheritOnly = $false
      for ($i = 0; $i + 2 -le $flags.Length; $i += 2) {
        if ($flags.Substring($i, 2) -eq "IO") { $isInheritOnly = $true }
      }
      if ($isInheritOnly) { continue }
      $sid = ConvertTo-CanonicalSid $parts[5]
      if ($sid -eq "S-1-3-4") { continue }
      if (Test-IsTrustedWriteSid $sid) { continue }
      $rights = ConvertFrom-SddlRights $parts[2]
      if (($rights -band $mask) -ne 0) {
        return @{ Sid = $sid; Reason = ("Allow 0x{0:X}" -f $rights) }
      }
      continue
    }
    # 简单 Deny：忽略（保守：可能误拒，不会因 Deny 而放行）
    if ($aceType -eq "D") { continue }
    # XA/XD/对象 ACE/未知类型：不求值条件，保守拒绝（避免截断或误判）
    $sidGuess = ConvertTo-CanonicalSid $parts[5]
    return @{ Sid = $sidGuess; Reason = ("条件/未知 ACE (" + $aceType + ")") }
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
  param([Parameter(Mandatory = $true)][string]$ExePath, [string]$ExpectedUserSid = "", [string[]]$ExtraSids = @())
  $items = @(@{ Path = $ExePath; Kind = "File" })
  $dir = Split-Path -Parent $ExePath
  $kind = "ExeDir"
  while ($dir) {
    $parent = Split-Path -Parent $dir
    $isVolumeRoot = (-not $parent -or $parent -eq $dir)
    # 卷根用 VolumeRoot 掩码（不含自身 DELETE）；ExeDir 若恰为卷根仍按 ExeDir（含写文件等更严规则）
    if ($kind -eq "ExeDir") { $itemKind = "ExeDir" }
    elseif ($isVolumeRoot) { $itemKind = "VolumeRoot" }
    else { $itemKind = "Ancestor" }
    $items += @{ Path = $dir; Kind = $itemKind }
    if ($isVolumeRoot) { break }
    $kind = "Ancestor"
    $dir = $parent
  }
  foreach ($it in $items) {
    $sddl = $null
    try { $sddl = (Get-Acl -LiteralPath $it.Path -ErrorAction Stop).Sddl } catch { $sddl = $null }
    if (-not $sddl) {
      return @{ Path = $it.Path; Sid = ""; Reason = "无法读取 ACL" }
    }
    $hit = Get-UntrustedWriteAce -Sddl $sddl -ExpectedUserSid $ExpectedUserSid -Kind $it.Kind -ExtraSids $ExtraSids
    if ($hit) { return @{ Path = $it.Path; Sid = $hit.Sid; Reason = $hit.Reason } }
  }
  return $null
}
