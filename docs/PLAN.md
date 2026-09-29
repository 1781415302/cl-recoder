# CL Recoder 开发实施计划

> 版本：v1.1（评审裁决后定稿：4 视角评审 33 条有效意见全部处置——32 采纳、1 部分采纳） 日期：2026-09-28
> 本文档是唯一的架构与契约来源。执行 Agent（workflow 子代理）必须遵守第 9 节的约束划分；
> 各 Agent 之间看不到彼此代码，只能依赖第 3、4 节的契约协调——契约自包含，禁止口头约定。

---

## 0. 需求基线（Locked Requirements，所有 stage 的对齐锚点）

| # | 需求 | 决策 |
|---|------|------|
| R1 | 平台 | 仅 Windows 10 (1803+) / 11，x64 |
| R2 | 形态 | 托盘常驻小工具 + 点开的 GUI 数据面板 |
| R3 | 设备维度 | 鼠标、键盘、手柄**全部按设备型号**（VID/PID+产品名）分开统计；同型号设备合并（Windows 无序列号，接受） |
| R4 | 键盘 | 每个键 × 每天的按下次数（物理键，scan code 为准，跨布局稳定） |
| R5 | 鼠标 | 每个按键 × 每天（左/右/中/X1/X2 + 滚轮上/下/左/右），与手柄**完全分开** |
| R6 | 手柄 | 每个按键 × 每天，独立于鼠标统计 |
| R7 | 组合键 | 仅修饰键组合（Ctrl/Shift/Alt/Win + 非修饰键），× 每天 |
| R8 | 应用 | 按 exe 记录前台时长 + 该应用内按键数/点击数，× 每天 |
| R9 | 明确不做 | 热力图、网络流量、额定寿命对照（只记原始次数）、窗口标题级统计 |
| R10 | 提权 | **要**统计"以管理员运行"的程序里的输入 → 采集进程必须提权（UIPI 已核实为硬性要求） |
| R11 | WhatPulse 导入 | 只读读取其本地 SQLite，导入到**独立命名空间**展示，绝不与本软件数据混合 |
| R12 | 存储/导出 | 本地 SQLite（WAL）；导出 CSV / JSON |
| R13 | 技术栈 | Rust 采集端 + Tauri v2 前端；性能优先 |
| R14 | UI | 浅色、简洁，遵循 ui-ux-pro-max skill 检索产出的设计令牌（见 §4.9） |

---

## 1. 整体设计理念

**核心目标**：一个本地优先的输入计数器——回答"我的每只外设、每个键，每天/累计被按了多少次"，并保留 WhatPulse 的历史视角。差异化三点：①设备型号维度（WhatPulse 完全没有）；②手柄与鼠标分离（WhatPulse 合并）；③数据自主（本地 SQLite + 导出 + WhatPulse 只读导入，无账号无云）。

**为什么是"提权采集进程 + 普通 GUI 进程"两进程**：
- 已核实（微软《Windows Integrity Mechanism Design》）：UIPI 按完整性级别过滤输入投递，普通权限进程收不到"发往提权程序"的 Raw Input 且**无任何报错**。R10 要求覆盖提权程序 ⇒ 采集端必须 High IL 运行。
- 最小提权面：只有采集器提权（它只做计数与写库，无网络、无任意路径写）；GUI/WebView 保持中完整性，攻击面小。
- 故障隔离：GUI 崩溃/升级不影响采集；采集崩溃 GUI 仍能看历史。
- 两个进程**不共享内存、不共享 SQLite 连接**：SQLite 文件（WAL）是数据集成点，named pipe 仅传控制指令（status / set_paused / shutdown）。

**为什么纯函数统计引擎**：键盘统计里有非显然逻辑（自动重复去重、修饰键状态机、组合键判定）。把它做成不依赖任何 OS API 的纯函数 crate，合成事件流即可单测——这是本项目正确性的根基，也是 workflow 并行开发的测试锚点。

**性能取向**：常驻进程 CPU 目标 <1%（事件驱动，无轮询键盘鼠标；手柄 XInput 轮询约 125Hz×4 槽，成本可忽略）；内存目标 <50MB；写库批量（2s 一批，WAL + synchronous=NORMAL；2026-09-28 变更：配合前端 1s 轮询做近实时）。

**扩展性预留**（不实现，只不堵死）：
- `devices.kind`、`input_daily.code` 是开放枚举，未来加"媒体键/HID 直连手柄"只是新 code 段；
- `wp_*` 表以 `wp_` 为导入源命名空间，未来可加 `*` 其他导入源（如新的导入器建 `xx_*` 表）；
- 引擎输入是事件流，未来替换采集实现（如 UIAccess）不影响统计逻辑。

**必须遵守的设计原则**：
1. 契约单一来源：跨 crate 共享的类型/协议/DDL 只在 `crates/core` 与 `crates/store` 定义一次。
2. 本地优先：任何功能不得依赖网络；遥测为零。
3. 防御性兜底：未知设备、hDevice=0、RDP 虚拟设备、UWP frame 窗口、损坏行——一律归桶/跳过，**绝不 crash、绝不丢其他事件**。
4. 数据不混合：自有数据与 WhatPulse 导入数据物理分表，UI 分区展示。
5. 最小权限：GUI 对统计库只读（导入除外）；提权只属于采集器；pipe DACL 限定当前用户。
6. WhatPulse 库只读：导入前先复制文件再打开副本，绝不写 WhatPulse 的任何文件。

---

## 2. 系统架构设计

### 2.1 进程与部署形态

```
┌─────────────────────────────────────────────────────────────┐
│ 进程 A：cl-recoder-collector.exe（High IL，计划任务 ONLOGON 自启）│
│                                                              │
│  raw_input 线程          gamepad 线程        apps 线程        │
│  (WM_INPUT 消息循环)     (gilrs/xinput 轮询) (SetWinEventHook)│
│        └────────────────────┬────────────────────┘           │
│                        crossbeam channel                     │
│                              ▼                               │
│  aggregator 线程: Engine(纯) → 内存日聚合 → 每 2s flush        │
│                              ▼                               │
│  store(writer): SQLite %LOCALAPPDATA%\ClRecoder\stats.db (WAL)│
│  ipc_server 线程: \\.\pipe\clrecoder-control (显式 DACL)       │
└─────────────────────────────────────────────────────────────┘
              ▲ pipe(控制)                    ▲ ro 读
┌─────────────┴───────────────────────────────┴───────────────┐
│ 进程 B：cl-recoder.exe（Tauri v2 GUI，中完整性，HKCU Run 自启）  │
│  Rust 侧: commands(overview/keys/apps/combos/wp/export/import)│
│           keylabel(GetKeyNameTextW) · collector_ctl(pipe 客户端)│
│           · autostart(schtasks 提权辅助) · tray               │
│  WebView: React 18 + Vite + TS + Tailwind v4 + Recharts       │
└──────────────────────────────────────────────────────────────┘
```

### 2.2 新增模块与职责边界

| 模块（crate/目录） | 职责 | 禁止 |
|---|---|---|
| `crates/core` | 跨进程共享：设备标识、code 空间枚举、RawEvent、IPC 协议、Qt 键码映射、日期工具 | 依赖 windows/tauri/rusqlite 任何一边的专有 API；包含任何 IO |
| `crates/engine` | **纯函数**键盘统计状态机（repeat 去重、修饰键跟踪、组合键判定） | 依赖 windows crate、做 IO、碰鼠标/手柄事件 |
| `crates/store` | SQLite schema/迁移；writer（collector 用）/reader（GUI 用）的全部 SQL | 包含业务统计逻辑；GUI 写自有统计表（导入表除外） |
| `crates/collector` | 三个采集源（raw_input/gamepad/apps）→ channel → aggregator → store；pipe 服务端；selftest 模式 | 任何 UI；除 stats.db/settings 外写任何文件；网络 |
| `src-tauri`（GUI Rust） | 托盘与窗口；全部 Tauri commands；键帽显示名；WhatPulse 导入；导出；collector 控制；自启管理 | 写 stats.db 自有统计表（只读；仅 import 写 wp_* 表）；内嵌统计逻辑 |
| `ui`（前端） | 页面渲染、图表、日期范围选择、设置交互 | 绕过 invoke 直接读文件/DB；引入 UI 库之外的重依赖 |

