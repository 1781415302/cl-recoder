# CL Recoder 评审修复 — 工程实施方案（Dev Plan）

> 输入：上一轮代码评审报告（H1/H2、M1–M4、L1–L3 及收尾项）。
> 性质：**修复性迭代**。不改架构、不改线格式契约语义、不新增依赖、不动 PLAN 已锁定的字面契约。
> 读者：workflow 编排的执行 agent。本文档锁架构与契约，实现细节放权。
> 状态：经 4 路独立评审裁决后定稿。

---

## 1. 整体设计理念

### 核心目标
消除评审确认的 6 个真实缺陷（2 个 High、4 个 Medium），完成 3 个 Low 级修正与若干收尾项；全程保持「数据不落网、绝不 crash、契约逐字稳定」的项目铁律。

### 为什么采用这种设计
- **缺陷修复遵循"最小正确改动"**：所有对外线格式（Tauri command 参数形状、pipe NDJSON、settings.json、DDL、导出文件格式）全部保持现状。已验证唯一的线格式违约（MouseDistance）按 camelCase 全局约定修前端而非破例改 Rust。
- **并发修复选"每连接工作线程"而非 overlapped 改造**：`serve_client` 与现有测试的阻塞读写语义零改动，风险面最小；接受线程 + 唤醒器即可解决「卡死连接占死管道」。
- **默认路径解析放后端**而非前端：`import_whatpulse` 增加 `AppState` 注入后做三级解析（参数 → settings → 默认探测），同时消除前端硬编码的个人路径；任何未来调用方自动继承。
- **WhatPulse 快照用文件复制而非 SQLite Backup API**：`rusqlite` 白名单锁定 `features=["bundled"]`，复制 `db` + `-wal`（best-effort）即可读到未 checkpoint 数据，零依赖变更。
- **缓存性能修复只动 `taskExists`**：它是 PLAN 之外的补充字段，与 500ms pipe 探测契约正交；缓存 + 写路径回写即可，不引入新命令。

### 扩展性
- 三级路径解析是纯函数 + State 注入，后续若加"导入对话框选文件"或"自动探测其他统计软件"都可复用同一入口。
- pipe 工作线程模型为将来新增命令（如运行时改配置）留出了并发余量。
- TTL 缓存模式可推广到其他高频外部命令探测。

### 必须遵守的设计原则
1. PLAN §1 铁律不变：绝不写 WhatPulse 源文件、查询失败返回空数据、GUI 不崩。
2. §4.4 pipe 线格式、`spawn` 公共签名、DTO camelCase 约定、settings.json snake_case —— 逐字保持。
3. 依赖白名单不增不减（包括 rusqlite 不加 `backup` feature）。
4. 测试必须可移植（hermetic），不得依赖本机特定文件是否存在，也不得真实调用 schtasks。

---

## 2. 系统架构设计

### 模块改动总览

| 模块 | 改动性质 | 职责边界变化 |
|---|---|---|
| `crates/collector/src/ipc_server.rs` | 并发模型加固 | 不变：仍是唯一 pipe 服务端、唯一置位 Flags 的地方 |
| `crates/collector/src/engine_loop.rs` | 暂停期 Engine 状态维护 | 不变：仍是唯一聚合点 |
| `crates/collector/src/main.rs` | join 前自连唤醒兜底 | 不变 |
| `src-tauri/src/commands/import.rs` | 复制集扩展 + 路径三级解析 | 不变：仍是 wp_* 唯一写入处 |
| `src-tauri/src/commands/collector_ctl.rs` | taskExists 缓存 + 错误码文案 | 不变：仍是唯一 pipe 客户端/schtasks 入口 |
| `src-tauri/src/commands/devices.rs` | 仅新增一个序列化断言测试 | 零行为变化 |
| `src-tauri/src/state.rs` | 测试可注入路径 | `AppState` 增加私有字段，公开 API 不变 |
| `ui/src/api/types.ts` | MouseDistance 字段名对齐 | 契约修正 |
| `ui/src/api/mock.ts` | 同步修正 + 去硬编码 | 契约修正 |
| `ui/src/components/DeviceStatsPage.tsx` | 读字段名修正 | — |
| `ui/src/pages/WhatPulse.tsx` | 去硬编码路径 + 空态渲染失败报告 | — |
| `ui/src/pages/Dashboard.tsx` | "今日"标签语义 | — |
| `ui/src/pages/Settings.tsx` | 禁用提示文案 | — |
| `src-tauri/capabilities/default.json` | 删除未用权限 | 最小权限 |
| `ui/package.json` | 删未用 npm 依赖 | — |

