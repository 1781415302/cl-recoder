# 更新记录

## 未发布

- 鼠标页支持设置/清除型号昵称（同型号多只鼠标共用）；修复昵称输入框在输入法组合输入时回车误提交。

## v0.4.1（2026-10-16）

本次发布汇总 v0.4.0 开发阶段的实时输入、扳机热力、界面改版和应用内更新；统计库继续使用 schema 4。v0.3.0 的安装与安全改动也包含在本版中，从已发布的 v0.2.x 升级时请遵循 README 的迁移说明。

- 修正 Tauri 开发与构建钩子的工作目录，支持从仓库根目录直接构建安装包。

对应方案：实时输入展示、扳机热力与外设布局（`DEVPLAN-input-live-trigger`）；验收记录：[docs/ACCEPTANCE-input-live-trigger.md](docs/ACCEPTANCE-input-live-trigger.md)。

### 实时输入

- 键盘 / 鼠标 / 手柄分布图可显示当前按住状态与短按反馈（约 100ms）；统计暂停时仍可看实时动作，不写入历史。
- 手柄图内展示左右摇杆当前位置与 LT/RT 行程百分比；实时态来自独立只读管道 `\\.\pipe\clrecoder-live-v1`，不改动既有 control 协议。
- GUI 每 40ms 至多一笔快照请求；页面隐藏或卸载时停止请求；live 不可用时只提示实时不可用，不清空历史列表。

### 扳机热力（历史）

- 新增 LT/RT 累计停留热力条（0～100%、101 格），左右共同色标；与既有 gilrs 0.33 按压计数相互独立。
- 热力仅历史：不接收实时位置；旧按钮次数无法重建连续热力，需新采集器开始记录。

### 外设图与交互

- 统一浅色配色、导航、卡片与设置页；日期范围改为快捷分段与自定义日期。
- 重做鼠标、手柄分布图外形与控件布局；键盘保持 ANSI-104，三种外设均支持切换按键频次热力。
- 鼠标左右键的绘制与点击使用同一轮廓，滚轮区域独立；分布卡按容器宽度调整布局，避免小窗口汇总内容溢出。
- 手柄模拟量按具体 XInput 连接选择；鼠标实时按物理来源（sourceKey）匹配；历史仍按原型号聚合。

### 数据与升级

- 统计库 schema 升至 **v4**（追加扳机热力表，不替换既有运动表）。v0.3.0 采集器不支持 v4 并会拒写，**GUI 与 collector 须同批升级**。
- 升级前正常退出 GUI 与 collector，备份 `stats.db` 及未合并 WAL、`settings.json` 与旧程序后再替换。若已迁移 schema 4，回退旧 collector 必须恢复升级前数据库备份，不能仅换回旧 exe。
- 新版 GUI 读 v3 库仍可看旧数据，扳机热力提示需更新采集器；导出格式版本适配 v4，旧字段语义不变。

### 收尾

- 鼠标左右键命中区与绘制轮廓一致（含焦点环），并清理相关失效 CSS。
- 删除未使用的 `TopBarChart` 组件；取消对 `output/playwright/*.png` 截图的 Git 追踪并移除仓库内副本。
- 新增 GitHub Actions CI（`frontend` / `rust` / `scripts` 三个 job）。

### 应用内更新

- 设置页新增「软件更新」：手动检查 GitHub Release，确认后下载、校验、停止采集器、备份统计库并以一次 UAC 拉起安装包（被动更新）；安装完成后自动重开 GUI 并尝试拉起采集器。
- 仅安装版布局（程序位于 `%ProgramFiles%` 下）可自动安装；便携版只提供打开发布页。不启动自动检查、不后台轮询。
- 完整性依赖 Release 资产的 GitHub `digest`（sha256）；缺失或不符时不提供自动安装。`digest` 不能防发布账号被攻破；发布物仍未代码签名。
- 资产名约定 `CL-Recoder-<ver>-windows-x64-setup.exe`。纯逻辑在 `clrecoder-update` crate（Linux 可测），Windows 胶水与设置页 UI 接线完成。

### 整改

- 为既有 `unsafe` 块补充具体 `// SAFETY:` 不变量说明，并在 collector / GUI 主 crate 启用 `clippy::undocumented_unsafe_blocks` 警告。
- 导出路径用 `enum_wire_name` 取代非测试代码中的 `unwrap`，单测对齐四类枚举线格式。
- 将 `theme.css` 按原分节拆为 `ui/src/styles/NN-*.css`，构建产物 CSS 与拆分前逐字节一致。
- 前端接入 eslint（flat config）与 vitest；CI frontend job 在 typecheck 后增加 `lint`。

### 兼容保留

- 控制协议、脚本、按钮编码、暂停/恢复、窗口活动、DPI、鼠标距离、摇杆 R 行程与热力、日期查询、WhatPulse、明细与导出均保持原口径。
- WebView 软件渲染 / 恢复配置与 RTSS 相关行为不变。


## v0.3.0（未发布）

对应方案：[docs/DEVPLAN-hardening-v4.md](docs/DEVPLAN-hardening-v4.md)；验收记录：[docs/ACCEPTANCE-hardening-v4.md](docs/ACCEPTANCE-hardening-v4.md)。

### 安全与安装

- 安装包改为按机器安装（`perMachine`，`%ProgramFiles%\CL Recoder\`），安装时需要一次 UAC。从 v0.2.x 升级请按 README 的按用户安装迁移步骤操作。
- 采集器自启脚本在创建或修复计划任务前检查采集器路径权限，普通用户可写时拒绝（退出码 7，只读）；修复时核对任务实际执行目标，不一致报冲突（退出码 5，只读）。
- 发布版 GUI 不再从当前工作目录查找提权脚本。
- 安装版 GUI 在设置页对仍指向旧按用户安装位置的采集器任务或开机自启项给出只读提示。

### 正确性

- 锁屏/解锁、控制台或远程连接、睡眠/唤醒时清空键盘按下状态，避免修饰键残留导致组合键多记（防御性改动，旧版复现情况见验收记录）。
- 统计库 schema 版本高于程序支持值时，采集器拒绝写入（退出码 3），GUI 显示升级横幅。
- 导出的各文件出自同一数据库快照。
- CSV 导出对外部来源文本单元格加 `'` 前缀以防公式注入（JSON 不变；列清单见 README“已知边界”）。
- WhatPulse 导入改用 SQLite Online Backup 取一致快照，失败时退回文件复制。

### 性能

- 仪表盘概览对两天前及更早的数据按天缓存，轮询仅查询“昨天 + 今天”；数值与全量查询逐项相等。
