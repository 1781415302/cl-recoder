# 实时输入、扳机热力与外设布局（v0.4.0）验收记录

日期：2026-10-14。分支：`feature/input-live-trigger-S10`（汇合 S0–S9）。对应方案：`DEVPLAN-input-live-trigger`（staging 编排副本；不替代主仓 docs 中的既有方案文件）。

工程实现与可在 Linux / wine 上执行的自动验证由本步（S10）汇总。凡须在真实 Windows 键鼠/手柄、锁屏、睡眠与安装迁移环境确认的项目均**未验收**，本文不以 unit / mock / wine 结果冒充真机通过。

## 真机验收状态

| 编号 | 内容 | 状态 |
|---|---|---|
| M1 | 实时低延迟可读：键鼠/手柄按下、短按、摇杆与扳机行程在分布图上及时反馈 | 未验收 |
| M2 | 连续轻按扳机产生热力；raw0/1 持续无热度；越过既有 0.33 阈值仍只增加一次按钮计数 | 未验收 |
| M3 | 统计暂停后实时仍显示；历史热力与按钮计数不再增长 | 未验收 |
| M4 | 页面隐藏期间零 live 请求；恢复后不回放陈旧点亮 | 未验收 |
| M5 | 旧 v3 库继承：升级前后旧行与配置一致；扳机热力 needs_upgrade 直至新 collector 写入 | 未验收 |
| M6 | GUI / collector 同批升级；schema 4 迁移成功；`collector --version` 为 0.4.0 | 未验收 |
| M7 | 回退：恢复升级前 DB 备份后旧 collector 可写；仅换 exe 不可继续写 schema 4 | 未验收 |
| M8 | Win+L / 会话锁定、睡眠恢复后不保留陈旧实时态；gamepad 门控正确 | 未验收 |
| M9 | 安装/便携路径与任务策略在真实 PowerShell 5.1 / 7 宿主上通过 | 未验收 |
| M10 | 四槽 XInput 连接选择、ABXY/LB/LT/RB/RT 不串位、Guide 不可用 | 未验收 |

手工门（方案 §8.2）在缺少停止/替换当前生产采集器授权时一律标未执行。

## 自动验证证据（计划在 S10 回归中记录）

下列门禁由本步执行并回填摘要（详见本文件末「S10 回归摘要」）：

- `cargo test --workspace --locked`（Linux 原生可编译 crate；Windows-only 目标在可用时经 `x86_64-pc-windows-gnu` + wine）
- `cargo clippy --workspace --all-targets --locked -- -D warnings`（至少覆盖非 windows-only crate；Windows 门另报）
- `npm --prefix ui test`（须包含 T1 `live-input.test.mjs`、T2 `input-diagrams.test.mjs`）
- `npm --prefix ui run build`
- 现有路径/任务 PowerShell 测试：在可用宿主上执行；缺 WindowsIdentity / 计划任务时 B 节标未执行
- 临时 v3 DB 升级一致性：由既有 store 迁移测试覆盖；不以生产库试升
- 生产候选构建与 `collector --version=0.4.0`：须独立 `CARGO_TARGET_DIR`，未经部署授权不启动真实库迁移

约束回顾：自动测试使用临时 DB 与专用 pipe，不停止用户正在运行的 collector；浏览器 mock 截图仅作界面参考。

## 与方案的偏差

无。本步（S10）仅版本号、文档、ACCEPTANCE 与整体回归；未改业务源码，未新增第三方依赖。

## S10 回归摘要

日期：2026-10-14（Africa/Dar_es_Salaam，UTC+3）。工作树：`wt-S10` / `feature/input-live-trigger-S10`。基线 tip：`651509c`（merge S9）。

### 版本

| 位置 | 版本 |
|---|---|
| `Cargo.toml` `[workspace.package]` 及 `Cargo.lock` 工作区 6 包 | 0.4.0 |
| `src-tauri/tauri.conf.json` | 0.4.0 |
| `ui/package.json` / `ui/package-lock.json`（根包） | 0.4.0 |
| wine：`cl-recoder-collector --version`（独立 `CARGO_TARGET_DIR=.staging-v3/input-live/input-live-target`，windows-gnu release） | `cl-recoder-collector 0.4.0` |

`ui/package.json` 的 `test` 脚本已收入 T1（`live-input.test.mjs`）与 T2（`input-diagrams.test.mjs`）；无新 npm/crates 依赖。

### 自动测试

| 门禁 | 结果 |
|---|---|
| Linux 原生 `cargo test`（core/engine/store/diagnostics；排除 windows-only collector/GUI） | **通过**：core 64、diagnostics 9、engine 54、store 57 |
| Linux 原生 `cargo clippy --workspace --all-targets --locked --exclude clrecoder-collector --exclude cl-recoder -- -D warnings` | **通过** |
| wine `cargo test -p clrecoder-collector --target x86_64-pc-windows-gnu` | **通过**：227 passed，1 ignored（真实 Win32 只读 heap 名，默认不跑） |
| wine `cargo test -p cl-recoder --target x86_64-pc-windows-gnu` | **156 passed，3 failed，1 ignored**。3 项失败与 hardening-v4 基线相同：`collector_health` 进程探测 2 项、`keylabel::ctrl_scancode_yields_ctrl_like_label`；属 wine 环境差异，需 Windows 复核 |
| wine `cargo clippy -p clrecoder-collector -p cl-recoder --all-targets --locked --target x86_64-pc-windows-gnu -- -D warnings` | **未通过（2 项）**：`crates/collector/src/live_server.rs` — `unnecessary_mut_passed`（`GetOverlappedResult`）、`result_large_err`（`encode_reply`）。属 S4a 模块，**不在 S10 允许改文件清单内**；native clippy 未覆盖该 windows-only 路径 |
| `npm --prefix ui test` | **通过**：110 / 110（含 T1/T2） |
| `npm --prefix ui run build`（tsc + vite） | **通过** |
| PowerShell 7.4.6（Linux）：`collector-path-trust.tests.ps1` | **通过**：65 项断言 |
| PowerShell 7.4.6（Linux）：`collector-task-policy.tests.ps1` | **A 节通过**（约 20 项）；**B 节未执行**（依赖 `WindowsIdentity` / 计划任务，须 Windows PS 5.1/7） |
| 临时 v3→v4 DB 升级一致性 | 由 store 既有迁移测试覆盖（`trigger_v3_db_upgrade_appends_v4_and_preserves_old_rows` 等）；未触碰生产库 |
| 生产候选 NSIS / 真机安装迁移 | **未执行**（无部署授权）；仅构建并运行候选 collector `--version` |

### 风险与后续

1. **Windows clippy 门**：`live_server.rs` 两处 `-D warnings` 失败需在允许改 L2/L3 的后续修复步处理，否则正式 `cargo clippy --workspace --all-targets --target windows-gnu -D warnings` 不能绿。
2. **GUI wine 三失败**：与 v0.3.0 基线一致，不以 wine 通过冒充；须真机复核进程探测与 Ctrl 键标签。
3. **真机门 M1–M10**：全部未验收；不得以 unit/mock/wine 代替。
4. **同批升级**：schema 4 后旧 collector 拒写；回退须恢复 DB 备份。未授权不得替换用户正在运行的采集器。
5. 本步未 push、未 merge、未 release、未改用户主分支。

