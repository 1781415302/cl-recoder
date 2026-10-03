# CL Recoder 采集器堆损坏修复 Plan

日期：2026-09-30。交付对象：另一个 Coding Agent。本文定义修复与验收，不包含实现方法体；本次定位过程没有修改项目源码、启动/关闭采集器、写统计库或变更计划任务。

根目录：`C:\Users\17814\Documents\cl recoder`。以下源码路径相对于根目录。执行终端为 `pwsh`。基线是当前工作树，必须保留已完成的 correctness-v2 修复以及用户未提交修改。

## 1. 整体设计理念

### 已确认的问题及证据

采集器发生了真实进程崩溃。本机 Windows Application Error 1000 记录：

| 时间（北京时间） | 程序 | 异常码 | 检测到异常的模块 |
|---|---|---|---|
| 2026-09-30 19:35:50 | target/release/cl-recoder-collector.exe | 0xC0000374 | ntdll.dll |
| 2026-09-30 21:18:55 | 同上 | 0xC0000374 | ntdll.dll |

计划任务 `ClRecoderCollector` 最近启动时间为21:18:09，结果为十进制3221226356，即0xC0000374。该状态码为 `STATUS_HEAP_CORRUPTION`；ntdll通常是检测到堆损坏的位置，不能据此认为是Windows自身故障。[Microsoft NTSTATUS定义](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-erref/596a1078-e883-4972-9bbc-49e60bebca55)

明确的源码缺陷在 `crates/collector/src/device.rs:309–329` 的 `raw_input_device_name`：探测得到size后，分配 `Vec<u16>` 的元素数为 `size/2+1`，随后仍把size作为容量传给 `GetRawInputDeviceInfoW(..., RIDI_DEVICENAME, ...)`。

**该命令的pcbSize单位是字符数，不是字节数；W版本使用UTF-16码元。** API文档在RIDI_DEVICENAME条目明确指出这是通用字节描述的例外。[Microsoft GetRawInputDeviceInfoW契约](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getrawinputdeviceinfow)

本机只读验证已枚举实际键盘/鼠标；验证程序分配足够大的物理缓冲区，然后观察旧代码逻辑边界后的写入，**没有执行真实越界**：

| 类型 | API所需UTF-16码元 | 旧代码分配u16数 | 被写入的旧逻辑边界外码元 |
|---|---:|---:|---:|
| 鼠标 | 79 | 40 | 39 |
| 鼠标 | 89 | 45 | 44 |
| 键盘 | 95 | 48 | 47 |
| 键盘 | 70 | 36 | 34 |

实际验证缓冲区尾部canary全部保持，证明写入范围大于旧分配范围，同时验证自身没有破坏堆。详见 `docs/collector-heap-corruption-evidence.json`。

### 核心目标

修复设备名称查询的长度单位和容量合同，防止首次识别设备时破坏堆；加入能真正覆盖缓冲区分配的回归，而非仅测试路径解析替身。

堆越界可能在后续分配/释放才被发现，因此会表现为刚启动立即退出，或先显示运行再过几十秒退出。Rust的catch_unwind不修复原生越界，也不能把STATUS_HEAP_CORRUPTION变成可恢复的普通panic。

这一错误在初始commit已存在；本次不是回退correctness-v2的设备来源隔离。现有 `resolve_with` 测试替身绕过真实Win32读取，因此前一轮自动验收虽通过，没有覆盖该缺陷。

### 明确设计决策

- 修改范围只涉及device.rs的名称读取及其测试；不重构进程/IPC/界面/自启。
- 名称读取全程使用UTF-16码元计数；引入很小的私有可注入读取helper，生产和回归必须共用它。
- 采用DWORD对齐的拥有型缓冲区，确保实际可写容量不小于告知Win32的容量。
- 对大小变化采用有界重试；不可读设备仍走已有未知设备回退，不能退出整个采集器。
- 不新增依赖/feature/数据库迁移，不删除任何用户数据。未来增加其它Win32信息命令时必须另行定义单位，不能复用“所有pcbSize都是字节”的错误前提。

## 2. 系统架构设计

保持调用链：`raw_input`首次收到设备事件 → `DeviceResolver::resolve` → `raw_input_device_name` → 新的读取helper与Win32 adapter → 完整接口路径 → 既有VID/PID/注册表名称解析 → 既有缓存和Writer型号归并。

