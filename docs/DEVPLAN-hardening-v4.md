# CL Recoder 加固与性能修复实施方案（hardening-v4）

日期：2026-10-12。状态：**已经过两轮评审并按裁决修订（修订版 r2）**，评审记录见 `docs/DEVPLAN-hardening-v4-review.md`。用途：交给独立 context 的 Coding Agent 按第 8 节顺序实施。本文只提供设计与验收合同，不提供实现代码。

项目根目录：`C:\Users\17814\Documents\cl recoder`（评审副本在 Linux `/workspace/cl-recoder`）。文中路径相对于根目录，行号以 v0.2.0（10 月 8 日提交）工作树为准，实施前须重新定位。所有 Windows 命令使用 `pwsh`。

**文档优先级**：本次需求及本文 > 当前实现事实 > `DEVPLAN-usability-runtime-v3.md` > `DEVPLAN-correctness-v2.md` > 更早文档。本文**显式推翻** correctness-v2 §1.3-5“不新增 Backup API feature”这一条（见 §4.4），其余旧合同继续有效。

## 1. 整体设计理念

### 1.1 目标与可验证结果

| 编号 | 优先级 | 问题（已核实） | 修复后结果 |
|---|---|---|---|
| H1 / A4 | P1（待 v0.2.0 基线确认，不复现则降为防御性） | Win+L、Ctrl+Alt+Del、UAC 安全桌面、休眠期间的 key-up 不投递到 Raw Input，`Engine.held` 残留修饰位；下次真实按该键前每次按键多记组合 | 锁屏/解锁/挂起/恢复时清空键盘按下状态；设备身份、鼠标/运动来源不受影响 |
| H2 / A1–A3 | P2 | NSIS 默认 per-user 安装到 `%LOCALAPPDATA%\CL Recoder`，`/RL HIGHEST` 登录任务运行用户可写目录中的 exe；`find_script` 搜索 CWD；`-RepairOnly` 不核对任务实际执行目标 | 安装版进入 Program Files；脚本拒绝为用户可写 exe 建/修任务；发布版不搜 CWD；修复核对 Exec |
| H3 / P1 | P2（性能） | 概览轮询（500ms）每次全范围扫描：实测 1 年约 23ms、全部约 49ms | 冻结区按天缓存，轮询仅查“昨天+今天”，稳态 <1ms；数值与无缓存实现逐项相等 |
| H4 / A9 | P3 | WhatPulse 导入以文件复制获取主库与 WAL，非原子，可能得到撕裂副本 | 以 SQLite Online Backup API 取一致快照；失败时退回现有复制路径 |
| H5 | P3 | 导出各文件非同一快照；旧程序打开新 schema 不报错；CSV 公式注入 | 导出单读事务；schema 过新硬拒绝且有明确提示；外部来源文本单元格加 `'` 前缀 |

### 1.2 设计原则

1. 不改变两进程架构、IPC 线协议、SQLite schema（schema 仍为 v3），不新增 crate 依赖；仅允许为既有依赖开启 feature：`rusqlite/backup`，以及 `windows` 中**以实际编译确定的最小集合**（预期仅新增 `Win32_System_RemoteDesktop`；电源通知相关符号预期位于已启用的 `Win32_UI_WindowsAndMessaging`，若编译证明需要 `Win32_System_Power` 才可加入）。禁止加入未被引用的 feature，实施记录须写明最终集合及编译证据（审阅 I5）。
2. 用户可见数值不得因性能优化而改变；任何缓存必须有“与全量查询结果相等”的回归测试。
3. 安全加固不得静默破坏已安装用户：迁移路径必须在 README 与 GUI 提示中可执行。
4. 纯逻辑一律可在 Linux `cargo test` 运行；Win32 行为只在 Windows 手工验收中声明，不以单测冒充。

### 1.3 非目标（已裁定不修）

| 项 | 不修理由 |
|---|---|
| 管道每连接一线程 / 无读超时 / DACL 完全控制 | 需同用户恶意进程，攻击者已可直接结束 GUI；收益低 |
| WM_ENDSESSION 尾部 ≤0.5s 丢失 | 量级可忽略，引入关机竞态风险 |
| 坏运动行隔离（整批回滚） | 入口已过滤，未发现可触发路径 |
| 覆盖索引 / `WITHOUT ROWID` | 实测分别 +15% / −80%；H3 后更无必要 |
| 只读连接池、流式导出 | 查询均在几十毫秒内；H3 后锁争用进一步下降 |
| 表格虚拟化、路由懒加载 | 收益小于维护成本 |
| 调整 flush 间隔到 2–5s | 今日数字跳变，损害体验 |
| 柱状图动画、`opener` 权限、隐藏时停止轮询 | 误报或已实现 |
| 午夜 ≤1s 归前一天、WAL 不收缩 | 轻微，不在本轮 |
| 日志轮转失败后永久停写（原 H6） | 不在用户批准的清单内（审阅 I4），列为后续候选（r1 草案：失败后 30s 起指数退避重试轮转，不突破 256 KiB 上限）|

## 2. 模块与职责