### 禁止耦合
- collector 不感知 GUI/前端；`engine_loop` 不得引入除喂 Engine 外的暂停期副作用。
- GUI 端不做聚合、不做 WhatPulse 源库校验之外的 IO（复制动作除外）。
- 前端不得出现绝对用户路径；任何"默认探测"逻辑只允许出现在 Rust 侧。
- wp_* 表 SQL 仍只属于 `clrecoder_store`；import.rs 只组织数据、不写 SQL。
- `stats_path` 注入仅服务 `with_ro` 兜底重连；写路径命令（`set_device_nickname`、导入的 `stats_db` 实参）继续使用 `db::stats_db_path()` 全局路径，不扩散。

---

## 3. 文件级设计

### 3.1 `crates/collector/src/ipc_server.rs`（修改）
- **作用**：pipe 控制服务端。
- **改什么**：`run_server` 的 accept 循环——连接成功后不再就地 `serve_client`，移交工作线程；增加 accept 唤醒机制；模块头注释（12-13 行"单接受线程串行处理"等旧模型描述）与 `serve_client` 注释（约 169、209 行的"由 serve_client 关闭"措辞）随新模型同步更新。
- **对外接口不变**：`pub fn spawn(flags: Arc<Flags>, status: Arc<RuntimeStatus>) -> JoinHandle<()>`（保留原有 `#[must_use = "..."]` 消息）。
- **测试**：新增「卡死连接不阻塞服务」回归测试；既有 4 个测试不得改动语义。

### 3.2 `crates/collector/src/engine_loop.rs`（修改）
- **作用**：聚合主循环。
- **改什么**：`handle_event` 的 paused 分支——`RawEvent::Keyboard` 喂 `engine.on_key` 后丢弃输出，其余照旧丢弃；模块 doc 第 8 行"暂停时直接丢弃"改为"丢弃但键盘边沿仍喂 engine"；`handle_event` 内 paused 分支注释同步。
- **测试**：新增暂停期键盘边沿回归测试（见 §4.2）。

### 3.3 `crates/collector/src/main.rs`（修改）
- **作用**：collector 入口。
- **改什么**：观察到 `flags.shutdown` 后、`ipc.join()` 之前，以小循环反复「自连管道客户端 + `ipc.is_finished()` 检查」（约 20ms 间隔），直到 accept 线程退出再 join——生产侧兜底，免疫 worker 在置位与唤醒之间出意外的残余窗口。

### 3.4 `src-tauri/src/commands/import.rs`（修改）
- **作用**：WhatPulse 导入。
- **改什么**：
  1. 临时文件名加进程号：`clrecoder-wp-{pid}-{ts}.db`（消除并行测试与 UI 连点的毫秒戳碰撞面）；
  2. 复制主库（失败→早退报告）+ **best-effort** 复制 `{src}-wal`（失败静默跳过；`-shm` **不复制**——RW 打开副本时 SQLite 自动重建，且 -shm 持锁概率最高、复制最易失败）；
  3. 副本打开方式由 `SQLITE_OPEN_READ_ONLY|NO_MUTEX` 改为 `Connection::open(&tmp)`（WAL 恢复需要写副本自身；绝不写 WhatPulse 源文件的铁律不变）；
  4. 清理统一删 `{tmp}`/`{tmp}-wal`/`{tmp}-shm` 三件套（沿用 `state.rs` `testutil::remove_all` 同款 OsString push 模式拼旁路名）；
  5. `import_whatpulse` 命令注入 `AppState`，`spawn_blocking` 内先 `resolve_wp_source` 再 `run_import`；
  6. 模块 doc（约 3、9 行"只读打开副本"措辞）与注释同步更新；
  7. **`OpenFlags` 的 `use` 从顶层移入 `#[cfg(test)] mod tests`**——改造后非 test 代码不再引用它，不移会导致 `clippy -D warnings` 失败。
- **新增**：`resolve_wp_source` 纯函数。