### 2.3 数据流（写路径）

OS 输入 → 三个采集线程各自翻译为 `RawEvent`/`Foreground` 事件（设备解析、滚轮 delta→刻度、按键边沿提取都在采集层完成）→ crossbeam channel → aggregator：Engine 处理键盘事件产出按键/组合键计数；所有事件按当前前台 exe 归属 app 计数 → 内存聚合表（以 `(day, …)` 为键，跨天自然开新桶）→ 每 2s 若脏则单事务 flush 到 SQLite → WAL 可被 GUI 并发读。

### 2.4 数据流（读路径）

前端 `invoke` → Tauri command（async + spawn_blocking）→ `store::reader`（GUI 的 ro 连接，busy_timeout=5000ms）→ serde JSON → React Query 展示（1s 轮询今日数据，近实时）。

### 2.5 禁止耦合清单

- `engine` 禁止 `use windows`（用 CI/测试保证：engine crate 不声明该依赖）。
- GUI 的 keylabel（GetKeyNameTextW）属于**展示层**，禁止下沉到 collector/store；collector 不做任何键名翻译。
- WhatPulse 导入逻辑只存在于 `src-tauri/src/commands/import.rs`；collector 完全不知道 WhatPulse 存在。
- 前端不得做数据聚合（跨天求和、TopN 排行、按设备/种类汇总等一律由 SQL 完成）；仅允许对查询返回的单行做展示格式化（数字缩写、日期本地化）。
- `core::ipc` 协议结构变更必须同版升级两侧（类型共享保证编译期一致）。

---

## 3. 文件级设计

仓库根：`C:\Users\17814\Documents\cl recoder\`（空仓库，全新创建）。

```
cl-recoder/
├─ PLAN.md  README.md  .gitignore  rust-toolchain.toml(stable)
├─ Cargo.toml                    # [workspace] members = 5 个 crate；[workspace.dependencies] 锁版本
├─ scripts/
│  ├─ install-collector-task.ps1 # schtasks /Create /TN ClRecoderCollector /SC ONLOGON /RL HIGHEST /F
│  ├─ uninstall-collector-task.ps1
│  └─ check-task.ps1             # 查询任务是否存在并输出 exists|missing
├─ crates/
│  ├─ core/src/{lib.rs, codes.rs, event.rs, ipc.rs, qtkeys.rs, day.rs}
│  ├─ engine/src/lib.rs          # 纯状态机 + #[cfg(test)] 单测
│  ├─ store/src/{lib.rs, schema.rs, writer.rs, reader.rs}
│  ├─ collector/src/{main.rs, device.rs, raw_input.rs, gamepad.rs, apps.rs,
│  │                engine_loop.rs, ipc_server.rs, selftest.rs}
├─ src-tauri/
│  ├─ Cargo.toml  build.rs  tauri.conf.json  capabilities/default.json  icons/
│  └─ src/
│     ├─ main.rs                 # tauri builder、插件注册（single-instance 必须第一）、托盘、窗口事件
│     ├─ state.rs                # AppState{ ro_conn: Mutex<Connection>, settings: RwLock<Settings> }
│     ├─ db.rs                   # ro/rw 连接打开（busy_timeout、WAL 容错）
│     ├─ keylabel.rs             # scancode→当前布局键名（带缓存）
│     └─ commands/
│        ├─ mod.rs               # generate_handler 汇总
│        ├─ overview.rs  devices.rs  keys.rs  apps.rs  combos.rs   # 查询类（调 store::reader）
│        ├─ wp.rs                # WhatPulse 只读查询
│        ├─ import.rs            # WhatPulse 导入（唯一写 wp_* 的地方）
│        ├─ export.rs            # CSV/JSON 导出
│        ├─ collector_ctl.rs     # pipe 客户端 + schtasks 提权辅助 + 启动采集器
│        └─ settings.rs          # settings.json 读写 + GUI 自启(autostart 插件)
└─ ui/
   ├─ package.json  vite.config.ts  index.html
   ├─ src/
   │  ├─ main.tsx  App.tsx  theme.css          # §4.9 设计令牌 CSS 变量
   │  ├─ api/types.ts           # §4.7 TS 契约（手写，与 Rust serde 对齐）
   │  ├─ api/client.ts          # invoke 包装
   │  ├─ components/{Sidebar,StatCard,DataTable,TrendChart,TopBarChart,
   │  │              DateRangePicker,EmptyState,DeviceTabs}.tsx
   │  └─ pages/{Dashboard,Keyboard,Mouse,Gamepad,Apps,Combos,WhatPulse,Settings}.tsx
```

关键文件说明（其余为常规拆分，executor 自主）：

| 文件 | 为什么存在 | 暴露 |
|---|---|---|
| `crates/core/src/codes.rs` | 三种外设的 code 空间 + 归一化规则是全系统最重要的契约 | `DeviceKind`、`MouseButton`、`GamepadButton`、`ModsBits` 常量、`normalize_scancode`、`modifier_bit` |
| `crates/core/src/event.rs` | 采集层→aggregator 的事件语言 | `DeviceKey`、`RawEvent`、`AggEvent` |
| `crates/core/src/ipc.rs` | GUI↔collector 控制协议 | `PIPE_NAME`、`CtlRequest`、`CtlResponse`、协议常量 |
| `crates/core/src/qtkeys.rs` | WhatPulse 键码是 Qt 码，映射表是导入质量的关键 | `qt_key_label(code: i64) -> String` |
| `crates/engine/src/lib.rs` | 唯一有非显然算法的地方，必须可独立测试 | `Engine::{new,on_key}` |
| `crates/store/src/writer.rs` | collector 唯一写入口；批量 upsert 事务 | `Writer::{get_or_create_device, flush(FlushBatch)}` |
| `crates/store/src/reader.rs` | GUI 全部查询 SQL 的唯一归属 | 各 `query_*` 函数（§4.5） |
| `crates/collector/src/raw_input.rs` | 键鼠采集：注册/消息循环/解析/设备缓存/滚轮累计 | `spawn(tx) -> JoinHandle` |
| `crates/collector/src/apps.rs` | 前台应用跟踪（含 UWP 兜底）+ 秒数归账状态 | `spawn(tx, fg_state)` |
| `crates/collector/src/engine_loop.rs` | 消费事件、跑 Engine、维护日聚合、2s flush、跨天、暂停 | `spawn(rx, writer, flags, fg, status)` |
| `src-tauri/src/keylabel.rs` | 键帽显示（布局相关）只能在 GUI 侧 | `key_label(sc: u16) -> String`（带 LRU 缓存） |
| `src-tauri/src/commands/import.rs` | WhatPulse 导入的唯一实现处 | `import_whatpulse(path)` command |
| `ui/src/theme.css` | 令牌即规范，前端不允许出现令牌外的颜色/字号 | `:root{--*}` 变量表 |

---

## 4. 接口与数据结构设计（自包含契约）

> 本节是各 workflow Agent 的唯一协调点。所有 Rust 结构 `derive(Debug, Clone, Serialize, Deserialize)`；
> 所有 `day` 一律 `YYYY-MM-DD`（本地时区）字符串。

### 4.1 core::codes —— code 空间（最重要的契约）

```rust
pub enum DeviceKind { Keyboard, Mouse, Gamepad }          // serde 小写: "keyboard"|"mouse"|"gamepad"

/// code 用 u16 表达，但其唯一性只在设备种类内成立（MouseButton 1..9 与 GamepadButton 1..17 值域重叠，
/// 键盘 scancode 低段也有重叠）；消歧靠 input_daily.device_id → devices.kind，禁止按 code 值域判断设备种类。
pub enum MouseButton { Left=1, Right=2, Middle=3, X1=4, X2=5, WheelUp=6, WheelDown=7, WheelLeft=8, WheelRight=9 }