| 模块 | 本次职责 | 明确禁止 |
|---|---|---|
| collector `raw_input` | 在现有 message-only 窗口上注册会话/电源通知，在窗口过程中经同一 `tx` 发 `KeyboardSourcesReset` | 新线程、新窗口类注册、修改来源 ID 分配 |
| core / engine | 不改（复用 `AggEvent::KeyboardSourcesReset` 与 `Engine::clear_sources`） | 新增事件变体 |
| GUI `commands::overview` + 新 `overview_cache` | 冻结区缓存、合并、失效 | 改 `store::reader::overview` 的 SQL 语义或 DTO 线形状 |
| store `reader` | 新增 `overview_parts(conn, from, to)` 分段查询（复用既有 SQL） | 修改既有函数签名 |
| store `schema` | `migrate` 拒绝过新版本；新增 `check_schema_not_newer`（只读） | 修改迁移 SQL |
| GUI `commands::import` | 以 backup API 生成副本 | 写源库、改校验/替换合同 |
| GUI `commands::export` | 读事务包裹、CSV 前缀 | 改列名/JSON 版本 |
| GUI `commands::collector_ctl` | `find_script` 收窄；新退出码 7 文案 | 改 runas/Wait 合同 |
| scripts `install-collector-task.ps1` | ACL 检查、Repair 时 Exec 核对 | 结束进程、改三项策略 |
| `tauri.conf.json`、README | perMachine 与迁移说明 | 改 identifier / productName |

## 3. 文件级清单

| 文件 | 操作 | 对应 |
|---|---|---|
| `Cargo.toml` | `windows` 增编译所需最小 feature（预期 `Win32_System_RemoteDesktop`）；`rusqlite` 增 `backup` | H1/H4 |
| `crates/collector/src/raw_input.rs` | 通知注册/注销、窗口过程分支、纯函数 `session_event_resets_keyboard` 及测试 | H1 |
| `crates/store/src/schema.rs` | 过新版本检查 + 新错误变体使用 | H5 |
| `crates/store/src/lib.rs` | `StoreError::SchemaTooNew { found, supported }` | H5 |
| `crates/store/src/reader.rs` | `overview_parts` + 测试 | H3 |
| `src-tauri/src/commands/overview.rs`、新 `src-tauri/src/overview_cache.rs`、`state.rs` | 缓存及失效 | H3 |
| `src-tauri/src/commands/import.rs` | backup 快照 + 回退 + 导入后缓存失效 | H4/H3 |
| `src-tauri/src/commands/export.rs` | 读事务、`csv_text_field` | H5 |
| `src-tauri/src/db.rs`、`commands/collector_health.rs`（或现有状态命令） | GUI 侧 schema 过新检测与提示 | H5 |
| `crates/collector/src/main.rs` | `Writer::open` 失败为 SchemaTooNew 时专用诊断码与退出码 | H5 |
| `src-tauri/src/commands/collector_ctl.rs` | `find_script` cfg 收窄；`Some(7)` 文案；旧安装只读提示（任务 Exec + HKCU Run）；测试 | H2 |
| `scripts/install-collector-task.ps1`、`scripts/tests/*` | ACL、Exec 核对 | H2 |
| `src-tauri/tauri.conf.json`、`README.md` | perMachine、迁移步骤 | H2 |

## 4. 详细设计

### 4.1 H1 锁屏/安全桌面/睡眠后的按键残留

**前置基线（审阅 I10，必须先做）**：在 v0.2.0 上执行 M1 步骤并记录当天 `combo_daily` 中 `WIN+A/B/C` 增量。若能复现（增量 >0），H1 按 P1 缺陷修复；若不能复现（系统实际投递了 key-up），H1 降级为**防御性改动**，仍可合入但验收文档必须如实写“旧版未复现，本改动为防御性”，不得宣称修复了已观察缺陷。

**根因（推断，待基线确认）**：`Engine::clear_sources`（`crates/engine/src/lib.rs:133`）只在 raw_input loop 启动时被 `KeyboardSourcesReset` 调用（`raw_input.rs:220`，消费于 `engine_loop.rs:393`）。安全桌面期间 key-up 不进入用户桌面的 Raw Input，`held` 中 `(source, 0xE05B)` 残留，`mods_held` 带 WIN 位。

**事件选择**：复用 `AggEvent::KeyboardSourcesReset`。它只清 Engine 的 held/mods，不清 raw_input 侧来源表、鼠标累计、运动来源、`DeviceResolver` 缓存，正符合“不丢设备身份”。不新增变体，因此 core/engine/selftest 不改。

**通知注册**（均在 `message_loop` 创建窗口并 `register_devices` 成功之后、`SetTimer` 附近）：

