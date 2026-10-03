# v3 实施方案评审与裁决记录

日期：2026-10-01。主文档：[DEVPLAN-usability-runtime-v3.md](./DEVPLAN-usability-runtime-v3.md)。本记录是设计评审，不表示功能已实现或已部署。

## 评审前提

需求为此前运行诊断中的任务策略、故障可观测性和后台查询开销，以及默认今日、Xbox标签修正、应用时长格式、统计视觉改进。用户确认TL指RB。保持既有heap/correctness修复、数据库schema v2、输入code与计数语义；只交付工程Plan，禁止评审期间修改源码、生产任务或数据。

先用3个只读子代理探索输入边界、UI/导出和存储/部署，再用4个只读子代理审查初稿。评审者获得需求、工作树、完整Plan及“不扩散需求、不规定显然实现、意见必须定位源码/章节”的约束。主Agent复核证据后逐条裁决；已采纳内容直接合入最终主文档。

| 评审代理 | 责任范围 |
|---|---|
| explore_storage_deploy | 日志初始化/限频、任务更新副作用、权限与部署 |
| explore_input_edges | 手柄code、全历史/区间口径、mock与范围一致性 |
| explore_ui_export | 组件契约、WP单位/身份、可访问性、QA环境 |
| review_time_scope | 今日/午夜竞态、隐藏查询、Stage依赖、允许范围 |

## 逐条裁决

下表位置均为当前基线源码或最终Plan章节，方便执行者查证。重复意见合并，不算成不同缺陷。

| ID | 意见及证据定位 | 裁决与最终约束 |
|---|---|---|
| P01 | 任务XML更新不自动保证无触发副作用或保全安全描述符；原scripts/install-collector-task.ps1使用/Create /F | 采纳。§4.3更新仅0x34，保留principal/logon/SDDL并回读；仅接受本用户InteractiveToken＋LogonTrigger；RepairOnly禁止Create/Run/Stop |
| P02 | crates/collector/src/main.rs:264先安装env_logger；后安装共享log adapter会失败 | 采纳。§4.1先parse mode，Run取得单实例后安装一次adapter；selftest走stderr；不得忽略SetLoggerError或虚报日志可用 |
| P03 | message作为限频key会随文案变化；有界LRU淘汰可能破坏严格60秒限频 | 采纳。§4.1稳定code、63个独立桶＋永久overflow桶，共64项；不淘汰/迁移；只按真正写入更新限频状态 |
| P04 | ui/src/components/DeviceTabs.tsx:71展示DeviceRow.total全历史；默认今日可能让用户误读，原允许清单未含此文件 | 采纳问题，采用最小范围方案。§4.6/S9在DeviceStatsPage紧邻tabs解释全历史；不扩改DeviceTabs，不把其DTO总量改成区间量。输入/时间代理重复意见合并 |
| P05 | ui/src/api/client.ts:111/116 WP鼠标mock丢from/to；mock.ts:400自有鼠标距离固定日期 | 采纳。§4.6/S4透传日期并修mock；WP逐日keys/combos同样必须合法day、无活动范围为空，不以mock掩盖真API语义 |
| P06 | 原DeviceLayoutStats只有onSelect，没有父组件selectedCode输入；范围变更无法可靠清选择 | 采纳。§4.8受控selectedCode:number\|null＋必需onSelect；父页按设备/范围语义reset，导航focus独立于选择 |
| P07 | RankedList无法仅从rows判断API上限 | 采纳。§4.8显式queryLimit；达到上限仅说明本次最多加载N项，不推断全库更多行；所有已查询行可分页 |
| P08 | src-tauri/src/commands/wp.rs:58/73及crates/store/src/reader.rs:635/658证明appsTotal为秒；WhatPulse.tsx:158却显示应用条目，mock.ts:295按条数生成 | 采纳。§4.6/S4 mock算全范围应用秒总和；§4.7/S9 KPI用前台时长和fmtDuration；不改字段名、存储单位或Top-N统计口径 |
| P09 | 原S10真实Tauri窗口QA早于候选构建，且可能影响正在运行GUI | 采纳。§8.4/S10仅Browser mock；真实capability/listen、隐藏/最小化恢复、失焦可见移S12获授权的候选环境，未验证明确交接 |
| P10 | S8新增Settings query先消费policy，但原policy放S9交付，隔离agent无可用接口 | 采纳。§3/§4.5/S3先交付queryPolicy；S8消费，S9再接既有hooks；共享文件写入按硬依赖串行 |
| P11 | “全部时长转换”可能误将ImportReport.durationMs等操作耗时当秒 | 采纳。§1 U3与§4.7明确只指应用统计；导入/探测延迟保留毫秒 |
| P12 | 初始revision=0/inactive若拒绝会永不ready；重试可能重复listener或用getter失败覆盖有效event | 采纳。§4.4初版本null，首0接受；并发start幂等，snapshot失败保留有效event，listener仍在时只重试getter；stop清迟到unlisten |
| P13 | 恢复active时today发布顺序及hook复制range可能发一轮旧日期查询 | 采纳明确化要求；拒绝据此另起架构。复核原设计已要求active/today原子发布，最终§4.4/4.5再锁定render中派生range、禁止effect复制range；没有改为每页定时器或全局range状态 |

## 主Agent补充复核

- 当前src-tauri/tauri.conf.json bundle.resources指向../target/release collector；候选GUI可能打包生产旧exe。最终§8.4锁定独立暂存工程，先collector后GUI，打包后重新核对hash；不覆盖运行中的生产release。
- 接口补齐：新crate包名、TaskPolicy/DiagnosticsInfo前后端签名、Provider/bridge、Range/DateRangePicker、queryPolicy默认与enabled取AND、设备导航/未知码函数、排行函数、DataTable完整props。避免隔离agent猜出口和共享类型。
- Stage写入检查：main/mod顺序S1→S2→S3；client/types/mock顺序S2→S4→S5；keys顺序S4→S5；package顺序S6→S7→S10。独立链可并行，共享文件不以职责不同作为并行理由。
- 源码证据支持修手柄标签而非交换历史；Overview范围SUM修正保留零输入设备；WP主键身份来自既有reader，不猜label，不新增迁库。
- 自动测试关卡继续跳过旧生产IPC测试和条件真WP导入；新fixture只用临时DB/log/假任务runner。编译/Browser测试与真实环境验收分开，禁止用前者冒充后者。

本轮修改仅两份v3设计文档。既有未提交源码属于基线，不在本轮改写、提交或回退。执行者应以最终主文档为准，按§8 Stage Map调度，返回自动验收与环境验收各自的事实。