### 3.5 `src-tauri/src/commands/collector_ctl.rs`（修改）
- **改什么**：`scheduled_task_exists` 增加 30s TTL 缓存（探针可注入）；`collector_autostart_enable`/`disable` 写后校验与 `start_collector_and_wait` 的诊断文案改用实时查询并回写缓存；`raw_exchange` 两处 `CreateFileW` 失败分支共用 `pipe_open_err` 分类文案函数。

### 3.6 `src-tauri/src/state.rs`（修改）
- **改什么**：`AppState` 增加私有 `stats_path: PathBuf`（`with_ro` 重连用它）；构造器分层为私有 `build(stats_path, settings)` + `new()`（默认路径 + 加载 settings）+ `pub(crate) with_db_path(path)`（注入路径 + `Settings::default()`，测试彻底 hermetic）。
- **测试改动**：`app_state_fallback_conn_returns_empty_semantics` 改为 `AppState::with_db_path(<不存在的临时路径>)`；`app_state_with_ro_on_real_db` 的字面构造 `AppState{..}` 改为 `with_db_path(f)`（顺带消灭最后一个字面构造点）。

### 3.7 `src-tauri/src/commands/devices.rs`（修改）
- 仅测试区新增：`MouseDistanceDto` 序列化逐字断言（`"totalInches"`/`"distanceInches"` 在场、`total_inches` 缺席），钉住契约。

### 3.8 `ui/src/api/types.ts`（修改）
- `MouseDistance`/`MouseDistanceDay` 字段改为 `totalInches`/`distanceInches`（`days[]` 目前无消费点，改名仅保类型一致）。

### 3.9 `ui/src/api/mock.ts`（修改）
- `mockMouseDistance` 返回 camelCase；`state.settings.wpDbPath` 改 `null`；`mockWpMeta` 兜底 `sourcePath` 去掉硬编码用户目录（改 `%LOCALAPPDATA%\WhatPulse\whatpulse.db` 字样）。

### 3.10 `ui/src/components/DeviceStatsPage.tsx`（修改）
- `mouseDist.data?.total_inches` → `totalInches`（约 123、129 两处）。

### 3.11 `ui/src/pages/WhatPulse.tsx`（修改）
- `dbPath` 表达式改 `settings.data?.wpDbPath ?? ""`；引导文案改为「默认探测 %LOCALAPPDATA%\WhatPulse\whatpulse.db」语义，不展示具体用户路径。
- **`!meta.data` 空态分支补渲染 `{report ? <ImportReportCard report={report} /> : null}`**——既有缺陷：空态下导入失败（ok:false 报告）界面零反馈。

### 3.12 `ui/src/pages/Dashboard.tsx`（修改）
- 三张 KPI 卡标签：`range.to === todayDay()` 时用"今日X"，否则用 `fmtDayShort(range.to)` + "X"（如"9/28 按键"）——`overview.today` 本就是 `to` 日拆分，文案须与数据同源。

### 3.13 `ui/src/pages/Settings.tsx`（修改）
- 采集器自启 Row 的 hint：仅 `taskOn` 为真的分支末尾追加"（不影响当前已运行的采集器）"；`未配置` 分支不加。

### 3.14 `src-tauri/capabilities/default.json`（修改）
- 删 `"autostart:default"`（前端无 autostart 插件调用；GUI 自启走 Rust `ManagerExt`，不受 capability 约束）。
- **保留 `opener:default`**：PLAN §5.5 预留"导出完成后打开所在文件夹"功能，占位不删（同状态但刻意保留，写注释说明）。

### 3.15 `ui/package.json`（修改）
- 删 `dependencies` 中未被 import 的 `@tauri-apps/plugin-autostart`；`npm install` 同步 lockfile。`plugin-opener` 同理保留（与 capability 决策一致）；`plugin-dialog` 被 `Settings.tsx` 使用，不得动。

---

## 4. 接口与数据结构设计（自包含契约）

### 4.1 ipc_server（内部并发改造）