1. `WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION)`。该 API 只要求窗口句柄，消息直接发往该窗口，message-only 窗口可接收（与广播无关）。**登录早期 WTS 服务可能未就绪**（返回 `RPC_S_INVALID_BINDING`）：失败不致命，记限频 warn，并在现有 25ms `WM_TIMER` 分支内以 ≥5s 间隔重试，最多 60 次；成功后停止重试。
2. 电源：message-only 窗口**不接收**顶层广播的 `WM_POWERBROADCAST`。使用 `RegisterSuspendResumeNotification(HANDLE(hwnd), DEVICE_NOTIFY_WINDOW_HANDLE)`（Windows 8+），系统将 `WM_POWERBROADCAST` 直接发送到该窗口。返回的 `HPOWERNOTIFY` 保存在 `RawInputState`。
3. 不创建隐藏顶层窗口：上述两者已覆盖；若第 9 节手工验收证明 message-only 窗口收不到 WTS 消息，回退方案为把同一 STATIC 子类化窗口改为不可见顶层窗口（`WS_POPUP`、无 `WS_VISIBLE`、`WS_EX_TOOLWINDOW`），其余不变——该回退需单独评审，不在本合同默认实现内。

**注销**：`WindowGuard::drop`（DestroyWindow 之前）按逆序调用 `UnregisterSuspendResumeNotification` 与 `WTSUnRegisterSessionNotification`；句柄为空则跳过。loop 1s 重建时新窗口重新注册，旧注册随旧窗口注销，避免泄漏/重复投递。

**窗口过程分支**（`raw_input_wndproc`，`raw_input.rs:324` 起，与现有分支同样 `catch_unwind`）：

| 消息 | wParam | 动作 |
|---|---|---|
| `WM_WTSSESSION_CHANGE` | `WTS_SESSION_LOCK`、`WTS_SESSION_UNLOCK`、`WTS_CONSOLE_CONNECT`、`WTS_REMOTE_CONNECT` | 发 `KeyboardSourcesReset` |
| `WM_POWERBROADCAST` | `PBT_APMSUSPEND`、`PBT_APMRESUMEAUTOMATIC`、`PBT_APMRESUMESUSPEND` | 发 `KeyboardSourcesReset`；返回 `TRUE` |
| 其他 wParam | — | 不动作 |

判定写成纯函数 `fn session_event_resets_keyboard(msg: u32, wparam: usize) -> bool`，Linux 可单测。复位经窗口线程的同一 `tx` 发送，因此与 WM_INPUT 的 FIFO 顺序天然一致：解锁前排队的输入先于复位处理，解锁后输入在复位之后。

UAC 安全桌面与 Ctrl+Alt+Del **不产生** WTS 锁定通知（Ctrl+Alt+Del 进入 Winlogon 桌面时若用户选择“锁定”才会产生）。这两种情况不在可靠通知覆盖内，补救为：**修饰键 key-up 缺失的兜底**——在 `on_wm_input` 处理键盘 down 时不做额外推断（避免误伤正常按住）。接受该残留（下一次按下并释放该修饰键即自愈），在 README“已知限制”中说明。理由：用 `GetAsyncKeyState` 交叉校验需在热路径调用 Win32 且对 INPUTSINK 场景语义不确定，风险高于收益。

**现代待机限制（审阅 I6）**：S0 Modern Standby 机器进入待机通常**不发送** `PBT_APMSUSPEND`（恢复时也可能只有 `PBT_APMRESUMEAUTOMATIC` 或都没有）。多数情况下进入待机伴随锁屏，由 WTS LOCK/UNLOCK 覆盖；“禁用唤醒时需要登录”的现代待机机器可能两者都不触发，残留至下次按键自愈，记入 README 已知限制。

**副作用**：解锁时用户若正物理按住某键，复位后自动重复的 down 会被计为一次新按下（每次解锁至多一次，可忽略）。暂停态下复位同样执行（暂停只影响计数，不影响状态维护，与 correctness-v2 §5.2 一致）。

**测试**：
- Linux 单测：`session_event_resets_keyboard` 真值表（全部列举的 wParam 为 true，`WTS_SESSION_LOGON`/`PBT_POWERSETTINGCHANGE`/任意未知值为 false）。
- Linux 单测（engine_loop，已有基础设施 `engine_loop.rs:1586` 附近）：WIN down → `KeyboardSourcesReset` → A down/up 不产生组合；随后 WIN down/up 计 1 次。
- Linux 单测（审阅 I9）：`KeyboardSourcesReset` 之后到达的孤立 WIN key-up 不产生任何计数、不 panic、`mods_held` 保持 0；随后 A down 不产生组合。
- Windows 手工验收 M1（先在 v0.2.0 执行作为基线，再在新版执行）：开启采集器，按 Win+L 锁屏、解锁，随后键入 `abc` 20 次；`combo_daily` 当天不得新增 `WIN+A/B/C`；日志出现 `raw_input.session_reset`（新 Info 代码，限频）。M2：睡眠/唤醒后同样检查；记录 `powercfg /a` 输出，区分 S3 与 S0 现代待机，S0 机器只以“锁屏覆盖”为通过标准，日志无 `PBT_APMSUSPEND` 不算失败。M3：重启 explorer 不影响；M4：登录后 1 分钟内锁屏，确认 WTS 注册重试已成功（日志）。

**风险/回滚**：通知注册失败只降级为当前行为。回滚即删除注册与两条分支，无数据迁移。

### 4.2 H2 提权任务指向用户可写路径

#### 4.2.1 安装版：`perMachine`

`src-tauri/tauri.conf.json` 的 `bundle` 增加：

```json
"windows": { "nsis": { "installMode": "perMachine" } }
```