pub enum GamepadButton { South=1, East=2, North=3, West=4, LeftTrigger=5, LeftTrigger2=6,
  RightTrigger=7, RightTrigger2=8, Select=9, Start=10, Mode=11, LeftThumb=12, RightThumb=13,
  DPadUp=14, DPadDown=15, DPadLeft=16, DPadRight=17 }

/// 修饰键位掩码（L/R 合并）
pub mod mods { pub const CTRL: u8=1; pub const SHIFT: u8=2; pub const ALT: u8=4; pub const WIN: u8=8; }

/// 键盘归一化 scancode：u16 = MakeCode | (E0?0xE000:0) | (E1?0xE100:0)
/// 非显然规则（必须实现）：
/// - E1 前缀（Pause 键 E1 1D 45 序列）与 VKey==VK_PAUSE(0x13) 的 0x45 伴随事件都归一化为 0xE11D，
///   由 Engine 的按下状态表天然去重成 1 次计数；
/// - MakeCode==0xFF（KEYBOARD_OVERRUN_MAKE_CODE）丢弃；
/// - MakeCode==0 且 VKey!=0 的情况由采集层（raw_input）先用 MapVirtualKeyW(VKey, MAPVK_VK_TO_VSC_EX)
///   解析出 scancode 后再调用本函数——core 不依赖 windows，本函数保持纯函数。
pub fn normalize_scancode(make: u8, e0: bool, e1: bool, vkey: u16) -> Option<u16>;

/// 修饰键 scancode 集合（布局无关，固定）：0x2A/0x36(0xE036)→SHIFT，0x1D/0xE01D→CTRL，
/// 0x38/0xE038→ALT，0xE05B/0xE05C→WIN。返回 Option<u8>（位或语义）。
pub fn modifier_bit(sc: u16) -> Option<u8>;
```

### 4.2 core::event —— 采集层 → aggregator 的事件

```rust
pub struct DeviceKey { pub kind: DeviceKind, pub vid: u16, pub pid: u16, pub name: String }
// vid/pid 未知时为 0；name 兜底规则见 §5.3。

pub enum RawEvent {
    Keyboard { device: DeviceKey, sc: u16, down: bool },
    MouseClick { device: DeviceKey, button: MouseButton },      // 只投递按下边沿；滚轮刻度由采集层折算
    GamepadPress { device: DeviceKey, button: GamepadButton },  // 只投递按下边沿
}
pub enum AggEvent { Input(RawEvent), Foreground { exe: String } }   // Foreground 由 apps 线程发出
```

**DeviceKey 解析契约（collector::device）**：
- hDevice → `GetRawInputDeviceInfoW(RIDI_DEVICENAME)` 得路径 `\\?\HID#VID_%04X&PID_%04X[&REV_..&MI_..]#...`；
  正则提取 `VID_([0-9A-Fa-f]{4})&PID_([0-9A-Fa-f]{4})`；`name` 从注册表 `HKLM\SYSTEM\CurrentControlSet\Enum\HID\VID_..\PID_..\..\FriendlyName` 取，取不到用 `DeviceDesc`，再取不到用 `"HID 设备 {VID:04X}:{PID:04X}"`。
- **hDevice==0（precision touchpad，已文档化）或解析失败或路径非 HID 形态（RDP/虚拟设备）**：
  归入固定桶 `DeviceKey{kind, vid:0, pid:0, name:"未知/虚拟设备"}`，按 kind 分桶（keyboard/mouse 各一）。
- hDevice→DeviceKey 结果按句柄缓存（HashMap），热插拔后新句柄自然重解析；DeviceKey→device_id 缓存由 aggregator 维护。

### 4.3 engine —— 键盘纯状态机

```rust
pub struct Engine { /* held: HashSet<u16>, mods_held: u8, 今天不必存——按参数传入 */ }
impl Engine {
    pub fn new() -> Self;
    /// 输入一个键盘事件，输出 0..=2 条计数（Key 和/或 Combo）。
    /// 语义契约：
    /// 1) down 且 held 中已存在该 sc → 自动重复，返回 None（不产计数），held 不变；
    /// 2) down：插入 held；该键计数 Key{sc} 恒产出（含修饰键自身，修饰键也磨损）；
    /// 3) down 且该键非修饰键且 mods_held != 0 → 产出 Combo{mods: mods_held, code: sc}；
    /// 4) up：从 held 移除（不存在则忽略）；修饰键 up 清除对应 mod 位（L/R 合并位仅在两侧都抬起后清零，
    ///    实现为：按"物理 sc 集合"跟踪，mods_held 每次由 held 中的修饰键重算）；
    /// 5) mods_held = OR(held 中所有键的 modifier_bit)。
    pub fn on_key(&mut self, sc: u16, down: bool) -> EngineOut;
}
#[derive(Debug, PartialEq)]
pub struct EngineOut { pub key: Option<u16>, pub combo: Option<(u8, u16)> }  // (mods, code)
```

**必须覆盖的单测**（合成事件流）：按住 'A' 产生 N 个重复 make → 恰好 1 个 Key；Ctrl+C 连按两次 → 2 个 Combo + 2 个 Key(C)；Ctrl 按住时依次按 C、V → 2 个 Combo（mods 同）；Shift+A 松开 Shift 再按 A → 第二个 A 无 Combo；Pause 的 E1 双事件 → 1 个 Key(0xE11D)；右侧 Ctrl(0xE01D)+X 与左侧等价（mods 同为 CTRL）；纯修饰键按下产 Key 不产 Combo。

### 4.4 core::ipc —— 控制协议

- 管道名：`\\.\pipe\clrecoder-control`；字节模式、PIPE_WAIT、`PIPE_REJECT_REMOTE_CLIENTS`；
  **显式 DACL**：`ConvertStringSecurityDescriptorToSecurityDescriptorW("D:P(A;;GA;;;<当前用户SID>)", ...)`（SID 运行时解析；防计划任务被误配为 SYSTEM 时 GUI 被拒）。
- NDJSON：一行请求（≤8KB，超长即断开）→ 一行响应；UTF-8。

```rust
pub enum CtlRequest { Status, SetPaused { paused: bool }, Shutdown }   // serde tag="cmd", 内部 snake_case
pub struct StatusData { pub paused: bool, pub version: String, pub started_at: String,
                        pub last_event_at: Option<String>, pub events_seen: u64 }
/// 普通结构体（不是 enum——serde 默认枚举表示产不出 {"ok":true} 形状）：
/// 序列化结果恰为 {"ok":true,"data":{...}} / {"ok":false,"error":"..."}
pub struct CtlResponse { pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")] pub data: Option<StatusData>,
    #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<String> }
```

线格式示例（GUI 必须能按此实现客户端）：

```
→ {"cmd":"status"}
← {"ok":true,"data":{"paused":false,"version":"0.1.0","started_at":"2026-09-27T22:00:00+08:00","last_event_at":"2026-09-27T22:14:31+08:00","events_seen":48219}}
→ {"cmd":"set_paused","paused":true}
← {"ok":true,"data":null}
→ {"cmd":"bogus"}
← {"ok":false,"error":"unknown command"}
```

### 4.5 store —— DDL 与读写接口

**schema v1 完整 DDL**（`store::schema::migrate(conn)` 幂等执行，`schema_migrations` 表记录版本）：