```rust
// crates/collector/src/ipc_server.rs

/// 公共入口签名不变（#[must_use] 消息保留原文）。
pub fn spawn(flags: Arc<Flags>, status: Arc<RuntimeStatus>) -> std::thread::JoinHandle<()>;

/// 服务主体（签名改为按引用拿 Arc——worker 需要 Arc 克隆）。
fn run_server(flags: &Arc<Flags>, status: &Arc<RuntimeStatus>);

/// windows 0.62 的 HANDLE 是 *mut c_void 包装、非 Send——句柄移交工作线程必须经此 newtype。
/// SAFETY：内核句柄进程内全局有效；Send 仅表达所有权移交（单 worker，无需 Sync）。
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

/// serve_client 签名不变，由工作线程调用。
unsafe fn serve_client(pipe: HANDLE, flags: &Flags, status: &RuntimeStatus);

/// 单次自连探测：尝试以客户端身份打开管道，成功即关闭。
/// 供唤醒循环与 main 的 join 兜底共用。
pub(crate) fn prod_pipe();
```

**accept 循环契约（非显然，须照做）**：
1. `run_server` 内创建 `let accept_done = Arc::new(AtomicBool::new(false))`。
2. 循环：`flags.shutdown` 检查 → `CreateNamedPipeW`（保留 `FILE_FLAG_FIRST_PIPE_INSTANCE` 首迭代语义）→ `ConnectNamedPipe` 阻塞 → **`Ok` 与 `ERROR_PIPE_CONNECTED` 两条路径统一在 match 之后再查一次 `flags.shutdown`**：
   - 置位 → `CloseHandle(handle)` + `break`（保证 join 返回时所有服务端实例已关闭，管道名消失）；
   - 未置位 → `Arc::clone` 出 `flags`/`status`/`accept_done` + `SendHandle(handle)`，`drop(std::thread::spawn(move || ...))` 起 worker（`JoinHandle` 是 `#[must_use]`，用 `drop` 显式 detach）。
3. **所有退出路径**（含 `build_security_attributes` 失败的早退 `return`）都先 `accept_done.store(true, Ordering::Release)`。

**worker 契约**：`serve_client` 返回后检查 `flags.shutdown`——置位则进入 `wake_accept(&accept_done)`。

**唤醒契约（`wake_accept`）**：循环调用 `prod_pipe()`——`CreateFileW` 任何结果（成功、`ERROR_PIPE_BUSY`、`ERROR_FILE_NOT_FOUND` 或其它）都**不区分处理**：成功立即 `CloseHandle`，失败不重判，统一睡 ~20ms 再试；退出条件**只有** `accept_done` 置位或累计 ~5s 上限。理由：accept 线程只可能死在 `ConnectNamedPipe`（此时实例必存在，prod 必成功）或 1s 建实例失败 sleep（循环顶自查退出）；`CreateNamedPipeW` syscall 期间存在"零实例"微秒窗口，`FILE_NOT_FOUND` 早退会让 accept 在新实例上永久阻塞 → join 挂死。
`prod_pipe` 的 `CreateFileW` 参数参照现有测试 `open_client`：`GENERIC_READ`（单边打开 duplex 管道合法）、share=0、`OPEN_EXISTING`、`FILE_FLAGS_AND_ATTRIBUTES(0)`、sa=None。

**main 侧兜底契约**：`main.rs` 在 `while !flags.shutdown` 轮询退出后，`ipc.join()` 前循环 `prod_pipe()` + 检查 `ipc.is_finished()`（~20ms 间隔）——免疫 worker panic/未来新增置位点；join 本身不变。

**线程堆积声明（已知取舍，写进模块 doc）**：worker 无读超时、detach 语义、随进程退出回收；同用户 SID 的 DACL 即威胁边界，同用户恶意进程理论上可堆积 N 个阻塞线程，比"整条管道占死"的现状已是严格改善。

**测试契约**：
- 新增 `stalled_client_does_not_block_new_connections`——沿用 `SERVER_LOCK` 串行；`open_client` 后不发数据；随后 `transact("status")` 成功、`transact("shutdown")` 成功；**断言完成后必须 `CloseHandle` stalled 客户端句柄**（泄漏的服务端实例会撑住管道名，后续测试首个 `FILE_FLAG_FIRST_PIPE_INSTANCE` 实例创建会永久 `ERROR_ACCESS_DENIED` → 连锁超时）；收尾 join 用有界等待（`is_finished` + deadline 循环），让回归失败表现为 fail 而非 CI 挂起。
- 既有 4 个测试的 `server.join()` 保持原样——worker 侧唤醒器保证其返回。

### 4.2 engine_loop 暂停期输入语义

