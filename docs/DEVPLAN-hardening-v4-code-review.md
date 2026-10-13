
## 处理结果

以下修复记入 `hardening-v4` 的单独提交，未改写既有历史。

- **M-1 已修复**：`collector_ctl.rs` 新增 `COLLECTOR_EXIT_3_MESSAGE` / `collector_exit_message`。GUI 经计划任务或 runas 拉起采集器，无法直接取得其退出码，因此在“立即启动”和“启用自启后启动”失败时，若 GUI 只读检测到 schema 过新，改为返回“统计库版本过新……请升级程序”的专门文案（含库版本与支持版本）；成功路径不查询 SQLite。新增单测 `hardening_v4_collector_exit_code_3_message`。
- **M-2 已修复**：`collector-path-trust.ps1` 的受检集合增加 Power Users（S-1-5-32-547）和 `-ExtraSids`。`install-collector-task.ps1` 在核对提权身份与 ExpectedUserSid 一致后，传入当前令牌的用户 SID 与全部组 SID（`Get-TokenWatchedSids`，即 `WindowsIdentity.Groups`），并剔除 Administrators、SYSTEM、服务 SID 等特权主体，以免提权令牌使 Program Files 被误拒。OWNER RIGHTS 只约束 Owner，而 Owner 属受检集合时已直接判为不可信，所以单独忽略 OW ACE 不改变结论。脚本保持 PS 5.1 语法；纯函数接缝不变，新增 10 项断言（共 38 项），可在 Linux 上运行。
- **L-1 已修复**：`backup_snapshot` 每次 step 前把源连接的 busy_timeout 设为 min(5s, 剩余时间)，重试间隔也不超过剩余时间，总耗时不再超出 `total`。
- **L-2 已修复**：`retry_wts_registration` 的注释写明了对常驻 25ms 定时器的依赖。
- **L-3、L-4**：接受，不修改。