```sql
PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;

CREATE TABLE devices(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL CHECK(kind IN ('keyboard','mouse','gamepad')),
  vid INTEGER NOT NULL DEFAULT 0, pid INTEGER NOT NULL DEFAULT 0,
  name TEXT NOT NULL,
  first_seen TEXT NOT NULL, last_seen TEXT NOT NULL,
  UNIQUE(kind, vid, pid, name)
);
CREATE TABLE input_daily(
  device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  day TEXT NOT NULL, code INTEGER NOT NULL, count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(device_id, day, code)
);
CREATE INDEX idx_input_daily_day ON input_daily(day);
CREATE TABLE combo_daily(
  day TEXT NOT NULL, mods INTEGER NOT NULL, code INTEGER NOT NULL, count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(day, mods, code)
);
CREATE TABLE app_daily(
  day TEXT NOT NULL, exe TEXT NOT NULL,
  foreground_secs INTEGER NOT NULL DEFAULT 0,
  key_count INTEGER NOT NULL DEFAULT 0, click_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(day, exe)
);
-- 以下为 WhatPulse 导入镜像（§4.8；每次导入整体重建）
CREATE TABLE wp_import_meta(id INTEGER PRIMARY KEY CHECK(id=1), imported_at TEXT NOT NULL,
  source_path TEXT NOT NULL, source_size INTEGER, date_min TEXT, date_max TEXT, note TEXT);
CREATE TABLE wp_key_daily(day TEXT NOT NULL, qt_key INTEGER NOT NULL, label TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, qt_key));
CREATE TABLE wp_combo_daily(day TEXT NOT NULL, combo TEXT NOT NULL, label TEXT NOT NULL DEFAULT '',
  count INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, combo));
CREATE TABLE wp_app_daily(day TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL DEFAULT '',
  seconds INTEGER NOT NULL DEFAULT 0, keys INTEGER NOT NULL DEFAULT 0, clicks INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(day, path));
CREATE TABLE wp_mouse_daily(day TEXT NOT NULL, clicks INTEGER NOT NULL DEFAULT 0,
  distance_inches REAL NOT NULL DEFAULT 0, PRIMARY KEY(day));
CREATE TABLE wp_mouse_buttons_daily(day TEXT NOT NULL, button_code INTEGER NOT NULL, label TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, button_code));
CREATE TABLE wp_mouse_scroll_daily(day TEXT NOT NULL, direction_code INTEGER NOT NULL, label TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, direction_code));
```

**Writer（collector 用，全部单事务批量）**：

```rust
pub struct Writer { /* Mutex<Connection> + device 缓存 */ }
impl Writer {
    pub fn open(path: &Path) -> Result<Self>;                       // 建目录、migrate、WAL、busy_timeout(10s)
    /// 设备不存在则插入（first_seen/last_seen=now），存在则刷新 last_seen（节流：每 flush 一次）
    pub fn get_or_create_device(&self, d: &DeviceKey) -> Result<i64>;
}
pub struct FlushBatch {
    pub input: Vec<(i64 /*device_id*/, String /*day*/, u16 /*code*/, u64 /*count*/)>,
    pub combos: Vec<(String, u8, u16, u64)>,
    pub apps: Vec<(String /*day*/, String /*exe*/, u64 /*secs*/, u64 /*keys*/, u64 /*clicks*/)>,
}
impl Writer {
    /// 单事务 upsert，INSERT ... ON CONFLICT DO UPDATE count=count+excluded.count。
    /// 失败语义：调用方（aggregator）保留聚合桶、下个 tick 重试、日志限频——flush 失败绝不丢计数、绝不panic退出。
    pub fn flush(&self, b: &FlushBatch) -> Result<()>;
}
/// WhatPulse 导入写入（wp_* 的 SQL 唯一归属仍是 store；import.rs 只做编排与 Rust 侧聚合）
pub struct WpImportBatch { /* 字段 = 各 wp_* 表的行结构 + meta，executor 对齐 §4.5 DDL */ }
impl Writer { pub fn rebuild_wp_tables(&self, b: &WpImportBatch) -> Result<()>; }  // 单事务：清空全部 wp_* 并重建 + 写 wp_import_meta(id=1)
```

**Reader（GUI 用，ro 连接）** —— 每个 GUI 查询命令至少对应一个 reader 函数，全部 SQL 只写在这里：

```rust
pub struct DeviceRow { pub id: i64, pub kind: DeviceKind, pub vid: u16, pub pid: u16,
    pub name: String, pub first_seen: String, pub last_seen: String, pub total: u64 }
pub struct DayCount { pub day: String, pub total: u64 }
pub struct KeyDailyRow { pub day: String, pub code: u16, pub count: u64 }        // label 由 GUI keylabel 补
pub struct TopKeyRow { pub code: u16, pub total: u64, pub label: String }        // label 参数传入由 SQL 外拼接亦可
pub struct AppRow { pub exe: String, pub seconds: u64, pub keys: u64, pub clicks: u64 }
pub struct ComboRow { pub mods: u8, pub code: u16, pub total: u64 }
pub struct AppDayRow { pub day: String, pub exe: String, pub seconds: u64, pub keys: u64, pub clicks: u64 }
pub struct ComboDayRow { pub day: String, pub mods: u8, pub code: u16, pub count: u64 }

pub fn devices(conn) -> Result<Vec<DeviceRow>>;                       // 含 lifetime total（LEFT JOIN SUM）
pub fn daily_totals(conn, from: &str, to: &str, kind: Option<DeviceKind>) -> Result<Vec<DayCount>>;
pub fn today_by_device(conn, day: &str) -> Result<Vec<(i64, u64)>>;
pub fn overview(conn, from: &str, to: &str) -> Result<OverviewData>;  // 一次组合出 §4.7 Overview（today 按 devices.kind 拆分为 keys/clicks/gamepad）
pub fn key_daily(conn, device_id: i64, from: &str, to: &str) -> Result<Vec<KeyDailyRow>>;
pub fn top_keys(conn, device_id: i64, from: &str, to: &str, limit: u32) -> Result<Vec<TopKeyRow>>;
pub fn apps(conn, from: &str, to: &str, limit: u32) -> Result<Vec<AppRow>>;       // 范围聚合，按秒降序
pub fn app_daily_rows(conn, from: &str, to: &str) -> Result<Vec<AppDayRow>>;      // 逐日明细（CSV/JSON 导出用）
pub fn combos(conn, from: &str, to: &str, limit: u32) -> Result<Vec<ComboRow>>;
pub fn combo_daily_rows(conn, from: &str, to: &str) -> Result<Vec<ComboDayRow>>;  // 逐日明细（导出用）
// wp_ 系列：wp_meta(conn), wp_overview(conn,from,to)→WpOverviewData, wp_top_keys(conn,from,to,limit),
// wp_key_daily_rows(conn,from,to), wp_top_combos(conn,from,to,limit), wp_apps(conn,from,to,limit),
// wp_mouse(conn,from,to), wp_mouse_buttons(conn,from,to,limit), wp_mouse_scrolls(conn,from,to,limit)
```

### 4.6 collector 内部线程接口

