# collector-task-policy.ps1 —— CL Recoder 采集器计划任务三项策略【纯变换】（usability-runtime-v3 §4.3）
#
# 职责边界（§2.1）：只做深克隆 + 三个命名空间 Settings 叶子节点的修改/补齐——
#   ExecutionTimeLimit          = PT0S    （无执行时限）
#   DisallowStartIfOnBatteries  = false   （允许电池启动）
#   StopIfGoingOnBatteries      = false   （切换电池不停止）
# 注册信息 / 账户（Principals）/ 触发器（Triggers）/ 动作（Actions）/ 其它 Settings 节点 /
# 安全描述符一概不动；不触碰系统计划任务、无任何副作用；原 [xml] 输入保持不变；
# 重复应用等价（幂等）。GUI 后端不直接调用本文件——install-collector-task.ps1
# dot-source 后使用；测试脚本亦直接 dot-source 做纯矩阵断言。
#
# 真机实证（2026-10-02，Windows 11，schtasks /Create /SC ONLOGON /RL HIGHEST 导出）：
# 任务 XML 的 Settings 子节点无固定顺序，且 /Create 产物可能【完全没有】ExecutionTimeLimit
# 节点（缺省 PT72H 为运行时默认值）——settingsType 为无序集合，缺节点时追加到
# Settings 末尾即可；Settings 本身缺失时创建后追加为 Task 最后子节点。

$script:CLREC_TASK_NS = "http://schemas.microsoft.com/windows/2004/02/mit/task"

function Get-CollectorTaskPolicyXml {
  param(
    [Parameter(Mandatory = $true)]
    [xml]$Definition
  )

  # —— 深克隆：LoadXml(OuterXml) 生成独立文档，与输入对象零共享（原 XML 不变）——
  $out = New-Object System.Xml.XmlDocument
  $out.LoadXml($Definition.OuterXml)
  $taskEl = $out.DocumentElement
  if (-not $taskEl) {
    throw "Get-CollectorTaskPolicyXml：输入不是有效的任务 XML（无文档元素）"
  }

  # —— 定位 / 补齐 Settings（只动三个命名空间叶子，其余节点一概保留）——
  $settings = $taskEl.GetElementsByTagName("Settings", $script:CLREC_TASK_NS) | Select-Object -First 1
  if (-not $settings) {
    $settings = $out.CreateElement("Settings", $script:CLREC_TASK_NS)
    [void]$taskEl.AppendChild($settings)
  }

  foreach ($leaf in @(
    @{ Name = "ExecutionTimeLimit"; Value = "PT0S" },
    @{ Name = "DisallowStartIfOnBatteries"; Value = "false" },
    @{ Name = "StopIfGoingOnBatteries"; Value = "false" }
  )) {
    $node = $settings.GetElementsByTagName($leaf.Name, $script:CLREC_TASK_NS) | Select-Object -First 1
    if ($node) {
      $node.InnerText = $leaf.Value
    } else {
      $new = $out.CreateElement($leaf.Name, $script:CLREC_TASK_NS)
      $new.InnerText = $leaf.Value
      [void]$settings.AppendChild($new)
    }
  }

  return $out
}