```rust
// crates/collector/src/engine_loop.rs — Aggregator::handle_event 的 paused 分支

self.status.record_event();          // "收到即计"语义不变，仍在 paused 检查之前
if paused {
    // 键盘边沿照常喂状态机（维护 held/修饰键集合，输出丢弃不计数），
    // 防止暂停期松开/按下的键在恢复后被误判（漏计首按 + 幽灵组合键）。
    if let RawEvent::Keyboard { sc, down, .. } = raw {
        let _ = self.engine.on_key(sc, down);
    }
    return;
}
self.on_input(raw);
```

`Engine::on_key` 是 `pub fn on_key(&mut self, sc: u16, down: bool) -> EngineOut` 纯状态机（无 IO、无 windows 依赖）；`EngineOut` 带 `#[must_use]`，必须 `let _ =` 显式丢弃。其余 `RawEvent` 变体无内部状态可陈旧，照旧整条丢弃。

**测试契约**：`paused_keyboard_edges_keep_engine_state_fresh`，用 `agg_with_frozen_cursor` 构造，**必须显式 `agg.flags.paused.store(true, Ordering::Release)`**——helper 只设 `paused_observed:true`、flag 默认 false，不置位会走恢复沿消解直接计数（测试假绿）：
- paused 下注入 `Keyboard{sc:X, down:true}` → `inputs`/`combos`/`apps` 全空；
- `paused.store(false)` 后发 `Keyboard{sc:X, down:false}` 再发 `down:true` → `inputs[(dev,day,X)]` 恰计 1（首按不被当自动重复吞掉、也不重复计）；
- 推荐附加：暂停前注入 `Ctrl down`，暂停期 `Ctrl up`，恢复后 `C down` → `combos` 仍空（无幽灵 Ctrl+C）。

### 4.3 import.rs —— 复制集与路径三级解析

```rust
/// 源路径三级解析（参数 > settings 覆盖 > 默认探测）。
/// Err = 无法确定源路径（如 %LOCALAPPDATA% 解析失败）。
pub(crate) fn resolve_wp_source(path_arg: &str, settings_wp_db_path: Option<&str>) -> Result<PathBuf, String>;
// 语义：path_arg.trim() 非空 → 用它；
//       settings_wp_db_path.trim() 非空 → 用它（settings 里允许写入空串，须同样 trim 判定）；
//       否则 dirs::data_local_dir()?.join("WhatPulse").join("whatpulse.db")
//       ——data_local_dir 失败时 Err，绝不兜底成 "./WhatPulse/whatpulse.db"。

/// 命令签名（TS 侧 invoke 键不变：仍只有 {path}）。
#[tauri::command]
pub async fn import_whatpulse(
    path: String,
    state: tauri::State<'_, AppState>,
) -> Result<ImportReport, String>;
```

- 命令体内：`let st = state.inner().clone()`（沿用 `settings.rs` 先例），进 `spawn_blocking` 后 `resolve_wp_source(&path, st.settings 读守卫内的 wp_db_path)`；**解析失败走 `failed_report(started, Vec::new(), msg)` → `Ok(ImportReport{ok:false})`**——与既有"失败不返回 Err"命令语义一致（前端侧由 §3.11 的空态渲染保证可见）。
- `run_import(source: &Path, stats_db: &Path)` 签名不变；stats_db 实参维持 `db::stats_db_path()`。
- 复制段契约：`fs::copy(src, tmp)` 失败→早退报告；`{src}-wal` 存在则 best-effort 复制为 `{tmp}-wal`（失败静默跳过——WhatPulse 写入期间的锁竞争/checkpoint 竞态属预期，缺 wal 只是得到更旧快照）；**不复制 -shm**；旁路路径用 `OsString::push("-wal")` 拼接（非 UTF-8 路径安全）。
- `import_from_copy` 签名不变，内部 `Connection::open(&tmp)`（RW）；既有 `journal_mode=DELETE` fixture 不受影响。
- `meta.note` 只含聚合期 warnings、末段"无数据行"警告不落库——维持现状（已知既有不一致，本 sprint 不修）。

