# CL Recoder 加固与性能修复方案：独立评审与裁决记录

日期：2026-10-12。评审对象：`docs/DEVPLAN-hardening-v4.md` r0 初稿。本记录解释裁决依据；执行以实施方案修订版 r1 为准，不以评审建议替代合同。

## 调度与前置信息

评审以“怀疑者”立场重新对照源码（v0.2.0 工作树）、Win32 文档（WTSRegisterSessionNotification、RegisterSuspendResumeNotification、WM_POWERBROADCAST、Message-Only Windows）、Tauri v2 NSIS `installMode` 文档及 SQLite Online Backup / WAL / URI 文档复核 r0。评审维度：输入生命周期、部署与权限、缓存一致性、存储/导入/导出、用户体验回退。全程只读，未修改源代码，未触碰真实采集器、数据库、计划任务或 UAC；所有 Windows 行为结论均标注为需 M1–M9 真机验收。

## 逐条裁决

| 编号 / 严重度 | 意见与定位 | 源码 / 文档复核 | 裁决与最终落点 |
|---|---|---|---|
| R1 / 高 | r0 以 `day < today` 为冻结边界，并在午夜换日时重建缓存。collector 跨日后最后一个 flush 周期仍写“昨天”，flush 失败还会保留桶无限期重试，冻结后的“昨天”可能少计，违反“数值完全一致” | `engine_loop.rs:20,35` 失败保留聚合桶重试；`:284–288` 每 1s tick 才检查跨天，入桶日期为 `agg.today` | **采纳**。§4.3 冻结边界改为 today−2，“昨天”留在实时段；另加 10 分钟 TTL 作为极端长时间 flush 失败的安全网，并在方案中如实写出这是“有界陈旧”的唯一来源 |
| R2 / 高 | 查询失败经 `swallow` 映射为空数据；内存兜底连接返回空表。若这些结果进入缓存，切换到真实库后历史会长期显示为 0 | `commands/mod.rs:46–55`；`state.rs:96–99,128–133` 兜底与切换 | **采纳**。只缓存 `Ok` 且非兜底连接的结果；兜底→真实库切换时 generation+1 |
| R3 / 中 | r0 把缓存放在 `ro_conn` 同一 Mutex 内，首次填充“全部”期间（~49ms）所有页面查询排队，与“降低锁争用”目标相悖且引入锁顺序问题 | `state.rs:61,108` 单一 Mutex | **采纳**。独立 `Mutex<OverviewCache>`，规定锁顺序：先 ro 后 cache，cache 锁只在读写条目时短暂持有 |
| R4 / 中 | r0 假设 WTSRegisterSessionNotification 一次成功。collector 由 ONLOGON 任务在登录极早期启动，Terminal Services 可能尚未就绪（文档列出 RPC_S_INVALID_BINDING，建议等待 `Global\TermSrvReadyEvent`）；另外 loop 1s 重建时旧注册未注销 | `raw_input.rs:200–216` 窗口重建与 `WindowGuard` | **采纳**。§4.1 加入定时器内限次重试与 `WindowGuard` 中先注销再 DestroyWindow。未采用“阻塞等待 TermSrvReadyEvent”，以免阻塞输入线程 |
| R5 / 中 | r0 声称覆盖 UAC 安全桌面与 Ctrl+Alt+Del。二者切换到 Winlogon/安全桌面时**不产生** WTS 锁定通知，r0 测试步骤会得出假阳性结论 | Win32 WTS 文档：仅 LOCK/UNLOCK/CONNECT/LOGON 等会话事件 | **采纳**。§4.1 如实声明限制、写入 README 已知限制；拒绝引入 `GetAsyncKeyState` 热路径交叉校验（风险高于收益） |
| R6 / 高 | r0 ACL 判据只看 Users/AU/Everyone。per-user 安装目录 `%LOCALAPPDATA%\…` 的写权限来自**当前用户本人 SID**的 FullControl，并非这些组，r0 的检查会放行真实问题场景；同时对 `C:\` 的 AU 创建文件夹 ACE 会误报 | Windows 默认 ACL：用户配置文件目录仅 SYSTEM/Administrators/用户本人；`C:\` 对 AU 的 AppendData 为 CI/InheritOnly 语义 | **采纳**。§4.2.2 判据加入 `-ExpectedUserSid` 与 INTERACTIVE，父目录仅看创建/删除子项权限，忽略 InheritOnly ACE；新退出码 7 与 GUI 文案；判定拆纯函数供 SDDL 夹具测试 |
| R7 / 中（UX） | r0 对所有 CSV 字段加 `'` 前缀。按键标签 `-`、`=`、`+` 是合法数据，会被改成 `'-`，破坏键名列及依赖导出的用户脚本 | `export.rs:118` 被 8 处调用，其中含键名列 | **采纳**。§4.5(c) 新增 `csv_text_field`，仅用于外部来源文本列（设备名/昵称/exe/应用名/WP 文本），键名与数值列不变 |
| R8 / 中 | r0 未注意 correctness-v2 §1.3-5 明确“不新增 Backup API feature”，与 H4 冲突；且只读打开 WAL 源在缺 `-shm` 且目录不可写时会失败，r0 无回退 | `DEVPLAN-correctness-v2.md` §1.3；SQLite WAL 文档“read-only WAL 需 -shm 存在或可创建” | **采纳**。方案开头显式推翻旧条款并给出理由；§4.4 失败回退原复制路径，禁止 `immutable=1`；README 注明可能创建 `-shm` |
| R9 / 高（决策） | r0 把 perMachine 与 ACL 检查当作独立可选项。若仅做 ACL 检查而不做 perMachine，默认安装版会被 exit 7 拒绝，安装版自启功能实际失效 | `tauri.conf.json` 无 `bundle.windows.nsis`，默认 currentUser | **采纳**。§10-1 写明二者耦合，必须同时决策；建议采用 perMachine |
| R10 / 低 | r0 复用 `StoreError::WalUnavailable("未知 schema 版本")` 报告过新版本，类型误导；collector 失败返回 1 后计划任务不重试本是好事，但无专用诊断，GUI 只显示“未运行” | `schema.rs:163`；`main.rs:215–221` | **采纳**。新增 `SchemaTooNew` 变体、collector 专用诊断码与退出码 3（已核实 1/2 已用、3 空闲）、GUI 横幅 |
| R11 / 低 | 建议同时把概览前端轮询改为 2s 以叠加收益 | — | **拒绝**。与“不调 flush/刷新节奏、不影响实时性”的已定非目标冲突；H3 后单次 <1ms 已无必要 |
| R12 / 低 | 建议为 H1 新增 `KeyboardStateClear` 事件以区别于 loop 重建 | `engine_loop.rs:393` 仅调用 `clear_sources`，不触及设备身份 | **拒绝**。现有事件语义已完全匹配，新增变体会波及 core/selftest 穷尽匹配，属无收益改动 |

**拒绝项**：R11、R12，理由如上。

## 评审确认且无需更改的结论

- `RegisterSuspendResumeNotification(DEVICE_NOTIFY_WINDOW_HANDLE)` 将 `WM_POWERBROADCAST` 直接发送到指定窗口，适用于 message-only 窗口；顶层广播方式不适用——r0 判断正确。
- `WTSRegisterSessionNotification` 以窗口句柄为目标，非广播，message-only 窗口理论上可接收；仍列入 M1 真机确认，并保留“不可见顶层窗口”作为需另行评审的回退。
- 复位事件经窗口线程同一 `tx` 发送，与 WM_INPUT 保持 FIFO 顺序，无需额外同步。
- `rusqlite 0.40.2` 的 `backup` 为特性开关，`bundled` SQLite 已含 backup 接口，不新增 crate，lockfile 不变；仍要求实施时 `--offline` 构建确认。
- 导出读事务在 WAL 下不阻塞 collector 写入；`Transaction` 解引用为 `Connection`，导出函数签名无需修改。
- 概览缓存不受 DPI 变更影响（概览仅含计数）；WhatPulse 导入只写 `wp_*` 表，不影响 `input_daily`，失效仅为保守措施。

## 收尾复核

1. r1 已逐条落实 R1–R10，修订记录见方案末尾。
2. 剩余风险：H1 的 WTS 接收能力、perMachine 安装后 GUI 是否以非提权令牌启动、per-user 卸载是否保留数据目录，均只能真机验证（M1、M7、M8）。
3. 本轮只新增设计与评审文档，没有修改项目源代码；方案中的测试是未来实施关卡，不得视为已通过。
4. 待用户决策项见方案 §10，其中 perMachine 为阻塞 M-E 的唯一决策。

## 独立审阅（r1）

日期：2026-10-12。评审对象：`DEVPLAN-hardening-v4.md` r1 及上文自评。只读复核 v0.2.0 工作树；未改源码、未提交。本机无 cargo registry 缓存，`--offline` 构建与 windows-rs 符号路径**未能实测**，相关结论标“需编译确认”。

| 编号 / 严重度 | 意见与定位 | 证据 | 要求的修改 |
|---|---|---|---|
| I1 / 高（阻塞 M-E） | perMachine 迁移遗漏 GUI 自启：GUI 依赖 `tauri-plugin-autostart`（HKCU Run 写入 exe 绝对路径）。旧 per-user 版卸载后 Run 项可能残留指向已删除 exe，或两份共存时登录启动旧 GUI；新 GUI 的“开机启动”开关可能显示已启用但路径为旧 | `src-tauri/Cargo.toml:13`；§4.2.1 只处理计划任务 | §4.2.1 迁移与 GUI 检测同时核对 Run 项路径，不符时提示“重新开启开机启动”（关→开重写）；M8 增加检查 Run 项；README 步骤补齐 |
| I2 / 中 | GUI“任务指向旧位置”检测在开发布局误报：GUI 在 `target\debug`、collector 常在 `target\release`，或 `-CollectorPath` 自定义时 | §4.2.1-2 判据“≠ 当前 exe 同目录” | 判据收窄为：仅安装版（exe 位于 Program Files）且任务 Exec 位于 `%LOCALAPPDATA%` 时提示；其余只记日志 |
| I3 / 中 | CSV 前缀“仅外部文本列”与现有实现形状不符：`write_csv_file(header, rows: Vec<Vec<String>>)` 对所有单元格统一调用 `csv_field`（`export.rs:118–138`），自评 R7 所说“8 处调用”与代码不符（实际在一个通用写函数内 2 处）。方案未规定如何按列区分 | `export.rs:118,135,138` | 明确实现方式：在构造行时对外部文本列调用 `csv_text_field`（写入前预处理），`write_csv_file` 保持只做 RFC4180 转义；列清单逐表列入方案，测试逐表断言 |
| I4 / 中（范围） | H6 日志轮转不在用户批准的清单内（t6 核实结论仅 5 项 + 小改动，日志轮转被列为“不建议/未列入”），属范围扩张 | 上轮核实结论 vs §1.1 H6 | 从本轮移出，或保持“可选”且默认不做；§8 M-A 去掉 H6 |
| I5 / 中（需编译确认） | Cargo feature 可能不对：windows-rs 中 `RegisterSuspendResumeNotification`/`UnregisterSuspendResumeNotification`、`HPOWERNOTIFY`、`DEVICE_NOTIFY_WINDOW_HANDLE`、`PBT_*`、`WM_POWERBROADCAST` 位于 `Win32_UI_WindowsAndMessaging`（winuser.h），`Win32_System_Power` 很可能多余；WTS 函数在 `Win32_System_RemoteDesktop` 正确 | `Cargo.toml:37–50` 已含 WindowsAndMessaging | 改为“以实际编译决定最小 feature 集”，禁止无用 feature |
| I6 / 中 | 现代待机（S0 Modern Standby）机器上通常**不发** `PBT_APMSUSPEND`；M2“睡眠/唤醒”在这类笔记本上可能得到“无日志”的假阴性。多数情况进入待机伴随 WTS LOCK，可覆盖 | Win32 文档 Modern Standby 与 PBT_APMSUSPEND 说明 | §4.1 声明该限制；M2 记录机器 `powercfg /a` 结果并区分 S3/S0 |
| I7 / 低 | `get_overview` 现走 `blocking_query`→`with_ro`，闭包只拿到 `&Connection`；“兜底不缓存”和“兜底→真实库切换 generation+1”需要在闭包内用 `is_fallback_conn` 判定或改 `with_ro` 返回切换标志，方案未定 | `state.rs:124–137`；`overview.rs:96–101` | 规定：切换发生在 `with_ro` 内，由其 bump generation；缓存路径在闭包内调用 `is_fallback_conn` 跳过缓存 |
| I8 / 低 | 缓存正确性复核结论（无需改）：`reader::overview` 的 `today` 按 `d.day = to` 而非系统日计算（`reader.rs:472`），实时段包含 `to`，合并正确；设备列表与昵称来自实时 LEFT JOIN，改名即时；无删除/合并设备接口（只删 `wp_*`，`writer.rs:345–351`），WhatPulse 导入不触 `input_daily`；时区为 Africa/Dar_es_Salaam 无 DST，day 为本地字符串，键随 `frozen_upto` 变化，时钟回拨/改时区只会换键。改系统时区时 collector 至多 1s 内仍按旧 today 写入，偏差不超过一天，仍落在实时段（today−1..today），不破坏冻结段。 | 同左 | 无；建议在性质测试中加入 `to` 为过去日期、`from==to`、`from>frozen_upto` 三类边界 |
| I9 / 低 | 复位排序复核（无需改）：WTS/POWER 消息由同一窗口线程处理并经同一 `tx` 发送，与 WM_INPUT FIFO 一致（`raw_input.rs:220,516`）；解锁后迟到的 key-up 对已清空状态是无害 no-op，需确认 `Engine` 对未知 up 不计数——已有 `engine_loop.rs:1586–1670` 测试覆盖 reset 后行为，建议补一条“reset 后孤立 key-up 不计数/不报错” | 同左 | 补单测 |
| I10 / 低 | M1 验收缺基线：未在旧版本上先复现“Win+L 后 WIN+A 增加”，无法证明修复有效（若系统实际投递了 key-up，M1 恒通过） | §4.1 M1 | M1 前置：v0.2.0 下同步骤记录 combo 增量；若旧版不复现，H1 降级为防御性改动并在验收中如实记录 |
| I11 / 低 | H4：`Backup::run_to_completion` 期间源被写会重启拷贝，WhatPulse 高频写入时可能长时间不完成；方案“最多数秒”无依据 | SQLite backup 文档：源被其他连接修改时重启 | 加总时限（如 30s）超时即走回退路径；或改单步 `step(-1)`（一次性在一个读事务内完成，不重启） |
| I12 / 低 | collector 退出码 3、脚本退出码 7 已核实空闲（`main.rs:106–220` 用 0/1/2；脚本用 1/5/6，GUI 映射 4=UAC 取消）——方案“确认后定稿”可直接定稿 | 同左 | 改为定稿 |

**自评中需更正的结论**：R7 证据“8 处调用”不准确（见 I3）；“确认无需更改”中 rusqlite `backup` 特性开关的结论成立（Cargo.lock `rusqlite 0.40.2`，特性无额外依赖），但 offline 构建本机无法验证，仍须实施第一步执行。

**阻塞实施的项**：I1（M-E 前必须补进方案）；I3、I4 需在开工前写定（影响 M-A）。其余可在实施中处理。

**对 §10 待决问题的意见**：1 同意采用 perMachine（须补 I1）；2 同意做 GUI 只读提示，但按 I2 收窄判据并包含 Run 项；3 不同意本轮做 H6（I4）；4 同意外部文本列加 `'` 前缀，键名与数值列不加。

