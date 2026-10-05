# 连续运动与 DPI 方案评审记录

日期：2026-10-04。最终方案：[外设连续运动、鼠标 DPI 与统计页面实施方案](./DEVPLAN-motion-dpi.md)。本记录为设计裁决；新功能尚未实现或验收。

## 评审范围

用户已确认：左右摇杆停留时间热力图及连续推动行程；每个鼠标自动读取当前 DPI 或手动填写；修正距离口径；优化鼠标、手柄页面；保留上轮最终构建、脚本部署和真实验收收尾。方案供独立 context 的 workflow agent 执行，接口与依赖必须自包含，实现细节放权。

基线为 d86de51 及执行前工作树。三个只读评审代理均获得完整需求、基线、Plan路径及禁止扩需求/改源码/操作生产状态的边界：

| 代理 | 责任 |
|---|---|
| review_motion_math | 采样、去噪、暂停、时间、断连、flush/退出 |
| review_dpi_protocol | HID++/Windows报告、设备关联、缓存与物理距离误差 |
| review_motion_store_ui | 迁库、命令/导出、UI接口、旧数据可访问性、workflow编译依赖 |

## 逐条裁决

| ID | 证据与意见定位 | 裁决与最终方案 |
|---|---|---|
| C01 | gilrs 0.11.2 gamepad.rs:181–202及ev/filter.rs:151–161：默认deadzone使用另一轴缓存；Plan原§4.3读gilrs缓存不是真正原始二维位置 | 采纳。§4.3直接XInputGetState完整四轴，按i16正负范围归一化；旧gilrs按钮路径不改。新增xinput_motion.rs及明确Windows feature，不把入口选择留给执行者 |
| C02 | collector/main.rs:240先join aggregator；raw_input.rs:125仍阻塞GetMessage；engine_loop.rs:538仅排当时channel。原Plan要求退出排尾却无生产者完成顺序 | 采纳。§4.3.1 producer_stop/drained屏障，先Raw排尾与生产者停止，后aggregator排空/最后flush；新RawInputRunner stop_and_join接口 |
| C03 | ipc_server.rs:67/405只有paused bool；两个采样间暂停又恢复可能都见false，跨暂停积路程 | 采纳。§4.1/4.3.1一致Mutex控制快照与epoch，真实沿改变epoch；IPC线形状保持，S4允许内部Flags修改，S5验短暂停反例 |
| C04 | 原§4.2四帧0/20/1020/1040ms保持示例与dt>250ms丢弃矛盾 | 采纳。明确中间每20ms有完整同点帧；另测两帧相隔1秒不补段，保留250ms上限 |
| C05 | 原§4.2单调dt与UTC区间差在容差内时，跨午夜分配比例未定义 | 采纳。按UTC午夜比例分配单调dt，末段保留微秒余差；UTC不递增reset；给120ms墙钟/20ms单调各归10ms示例 |
| C06 | math评审复核Writer/aggregator原空批次判断：只保持无按钮时可能被提前返回 | 采纳。§4.4两处判空均纳入motion，S5验纯保持数据落库；新旧所有增量同事务回滚/合回 |
| C07 | ContainerId是功能节点分组；Logitech协议0xFF既可有线也可receiver，原§4.3未锁判定 | 采纳。devnode/USB祖先＋同ContainerId唯一endpoint；0x0005 GetDeviceType必须Mouse=3，Receiver=7或无法证明拒绝自动读取，不扫描无线槽位 |
| C08 | HIDP_CAPS输入/输出长度含report ID；原20字节协议形状不能直接证明Windows缓冲长度 | 采纳。§4.3限定本阶段CAPS两方向恰20、声明长报告；按实际report ID解析7/20有效帧，使用overlapped WriteFile/ReadFile |
| C09 | 0x2201只有查询当前DPI；2秒轮询不能精确定位硬件切档时刻 | 采纳。将主张收敛为最近有效读数分段；切档到下一成功读取存在误差，UI称估算，测试新读数发布边界；不扩大成未验证的档位通知实现 |
| C10 | CancelIoEx不等待且不保证驱动取消；原“整次1500ms返回且等待收尾”不可同时承诺 | 采纳。1500ms为业务接受期限；独立slot持有pending IO资源至完成，过期发布Unavailable、不接受迟到结果、不另起probe；输入/manual刷新与最终统计flush不被阻塞 |
| C11 | engine_loop.rs:298–318、selftest.rs:251–267穷尽AggEvent；engine_loop.rs:495构造FlushBatch；main.rs:221旧spawn签名。原S1/S2/S4变更使S3/S4在S5前不能编译 | 采纳问题并重划归属。S1仅类型/tracker；S2补旧FlushBatch构造Default；S3声明HID模块；S4把鼠标事件、consumer和调用方一起完成；S5接手柄。共享文件严格串行，不用丢弃事件的临时match冒充验收 |
| C12 | DeviceStatsPage.tsx:59–69已有型号入口；旧schema或无新source时，原新API只接受sourceId使旧按钮/距离不可访问 | 采纳。§4.5新增独立getMouseLegacy(deviceId)；§4.6 source/model联合选择，fallback useDevices与旧按钮查询。旧历史不归某个物理source，不伪造来源 |
| C13 | import.rs:676–678基线GUI WP导入已调用schema::migrate，与原§7“只有collector迁库”冲突 | 采纳。保留原WP导入例外，不改import.rs；新增motion查询/配置不迁库，正常部署由collector启动迁库 |
| C14 | 原heatCells/StickHeatmapProps没有共同scale参数，正文却要求两侧共用，隔离agent会实现不同契约 | 采纳。直接把scaleMaxSeconds加入两个完整签名；页面统一1250格max，强度算法只用该scale；原始秒数不变 |
| C15 | 原summary包含days却要求每日明细仅展开才查询，接口没有独立daily入口 | 采纳并选择最小实现。新motion日汇总随summary返回，展开复用；仅按钮getKeyDaily及legacy按展开查询。没有新增另一套日明细API |

## 追加核对

- 新增store行类型给出完整Rust字段和类型，GUI再映射camelCase，避免“number对应什么Rust类型”成为跨agent决策。
- DpiProbeResult在S3的hidpp_dpi定义，endpoint匹配用Unsupported/Ambiguous/Unique枚举；不依赖未交付worker或解析错误文字。
- 首次来源注册失败保留压缩运动增量，注册后绑定ID；不靠重新喂旧帧重算，不增加逐点持久队列。
- CSV scope=own追加motion文件，scope=wp保留原WP文件；JSON保持基线自有根数据＋可选WP，明确motion/行数/版本3。
- 成品页去掉工程实现说明，保留必要的型号按钮、未配置覆盖和旧历史口径；按钮图与热力的数据职责分开。

评审意见全部采纳，具体方案由主Agent复核后确定。没有将意见直接交给执行者自行选择；最终有效契约以主文档为准。
