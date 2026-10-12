# hardening-v4（v0.3.0）验收记录

日期：2026-10-12。分支：`hardening-v4`。对应方案：[实施方案 r2](./DEVPLAN-hardening-v4.md)，[评审](./DEVPLAN-hardening-v4-review.md)。

工程实现与非 Windows 环境自动验证已完成。凡须在真实 Windows 上确认的项目（M1–M9）均**未验收**，本文不以 wine 或 Linux 结果代替。

## 真机验收状态

| 编号 | 内容 | 状态 |
|---|---|---|
| M1 | v0.2.0 基线：安全桌面松开修饰键后组合键残留是否可复现 | 未验收 |
| M2 | 新版锁屏/解锁、Win+L、睡眠/唤醒后修饰键状态清空，诊断日志出现 `raw_input.session_reset` | 未验收 |
| M3 | 按机器安装到 `%ProgramFiles%`、启用采集器自启、任务执行目标正确 | 未验收 |
| M4 | 便携版置于用户可写目录时启用自启被拒绝（退出码 7）并显示提示；移入 `C:\Program Files\CL Recoder Portable\` 后成功 | 未验收 |
| M5 | 从 v0.2.x 按 README 迁移；旧任务/旧 Run 项提示出现与消失；修复冲突报退出码 5 且不修改 | 未验收 |
| M6 | 高 schema 统计库：采集器退出码 3，GUI 红色横幅 | 未验收 |
| M7 | WhatPulse 运行中导入：快照一致，源库仅可能出现 `-shm` | 未验收 |
| M8 | CSV 在 Excel 中打开，前缀单元格不被当作公式 | 未验收 |
| M9 | 大库仪表盘轮询 CPU 与切换日期范围的数值一致性 | 未验收 |

H1（会话事件清空键盘状态）属防御性改动：M1 基线尚未复现，其必要性以真机结果为准。

## 自动验证证据（Linux 主机，交叉目标 x86_64-pc-windows-gnu，wine 运行）

- Linux 原生：core 52、engine 38、store 51、diagnostics 9 项全部通过。
- wine：collector 197 通过、1 跳过；GUI（cl-recoder）140 通过、3 失败、1 跳过。3 项失败（`collector_health` 进程探测 2 项、`keylabel::ctrl_scancode_yields_ctrl_like_label`）在 v0.2.0 基线同样失败（基线 120 通过/3 失败），属 wine 环境差异，需在 Windows 上复核。
- Clippy `-D warnings`：windows-gnu 目标 workspace/all-targets 与 Linux 可编译 crate 均通过。
- 前端：69 项测试通过；TypeScript/Vite 生产构建通过。
- PowerShell 7.4（Linux）：`collector-path-trust.tests.ps1` 28 项断言通过；`collector-task-policy.tests.ps1` A 节 20 项通过，B 节依赖 `WindowsIdentity`/计划任务，须在 Windows（PowerShell 7 与 5.1）上运行。
- 概览缓存基准（release，wine，160600 行）：一年范围全量 36.7 ms、缓存 0.30 ms；全部时间 51.9 ms、缓存 0.28 ms。仅作相对参考。
- 未进行 NSIS 打包与安装测试。

## 与方案的偏差

1. WhatPulse 快照源库以 `SQLITE_OPEN_READ_ONLY | NO_MUTEX` 标志打开，未使用 `file:?mode=ro` URI，以避免 Windows 路径的 URI 转义问题；只读语义相同。
2. 路径可写性检查：上级目录仅计 DeleteChild、Delete、WriteDAC、WriteOwner、GenericAll，因 `C:\` 默认对 Authenticated Users 授予非继承的“创建文件夹”权限；采集器所在目录另计 AddFile/AddSubdirectory。
3. CSV 前缀列在方案清单外增加 `wp_combos` 的组合键与标签列（WhatPulse 原文，属外部来源）。
4. schema 过新提示使用独立命令 `get_app_notices`（30 秒轮询），旧安装提示使用设置页专用命令 `get_legacy_install_hints`，未并入 500 ms 轮询的 `collector_status`，以免高频轮询承担注册表与文件查询。
5. `Win32_System_RemoteDesktop`、`Win32_System_Power` 特性加在 collector crate 而非根 `Cargo.toml`。
6. 任务策略测试 B3b：`-RepairOnly` 且找不到采集器时由原行为改为退出码 1（与新的统一定位逻辑一致）。
7. 代码审阅后补充（见 [代码审阅](./DEVPLAN-hardening-v4-code-review.md)“处理结果”）：路径可信检查的受检集合扩展为固定主体 + Power Users + 当前提权令牌的全部非特权组；采集器退出码 3 在启动失败时映射为专门文案；备份总时限包含单次 step 内的忙等待。
