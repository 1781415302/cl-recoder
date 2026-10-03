# CL Recoder 采集器堆损坏修复——S4 部署与稳定性验收记录

日期：2026-10-01。对应 Plan：`docs/DEVPLAN-collector-heap-corruption.md`（S1—S3 由前一执行者完成并经复审，本记录覆盖部署与 S4）。

## 部署

| 项 | 值 |
|---|---|
| 候选产物 | `target/heap-candidate/release/cl-recoder-collector.exe` |
| 候选 SHA256 | `ca5ea3f3181a01619680ac04fbc7e705f707f0b36be47d81fb654af818794221` |
| 部署目标（计划任务指向） | `C:\Users\17814\Documents\cl recoder\target\release\cl-recoder-collector.exe` |
| 部署后 SHA256 校验 | 与候选一致 |
| 旧 exe 备份 | `target/release/cl-recoder-collector.exe.pre-heapfix.bak`（SHA `decc74ae…d1d96`，即发生 3 次崩溃的修复前版本） |
| 部署时间 | 2026-10-01 15:39（collector 未运行时替换，GUI 运行不受影响） |

候选与源码一致性：部署前用当前工作树重建到候选目录，2.2s 完成且哈希不变（无重编译），证明候选即当前源码产物。

## 启动基线

- 启动方式：`schtasks /Run /TN ClRecoderCollector`（生产启动路径）
- 启动时间：2026-10-01 15:39:59；PID：122992（观察全程未变，无重启）
- 任务状态：Running（LastResult 0x41301 = 任务运行中，非崩溃码）
- 控制管道 `\\.\pipe\clrecoder-control` 启动后即存在
- 事件日志基线：collector 最后崩溃为 2026-10-01 09:27:06（0xC0000005，旧 exe）；2026-09-30 19:35:50 / 21:18:55 为 0xC0000374。均为本部署前历史证据。

## 观察结果（15:39:59 → 15:56:05，共 16 分钟）

| 检查项 | 结果 |
|---|---|
| 进程存活 | 全程存活，PID/StartTime 未变 |
| 新 APPCRASH 1000（自启动时刻起） | **0 条**（查询已自证执行：同期事件总数 0） |
| 控制管道 | 持续存在 |
| 统计写入 | `stats.db-wal` 在观察末秒（15:56:05）仍在写入，`stats.db` 15:54:53 flush——输入事件经 aggregator 正常落库，说明首次设备识别的名称读取（原堆破坏调用点）已正常执行 |

**结论：S4 通过。** 历史崩溃窗口为启动后 26–99 秒，本次修复版连续运行 16 分钟、覆盖首次设备识别与持续输入统计，无任何新崩溃。

## 方法说明与限制

- 后台观察脚本首轮的 `tasklist /fi` 过滤在后台 shell 中被参数转义破坏产生 12 次"进程消失"误报；已用 `Get-Process`（PID/StartTime 不变）、任务状态、管道存在性与 WAL 实时写入四路交叉验证推翻误报。存活判定以交叉验证为准。
- 外设物理重连（Plan 建议项）未执行：无人值守无法物理插拔。拔插场景的名称查询路径已由 S2 native 冒烟（11 台真实键鼠只读读取一致）与确定性回归（句柄复用新 ID、 forget_handle）覆盖，风险较低；用户可在日常使用中留意。
- 后续判读：本记录之后的 collector 崩溃（如再出现 0xC0000374/0xC0000005）属于新证据，应保留事件信息进入 dump / Page Heap 诊断，不再重复部署或加重启策略。
- 回滚方式：用 `.pre-heapfix.bak` 覆盖回 `target/release/cl-recoder-collector.exe`（仅作诊断对照用，不建议长期回退）。