结果：安装到 `%ProgramFiles%\CL Recoder\`，安装时需要一次 UAC；GUI 本身仍以普通权限运行（Tauri 安装器结束时“运行程序”以原用户令牌启动，需手工验收确认，见 M7）。数据仍在 `%LOCALAPPDATA%\ClRecoder\`，不受影响。

**既有 per-user 安装的迁移**（Tauri NSIS 不会跨 installMode 自动卸载旧安装，两份共存）：
1. README“数据与升级”新增 v0.3.0 专节：先在旧 GUI 停用采集器自启 → 运行旧目录 `scripts\stop-collector.ps1` → 从“应用和功能”卸载旧版（per-user 卸载不删数据目录，需验收确认 M8）→ 安装新版 → 在新 GUI 启用自启。
   README 同时补：卸载旧版后，在新 GUI 托盘把“开机自启”**关闭再开启**一次，以重写 HKCU Run 项（审阅 I1）。
2. GUI 启动时的**只读**迁移提示（不自动提权、不自动修改）。仅当**当前 GUI 为安装版**（`current_exe` 位于 `%ProgramFiles%` 下）时检测，其余布局（开发 `target\debug`/`release`、便携版、`-CollectorPath` 自定义）只记日志不提示（审阅 I2）：
   - 采集器自启：计划任务存在且 Exec 路径位于 `%LOCALAPPDATA%` 下 → 设置页提示“采集器自启任务仍指向旧的按用户安装位置，请先停用再重新启用”。
   - GUI 自启：`tauri-plugin-autostart`（`src-tauri/Cargo.toml:13`，托盘开关 `main.rs:110–120`）写入的 HKCU `Software\Microsoft\Windows\CurrentVersion\Run` 项存在且其路径不等于当前 exe → 提示“开机自启指向旧位置，请在托盘关闭再开启开机自启”（审阅 I1）。读取用 `windows` 既有 `Win32_System_Registry` feature 只读打开 HKCU，值名以插件实际写入名为准（实施时读插件源码确认）。
   - 判定写成纯函数 `fn legacy_install_hints(current_exe, task_exec: Option<&str>, run_value: Option<&str>, program_files, local_appdata) -> Vec<Hint>`，Linux 单测覆盖安装版/开发/便携/无任务/无 Run 项/路径大小写与引号。

#### 4.2.2 脚本 ACL 检查（安装版与便携版通用）

在 `install-collector-task.ps1` 创建路径（`:144–170`）定位 `$exe` 之后、`schtasks /Create` 之前，以及已有任务分支（§5 校验后）对任务 Exec 路径，执行 `Test-CollectorPathTrusted`：

- 检查对象：exe 文件本身、其所在目录、以及逐级父目录直到卷根。
- 不可信判据：任一对象的 DACL 中，对 `S-1-5-32-545`（Users）、`S-1-5-11`（Authenticated Users）、`S-1-1-0`（Everyone）、`S-1-5-4`（INTERACTIVE）、或 **`-ExpectedUserSid`（当前登录用户本人）** 存在 Allow ACE 含 `WriteData/AppendData/WriteAttributes/WriteExtendedAttributes/Delete/DeleteSubdirectoriesAndFiles（仅目录）/ChangePermissions/TakeOwnership/GenericWrite/GenericAll` 之一；或对象 Owner 为上述 SID。对父目录只看“可在其中创建/重命名子项”的权限（`AppendData`=CreateDirectories、`WriteData`=CreateFiles、`DeleteSubdirectoriesAndFiles`），因为父目录可写才能替换子目录。卷根 `C:\` 的 Authenticated Users“创建文件夹”默认 ACE 只有 `InheritOnly`（CI）不作用于已存在子目录，判定时**忽略仅继承到新建子项的 ACE（InheritOnly）**。
- 判定失败：只读退出 **exit 7**（已核实脚本现用 1/5/6，GUI 另以 4 映射 UAC 取消，7 空闲，定稿，审阅 I12），`Write-Error` 列出首个不可信对象与 SID；不修改任何任务。
- 开发布局（`target\release`）同样受检；开发者可用新参数 `-AllowUserWritable`（仅在手动命令行传入；GUI 永不传）跳过，脚本输出醒目警告。
- 便携版用户的建议路径写入 README：放到 `C:\Program Files\CL Recoder Portable\`（需管理员复制）。

GUI `collector_ctl.rs:480` 附近的退出码映射新增 `Some(7)`：“采集器所在目录普通用户可写，以最高权限自启存在被替换风险，已拒绝创建/修复。请使用安装版或将程序放到仅管理员可写的目录（详见 README）。”

#### 4.2.3 `-RepairOnly` 与已有任务的 Exec 核对

在 §5 定义兼容性校验（`install-collector-task.ps1:186–241`）之后追加：
- 读取 `Actions`：必须恰好一个 `Exec`；`Command` 去除首尾引号、`[Environment]::ExpandEnvironmentVariables` 后，与“期望路径”做 `[IO.Path]::GetFullPath` + `OrdinalIgnoreCase` 比较；`Arguments` 必须缺失或为空白；`WorkingDirectory` 不检查。
- 期望路径：`-RepairOnly` 与非 Repair 共用 4 节的定位顺序（`-CollectorPath` > 脚本目录 `..\cl-recoder-collector.exe` > 开发布局）；RepairOnly 下找不到期望 exe → exit 1（原有语义的扩展，文案写明）。
- 不符 → exit 5（沿用“冲突，只读未修改”文案），`Write-Error` 同时打印实际与期望路径。
- Exec 路径通过后再执行 4.2.2 的 ACL 检查（exit 7）。

#### 4.2.4 `find_script` 收窄

`collector_ctl.rs:503–518`：CWD 与 `CWD\..` 两个候选包入 `if cfg!(debug_assertions)`；发布版仅 `current_exe` 同目录 `scripts\` 与同目录。用 `cfg!` 而非 `#[cfg]`，保持两种构建都编译该代码，避免 dead-code 分支。