```rust
// raw_input.rs
pub fn spawn(tx: crossbeam::Sender<AggEvent>) -> std::thread::JoinHandle<()>;
// 内部：注册 keyboard(UsagePage 1,Usage 6) + mouse(1,2) + Consumer Control(UsagePage 0x0C,Usage 1——音量/播放等媒体键)
//   三组 RIDEV_INPUTSINK；message-only 窗口消息循环；
// WM_INPUT→RawEvent；RAWMOUSE ButtonFlags 提取按下边沿；RI_MOUSE_WHEEL/HWHEEL 用 i16 累计器：
// acc += delta; while acc >= 120 { emit WheelUp; acc-=120 }（负向同理）——高分辨率滚轮安全。

// gamepad.rs（gilrs：default-features=false, features=["xinput"]）
pub fn spawn(tx) -> JoinHandle;   // 轮询 next_event()（非阻塞）+ sleep(8ms)；
// ButtonPressed→GamepadPress；ButtonChanged(LeftTrigger2/RightTrigger2, v) 以 v 上穿 0.33 计一次（下穿复位）；
// DeviceKey{kind:Gamepad, vid:0, pid:0, name:"XInput 手柄"}（gilrs xinput 后端 name 恒定、无 VID/PID——
//   所有 XInput 手柄合并为一行，是对 R3 的已声明降级，见 §9.3）。同一连接内缓存。

// apps.rs
pub struct FgState { pub exe: String, pub since: std::time::Instant }   // exe/since 仅由 apps 线程写入
pub fn spawn(tx, fg: Arc<Mutex<FgState>>) -> JoinHandle;
// SetWinEventHook(EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, 自建消息循环线程)；
// HWND→GetWindowThreadProcessId→OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)→QueryFullProcessImageNameW→小写 basename；
// ApplicationFrameHost.exe 特判：EnumChildWindows 找类名 "Windows.UI.Core.CoreWindow" 的子窗口取真实 PID，失败用 "UWP 应用"；
// 解析失败 exe="unknown"。钩子注册后立即 GetForegroundWindow 解析一次并发送**初始** Foreground
// （EVENT_SYSTEM_FOREGROUND 不会对注册时已处于前台的窗口补发）；此后变化时：更新 FgState + tx.send(Foreground{exe})。

// engine_loop.rs
pub fn spawn(rx, writer: Arc<Writer>, flags: Arc<Flags>, fg: Arc<Mutex<FgState>>, status: Arc<RuntimeStatus>) -> JoinHandle;   // 持有 FgState 做秒数归账；status 上报 events_seen/last_event_at（S9 实现）
// Flags{ paused: AtomicBool, shutdown: AtomicBool }
// 聚合结构：HashMap<(i64,day,u16),u64>、HashMap<(day,mods,code),u64>、HashMap<(day,exe),AppAcc>；
// 每 1s tick：检查 shutdown/跨天；每 2s：若脏 → 构造 FlushBatch（含 FgState 增量秒数，按天切分）→ writer.flush → 清桶
//   （flush 失败：保留聚合桶，下个 tick 重试，日志限频）；
// 暂停/恢复：paused=true 时只丢弃 Input 事件，Foreground 照常处理（保持 exe 归属正确）；暂停瞬间先把 FgState
//   增量秒数归账一次并将 since 冻结在暂停时刻，恢复时 since=now——暂停区间不产生秒数，全程无减法无负值；
// DeviceKey→device_id 缓存调 writer.get_or_create_device。
```

### 4.7 GUI Tauri commands（GUI Agent 与前端 Agent 的接口）

Rust 签名（全部 `async fn` + `tauri::async_runtime::spawn_blocking` 包阻塞查询；state 注入 `AppState`）：

```rust
get_overview(from: String, to: String) -> Overview
get_devices() -> Vec<DeviceRow>
get_key_daily(device_id: i64, from: String, to: String) -> Vec<KeyDailyRowLabeled>   // 含 label
get_top_keys(device_id: i64, from: String, to: String, limit: u32) -> Vec<TopKeyRow>
get_apps(from: String, to: String, limit: u32) -> Vec<AppRowLabeled>                 // 含显示名(basename)
get_combos(from: String, to: String, limit: u32) -> Vec<ComboRowLabeled>             // label="Ctrl+Shift+T"
get_wp_meta() -> Option<WpMeta>
get_wp_overview(from: String, to: String) -> WpOverview
get_wp_keys(from: String, to: String, limit: u32) -> Vec<WpKeyRow>                   // 范围内按键聚合的 Top-N（label+total），count 降序
get_wp_combos(from: String, to: String, limit: u32) -> Vec<WpComboRow>
get_wp_apps(from: String, to: String, limit: u32) -> Vec<WpAppRow>
get_wp_mouse(from: String, to: String) -> Vec<WpMouseRow>
get_wp_mouse_buttons(from: String, to: String, limit: u32) -> Vec<WpMouseButtonRow>  // 按按钮码聚合 Top-N
get_wp_mouse_scrolls(from: String, to: String, limit: u32) -> Vec<WpMouseScrollRow>  // 按方向码聚合 Top-N
import_whatpulse(path: String) -> ImportReport
export_data(format: String /*"csv"|"json"*/, scope: String /*"own"|"wp"*/, from: String, to: String, path: String) -> ExportReport
collector_status() -> CollectorStatus        // 普通结构体（非枚举）：{running, paused?, startedAt?, lastEventAt?}，见 TS 块
set_collector_paused(paused: bool) -> Result<(), String>
collector_autostart_enable() -> Result<(), String>    // PowerShell -Verb RunAs 调 scripts/install-collector-task.ps1
collector_autostart_disable() -> Result<(), String>
collector_start_now() -> Result<(), String>           // 先 schtasks /Run，失败回退 runas 直接启动
get_settings() -> Settings
set_settings(patch: SettingsPatch) -> Settings        // gui_autostart 用 tauri-plugin-autostart 落实
```

**TS 契约（ui/src/api/types.ts 必须逐字对齐。锁定决策：所有 GUI→前端的 DTO 一律 `#[serde(rename_all="camelCase")]`——serde 默认按 Rust 字段名原样输出、不做任何转换，因此 DTO 必须显式加该属性；Rust 侧 DTO 与 store 行类型是两个东西，commands 负责映射）**：

```ts
type Range = { from: string; to: string };            // "YYYY-MM-DD"
interface DeviceRow { id: number; kind: 'keyboard'|'mouse'|'gamepad'; vid: number; pid: number;
  name: string; firstSeen: string; lastSeen: string; total: number }
interface TopKeyRow { code: number; total: number; label: string }
interface Overview { days: { day: string; total: number }[];
  today: { keys: number; clicks: number; gamepad: number };
  devices: { id: number; kind: 'keyboard'|'mouse'|'gamepad'; name: string; total: number }[]; }
interface KeyDailyRowLabeled { day: string; code: number; count: number; label: string }
interface AppRowLabeled { exe: string; name: string; seconds: number; keys: number; clicks: number }
interface ComboRowLabeled { mods: number; code: number; total: number; label: string }
interface WpMeta { importedAt: string; sourcePath: string; sourceSize: number | null;
  dateMin: string | null; dateMax: string | null; note: string }
interface WpOverview { days: { day: string; total: number }[]; keysTotal: number;
  combosTotal: number; appsTotal: number; mouseClicksTotal: number }
interface WpKeyRow { day: string; label: string; count: number }
interface WpComboRow { day: string; combo: string; label: string; count: number }  // label 为解析后的友好格式
interface WpAppRow { day: string; name: string; seconds: number; keys: number; clicks: number }
interface WpMouseRow { day: string; clicks: number; distanceMeters: number }       // 米 = 源英寸 × 0.0254（GUI 层换算）
interface WpMouseButtonRow { label: string; total: number }
interface WpMouseScrollRow { label: string; total: number }
interface ImportReport { ok: boolean; keys: number; combos: number; apps: number; mouseDays: number;
  dateMin: string | null; dateMax: string | null; warnings: string[]; durationMs: number }
interface ExportReport { ok: boolean; files: string[]; rows: number }
interface CollectorStatus { running: boolean; paused?: boolean; startedAt?: string; lastEventAt?: string | null }
interface Settings { guiAutostart: boolean; wpDbPath: string | null; firstRunDone: boolean }
interface SettingsPatch { guiAutostart?: boolean; wpDbPath?: string | null }
```

（注：**锁定决策：所有 GUI 对前端的 DTO 用 camelCase**（Rust DTO 显式 `#[serde(rename_all="camelCase")]`），TS 如上；core/ipc/store 内部结构不受此约束。）

### 4.8 WhatPulse 导入契约（对真实库已验证的列）

源：`%LOCALAPPDATA%\WhatPulse\whatpulse.db`（设置可覆盖路径；导入流程：复制到临时文件→只读打开副本→校验必需表存在（缺表跳过并写入 warnings）→按下列聚合→单事务整体重建 wp_* 表→写 wp_import_meta(id=1)→删临时文件）。**整体替换**语义：重复导入即刷新为最新快照。