## 独立审阅（r1）裁决与 r2 落点

日期：2026-10-12。用户已确认：采用 perMachine；做 GUI 只读迁移提示（按 I2 收窄）；CSV 前缀仅外部来源文本列；H6 移出本轮。方案已修订为 r2。

| 编号 | 裁决 | r2 落点 |
|---|---|---|
| I1 | **采纳** | §4.2.1：README 增“托盘关闭再开启开机自启”步骤；GUI 只读提示同时核对 HKCU Run 项路径（`tauri-plugin-autostart`）；M8 增 `reg query` 检查与关→开后消失的断言 |
| I2 | **采纳** | §4.2.1-2：仅当前 exe 位于 `%ProgramFiles%` 时提示，任务 Exec 位于 `%LOCALAPPDATA%` 才判旧位置；开发/便携/自定义路径仅记日志；纯函数 `legacy_install_hints` 单测 |
| I3 | **采纳**，并更正自评 R7 “8 处调用”的错误证据 | §4.5(c)：`write_csv_file`/`csv_field` 不变；`csv_text_field` 只加前缀、在构造行时调用；逐表列清单；逐表测试；实施时核对各 `label` 列来源 |
| I4 | **采纳** | 删除 §4.6 与 M-A 中的 H6，非目标表新增该项并保留草案摘要 |
| I5 | **采纳** | §1.2-1、§3：windows feature 以编译确定最小集合，预期仅 `Win32_System_RemoteDesktop`，禁止无用 feature，实施记录附编译证据 |
| I6 | **采纳** | §4.1 新增现代待机限制段；M2 记录 `powercfg /a`，S0 机器以锁屏覆盖为通过标准 |
| I7 | **采纳** | §4.3：generation 在 `with_ro` 切换处 bump；`get_overview` 闭包内 `is_fallback_conn`（改 `pub(crate)`）为真时跳过缓存 |
| I8 | **采纳**（建议测试） | §4.3 测试增加 `to` 为过去日期、`from==to`、`from>frozen_upto` 三类边界 |
| I9 | **采纳**（建议测试） | §4.1 测试增加“reset 后孤立 WIN key-up 不计数、不 panic、mods 为 0” |
| I10 | **采纳** | §4.1 前置基线：先在 v0.2.0 执行 M1；不复现则 H1 降为防御性改动并如实记录；§1.1 优先级与 §8 M-D 依赖同步 |
| I11 | **采纳**（选单次 `step(-1)` + 总时限） | §4.4 步骤 2：单次 `step(-1)`，Busy/Locked 200ms 重试，总时限 30s，超时回退文件复制；可注入时限测试 |
| I12 | **采纳** | collector 退出码 3、脚本退出码 7 定稿（注明已核实现用码） |

**拒绝项：无。** r2 已无阻塞实施项；Windows 行为（WTS 接收、现代待机、perMachine 启动令牌、卸载保留数据、Run 项）仍须 M1–M9 真机验收，`--offline` 构建与 windows feature 须实施第一步编译确认。