**测试**：
- Rust 单测：抽出 `fn script_candidates(exe_dir: Option<&Path>, cwd: Option<&Path>, include_cwd: bool) -> Vec<PathBuf>`，验证 `include_cwd=false` 时不含 cwd 候选；`find_script` 以 `cfg!(debug_assertions)` 传入。退出码 7 文案映射单测。
- Pester/pwsh 脚本测试（`scripts/tests/`，Windows 上运行）：把 ACL 判定拆成纯函数 `Get-UntrustedWriteAce -Sddl <string> -ExpectedUserSid <sid>`，用 SDDL 字符串夹具覆盖：Program Files 默认 ACL → 可信；`%LOCALAPPDATA%` 典型 ACL（用户 FullControl）→ 不可信；仅 InheritOnly 的 AU 创建文件夹 → 可信；Everyone Modify → 不可信。Exec 比较纯函数：带引号/不带引号/大小写/环境变量/有参数。
- Windows 手工验收：M5 per-user 旧版 + 新脚本启用 → exit 7 提示；M6 perMachine 安装后启用/修复成功，`schtasks /Query /XML` Exec 为 Program Files 路径；M7 安装结束“运行”后 GUI 进程非提权（任务管理器“已提升”列为否）；M8 卸载旧 per-user 版后 `%LOCALAPPDATA%\ClRecoder\stats.db` 仍在，并检查 `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run`：残留旧路径时新 GUI 出现提示，托盘关→开后该值变为 Program Files 路径、提示消失；同时检查计划任务提示按 I2 只在安装版出现；M9 手工把任务 Exec 改为 `notepad.exe` 后点“修复” → 冲突提示且任务未改。

**风险**：perMachine 强制安装时 UAC，并使静默 per-user 安装不可用（需用户决策，见 §10）；ACL 检查对域环境自定义 ACL 可能误拒——提供 `-AllowUserWritable` 手动逃生口。**回滚**：删除 `installMode` 恢复 per-user；脚本检查可单独回退。

### 4.3 H3 概览历史缓存

**位置选择：GUI Rust 侧**（`src-tauri`），不在前端。理由：前端 React Query 以 `(from,to)` 为键整体缓存 DTO，若在前端拆分需要两条查询并在组件内合并，且 `devices[].total` 的合并依赖设备 id 集合，前端实现更易出错；Rust 侧可直接以单元测试对比“缓存结果 == 全量 `reader::overview`”。

**冻结边界**：`frozen_upto = today_local − 2 天`。说明：collector 跨日时最后一个 0.5s 周期仍可能写“昨天”，且 flush 失败时聚合桶保留并重试（`engine_loop.rs:20,35`），写入“昨天”的延迟不可预知；把“昨天”保留在实时段，代价仅多扫一天。早于昨天的日期在正常运行中不会再被写入。

**分段查询**：store 新增
```rust
pub struct OverviewParts {
    pub days: Vec<DayCount>,             // [from, to] 内逐日（同 daily_totals）
    pub device_totals: Vec<(i64, u64)>,  // 按 device_id 的 [from, to] SUM（仅 count>0 的设备）
}
pub fn overview_parts(conn: &Connection, from: &str, to: &str) -> Result<OverviewParts>;
```
`get_overview(from, to)` 流程：
1. 若 `to <= frozen_upto` 或 `from > frozen_upto`：直接调用原 `reader::overview`（全历史范围不轮询，前端已不轮询；纯实时范围本身很小）。
2. 否则冻结段 `[from, frozen_upto]` 查缓存，未命中则 `overview_parts` 计算并存入；实时段 `[frozen_upto+1, to]` 用原 `reader::overview(conn, live_from, to)` 计算（得到 today 拆分、实时 days、设备列表及实时段 totals、设备名称/昵称为最新值）。
3. 合并：`days = frozen.days ++ live.days`（两段不重叠且均升序）；`devices[i].total = live.total + frozen_totals.get(id).unwrap_or(0)`；设备列表以实时查询为准（新增设备、昵称修改即时生效）；`today` 取实时结果。