**测试契约**：
- `import_reads_wal_only_rows`：fixture `journal_mode=WAL`，`execute_batch` autocommit 提交插入（未提交帧恢复时被跳过）；断言前置条件 `src-wal` 存在且非空；fixture conn **存活跨过** `run_import` 调用且 `TempFile` 要先于 conn 声明（drop 逆序，防 Windows 下残留临时文件）；导入后 `wp_key_daily` SUM 含 wal-only 行。
- `resolve_wp_source` 纯函数用例：`("C:\\x.db", None)` → `C:\x.db`；`("  ", Some("D:\\wp.db"))` → `D:\wp.db`；`("", None)` → 以 `WhatPulse\whatpulse.db` 结尾（dirs 可解析时）。
- 既有用例全保留（DELETE-journal fixture 走无 wal 分支）。

### 4.4 collector_ctl.rs —— taskExists 缓存与错误分类

```rust
static TASK_EXISTS_CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
const TASK_EXISTS_TTL: Duration = Duration::from_secs(30);

/// 可测核心：探针注入（单测用假探针，禁止真调 schtasks——hermetic 原则）。
fn task_exists_cached_impl(probe: impl Fn() -> bool) -> bool;
/// 生产封装。
fn scheduled_task_exists_cached() -> bool { task_exists_cached_impl(scheduled_task_exists) }
/// 写路径专用：实时查询 + 无条件回写缓存。
fn scheduled_task_exists_uncached() -> bool;

/// 连接错误分类文案（raw_exchange 两处 CreateFileW 失败分支共用）。
fn pipe_open_err(e: &windows::core::Error) -> String;
// ERROR_FILE_NOT_FOUND → "采集器未运行（控制管道不存在）"
// ERROR_ACCESS_DENIED  → "控制管道访问被拒（采集器可能以其他用户身份运行）"
// 其他 → 原样附错误描述；ERROR_PIPE_BUSY 分支语义不变
```

**使用契约**：
- `collector_status` → `scheduled_task_exists_cached()`；
- `collector_autostart_enable`/`disable` 写后校验 → `scheduled_task_exists_uncached()`；
- `start_collector_and_wait` 末路诊断文案 → `scheduled_task_exists_uncached()`（失败路径无性能考量，文案必须准确）；
- 缓存锁 guard 不跨 `Command::status()` 持有（先读缓存 drop guard → 跑 schtasks → 再 lock 回填）。

### 4.5 AppState 可注入路径

```rust
#[derive(Clone)]
pub struct AppState {
    pub ro_conn: Arc<Mutex<Connection>>,
    pub settings: Arc<RwLock<Settings>>,
    /// 统计库路径：仅 with_ro 兜底重连使用；写路径命令继续用 db::stats_db_path()。
    stats_path: PathBuf,
}

impl AppState {
    #[must_use] pub fn new() -> Self;                              // build(默认路径, Settings::load_from(..))
    #[must_use] pub(crate) fn with_db_path(stats_path: PathBuf) -> Self; // build(p, Settings::default())——hermetic
    fn build(stats_path: PathBuf, settings: Settings) -> Self;
}
```

`with_ro` 重连目标改 `self.stats_path`。注入不存在路径时：`open_ro` 失败 → 内存兜底 conn → `is_fallback_conn` 判定成立 → 每次 `with_ro` 重连必失败 → 永远兜底，与机器上是否有真实 stats.db 无关。

### 4.6 MouseDistance 线格式（前端修正目标）

```ts
// ui/src/api/types.ts —— 唯一正确形状（与 Rust MouseDistanceDto 一致）
export interface MouseDistanceDay { day: string; distanceInches: number; }
export interface MouseDistance { totalInches: number; days: MouseDistanceDay[]; }
```

`DeviceStatsPage.tsx` 读 `mouseDist.data?.totalInches`；`mockMouseDistance` 返回同名 camelCase 字段。**禁止**反向拆 Rust 的 `rename_all="camelCase"`。

### 4.7 契约断言（测试钉死的字面量）
- `MouseDistanceDto` JSON 含 `"totalInches":`、`"distanceInches":`，不含 `total_inches`（devices.rs 新增测试）。
- `resolve_wp_source` 三级用例见 §4.3。
- `task_exists_cached_impl` 注入探针用例：命中窗口内不重复 probe、过期后重新 probe、uncached 回写后下一次命中新值。

---

## 5. 核心流程设计