| wp 目标 | 源表 → 聚合 SQL 语义 | 列映射 |
|---|---|---|
| wp_key_daily | `keypress_frequency(day,hour,key,count)` GROUP BY day,key | qt_key=key; label=`qt_key_label(key)`; count=SUM(count)；跨 profile_id 直接求和（本机单 profile） |
| wp_combo_daily | `keycombo_frequency(day,hour,combo,count)` GROUP BY day,combo | combo=原文（格式实测 `"shift,87"` / `"control,65"`，多修饰为逗号分隔修饰名+Qt 码）；label=解析修饰名（shift→Shift, control→Ctrl, alt→Alt, meta/win→Win）+qt_key_label |
| wp_app_daily | `input_per_application(day,hour,path,keys,clicks)` + `application_active_hour(day,hour,path,msec_active)` 按 (day,path) 外连接 | path=源 path 原样（实测小写+正斜杠）；name=按 path 匹配 `applications.name`（匹配不到用 basename）；seconds=ROUND(SUM(msec_active)/1000.0); keys=SUM(keys); clicks=SUM(clicks) |
| wp_mouse_daily | `mouseclicks(day,hour,count)` + `mousedistance(day,hour,distance_inches)` | clicks=SUM; distance_inches=SUM（展示层换算：米 = 英寸 × 0.0254，换算在 GUI reader/DTO 层做） |
| wp_mouse_buttons_daily | `mouseclicks_frequency(day,hour,button,count)` GROUP BY day,button | button_code=button；label=静态表 {0:"左键",1:"中键",2:"右键",99:"其他"}，其余 `"按钮 {code}"`（WhatPulse 按钮码无公开文档，300+ 疑为扩展/手柄按键，保留原码，语义见 warnings） |
| wp_mouse_scroll_daily | `mousescrolls(day,hour,direction,count)` GROUP BY day,direction | direction_code=direction；label 静态表 {1:"向上",2:"向下",3:"向左",4:"向右"}（推断值，warnings 注明） |

`qt_key_label` 映射契约：0x20~0x7E 可打印 ASCII → 字符本身（大写字母）；已知特殊码静态表（**下列锚定值已对照 Qt 官方 qnamespace.h 核实；实现时仍须以官方头文件为唯一来源生成全表，禁止凭记忆补值**）：0x01000000 Escape、0x01000001 Tab、0x01000002 Backtab、0x01000003 Backspace、0x01000004 Return、0x01000005 Enter(小键盘)、0x01000006 Insert、0x01000007 Delete、0x01000008 Pause、0x01000009 Print、0x0100000A SysReq、0x0100000B Clear、0x01000010~17 Home/End/←/↑/→/↓/PageUp/PageDown、0x01000020~23 Shift/Ctrl/Meta(Win)/Alt、0x01000024~26 CapsLock/NumLock/ScrollLock、0x01000030+0..23 → F1~F24、0x01000061/62 BrowserBack/BrowserForward、0x01000070/71/72 VolumeDown/VolumeMute/VolumeUp、0x01000080~83 MediaPlay/MediaStop/MediaPrevious/MediaNext、0x01000086 MediaTogglePlayPause、0x01001103 AltGr；**Qt 没有 Key_NumPad0..9（小键盘数字即 0x30~0x39 + KeypadModifier），禁止杜撰该段**；其余 → `键 0x{code:X}`。

### 4.9 UI 设计令牌（ui-ux-pro-max 检索产出，前端唯一视觉来源）

`ui/src/theme.css` 必须包含以下完整令牌集（这就是全部令牌；前端不得引入令牌外的颜色/字号/阴影值）：

```css
:root {
  --color-bg:#F0FDFA; --color-card:#FFFFFF; --color-muted:#E8F1F4;
  --color-primary:#0D9488; --color-primary-hover:#14B8A6; --color-accent:#EA580C;
  --color-text:#134E4A; --color-text-muted:#475569; --color-text-placeholder:#94A3B8;
  --color-border:#99F6E4; --color-divider:#E5E7EB; --color-ring:#0D9488;
  --color-danger:#DC2626; --color-positive:#22C55E;
  --chart-1:#0D9488; --chart-2:#0080FF; --chart-3:#EA580C; --chart-4:#14B8A6;
  --chart-5:#22C55E; --chart-6:#EF4444; --chart-7:#94A3B8;
  --font-sans:'Fira Sans',sans-serif; --font-mono:'Fira Code',monospace;
  --text-caption:12px; --text-body:14px; --text-body-lg:16px; --text-title:18px;
  --text-display:24px; --text-kpi:32px;
  --space-1:4px; --space-2:8px; --space-3:12px; --space-4:16px; --space-5:24px; --space-6:32px;
  --grid-gap:16px; --radius-card:8px; --radius-lg:12px; --radius-sm:4px;
  --elevation-1:0 1px 3px rgba(0,0,0,.10); --elevation-2:0 4px 6px rgba(0,0,0,.10);
  --transition:180ms ease;
}
```

组件硬规则（来自 skill quick-reference/ux-guidelines，UI stage 验收项）：数据列 `tabular-nums`/等宽字体；表格可排序+默认值降序；图表 >15 类别改表格（Top N 柱图 N≤15 + 完整表格并存）；折线 ≤6 序列且带直接标签；图例可见可切换；悬浮 tooltip 键盘可达；空状态给引导文案+动作按钮；加载用骨架屏；数字千分位/缩写（1.2K）；日期本地化；hover 反馈 150-300ms；禁仅用颜色传达语义；桌面侧边导航（图标+文字，当前项高亮，宽 240px）；动画尊重 `prefers-reduced-motion`。

### 4.10 导出契约

- CSV：UTF-8 **带 BOM**（Excel 中文兼容）；每视图一文件，`{device}` = 设备 id（数字，避免设备名中的空格/中文/非法字符进文件名）：`keys_{device}_{from}_{to}.csv`（date,key,code,count，来自 key_daily）、`apps_{from}_{to}.csv`（date,exe,name,seconds,keys,clicks，来自 app_daily_rows）、`combos_{from}_{to}.csv`（date,mods,code,label,count，来自 combo_daily_rows）、`devices.csv`；WhatPulse 侧对应 `wp_keys_/wp_apps_/wp_combos_/wp_mouse_...csv`（列 = §4.7 同名 TS 类型字段）；导出目录由 tauri-plugin-dialog 保存框决定。
- JSON：单文件全量 `{ schema_version:1, generated_at, range:{from,to}, devices:[DeviceRow], input_daily:[KeyDailyRowLabeled 全设备逐日], combos:[ComboRowLabeled 聚合], apps:[AppRowLabeled 聚合], whatpulse:{meta:WpMeta, keys:[WpKeyRow], combos:[WpComboRow], apps:[WpAppRow], mouse:[WpMouseRow], buttons:[WpMouseButtonRow], scrolls:[WpMouseScrollRow]} }`（scope=own 时省略 whatpulse 节点；数组元素形状 = §4.7 同名 TS 类型）。

---

## 5. 核心流程设计

### 5.1 启动与互相发现

1. **collector**：计划任务 ONLOGON（`/RL HIGHEST`，交互会话）拉起 → 创建全局互斥体（第二实例立即退出）→ 打开/迁移 DB → 起三采集线程 + aggregator + pipe server → 就绪。
2. **GUI**：HKCU Run 拉起 → single-instance 插件（重复启动则聚焦已有窗口）→ 托盘（无窗口启动，点击托盘才 show）→ 打开 ro 连接（DB 不存在/短暂 BUSY：命令返回空数据 + 前端显示引导态，不报错弹窗）→ 每 1s 前端轮询今日数据；`collector_status()` 通过 pipe 探测（连接超时 500ms）。
3. **首次安装**：GUI 设置页"启用采集器自启"→ PowerShell `Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File scripts\install-collector-task.ps1'`（一次 UAC；**必须带 -ExecutionPolicy Bypass**——客户端默认 Restricted，提权也不改变策略，-File 会被直接拒绝）→ 任务内 `/TR` 写 collector.exe 绝对路径（GUI 运行时用 `current_exe().parent()` 解析；安装包由 bundle.resources map 落盘到主 exe 同目录，见 §8-S13）→ 成功后 `collector_start_now()`。

### 5.2 键鼠事件处理（正常流）

