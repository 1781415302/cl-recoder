# CL Recoder

本地优先的输入计数器：回答"我的每只外设、每个键，每天/累计被按了多少次"。

- **设备型号维度**：鼠标、键盘、手柄全部按设备型号（VID/PID + 产品名）分开统计；
- **手柄与鼠标完全分离**；组合键（Ctrl/Shift/Alt/Win + 非修饰键）× 每天；
- **应用维度**：前台程序使用时长 + 程序内按键/点击数；
- **数据自主**：本地 SQLite（WAL），导出 CSV/JSON，WhatPulse 历史只读导入到独立命名空间；
  无账号、无云、零遥测。

架构：**两个进程**。`cl-recoder-collector.exe`（提权，计划任务登录自启）负责采集与写库；
`cl-recoder.exe`（Tauri v2 GUI，普通权限，托盘常驻）负责展示、导入导出与控制。两者仅通过
SQLite 文件与本地命名管道交互。详见 [`docs/PLAN.md`](docs/PLAN.md)。

---

## 构建步骤

### 工具链要求

| 工具 | 版本 | 说明 |
|---|---|---|
| Windows | 10 (1803+) / 11，x64 | WebView2 运行时随系统自带，无需单独安装 |
| Rust | stable（`rust-toolchain.toml` 已固定） | 经 [rustup](https://rustup.rs) 安装；首次构建自动下载依赖 |
| Node.js + npm | Node 18+ | 仅用于前端与 Tauri CLI（`npx` 首次运行会联网拉取 `@tauri-apps/cli`） |
| NSIS | 无需手动安装 | Tauri CLI 首次打包时自动下载 |

### 从源码构建

```powershell
# 0. 进入仓库根目录
cd <仓库根目录>

# 1. 安装前端依赖（ui/node_modules）
npm --prefix ui install

# 2. 调试构建 + 全部单测（可选，验证环境）
cargo build --workspace
cargo test --workspace

# 3. 先单独产出 release 采集器（安装包会把该文件打进 resources）
cargo build --release -p clrecoder-collector

# 4. 打包（前端 vite 构建 → GUI release 构建 → NSIS 安装包）
#    必须在仓库根目录运行：tauri CLI 只会向下搜索子目录寻找 src-tauri/tauri.conf.json，
#    在 ui/ 内运行会因找不到项目而报错（ui/ 与 src-tauri/ 是兄弟目录）。
npx @tauri-apps/cli build
```

### 产物

- NSIS 安装包：`target/release/bundle/nsis/CL Recoder_0.1.0_x64-setup.exe`
- 绿色文件（同目录直接运行）：`target/release/cl-recoder.exe`、
  `target/release/cl-recoder-collector.exe`、`target/release/scripts/*.ps1`

> 注意：第 3 步必须先于第 4 步——`src-tauri/tauri.conf.json` 的 `bundle.resources`
> 以 `../target/release/cl-recoder-collector.exe` 为源，采集器不存在会导致打包失败。

---

## 首次使用（三步）

1. **安装 GUI**：双击 `CL Recoder_0.1.0_x64-setup.exe`。NSIS 按当前用户安装
   （默认位于 `%LOCALAPPDATA%` 下，如 `%LOCALAPPDATA%\CL Recoder`），**无需管理员权限**；
   安装完成后启动，程序进驻托盘（点击托盘图标打开面板）。
2. **启用采集器自启**：打开"设置"页 → 点"启用采集器自启"→ UAC 弹窗确认（仅需这一次）。
   成功后软件自动创建计划任务 `ClRecoderCollector`（用户登录时自启、最高权限）并立即启动
   采集器。为什么需要提权：只有提权进程才能收到"发往管理员程序"的输入（UIPI 限制），
   这是统计管理员程序内输入的前提。
3. **完成**：正常打字、点击，仪表盘的今日数字开始增长。GUI 默认随登录自启（设置页可关）。

随时可在设置页"暂停统计/恢复统计"，或"立即启动采集器"（若采集器意外退出）。

---

## 数据与卸载

### 数据存在哪里

| 内容 | 位置 |
|---|---|
| 统计数据库（SQLite，WAL 模式） | `%LOCALAPPDATA%\ClRecoder\stats.db` |
| 设置 | `%LOCALAPPDATA%\ClRecoder\settings.json` |

- 数据**永久保留**、不做自动清理；导出入口在设置页（CSV / JSON）。
- **删除数据 = 删除上述文件**（没有其他途径）。删除前建议先在设置页停用采集器自启并退出
  采集器，避免文件被占用。
- 数据库损坏（GUI 长期显示空/引导态）：停用采集器后删除 `stats.db`，采集器下次启动会自动
  重建空库（历史数据不可恢复，可先用导出功能备份）。

### 卸载（建议顺序）

1. **停用采集器自启**：设置页"停用采集器自启"（一次 UAC）；或以管理员运行
   `scripts\uninstall-collector-task.ps1`；或手动 `schtasks /Delete /TN ClRecoderCollector /F`。
2. **卸载 GUI**：Windows"设置 → 应用 → CL Recoder → 卸载"（或运行安装目录的卸载程序）。
   GUI 自启（注册表 HKCU Run）随卸载清理；也可先在设置页手动关闭。
3. **删除数据**（可选）：卸载程序**不会**删除数据目录，不需要了请手动删除
   `%LOCALAPPDATA%\ClRecoder\`。

### 常用脚本（随程序安装在主程序同目录 `scripts\` 下；仓库内为 `scripts\`）

```powershell
# 安装/重建采集器自启任务（需管理员；GUI 会自动经 UAC 调用）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-collector-task.ps1

# 检查任务是否存在（输出 exists 或 missing）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-task.ps1

# 删除自启任务（幂等，任务不存在时直接成功）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\uninstall-collector-task.ps1
```

---

## 已知边界

- **UAC 安全桌面 / 锁屏 / 登录界面收不到输入**：这些界面位于更高完整性级别的隔离桌面，
  提权采集进程同样不可见，此间数据出现缺口属预期（例如"是"按钮被按下的那次点击不会计数）。
- **同型号设备合并**：Windows 不向普通应用暴露外设序列号，统计键为设备型号
  （VID/PID + 产品名）。两只同型号的鼠标会合并为一行统计。
- **手柄仅支持 XInput（Xbox 系）**：所有 XInput 手柄合并为"XInput 手柄"一行（后端不提供
  VID/PID）；DirectInput / HID 直连手柄（如部分 PS 手柄）暂不统计，留作 v2（Raw Input 路线）。
- **自动重复按键不计入**：按住不松只计 1 次（按物理按下边沿统计，贴合"磨损"语义）。