| 层 | 本次职责 |
|---|---|
| 原有raw_input_device_name | 薄Win32 adapter：探测长度、以真实字符容量调用API、立即读取错误码、标准化结果 |
| 新私有read_device_name_with | 分配策略、有界重试、成功长度校验、有限范围内寻找NUL及UTF-16解码 |
| 新私有AlignedNameBuffer | 拥有DWORD对齐存储，暴露准确逻辑长度的可写u16切片 |
| 原有DeviceResolver/parse/registry | 完整保留；查询失败照旧未知桶、有效来源仍独立、移除失效仍有效 |

禁止把缓冲区错误处理放到Engine或aggregator；禁止用延长500ms状态探测、隐藏“未运行”、自动重启或禁用设备识别代替内存安全修复。

## 3. 文件级设计

### 唯一允许修改的源码

`crates/collector/src/device.rs`

- 替换raw_input_device_name中错误的长度单位/分配逻辑；保留函数的私有签名和调用者。
- 本文件内新增下节私有类型/helper；它们服务此API，不建立跨crate工具框架。
- 在现有tests模块增加确定性容量/重试回归，以及默认忽略的真实Win32只读读取测试。
- 修正“pcbSize语义为字节”的错误注释，补全unsafe的容量、对齐、生命周期说明。
- 保留原有型号缓存、forget_handle、resolve_with、解析与注册表测试；不全文件格式化。

不修改Cargo文件、raw_input.rs、engine_loop.rs、core类型、UI、collector_ctl.rs、IPC、安装脚本或SQLite。本文和证据JSON为交接文档，不是需要实现的新产品模块。

## 4. 接口与数据结构设计（自包含契约）

### 保持现有入口

`fn raw_input_device_name(hdevice: HANDLE) -> Option<String>`：返回完整设备接口路径；无法安全取得时None，调用者继续现有未知型号回退。

`DeviceResolver::resolve`、`resolve_with`、`forget_handle`、DeviceKey及其它模块的接口不变。

### 新私有读取接口

```rust
const MAX_DEVICE_NAME_CHARS: u32 = 32_768;
const MAX_DEVICE_NAME_READ_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceNameRead {
    Complete { used_chars: u32 },
    TooSmall { required_chars: u32 },
    Failed,
}

struct AlignedNameBuffer {
    words: Vec<u32>,
    logical_chars: usize,
}

fn read_device_name_with(
    probe: impl FnMut() -> Option<u32>,
    read: impl FnMut(&mut [u16]) -> DeviceNameRead,
) -> Option<String>;
```

AlignedNameBuffer私有方法签名：`new(chars: u32) -> Option<Self>`；`as_utf16_mut(&mut self) -> &mut [u16]`。这些是设计签名，不提供方法体。

### 容量与对齐不变量

1. `probe`返回最小所需UTF-16码元数，包括API要求的终止空间；不是UTF-8字节数、Rust字符串长度或Unicode标量数。
2. DWORD对齐存储用零初始化Vec<u32>；需要N个UTF-16码元时，底层word数为ceil(N/2)，可写UTF-16视图的**逻辑长度严格为N**，不暴露对齐padding。
3. 此处底层u32打包的ceil(N/2)是合法存储换算；禁止再次把RIDI_DEVICENAME所需字符数除以2作为u16元素数。
4. Win32 adapter只以传入切片的len设置输入pcbSize；指针指向该切片的有效存储。API完成前拥有型缓冲区不能释放、移动底层存储或resize；不能同时使用底层u32引用与其可变u16视图。
5. API返回UINT_MAX只代表失败。立即读取GetLastError：ERROR_INSUFFICIENT_BUFFER映射TooSmall（长度取该次输出pcbSize），其它错误映射Failed。不得把泛化文档中的返回值“bytes copied”再用于除以2计算名称长度。
6. API成功映射Complete，used_chars来自该次输出pcbSize；只允许 `0 < used_chars <= 输入切片len`。成功后在这段已分配、初始化的区域内找NUL；缺NUL或空名称返回None，不做无界PWSTR扫描。解码沿用String::from_utf16_lossy，保持原先的Unicode回退行为。

### 有界读取状态机

- probe失败、0、超过MAX：None，不分配/不调用read。
- 合法N：分配准确逻辑容量，调用read。
- Complete：先校验边界，再有限范围解析；得到非空字符串则返回Some。
- TooSmall：只有 `current_chars < required_chars <= MAX` 才扩容重试；不能解析失败调用留下的部分内容。最多3次填充调用（probe不计入），第三次仍不足就None。
- Failed、异常长度、无增长的TooSmall：None。允许设备在探测后拔出，不把它升级为进程级失败。