WM_INPUT(keyboard) → normalize_scancode → 修饰键? 只更新 Engine 状态并计数该键 : Engine.on_key 产 Key/Combo → aggregator：device_id 解析、`(device,day,code)` 计数 +1、当前 exe 的 key_count+1（**仅物理按下边沿计数，自动重复不计**——穿戴统计语义）。
WM_INPUT(mouse) → ButtonFlags 按下边沿 → MouseClick（滚轮按 §4.6 累计器）→ `(device,day,code)` + 当前 exe click_count+1。

### 5.3 异常与边界

| 场景 | 处理 |
|---|---|
| hDevice=0 / 非 HID 路径 / RDP | 归入 "未知/虚拟设备" 桶（§4.2），照常计数 |
| UAC 安全桌面 / 锁屏 | 系统级收不到输入，数据缺口属预期（README 说明） |
| 前台是提权程序 | collector 已提权，正常统计（这是 R10 的实现基础） |
| 跨天（23:59:59→00:00:00） | 事件按到达时的本地日期入桶；flush 按桶内 day 写行；FgState 秒数在日期边界切分到两个 (day,exe) |
| 暂停 | pipe 置 paused → aggregator 丢弃 Input 事件但保留 Foreground；暂停瞬间先把 FgState 增量秒数归账一次并冻结 since，恢复时 since=now——暂停区间不产生秒数，全程无减法无负值 |
| 媒体键（音量/播放等） | Consumer Control 页已注册（§4.6），以键盘事件进统计，键名由 GetKeyNameTextW 显示 |
| collector 崩溃 | 最多丢未 flush 的 2s 增量（接受）；重启后从新事件继续 |
| GUI 打开时 collector 正在 flush | WAL 读写互不阻塞；ro 连接设 busy_timeout=5000ms；GUI 查询失败返回空+前端容错重试（React Query） |
| 手柄拔插 | XInput 槽位状态由 gilrs 报 Connected/Disconnected；所有 XInput 手柄按固定名合并为一行（gilrs xinput 无 VID/PID 且 name 恒定——对 R3 的已声明降级，见 §9.3） |
| 触发器（LT/RT） | 模拟量上穿 0.33 计 1 次，回落后才可再计 |
| DB 损坏 | 打开失败→collector 日志+退出码非 0；GUI 显示引导（删除 stats.db 重建的说明） |

### 5.4 WhatPulse 导入流程

选文件（默认探测 `%LOCALAPPDATA%\WhatPulse\whatpulse.db`）→ 复制到 `%TEMP%\clrecoder-wp-{ts}.db` → 只读打开 → 按 §4.8 逐表校验+在 Rust 侧聚合（缺表→warning，继续）→ 调 `store::Writer::rebuild_wp_tables` 写入（wp_* 的 SQL 唯一归属 store；GUI 临时 rw 连接 busy_timeout=10s）→ 删临时文件 → 返回 ImportReport（行数/日期范围/warnings）→ 前端刷新 WhatPulse 页。校验锚点：`wp_key_daily` 总数必须等于源库 `SELECT SUM(count) FROM keypress_frequency`；自动化测试用**合成 fixture 库**（按 §4.8 源表 schema 造数）断言相等，真实库校验列为【人工】（测试在真实库缺失时 skip）。

### 5.5 导出流程

选格式/scope → 日期范围（默认全部）→ dialog 保存框 → 按 §4.10 生成 → ExportReport → opener 插件"打开所在文件夹"。

---

## 6. 数据存储与状态设计

- **唯一业务存储**：`%LOCALAPPDATA%\ClRecoder\stats.db`（SQLite WAL）。数据量估算：3 设备×365 天×~120 码 ≈ 13 万行/年——无需分区、无需清理，**永久保留**（用户唯一删数据途径：手动删文件，README 说明）。
- **设置**：`%LOCALAPPDATA%\ClRecoder\settings.json`：`{ "gui_autostart": bool, "wp_db_path": string|null, "first_run_done": bool }`。损坏/缺失→默认值重建。暂停状态**不持久化**（collector 重启即恢复统计）。
- **自启注册**：GUI 自启=tauri-plugin-autostart（HKCU Run）；collector 自启=计划任务 `ClRecoderCollector`（ONLOGON、HIGHEST、当前用户）。
- **状态归属**：统计状态只在 DB；运行状态（paused/started_at）只在 collector 内存（pipe 查询）；UI 侧无本地状态持久化（React Query 缓存即用即弃）。

---

## 7. 与现有代码的兼容方案

本仓库为**空仓库**（已核实），无存量代码兼容问题。真正的兼容面是三个外部系统：

1. **WhatPulse 共存**：两者可同时运行（Raw Input 支持多消费者注册，互不干扰）。对本软件的三条铁律：只读；先复制后打开；绝不写 WhatPulse 目录。导入期间 WhatPulse 正在写库是常态——复制出的快照可能缺最新几分钟数据，可接受（报告里带导入时间戳）。
2. **ui-ux-pro-max skill**：令牌已固化进 §4.9（本 plan 内嵌，前端 Agent 无需访问 skill）。若 UI Agent 在本机（skill 位于 `~/.zcode/skills/ui-ux-pro-max`），允许查阅其 quick-reference 补充组件细节，但**令牌值以 §4.9 为准**，不得引入令牌外颜色。
3. **Windows 环境**：Win10 1803+（WebView2 随系统分发）；NSIS per-user 安装（%LOCALAPPDATA%），数据目录同盘符用户目录，无 Program Files 写权限问题；计划任务需 UAC 一次。

---

## 8. Stage Map（workflow 调度蓝图）

> 硬依赖=必须串行；无依赖=可并行。验收点即 workflow 的 verify 关卡，全部可机器判定（标注【人工】的除外）。
> 通用验收（每个代码 stage 隐含）：`cargo build --workspace` 通过、`cargo clippy --workspace -- -D warnings` 无告警、不引入 §9 白名单外依赖。

