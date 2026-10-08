# 向当前用户的采集器发送正常退出请求，等待收尾写入完成；不强杀进程。
[CmdletBinding(SupportsShouldProcess)]
param()

$ErrorActionPreference = 'Stop'
if (-not $PSCmdlet.ShouldProcess('ClRecoderCollector', '请求采集器正常退出')) { return }

if (-not ('ClRecoderStopPipe' -as [type])) {
  Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class ClRecoderStopPipe {
  [DllImport("kernel32.dll", SetLastError=true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool GetNamedPipeServerProcessId(SafePipeHandle pipe, out uint pid);
}
'@
}

$pipe = [IO.Pipes.NamedPipeClientStream]::new('.', 'clrecoder-control', [IO.Pipes.PipeDirection]::InOut, [IO.Pipes.PipeOptions]::Asynchronous)
$reader = $null
$writer = $null
try {
  $pipe.Connect(1500)
  [uint32]$collectorId = 0
  if (-not [ClRecoderStopPipe]::GetNamedPipeServerProcessId($pipe.SafePipeHandle, [ref]$collectorId)) {
    throw '无法确认采集器进程，未发送退出请求。'
  }
  $collector = Get-Process -Id $collectorId -ErrorAction Stop
  if ($collector.ProcessName -ne 'cl-recoder-collector') { throw '管道服务端不是采集器，未发送退出请求。' }
  $writer = [IO.StreamWriter]::new($pipe, [Text.UTF8Encoding]::new($false), 1024, $true)
  $reader = [IO.StreamReader]::new($pipe, [Text.UTF8Encoding]::new($false), $true, 1024, $true)
  $writer.AutoFlush = $true
  $writer.WriteLine('{"cmd":"shutdown"}')
  $pending = $reader.ReadLineAsync()
  if (-not $pending.Wait(2000)) { throw '采集器未及时回复；请确认其状态后再升级或操作数据库。' }
  $response = $pending.Result | ConvertFrom-Json
  if ($response.ok -ne $true) { throw ('采集器拒绝退出请求：' + $response.error) }
  if (-not $collector.WaitForExit(15000)) { throw '已请求退出，但收尾尚未完成；请稍后确认进程已退出。' }
  Write-Output '采集器已正常退出。'
} finally {
  if ($null -ne $reader) { $reader.Dispose() }
  if ($null -ne $writer) { $writer.Dispose() }
  $pipe.Dispose()
}