### 输入输出例子

| 输入 | 期望 |
|---|---|
| probe=79，read写入78个码元及NUL | read看到len=79、地址%4=0；返回完整78码元名称 |
| probe=79，首次TooSmall{95}，二次成功 | read容量序列79、95；返回第二次完整名称 |
| probe=u32::MAX | None，read调用次数0 |
| 连续增长导致三次TooSmall | None，填充调用次数恰3，无第四次 |
| Complete长度超过已分配视图 | None，不切片越界、不panic |
| UTF-16中文或代理对 | 完整解码，不按UTF-8字节或Rust char计容量 |

生产必须调用这一个helper；不允许新helper仅存在于测试，而真实调用保留旧Vec分配。

## 5. 核心流程设计

1. 有效hDevice首次进入名称查询，Win32探测所需字符数。
2. helper检查大小上限，创建拥有型对齐缓冲区。
3. adapter以实际逻辑字符容量调用同一RIDI_DEVICENAME API。
4. 成功且边界有效，解码完整设备路径；走原来的型号/注册表解析。
5. 名称增长则有限扩容；失联/异常则返回None，原有键鼠事件仍在未知桶继续计数。
6. 原有缓存避免每条输入重复查询；原有拔出失效和新连接来源规则照旧。

不要在原有损坏实现上启动真实采集来“故意复现崩溃”。本次已通过足够大物理缓冲区的canary验证，执行者应使用确定性替身和修复后的真实adapter验证。

## 6. 数据存储与状态设计

- SQLite schema仍为2，stats.db、WAL、settings、wp_*、导出格式和型号归并不变；禁止删库解决崩溃。
- AlignedNameBuffer只在单次查询内存在，成功/失败/重试都由Rust拥有型存储释放；不使用LocalFree释放Vec内存。
- 名称读取None时复用既有未知桶与缓存语义；不引入新的长期故障状态或UI字段。
- 证据JSON只保存崩溃码、时间、源码/二进制指纹、数量和canary结果，不保存真实设备路径、输入记录或dump。

## 7. 兼容方案

- 完整保留correctness-v2物理来源ID、按来源held、跨键盘组合、鼠标累计、移除/重置和时长ledger。
- 现有parse_device_path/registry逻辑接收正确完整路径，不能为了规避查询去掉型号解析。
- 保持null/未知/拔出设备的统计回退；不调整键码或计数规则。
- 不改变500msIPC、启动方式、计划任务设置；当前退出证据是堆损坏，状态探测抖动/电池策略不是这次主修范围。
- 旧统计不做回填或重新归并；这次交付只保证未来查询的内存安全。
- 源码改完必须重建release collector，不能只跑debug单测后让用户继续用旧exe；实际任务当前指向target/release下的collector。

## 8. Stage Map

此任务很小，只有一个源码文件，**实现和同文件测试合并到同一stage**。不为并行调度把同一文件分给多个Agent。

| Stage | 工作与文件 | 硬依赖 | 验收 |
|---|---|---|---|
| S1 修复读取及自动回归 | device.rs：生产adapter、helper、对齐buffer、单测、默认ignored的native测试 | 无 | `cargo test -p clrecoder-collector device::tests`；下面确定性反例真实执行，不能仅查测试名字 |
| S2 只读Win32与源码审计 | 只读核对device.rs，运行native测试 | S1 | `cargo test -p clrecoder-collector device::tests::collector_heap_native_name_read_smoke -- --ignored --exact --nocapture`；实际键盘/鼠标名称与独立正确容量参考读取一致，记录设备数与容量，不输出设备路径 |
| S3 安全回归与release产物 | 不扩源码范围；构建产物与报告 | S1、S2 | 安全命令全部0退出；新release exe指纹/时间/构建命令明确，指出它与旧指纹不同 |
| S4 用户环境稳定性验收 | 修复后的候选二进制，人工或用户明确授权的集成验证 | S3，候选产物已实际换用 | 启动后正常打字/鼠标输入并观察至少10分钟，可用时包含一次外设重连；进程和管道稳定，没有新的对应APPCRASH1000；不以旧task LastTaskResult值作为新失败依据 |

### S1最低必需确定性回归

新增Rust测试统一 `collector_heap_` 前缀，至少覆盖：