| # | 目标 | 文件 | 依赖 | 验收点 |
|---|------|------|------|--------|
| S1 | 工作区脚手架：workspace Cargo.toml、tauri v2 项目（src-tauri+ui，React-TS-Vite-Tailwind）、core/store/engine/collector 骨架、rust-toolchain、.gitignore；**一次性写入 §9.1 白名单全部依赖到 [workspace.dependencies] 与各成员；collector/src/main.rs 建 mod 声明 + 各模块空文件（S5-S8 填充即编译）** | Cargo.toml, src-tauri/*, ui/*, 各 crate 骨架 | 无 | `cargo build --workspace` 与 `npm --prefix ui run build` 均通过；`cargo tauri dev` 拉起窗口列【人工】（长驻进程不可作自动关卡） |
| S2 | core 契约实现：codes/event/ipc/qtkeys/day + 单测 | crates/core/* | S1 | `cargo test -p clrecoder-core` 全绿；qtkeys 表含 §4.8 全部条目；IPC serde 往返逐字对齐 §4.4 示例；crates/core/Cargo.toml 无 windows 依赖（纯度达标） |
| S3 | engine 纯状态机 + §4.3 全部单测 | crates/engine/* | S2 | `cargo test -p clrecoder-engine` 全绿（列出的 7 组用例必须存在且通过） |
| S4 | store：schema/migrate/writer/reader + 单测（临时库） | crates/store/* | S2 | `cargo test -p clrecoder-store`：建表幂等；flush 两次同键计数翻倍；rebuild_wp_tables 单事务重建；reader 各查询返回 §4.5 形状 |
| S5 | raw_input 采集 | collector/src/{raw_input.rs,device.rs} | S2 | `cargo build -p clrecoder-collector` 通过 + 设备路径解析/滚轮累计器单测通过；真实输入冒烟并入 S9/S12【人工】清单 |
| S6 | gamepad 采集（gilrs/xinput） | collector/src/gamepad.rs | S2 | `cargo build -p clrecoder-collector` 通过；Cargo.toml 中 gilrs 为 default-features=false + features=["xinput"]（grep 可判定）；真实手柄验证并入 S12【人工·无手柄可 SKIP】 |
| S7 | apps 前台跟踪 | collector/src/apps.rs | S2 | `cargo build -p clrecoder-collector` 通过 + exe 解析纯函数单测通过；真实切窗冒烟并入 S9/S12【人工】清单 |
| S8 | pipe 服务端 + Flags | collector/src/ipc_server.rs | S2 | 集成单测：起 server→客户端 status/set_paused/shutdown 三请求往返符合 §4.4 示例 |
| S9 | collector 组装：main、engine_loop（聚合/flush/跨天/暂停）、selftest 汇总 | collector/src/{main.rs,engine_loop.rs,selftest.rs} | S3,S4,S5,S6,S7,S8 | 自动：`cl-recoder-collector --selftest-inject --db <临时库>`（合成事件注入 aggregator→store 全链路，无需真实输入；先删旧库）后 sqlite 断言 input_daily/combo_daily/app_daily 行正确、暂停区间秒数为 0、进程工作集 <100MB。人工：物理打字/滚轮/暂停冒烟并入 S12 清单 |
| S10 | GUI Rust：state/db/keylabel/全部 commands/export/import | src-tauri/src/** | S2,S4 | `cargo test -p cl-recoder`：import 断言——对**合成 fixture 库**（按 §4.8 源 schema 造数）导入后 `wp_key_daily` 总数==源 SUM；真实库测试存在但文件缺失时 skip；keylabel 对 0x1D→"Ctrl" 类输出；export 产出带 BOM CSV 且 JSON 可解析 |
| S11 | 前端全部页面/组件/theme.css（先用 §4.7 契约 mock 数据开发） | ui/src/** | S1 | `npm run build` 通过；8 页路由可导航；图表/表格满足 §4.9 硬规则清单（排序/Top15+表格/空态/骨架屏/tabular-nums）【人工抽查 3 页】 |
| S12 | 集成联调：前端↔commands 真实接线、托盘行为、设置页动作、首启引导 | ui/src/api/client.ts, src-tauri/src/main.rs | S9,S10,S11 | 【人工】端到端清单：托盘点击显窗/关闭缩托盘；仪表盘实时数字随打字增长；暂停生效；WhatPulse 页展示 1.5 年历史且与导入报告一致；导出文件可开；采集器空闲 CPU<1%、内存<50MB（任务管理器观察）；重启后自启生效（任务存在+采集在跑） |
| S13 | 发布打包：bundle.resources 带上 collector、图标、README、安装自检 | tauri.conf.json, scripts/*, README.md | S12 | `cargo tauri build` 产出 NSIS；bundle.resources 必须**map 形式** `{"../target/release/cl-recoder-collector.exe": "cl-recoder-collector.exe"}`（禁止列表形式——`../` 会被改名 `_up_` 落进子目录，计划任务路径断裂）；安装后 `schtasks /Query /TN ClRecoderCollector` 存在；采集进程完整性级别 High（`whoami /groups` 含 S-1-16-12288）【人工】 |

并行分组：`S1 → S2 → [S3 ∥ S4 ∥ S5 ∥ S6 ∥ S7 ∥ S8] → S9 → S12 → S13`；`S2,S4 → S10`（可与 S5-S9 并行）；`S1 → S11`（可与 S2-S10 全程并行，S12 收口）。

---

## 9. 给执行 Agent 的约束

### 9.1 架构决策 —— 禁止更改

1. 两进程拓扑（提权 collector + 中完整性 GUI）、SQLite-WAL 为数据集成点、pipe 只传控制指令。
2. §3 文件归属：模块边界不得挪动（如禁止把 SQL 写进 commands、把键名翻译塞进 collector）。
3. §4 全部类型/DDL/协议/令牌：字段名、取值域、线格式逐字对齐；需要新增字段必须走"最小增量且向后兼容"（新可选字段）。
4. 依赖白名单（禁止未列出的直接依赖）：
   - Rust：`windows =0.62`（features: Win32_Foundation, Win32_UI_Input, Win32_UI_WindowsAndMessaging, Win32_UI_Input_KeyboardAndMouse, Win32_UI_Accessibility, Win32_System_Pipes, Win32_Security, Win32_Security_Authorization, Win32_System_Registry, Win32_System_Threading, Win32_System_LibraryLoader, Win32_Storage_FileSystem, Win32_System_IO（S8：命名管道 OVERLAPPED 系 API 门控）, Win32_System_Console（GetConsoleWindow 隐藏生产控制台））、`gilrs 0.11(default-features=false, features=["xinput"])`、`rusqlite 0.4x(features=["bundled"])`、`serde 1`、`serde_json 1`、`chrono 0.4`、`crossbeam-channel 0.5`、`regex 1`、`dirs 5|6`、`tauri 2(tray-icon,image-png)`、`tauri-plugin-single-instance 2`、`tauri-plugin-autostart 2`、`tauri-plugin-dialog 2`、`tauri-plugin-opener 2`。
   - 前端：react 18、react-dom、typescript、vite 5|6、tailwindcss 4、recharts 2|3、@tauri-apps/api 2 及对应 plugin-JS 包、@tanstack/react-query 5。
5. 禁止引入 tokio/axum/任何网络服务；禁止遥测；禁止修改 PLAN.md 之外的规范文件（无存量代码，无"顺手重构"风险，但禁止创建本 plan 未列出的顶层目录）。
6. §4.3/§4.8 中标注"必须实现/必须存在"的非显然算法与映射表内容不得简化。

### 9.2 实现决策 —— executor 自主

变量命名、私有函数拆分、错误类型细化（thiserror 可自行加入白名单）、消息循环写法、缓冲策略、日志宏选型（log+env_logger 或简单 println 均可，但 release 默认静默）、注册表读取方式、Dialog/托盘菜单项文案微调、图表动效细节（遵守 §4.9 规则内）。

### 9.3 不确定点的既有裁决（本 plan 已替你决策，供主人复核）

| 决策点 | 采用 | 被放弃的备选 |
|---|---|---|
| 手柄技术路线 | gilrs(xinput)：后台可靠、Xbox 系覆盖好 | Raw Input HID 解析（能拿到 VID/PID、覆盖 PS 手柄，但描述符解析风险高、Xbox 360 dongle 不可见）→ 留作 v2 |
| 手柄设备身份 | 固定名"XInput 手柄"单行合并（gilrs xinput 后端 name 恒定、无 VID/PID——**对 R3 的显式降级：v1 手柄只有"全部手柄合计"粒度**） | 槽位级（拔插/换口会分裂统计，对累计视角伤害更大）；Raw Input HID 解析（v2 路线，可拿 VID/PID） |
| WhatPulse 按钮码/滚轮方向语义 | 保留原码 + 高置信静态标签（左/中/右键）+ 其余原码直显；滚轮方向为推断标签并在 ImportReport.warnings 声明（其内部编码无公开文档） | 全量猜测性硬编码映射（错误风险）；纯原码展示（用户读不懂） |
| 前端栈 | React 18 + Vite + Tailwind v4 + Recharts | Svelte（生态/图表库弱）、纯手写 DOM（成本高） |
| 应用时长语义 | 前台墙钟时长 | 扣除空闲（GetLastInputInfo）→ 可作未来开关 |
| 导入语义 | 整体替换快照 | 增量合并（去重复杂度不值） |
| 自动重复按键 | 不计入（物理按下边沿） | 计入（WhatPulse 语义未知，对磨损统计是噪声） |
| TS 类型生成 | 手写 types.ts（契约在 §4.7） | tauri-specta（额外复杂度） |

### 9.4 编码规范

Rust 2021 edition；`unsafe` 仅限 windows API 调用点并在文件内集中；公开 API 全部文档注释（中文）；错误处理 `Result` 贯穿，collector 顶层禁止 panic（采集线程 catch_unwind 兜底记录后继续）。前端：函数组件 + hooks；禁 any；组件文件 ≤300 行拆分。