**缓存结构**（`src-tauri/src/overview_cache.rs`，`AppState` 新增 `Arc<Mutex<OverviewCache>>`，**独立于 `ro_conn` 锁**，计算期间持有 ro 锁、只在读写缓存时短暂持缓存锁）：
- 键 `(from: String, frozen_upto: String, generation: u64)`；最多 4 个条目（不同起点：本周、本月、一年、全部），超过按 LRU 淘汰。每条目内存约为天数×(10B+8B)+设备数×16B，“全部”10 年约 70KB，上限 <0.5MB。
- `generation`：在以下时刻 `+1`（等价清空）：
  - ro 连接从内存兜底切换到真实库：由 `AppState::with_ro`（`state.rs:124–137`）在执行 `*guard = real` 的同一处 bump generation（`AppState` 持有缓存的 `Arc`），缓存路径不需要感知切换（审阅 I7）；
  - WhatPulse 导入完成（不影响 `input_daily`，但为安全统一清空，成本一次全扫）；
  - schema 过新检测改变状态（H5）；
- `frozen_upto` 由每次请求时的本地日期计算，跨午夜自然换键，旧条目由 LRU 淘汰；系统时间回拨时键改变同样正确。
- 安全网：每条目记录创建 `Instant`，**超过 10 分钟强制重算**，以覆盖“flush 失败重试持续超过一天”等极端情形。
- **只缓存成功结果**：`overview_parts` 返回 `Err` 时不写缓存，错误照常上抛（现有 `swallow` 映射为空数据的行为不变）。兜底内存连接上的结果不缓存：`get_overview` 的闭包内先调用 `AppState::is_fallback_conn(conn)`（改为 `pub(crate)`），为真时直接走原 `query_overview`、不读不写缓存（审阅 I7）。
- DPI 变化：概览只含计数，不含距离，无需失效。

**一致性论证**：在“冻结段数据不变”的前提下合并结果与单次全量查询逐项相等；`days` 排序、零输入设备 total=0（实时 LEFT JOIN 保证在场）与原语义一致。设备删除接口当前不存在；若未来加入需同步 `generation += 1`（写入 §9 约束）。

**测试**（Linux 可跑）：
- 性质测试：随机种子生成 3 设备 × 400 天数据，对多组 `(from,to,today)` 断言 `cached_overview == reader::overview`（今天参数注入为纯函数入参，不读 `Local::now`）。
- 边界（审阅 I8）：`to` 为过去日期（≤frozen_upto，走原路径）、`from == to`、`from > frozen_upto`（纯实时）三类各至少一例，与全量结果相等。
- 失效：写入冻结段新行后，未递增 generation 时结果为旧值（证明缓存生效）、递增后相等；10 分钟 TTL 用可注入时钟验证。
- 错误不缓存：首次查询注入失败连接，再次查询能得到正确值。
- 设备新增/昵称修改在冻结命中时即时可见。
- 基准（`#[ignore]`，手动）：两年 5 设备 16 万行，稳态单次 <1ms（不含首次填充）。

**风险**：首次进入“全部”范围仍为一次全扫（~49ms），之后每 10 分钟一次；可接受。**回滚**：`get_overview` 直接调用原 `query_overview`，删除缓存模块即可。

### 4.4 H4 WhatPulse 导入改用 Online Backup

**依赖**：`rusqlite 0.40.2`（Cargo.lock）的 `backup` 为纯 Rust 特性开关，`bundled` 编译的 SQLite 自带 `sqlite3_backup_*`，**不引入新 crate**，lockfile 不变。实施时第一步执行 `cargo build --offline -p cl-recoder`（或项目实际包名）确认无需联网。该变更**推翻** correctness-v2 §1.3-5，理由：文件复制在 WhatPulse 写入期间可能产生主库/WAL 不一致的副本，而 backup API 在源连接的读事务内取一致快照。

**流程**（替换 `import.rs:540–566` 的复制段，其余校验/读取/替换不变）：
1. 以 `OpenFlags::SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX | SQLITE_OPEN_URI` 打开源，URI 形式 `file:<path>?mode=ro`；`busy_timeout(5s)`。
2. `Connection::open(tmp)` 作为目标，`Backup::new(&src, &mut dst)?` 后调用**单次 `step(-1)`**（审阅 I11）：一次性在一个读事务内复制全部页，期间源的其他连接写入不会导致重启；`Busy`/`Locked` 时以 200ms 间隔重试，**总时限 30s**，超时或其他错误进入步骤 4 回退。不使用 `run_to_completion`（分步复制在源被并发写时会反复重启，完成时间无界）。
3. 关闭源连接，后续沿用 `import_from_copy(&tmp, …)`。
4. **回退**：步骤 1–2 任一失败（典型：源为 WAL 且 `-shm` 不存在、所在目录只读导致只读连接无法建 shm，或源被独占锁）→ 记 warning“一致快照不可用，退回文件复制”，执行原复制逻辑。这样不会比现状更差。
5. 不复制源 `-wal`（backup 已包含已提交 WAL 内容）；仅回退路径保持原 best-effort 复制。

**源只读保证**：`mode=ro` 不写主库；但打开 WAL 库时 SQLite 可能**创建或更新 `-shm`**（与 WhatPulse 自身行为相同，不改变数据）。README 导入说明补充一句。不使用 `immutable=1`（会忽略 WAL，导致更旧快照）。