1. 本机79/89/95以及偶数长度样本：read实际获得完整N码元可写视图，最后NUL可写，地址满足DWORD对齐，名称不截断；若恢复旧N/2分配，断言应失败而不产生真正越界。
2. 中文、非BMP代理对、首个NUL后的内容不参与结果。
3. probe失败/0/超限时不调用read；上限本身可接受。
4. ERROR_INSUFFICIENT_BUFFER对应的增长重试；3次失败封顶；无增长/超限请求立即失败。
5. Complete长度0/超过输入容量/缺NUL/空名称失败；普通读取错误失败。
6. 现有VID/PID、friendly name fallback、缓存、forget_handle回归保留。

### S2 native测试要求

- 函数名固定 `collector_heap_native_name_read_smoke`，放在device.rs现有tests模块，标记ignored，因此默认cargo test不执行真实设备枚举。
- 仅调用GetRawInputDeviceList和设备名称API，类型0/1分别为鼠标/键盘；必须调用修复后的raw_input_device_name，不能仅测试自己复制的一份正确公式。
- 独立参考读取使用足够大的DWORD对齐缓冲区，先探测再按字符数填充；将完整返回名与生产wrapper比较。参考区尾canary应保持。
- 枚举、两次调用之间设备可能拔出：该项跳过并计数；没有任何成功键盘/鼠标则如实报告环境无法覆盖native关卡，不声称native通过。
- 不注册Raw Input、不创建采集窗口、不启动collector、不访问控制管道、不读取输入、不写用户库、不弹UAC。

### S3自动关卡与发布边界

根目录pwsh：

```text
cargo test -p clrecoder-core -p clrecoder-engine -p clrecoder-store
cargo test -p clrecoder-collector -- --skip ipc_server::tests::
cargo test -p cl-recoder -- --skip import_real_whatpulse_db_when_present
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p clrecoder-collector --target-dir <本次唯一候选构建目录>
```

候选构建目录应独立于生产target/release，防止构建时覆盖/占用正在运行的旧exe；报告给出绝对候选exe路径、SHA256、来源版本与生成时间。不需要重建GUI或安装包来证明这个单文件修复。

自动测试不执行生产named-pipe测试，不调用schtasks /Run、taskkill、UAC或旧collector。不自行替换用户当前二进制；提供已验证候选产物和替换说明，由用户或已经得到部署授权的执行者完成最后一步。保持计划任务现有路径即可，必须确保实际路径上的文件已是新版本，否则S4没有意义。

S4要记录这轮启动时间、PID、exe路径/指纹，以它们限定事件日志；旧APPCRASH与旧LastTaskResult属于历史证据。若正确容量实现及native测试均通过但新版本仍同码崩溃，保留新事件信息进入进一步dump/Page Heap诊断，不能反复加重启策略宣称解决。

## 9. 执行Agent约束

### 架构必须遵守

- 单文件修复范围、字符容量、DWORD对齐、有限解析、有界重试、生产/测试共用helper、release交付合同严格按本文。
- 不把RIDI_DEVICENAME与RIDI_PREPARSEDDATA/RIDI_DEVICEINFO的单位混用；不以unsafe和catch_unwind作为“防崩溃”保证。
- 不新增依赖、Win32 feature、数据库迁移、权限/计划任务调整、GUI状态模型或监控框架。
- 保留现有未提交修复。不得reset、删库、格式化全仓、削弱断言、顺手清理app_time等其它文件。
- 不使用真实损坏代码进行堆破坏实验，不默认操作生产采集器。交接完成前必须明确哪些关卡实跑、哪些为环境/部署后的验收。

### 实现允许自主决定

- 私有helper局部组织、变量名、Vec存储转换的小工具、错误文案和fixture写法；代码风格沿用本文件。
- unsafe创建u16视图时需写明底层对齐、初始化、长度和独占借用安全依据；方法体不由本文规定。
- native枚举/参考读取的常规Win32样板和临时构建目录命名由executor决定；上述容量和测试意义不能变。

### 已作出的取舍

采用单文件薄adapter+可注入helper，而不是全局重写所有Win32调用；采用有界字符buffer与DWORD对齐，而不是只改一行后继续缺少容量回归；采用只读native查询验收，而不是无条件启动真实采集器；提供新release候选，而不是把debug测试通过等同用户现有exe已修复。三类证据已足够确定越界缺陷，现阶段无需安装调试器或访问受保护WER dump。