### 5.1 WhatPulse 导入（修复后）
1. 前端 `import_whatpulse(path)`；空/空白 → `resolve_wp_source` 三级解析（arg > settings > 默认探测）。
2. 解析失败 → `failed_report` → `Ok(ImportReport{ok:false, warnings:[原因]})`（前端空态/主分支均渲染 `ImportReportCard`，见 §3.11）。
3. 复制 `db` + best-effort `-wal` 到 `%TEMP%\clrecoder-wp-{pid}-{ts}.db` → `Connection::open` 打开副本（WAL 自动恢复、-shm 自动重建）→ 逐表聚合（缺表→warning 继续）→ `rebuild_wp_tables_with` 单事务写入 → 删三件套。
4. 异常：主库复制失败 → 早退报告；聚合某表 SQL 失败 → warning + 跳过；重建失败 → `ok:false`。文件复制非原子快照——checkpoint 交错时副本可能不完整，走 warning/失败报告兜底，不 crash、绝不写源库。

### 5.2 暂停期按键
1. paused 置位 → `handle_event` 丢弃 Input（Keyboard 边沿例外喂 engine）。
2. 暂停期 Ctrl 松开 → `held` 同步清除。
3. 恢复 → `on_pause_transition` 前台游标语义照旧；`held` 已最新，无漏计无幽灵组合。

### 5.3 pipe 并发
1. 正常连接 → accept → worker serve → 断开。
2. 卡死连接 → worker 阻塞在自己的 `ReadFile`，accept 已开下一实例，后续连接照常。
3. shutdown → worker 置 flag + 回响应 + `wake_accept` 循环 prod → accept 被唤醒 → 二次查 flag → 关柄退出 → `accept_done` 置位 → join 返回；main 侧 `is_finished`+`prod_pipe` 循环作第二重兜底。

### 5.4 collector_status 轮询（缓存后）
500ms 轮询不变；pipe 探测照旧；`taskExists` 走 30s 缓存；enable/disable 后缓存被实时值覆盖，下一次 status 立即正确。

### 5.5 Dashboard 末日标签
`range.to === todayDay()` → "今日X"；否则 `${fmtDayShort(range.to)} X`——与 `overview.today`（`to` 日拆分）语义同源。

---

## 6. 数据存储与状态设计

- **无 schema 变更**；`mouse_move_daily`/`wp_*` 表结构不动。
- `settings.json` 形状不变；三级解析是读取侧语义补强。
- `AppState.stats_path` 为内存字段，不持久化。
- WhatPulse 副本三件套生命周期 = 单次导入。

---

## 7. 与现有代码的兼容方案

| 已有代码 | 处理方式 |
|---|---|
| `ipc_server` 4 个进程内测试 | 语义全保留；worker 唤醒器保证 `server.join()` 照常返回 |
| `main.rs` shutdown 收尾 | 增加 `prod_pipe`+`is_finished` 循环，随后 `ipc.join()` 语义不变 |
| `raw_input.rs`/`gamepad.rs`/`apps.rs`/`selftest.rs` | **零改动**——不感知暂停；selftest 暂停窗口期零注入不受影响 |
| `engine_loop` 现有暂停沿测试 | 不动；新测试用 `agg_with_frozen_cursor` 同模式 + 显式置 flag |
| `import.rs` DELETE-journal fixture | 不破坏——无 wal 分支行为与现状一致 |
| `state.rs` 两个测试 | fallback 测试改 `with_db_path`；real-db 测试同样走 `with_db_path` |
| `commands/mod.rs` 注册、`client.ts` invoke 键 | 全部不变 |
| `PLAN.md` | 不修改；GUI 写 `devices.nickname` 超 PLAN 文本属既有事实，留待文档修订 |

---

## 8. Stage Map（workflow 调度蓝图）