**测试**：现有 `import_reads_wal_only_rows`（`import.rs:1150`）必须继续通过；新增：源连接持有未提交写事务时导入成功并得到提交前快照；源路径为目录/不存在时回退并按原合同失败；backup 失败注入（不可读文件）→ 回退复制路径仍按原校验合同返回；总时限用可注入时限（测试设 0）验证超时走回退。

**回滚**：删除 backup 分支，恢复调用复制函数；feature 可保留。

### 4.5 H5 小修

**(a) 导出单快照**：`run_export`（`export.rs:793`）在分派 CSV/JSON 前执行 `let tx = conn.unchecked_transaction()?;`（`BEGIN DEFERRED`），全部查询在其上运行，结束时 `tx.commit()`（只读事务提交即释放快照）；出错 drop 自动回滚。WAL 下只读事务首次读取时固定快照，collector 的写入不受阻塞（仅推迟 checkpoint 越过该快照，导出耗时为秒级以内）。`export_csv/export_json` 签名接受 `&Connection`，`Transaction` 可 `Deref`，签名不变。注意：GUI ro 连接本身为 `SQLITE_OPEN_READ_ONLY`，`BEGIN` 合法。
测试：导出过程中在另一连接写入（在 CSV 两个文件之间插入测试钩子不现实，改为：开启事务→外部写入→在同事务内两次查询结果一致的 store 级单测），并保留现有导出测试全绿。

**(b) schema 过新拒绝**：
- `schema.rs:157` 循环前：`if current > SCHEMA_VERSION { return Err(StoreError::SchemaTooNew{found: current, supported: SCHEMA_VERSION}) }`。替换现有 `_ => WalUnavailable("未知 schema 版本")` 的误导性错误类型（该分支保留为不可达防御）。
- 新增 `pub fn check_schema_not_newer(conn) -> Result<()>`：只读查询 `schema_migrations`；表不存在视为 0（新库）。
- collector：`main.rs:215` `Writer::open` 失败且为 `SchemaTooNew` 时写诊断 `store.schema_too_new`（Error）并以专用退出码 **3** 退出（已核实 `main.rs:106–220` 现用 0/1/2，定稿，审阅 I12），不进入重试循环。
- GUI：`state.rs` 打开 ro 连接成功后调用检查；过新则继续以该连接只读展示（读旧列通常仍可用），但通过现有健康/状态命令返回 `schemaTooNew: {found, supported}`，UI 顶部显示红色横幅“数据库由更新版本创建（vN），当前程序支持 vM，请升级程序；采集器已拒绝写入”。导入（`open_rw`→migrate）自然被拒并显示同一错误。
- 测试：store 单测在迁移后插入 version=99 行，`migrate` 返回 `SchemaTooNew{99,3}` 且不修改任何表；`check_schema_not_newer` 对新库/v3/v99 分别 Ok/Ok/Err。

**(c) CSV 公式注入**（审阅 I3 定稿）：
- 现状：`write_csv_file(path, header, rows: Vec<Vec<String>>)`（`export.rs:127–140`）对表头与每个单元格统一调用 `csv_field`（`export.rs:118`，RFC 4180 转义）。**`write_csv_file` 与 `csv_field` 保持不变**。
- 新增 `pub(crate) fn csv_text_field(s: &str) -> String`：首字符属于 `= + - @ \t \r` 时返回 `'` + 原串，否则原样返回；**只做前缀，不做转义**（转义仍由 `write_csv_file` 统一完成，避免双重转义）。
- 在各导出分支**构造行向量时**，仅对下表“加前缀列”调用 `csv_text_field`：

| 文件 | 列 | 加前缀列 | 不加（理由） |
|---|---|---|---|
| `devices.csv` | id,kind,vid,pid,name,firstSeen,lastSeen,total | `name`（含昵称） | 其余为数值/枚举/时间 |
| `keys_*.csv` | date,key,code,count | 无 | `key` 为内部键名表 |
| `apps_*.csv` | date,exe,name,seconds,keys,clicks | `exe`、`name` | 数值 |
| `combos_*.csv` | date,mods,code,label,count | 无 | `label` 为内部组合标签 |
| `mouse_sources.csv` | sourceId,deviceId,name,manualDpi | `name` | 数值 |
| 运动/手柄 4 表 | sourceId/deviceId,day,… | 无 | 全为数值/枚举 |
| `wp_keys_*.csv` | day,label,count | 无 | 键名 |
| `wp_combos_*.csv` | day,combo,label,count | 无 | 组合键名 |
| `wp_apps_*.csv` | day,name,seconds,keys,clicks | `name` | WhatPulse 外部文本 |
| `wp_mouse_*.csv` | day,clicks,distanceMeters | 无 | 数值 |
| `wp_mouse_buttons/scrolls_*.csv` | label,total | 无 | 按钮/方向标签 |

  实施时须逐表核对 `label` 列来源：若某 `label` 实际直接取自 WhatPulse 源库自由文本而非固定映射，则改列入“加前缀”，并在实施记录中说明。JSON 导出不加前缀。
- 测试（逐表）：每个含前缀列的表构造一行 `=cmd|' /C calc'!A0`、`@x`、`\tx`，断言输出单元格为 `'=cmd|' /C calc'!A0`（含逗号/引号时再经 RFC4180 加引号）；`keys_*`/`combos_*`/`wp_keys_*` 中键名 `-`、`=`、`+` 原样输出；`=a,b` → `"'=a,b"`。

## 5. 核心流程摘要

1. 锁屏：WTS LOCK → 窗口过程 → `tx.send(KeyboardSourcesReset)` → aggregator `clear_sources`；UNLOCK 再次复位。
2. 自启：GUI → runas 脚本 → 定位 exe → Exec 核对（已有任务）→ ACL 检查 → 创建/更新 → 回读。
3. 概览：请求 → 计算 frozen_upto → 缓存命中/填充 → 实时段查询 → 合并。
4. 导入：backup 快照 →（失败回退复制）→ 原校验与原子替换 → generation+1。

## 6. 数据与状态

无 schema 变更、无持久化格式变更（导出 JSON 版本不变；CSV 仅外部文本列可能多一个前缀 `'`，记入 CHANGELOG/README）。新增内存状态：OverviewCache（<0.5MB）、raw_input 的两个通知句柄。

## 7. 兼容性

- 旧 per-user 安装：见 4.2.1 迁移；未迁移的用户其任务仍可运行旧 exe（不被新脚本破坏），但新 GUI 的修复会提示冲突。
- Windows 7 不支持 `RegisterSuspendResumeNotification`：项目依赖 WebView2/Tauri v2 已要求 Windows 10+，无影响。
- 前端无需改动（H5b 横幅除外）。

## 8. 里程碑与顺序

| 里程碑 | 内容 | 依赖 | 关卡 |
|---|---|---|---|
| M-A | H5(a)(b)(c) | 无 | `cargo test --workspace`、`cargo clippy -D warnings`（Linux 可跑部分 + Windows 全量） |
| M-B | H3 概览缓存 | M-A 的 `SchemaTooNew` 失效钩子 | 性质测试、基准 <1ms |
| M-C | H4 backup | 无（与 M-B 并行，`import.rs` 与 `overview` 仅在 generation 调用处交汇，后合入者负责） | 导入测试全绿、`--offline` 构建 |
| M-D | H1 会话/电源复位 | v0.2.0 基线 M1 结果（决定 P1 或防御性） | 单测 + Windows M1–M4 |
| M-E | H2 perMachine/ACL/Repair/find_script/旧安装只读提示 | 无（用户已决策） | Pester 测试 + Windows M5–M9 |
| M-F | 发布 v0.3.0：README 迁移说明、CHANGELOG | 全部 | 前端 build、`tauri build` 产物在干净 VM 安装验证 |

每个里程碑单独提交、单独 PR，可独立回滚。

## 9. 给执行 Agent 的约束

1. 不得修改 schema、IPC 协议、导出 JSON 版本；不得新增 crate。
2. 任何新增写 `input_daily` 历史日期的功能（删除/合并设备、历史编辑）必须同步 `OverviewCache` generation。
3. Win32 新调用必须带 `// SAFETY:` 说明并在 `catch_unwind` 保护的分支内。
4. 手工验收结果写入 `docs/ACCEPTANCE-hardening-v4.md`；未执行的项必须标“未验收”，不得写成通过。
5. 本文档评审通过不代表实现通过任何关卡。

## 10. 用户决策记录（2026-10-12）

1. 采用 `perMachine`：已确认。
2. GUI 只读迁移提示：已确认，按 §4.2.1-2 收窄判据，包含 HKCU Run 项。
3. CSV 前缀：已确认，仅外部来源文本列，列清单见 §4.5(c)。
4. 日志轮转重试（原 H6）：移出本轮，列入非目标。

剩余待定：无阻塞项。若 M1 基线不能复现，是否仍合入 H1（建议合入，作为防御性改动）由实施后验收结论报告给用户。

## 修订记录

- r0（2026-10-12）：初稿。
- r1（2026-10-12）：按评审 R1–R10 修订：冻结边界改为 today−2 并加 TTL（R1）；错误/兜底结果不缓存与 generation（R2）；独立缓存锁（R3）；WTS 登录早期重试与注销（R4）；UAC/Ctrl+Alt+Del 限制如实声明（R5）；ACL 判据纳入当前用户 SID 与 InheritOnly 处理、新增 exit 7（R6）；CSV 前缀限定外部文本列（R7）；显式推翻 correctness-v2 Backup 约束及回退路径（R8）；perMachine 与 ACL 检查的耦合决策（R9）；schema 过新错误类型与 collector 不重试（R10）。
- r2（2026-10-12）：按独立审阅 I1–I12 与用户决策修订：HKCU Run 迁移提示与 M8（I1）；提示判据收窄到安装版（I2）；`csv_text_field` 构造期预处理与逐表列清单（I3）；移除 H6（I4）；windows feature 以编译确定（I5）；现代待机限制与 `powercfg /a`（I6）；generation 在 `with_ro` 内 bump、闭包内 `is_fallback_conn` 跳过（I7）；缓存边界测试（I8）；reset 后孤立 key-up 测试（I9）；M1 先做 v0.2.0 基线、不复现则降级为防御性（I10）；backup 改单次 `step(-1)` + 30s 总时限回退（I11）；退出码 3/7 定稿（I12）。