| # | 目标 | 涉及文件 | 依赖 | 验收点 |
|---|---|---|---|---|
| S1 | ipc_server 工作线程 + 唤醒 + main prod | `crates/collector/src/ipc_server.rs`、`crates/collector/src/main.rs` | 无 | `cargo test -p clrecoder-collector` 全过（含新增 stall 用例；4 个既有测试不改） |
| S2 | engine_loop 暂停期喂 Engine | `crates/collector/src/engine_loop.rs` | 无 | `cargo test -p clrecoder-collector` 全过（含新增 paused-edge 用例） |
| S3 | import.rs 复制集 + 三级解析 | `src-tauri/src/commands/import.rs` | 无 | `cargo test -p cl-recoder` 全过（含 WAL 用例 + resolve 用例） |
| S4 | collector_ctl.rs 缓存 + 错误分类 | `src-tauri/src/commands/collector_ctl.rs` | 无 | `cargo test -p cl-recoder` 全过（含注入探针用例） |
| S5 | state.rs 路径注入 | `src-tauri/src/state.rs` | 无 | `cargo test -p cl-recoder` 全过（**本机有 stats.db 时 fallback 测试也必须过**——当前会挂） |
| S6 | 前端契约与文案修正 | `types.ts`、`mock.ts`、`DeviceStatsPage.tsx`、`WhatPulse.tsx`、`Dashboard.tsx`、`Settings.tsx` | **软依赖 S3**：WhatPulse 路径项依赖后端空串解析，须同批生效（S8 前不单独发布该子项） | `cd ui && npm run build` 通过 |
| S7 | capabilities + package.json 精简 | `capabilities/default.json`、`ui/package.json` | 无 | `npm run build` + `cargo build --workspace` 通过 |
| S8 | **收敛验证** | 全部 | 依赖 S1–S7 | `cargo test --workspace` 0 fail；`cargo clippy --workspace -- -D warnings` 干净；`cd ui && npm run build` 干净 |

**并行性**：S1–S7 文件互不重叠，可全并行；S3/S4/S5 虽同属 `src-tauri` 但文件不重叠。S8 是收敛关卡。

---

## 9. 给执行 Agent 的约束

### 架构决策（禁止自行更改）
- 模块归属、文件边界、接口签名、线格式字段名 —— 一律按第 3/4 节执行。
- ipc_server 的工作线程模型 + 唤醒机制（`wake_accept` 全错误重试 / `accept_done` 全路径置位 / post-connect 二次查 flag / main 侧 prod 兜底）——禁止换方案。
- 暂停期喂 Engine 用 `let _ =` 丢输出，位置在 `record_event` 之后、paused return 之内。
- `import_whatpulse` 不改对外参数/返回值形状；`State` 注入只允许在 Rust 签名层。
- WhatPulse 复制集 = 主库（fatal）+ `-wal`（best-effort）；**不复制 `-shm`**；临时文件名含 pid。
- taskExists 缓存核心必须是可注入探针的纯函数；单测禁止真调 schtasks。
- `stats_path` 私有、仅服务 `with_ro`；写路径命令继续用全局 `db::stats_db_path()`。
- `raw_exchange` 的 HANDLE→File 包装、DACL、超时模式不变。
- 不新增依赖；`capabilities` 只删 `autostart:default`，`opener:default` 保留。

### 实现决策（可自主）
- 变量命名、注释措辞、辅助函数拆分粒度。
- `wake_accept`/`prod_pipe` 的具体节奏（上限 ~5s 不变）；句柄传递也可用 `usize` 等价替代 `SendHandle`（二选一）。
- Dashboard 末日标签精确文案（"今日"优先 + 末日日期兜底的语义不变）。
- `resolve_wp_source`/`pipe_open_err` 错误文案措辞。
- mock 占位字符串的具体写法。

### 编码规范
- 与现有代码风格一致（`//!` 模块文档、`///` 函数注释、中文错误文案、unsafe 附 SAFETY 注释）；行为变化的模块 doc 必须同步更新（ipc_server 线程模型、engine_loop 暂停语义、import 副本打开方式）。

### 不确定性抉择记录（已敲定）
- **ipc_server**：每连接 worker + 自连唤醒（弃 overlapped 重写——变更面最小）；waker 对一切失败重试（堵 FILE_NOT_FOUND 竞态）。
- **WhatPulse 快照**：文件复制 db+wal（弃 `VACUUM INTO`/Backup API——不碰白名单）。
- **MouseDistance**：改前端适配 Rust camelCase。
- **`import_whatpulse` 路径**：`path` 空串 → `State` 注入读 settings → 默认探测；失败走 `ok:false` 报告（前端空态补渲染保证可见）。
- **engine_loop**：喂 Engine 而非重置（重置会在"按住-暂停-松开"产生新错误）。
- **Dashboard**：动态文案。
- **worker 句柄传递**：`SendHandle` newtype 为主，`usize` 等价写法允许。
- **已知取舍（不修）**：worker 无读超时（同用户 DACL 是威胁边界）；meta.note 不含末段警告；STATIC 窗口类 hack；`set_device_nickname` 走 `Writer::open`。
