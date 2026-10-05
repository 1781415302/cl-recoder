//! HID 传输层（motion-dpi §4.3）——SetupAPI/HID 枚举唯一关联与限时 overlapped IO。
//!
//! 职责边界（§2 模块表）：不以 VID 或产品名独自匹配另一个设备；本模块只做**结构层**
//! 的唯一关联与不透明字节收发，不理解 HID++ 语义（编解码/探测序列见
//! [`crate::hidpp_dpi`]）。
//!
//! 关联规则（§4.3"自动读取的实际实现"，逐条对应）：
//! 1. Raw 接口 devnode 读取**非空** `DEVPKEY_Device_ContainerId`，并沿
//!    `DEVPKEY_Device_Parent` 上行确认 USB 祖先（设备 ID 以 `USB\` 开头；
//!    蓝牙等非 USB 直连不在支持范围）；
//! 2. 候选 = `GUID_DEVINTERFACE_HID` 枚举出的 HID collection，须**同 ContainerID、
//!    VID=046D** 且唯一——ContainerID/0xFF 本身不能排除接收器，那由 hidpp_dpi 的
//!    GetDeviceType 只读查询排除；不试其他 device index；
//! 3. `HidD_GetPreparsedData`/`HidP_GetCaps` 校验 vendor collection：本轮仅接受
//!    Input/OutputReportByteLength 都恰为 20 且声明 0x11 输入/输出报告；不满足走手动。
//!    通用鼠标输入 collection 不作为可写端点（候选一律排除 Raw 接口自身路径）。
//!
//! IO 规则（§4.3 原生IO）：共享读写、overlapped WriteFile/ReadFile；按 CAPS 分配
//! 20 字节 buffer，按实际返回字节数交给上层按 report ID 解码。`deadline` 是业务结果
//! 接受期限，**不是驱动一定收尾的硬保证**：到期先 CancelIoEx，然后在本调用内等待
//! IRP 真正收尾——期间持续持有 OVERLAPPED/buffer/句柄，不释放或复用，不接受迟到
//! 结果（取消后即使以成功收尾也报错拒绝）。
//!
//! 可测性拆分（沿袭 device.rs 惯例）：唯一性判定（[`select_vendor_candidate`]）、VID
//! 提取（[`vid_from_interface_path`]）、descriptor 门与 overlapped 等待状态机
//! （[`wait_overlapped_result`]，单测以命名管道注入真实未决 IO 验证取消/收尾语义）
//! 均为纯或可注入函数；SetupAPI/HID 的 unsafe 薄封装仅编译期覆盖，真机行为属 S11
//! 真实验收项。
//!
//! S3 交付态：本模块公开入口在 S4（mouse_dpi/motion_runtime 接线）前无 crate 内调用
//! 方，临时允许 dead_code 以维持零警告基线，S4 接线后移除。
#![allow(dead_code)]

use std::time::Instant;

use clrecoder_core::motion::MouseSourceDescriptor;
use windows::core::{HRESULT, HSTRING};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_Interface_PropertyW, CM_Get_DevNode_PropertyW, CM_Locate_DevNodeW,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, CM_LOCATE_DEVNODE_NORMAL, CONFIGRET, CR_BUFFER_SMALL,
    CR_NO_SUCH_VALUE, CR_SUCCESS, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetPreparsedData, HidP_GetCaps, HidP_GetValueCaps,
    GUID_DEVINTERFACE_HID, HIDP_CAPS, HIDP_REPORT_TYPE, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS,
    HidP_Input, HidP_Output, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_ContainerId, DEVPKEY_Device_Parent, DEVPROPTYPE,
};
use windows::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, DEVPROPKEY, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING,
    HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::CreateEventW;

/// vendor collection 的 VID 过滤（§4.3：同 ContainerID 且 VID=046D 且唯一；
/// 仅作过滤条件之一，不独自决定匹配）。
const LOGITECH_VID: u16 = 0x046D;
/// 本轮唯一接受的输入/输出报告字节数（§4.3：都恰为 20）。
const VENDOR_REPORT_BYTES: u16 = 20;
/// 本轮唯一接受的输入/输出 report ID（§4.3：声明 0x11 输入/输出报告）。
const VENDOR_REPORT_ID: u8 = 0x11;
/// USB 祖先确认的父链上行层数上限（HID collection → USB 接口 → USB 设备，
/// 通常 2 层即命中；上限仅防异常设备树拖慢探测）。
const MAX_ANCESTOR_DEPTH: usize = 6;
/// 未决 IO 的轮询间隔（单次探测 ≤6 次交换，轮询开销可忽略）。
const IO_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);

/// 唯一关联出的可写 vendor collection 端点（§4.3：私有句柄/关联信息）。
///
/// 句柄按共享读写 + overlapped 打开；Drop 时关闭。生命周期约定：在所有未决 IO 收尾
/// 后才 Drop——[`exchange`] 在 IRP 收尾后才返回（见 [`wait_overlapped_result`]），
/// 因此正常使用下不会带着未决 IO 析构（shutdown 迟延 IO 由存活执行槽持有到收尾或
/// 进程正常退出，§4.3）。
pub struct HidEndpoint {
    /// 打开的 vendor collection 句柄（共享读写、FILE_FLAG_OVERLAPPED）
    handle: HANDLE,
    /// 供 overlapped 完成通知用的匿名事件（自动重置）
    event: HANDLE,
    /// 关联接口路径（关联证据；不写日志、不出 UI/导出）
    #[allow(dead_code)]
    interface_path: String,
    /// CAPS 输入报告字节数（构造时验证 == 20；读缓冲按它分配）
    input_report_bytes: u16,
    /// CAPS 输出报告字节数（构造时验证 == 20；写缓冲按它分配）
    output_report_bytes: u16,
}

/// 关联结果（§4.3）：不支持 / 候选歧义 / 唯一端点。
///
/// 判定语义（§4.3）：Unsupported/Ambiguous 由枚举判定直通；transport/系统失败一律
/// Err（上层映射 Unavailable，不解析中文字符串）。
pub enum HidEndpointMatch {
    /// 无候选（非 USB/无 ContainerId/CAPS 不符等）——该连接不重探，走手动
    Unsupported,
    /// 同 ContainerID 命中多个合格候选——拒绝采信，走手动
    Ambiguous,
    /// 唯一合格 vendor collection（句柄已按共享读写打开）
    Unique(HidEndpoint),
}

impl Drop for HidEndpoint {
    fn drop(&mut self) {
        // SAFETY: 两个句柄均由 HidEndpoint::open 创建且仅在此关闭一次；
        // 约定所有未决 IO 已收尾（exchange 返回前等待 IRP 完成）。
        unsafe {
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.handle);
        }
    }
}

impl HidEndpoint {
    /// 按共享读写 + overlapped 打开唯一候选，并复核 CAPS 仍为 20/20。
    ///
    /// 枚举期（只读查询句柄）与正式打开之间设备可能变化，复核失败按 transport 失败
    /// 处理（Err → Unavailable，10 秒重试）。
    fn open(interface_path: &str) -> Result<HidEndpoint, String> {
        let wide = HSTRING::from(interface_path);
        // SAFETY: wide 在调用期间存活；成功返回的句柄由 HidEndpoint 持有并于 Drop 关闭。
        let handle = unsafe {
            CreateFileW(
                &wide,
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| format!("打开 vendor collection 失败: {e}"))?;
        let caps = match collection_caps(handle) {
            Ok(caps) => caps,
            Err(e) => {
                // SAFETY: 本函数刚创建的句柄，校验失败路径立即关闭。
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(e);
            }
        };
        if caps.input_report_bytes != VENDOR_REPORT_BYTES
            || caps.output_report_bytes != VENDOR_REPORT_BYTES
        {
            // SAFETY: 本函数刚创建的句柄，校验失败路径立即关闭。
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err("vendor collection CAPS 不再是 20/20，拒绝作为写端点".to_string());
        }
        // SAFETY: 匿名事件（无名称），返回句柄由 HidEndpoint 持有并于 Drop 关闭；
        // 失败路径同时关闭刚打开的设备句柄。
        let event = match unsafe { CreateEventW(None, false, false, None) } {
            Ok(event) => event,
            Err(e) => {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(format!("创建 overlapped 事件失败: {e}"));
            }
        };
        Ok(HidEndpoint {
            handle,
            event,
            interface_path: interface_path.to_string(),
            input_report_bytes: caps.input_report_bytes,
            output_report_bytes: caps.output_report_bytes,
        })
    }

    /// 一次不透明字节交换：写 20 字节请求 → 读一个输入报告（§4.3 原生IO）。
    fn transact(&mut self, request: &[u8; 20], deadline: Instant) -> Result<Vec<u8>, String> {
        // —— 写阶段：请求拷入 CAPS 大小的输出缓冲（report 0x11 已在 request[0]）——
        let mut out = vec![0u8; self.output_report_bytes as usize];
        out[..request.len()].copy_from_slice(request);
        let mut write_ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
        // SAFETY: out 在本函数栈内存续期（等待收尾后才析构）；write_ov 同栈存活，
        // 且该 OVERLAPPED 上同一时刻只有一个未决 IO。
        match unsafe { WriteFile(self.handle, Some(&out), None, Some(&mut write_ov)) } {
            Ok(()) => {}
            Err(e) if e.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {
                wait_overlapped_result(self.handle, &mut write_ov, Some(deadline), "写入")?;
            }
            Err(e) => return Err(format!("HID++ 写入提交失败: {e}")),
        }
        if Instant::now() >= deadline {
            return Err("HID++ 写入完成但业务期限已到，不再发起读取".to_string());
        }
        // —— 读阶段：按 CAPS 分配输入缓冲，等一个输入报告 ——
        let mut input = vec![0u8; self.input_report_bytes as usize];
        let mut read_ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
        // SAFETY: input 在本函数栈内存续期（等待收尾后才析构）；read_ov 同栈存活，
        // 且该 OVERLAPPED 上同一时刻只有一个未决 IO。
        match unsafe { ReadFile(self.handle, Some(&mut input), None, Some(&mut read_ov)) } {
            Ok(()) => {}
            Err(e) if e.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {}
            Err(e) => return Err(format!("HID++ 读取提交失败: {e}")),
        }
        let read = wait_overlapped_result(self.handle, &mut read_ov, Some(deadline), "读取")?;
        if read as usize > input.len() {
            return Err("HID++ 读取字节数超出 CAPS 缓冲（异常）".to_string());
        }
        input.truncate(read as usize);
        Ok(input)
    }

    /// 仅供单测构造不触真实 HID 的端点形状（假 transport 从不使用句柄）。
    #[cfg(test)]
    pub(crate) fn for_tests() -> HidEndpoint {
        HidEndpoint {
            handle: HANDLE::default(),
            event: HANDLE::default(),
            interface_path: "test:endpoint".to_string(),
            input_report_bytes: VENDOR_REPORT_BYTES,
            output_report_bytes: VENDOR_REPORT_BYTES,
        }
    }
}

/// 一次 HID++ 字节交换（§4.3 公开入口）。
///
/// `deadline` 已过时直接拒绝（不发起任何 IO）；未过期则写请求、读响应。
/// 到期语义见模块文档与 [`wait_overlapped_result`]。
pub fn exchange(
    endpoint: &mut HidEndpoint,
    request: &[u8; 20],
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    if Instant::now() >= deadline {
        return Err("HID++ 交换窗口已到期，未发起读写".to_string());
    }
    endpoint.transact(request, deadline)
}

/// OVERLAPPED 等待状态机（§4.3 取消规则）。
///
/// - `Some(deadline)`：到期 → CancelIoEx 发起取消，随后继续在本函数内轮询直到 IRP
///   真正收尾（收尾不设业务期限——"不是驱动一定收尾的硬保证"）；取消后即使操作以
///   成功收尾，也报错拒绝迟到结果。
/// - `None`：仅等待收尾（内部由本函数在取消后自行切换到此态）。
///
/// 返回时 IRP 必已收尾（或从未进入未决态）：调用者的 OVERLAPPED/缓冲在返回后才离开
/// 作用域，"完成前持续持有 buffer/OVERLAPPED/句柄，不释放或复用"由此结构保证。
fn wait_overlapped_result(
    handle: HANDLE,
    ov: &mut OVERLAPPED,
    mut deadline: Option<Instant>,
    what: &str,
) -> Result<u32, String> {
    let mut cancelled = false;
    loop {
        let mut transferred = 0u32;
        // SAFETY: handle 在 HidEndpoint 内持续有效；ov 指向本阶段唯一未决 IO，
        // 且在本函数返回（= IRP 收尾）前不被调用方释放或复用。
        match unsafe { GetOverlappedResult(handle, ov, &mut transferred, false) } {
            Ok(()) => {
                return if cancelled {
                    Err(format!("HID++ {what}在取消后完成，迟到结果不采信"))
                } else {
                    Ok(transferred)
                };
            }
            Err(e) => {
                if e.code() != HRESULT::from_win32(ERROR_IO_INCOMPLETE.0) {
                    // 含 ERROR_OPERATION_ABORTED（取消收尾）等终态：IRP 已结束，
                    // OVERLAPPED/缓冲此后才析构是安全的。
                    return Err(format!("HID++ {what}失败: {e}"));
                }
            }
        }
        if let Some(d) = deadline {
            if Instant::now() >= d {
                // SAFETY: handle 有效；ov 即本阶段唯一未决 IO。取消已发起或 IO 恰好
                // 先完成时本调用失败，均由后续轮询观察到终态。
                let _ = unsafe { CancelIoEx(handle, Some(ov)) };
                cancelled = true;
                deadline = None;
            }
        }
        std::thread::sleep(IO_POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------------
// 关联：Raw 接口定位 → ContainerId/USB 祖先 → 候选枚举 → 唯一选择
// ---------------------------------------------------------------------------

/// 在系统中为 `descriptor` 找唯一可写 vendor collection（§4.3 公开入口）。
///
/// Ok(Unsupported/Ambiguous) 是**判定**（该连接不重探）；Err 是 transport/系统失败
/// （上层映射 Unavailable，10 秒重试）。descriptor 门（非物理/无路径）在触任何
/// Win32 API 之前完成。
pub fn find_direct_endpoint(descriptor: &MouseSourceDescriptor) -> Result<HidEndpointMatch, String> {
    // 1) descriptor 门：仅物理、有原始接口路径的来源可能自动读取
    //    （§4.1 virtual:unknown 等固定桶 physical=false，禁自动/手动物理 DPI）。
    if !descriptor.physical {
        return Ok(HidEndpointMatch::Unsupported);
    }
    let Some(raw_path) = descriptor.interface_path.as_deref() else {
        return Ok(HidEndpointMatch::Unsupported);
    };
    // 路径须为可提取 VID 的 HID 接口形态（防 RDP/虚拟等形态漏网）。
    if vid_from_interface_path(raw_path).is_none() {
        return Ok(HidEndpointMatch::Unsupported);
    }

    // 2) Raw devnode：非空 ContainerId（缺失/全零 → 无关联依据 → Unsupported）。
    let Some(raw_container) = interface_property_bytes(raw_path, &DEVPKEY_Device_ContainerId)?
        .and_then(property_guid)
    else {
        return Ok(HidEndpointMatch::Unsupported);
    };

    // 3) USB 祖先确认（蓝牙等非 USB 直连 → Unsupported）。
    if !has_usb_ancestor(raw_path)? {
        return Ok(HidEndpointMatch::Unsupported);
    }

    // 4) 枚举候选事实（单个候选读取失败被跳过并计数）。
    let (candidates, failed) = enumerate_candidate_facts()?;

    // 5) 唯一性判定（纯函数）。
    match select_vendor_candidate(raw_container, raw_path, &candidates) {
        SelectedVendor::Ambiguous => Ok(HidEndpointMatch::Ambiguous),
        SelectedVendor::UniquePath(path) => Ok(HidEndpointMatch::Unique(HidEndpoint::open(&path)?)),
        SelectedVendor::None => {
            if failed > 0 {
                // 有候选读不出来：不能断言"不支持"（可能是 vendor collection 暂时
                // 不可开），按 transport 失败降级 → Unavailable（10 秒重试）。
                Err(format!(
                    "HID 枚举期间有 {failed} 个候选不可读，无法确认唯一 vendor collection"
                ))
            } else {
                Ok(HidEndpointMatch::Unsupported)
            }
        }
    }
}

/// 候选 vendor collection 的枚举期事实（纯数据，供唯一性判定与单测注入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateFacts {
    /// 候选接口路径
    pub(crate) interface_path: String,
    /// 候选 devnode 的 ContainerId（缺失/全零为 None → 永不入选）
    pub(crate) container: Option<[u8; 16]>,
    /// 从候选路径解析的 VID（非 HID 形态为 None → 永不入选）
    pub(crate) vid: Option<u16>,
    /// CAPS 输入报告字节数
    pub(crate) input_report_bytes: u16,
    /// CAPS 输出报告字节数
    pub(crate) output_report_bytes: u16,
    /// 输入 value caps 是否声明 0x11 报告
    pub(crate) declares_0x11_input: bool,
    /// 输出 value caps 是否声明 0x11 报告
    pub(crate) declares_0x11_output: bool,
}

/// 唯一性判定（纯函数，§4.3）：合格候选 = 同 ContainerID + VID=046D + 20/20 CAPS +
/// 声明 0x11 输入/输出报告，且不是 Raw 接口自身路径（通用鼠标输入 collection 不作为
/// 可写端点）。恰一个 → [`SelectedVendor::UniquePath`]；多于一个 → Ambiguous；
/// 零个 → None。
fn select_vendor_candidate(
    raw_container: [u8; 16],
    raw_path: &str,
    candidates: &[CandidateFacts],
) -> SelectedVendor {
    let mut matches = candidates.iter().filter(|c| {
        c.interface_path != raw_path
            && c.container == Some(raw_container)
            && c.vid == Some(LOGITECH_VID)
            && c.input_report_bytes == VENDOR_REPORT_BYTES
            && c.output_report_bytes == VENDOR_REPORT_BYTES
            && c.declares_0x11_input
            && c.declares_0x11_output
    });
    let Some(first) = matches.next() else {
        return SelectedVendor::None;
    };
    if matches.next().is_some() {
        return SelectedVendor::Ambiguous;
    }
    SelectedVendor::UniquePath(first.interface_path.clone())
}

/// 唯一性判定结果（内部）：None = 无合格候选。
#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedVendor {
    None,
    UniquePath(String),
    Ambiguous,
}

/// 枚举 `GUID_DEVINTERFACE_HID` 全部在场接口并收集候选事实。
/// 返回（事实列表, 读取失败的候选数）——失败候选被跳过，仅当最终没有唯一命中时
/// 才据此把结果降级为 Err（见 [`find_direct_endpoint`]）。
fn enumerate_candidate_facts() -> Result<(Vec<CandidateFacts>, usize), String> {
    let paths = enumerate_hid_interface_paths()?;
    let mut facts = Vec::new();
    let mut failed = 0usize;
    for path in paths {
        match candidate_facts(&path) {
            Ok(fact) => facts.push(fact),
            Err(_) => failed += 1,
        }
    }
    Ok((facts, failed))
}

/// 收集单个候选的事实：ContainerId（CM 接口属性，无需打开）+ VID + CAPS/报告 ID
/// （无访问权限共享打开，仅读 CAPS，不与占用该设备的进程争用）。
fn candidate_facts(interface_path: &str) -> Result<CandidateFacts, String> {
    let container =
        interface_property_bytes(interface_path, &DEVPKEY_Device_ContainerId)?.and_then(property_guid);
    let vid = vid_from_interface_path(interface_path);
    let caps = open_collection_caps(interface_path)?;
    Ok(CandidateFacts {
        interface_path: interface_path.to_string(),
        container,
        vid,
        input_report_bytes: caps.input_report_bytes,
        output_report_bytes: caps.output_report_bytes,
        declares_0x11_input: caps.declares_0x11_input,
        declares_0x11_output: caps.declares_0x11_output,
    })
}

/// CAPS/报告 ID 查询结果。
struct CollectionCaps {
    input_report_bytes: u16,
    output_report_bytes: u16,
    declares_0x11_input: bool,
    declares_0x11_output: bool,
}

/// 用无访问权限（dwDesiredAccess=0）共享打开候选，仅读 CAPS 后关闭。
fn open_collection_caps(interface_path: &str) -> Result<CollectionCaps, String> {
    let wide = HSTRING::from(interface_path);
    // SAFETY: wide 在调用期间存活；dwDesiredAccess=0 仅查询权限，共享读写不与
    // 其他持有者争用；句柄在本次查询后立即关闭。
    let handle = unsafe {
        CreateFileW(
            &wide,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map_err(|e| format!("打开 HID collection（查询）失败: {e}"))?;
    let result = collection_caps(handle);
    // SAFETY: 本函数刚创建的句柄，查询结束即关闭。
    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}

/// 在已打开句柄上取 CAPS 与 0x11 输入/输出声明（preparsed 用后即释放）。
fn collection_caps(handle: HANDLE) -> Result<CollectionCaps, String> {
    // SAFETY: handle 有效；preparsed 由 API 分配，两个查询完成后立即释放。
    unsafe {
        let mut preparsed = PHIDP_PREPARSED_DATA(0);
        if !HidD_GetPreparsedData(handle, &mut preparsed) {
            return Err("HidD_GetPreparsedData 失败".to_string());
        }
        let mut caps = HIDP_CAPS::default();
        let status = HidP_GetCaps(preparsed, &mut caps);
        if status != HIDP_STATUS_SUCCESS {
            let _ = HidD_FreePreparsedData(preparsed);
            return Err(format!("HidP_GetCaps 失败: NTSTATUS {}", status.0));
        }
        let result = CollectionCaps {
            input_report_bytes: caps.InputReportByteLength,
            output_report_bytes: caps.OutputReportByteLength,
            declares_0x11_input: declares_report_id(
                preparsed,
                HidP_Input,
                caps.NumberInputValueCaps,
                VENDOR_REPORT_ID,
            ),
            declares_0x11_output: declares_report_id(
                preparsed,
                HidP_Output,
                caps.NumberOutputValueCaps,
                VENDOR_REPORT_ID,
            ),
        };
        let _ = HidD_FreePreparsedData(preparsed);
        Ok(result)
    }
}

/// value caps 中是否出现目标 report ID（§4.3"声明 0x11 输入/输出报告"）。
///
/// fail-closed：无 caps 条目或 API 失败一律视为未声明（不满足走手动）。
/// HID++ vendor collection 的 19 字节输入/输出是单个 8 位 value 条目
/// （usage page 为厂商页），value caps 覆盖其 ReportId。
/// SAFETY: 调用方保证 preparsed 有效且未被释放；caps 缓冲按
/// HidP_GetCaps 报告的条目数分配，调用期间不释放/移动。
unsafe fn declares_report_id(
    preparsed: PHIDP_PREPARSED_DATA,
    report_type: HIDP_REPORT_TYPE,
    number: u16,
    report_id: u8,
) -> bool {
    if number == 0 {
        return false;
    }
    let mut caps = vec![HIDP_VALUE_CAPS::default(); number as usize];
    let mut count = number;
    let status = HidP_GetValueCaps(report_type, caps.as_mut_ptr(), &mut count, preparsed);
    if status != HIDP_STATUS_SUCCESS {
        return false;
    }
    let count = (count as usize).min(caps.len());
    caps[..count].iter().any(|c| c.ReportID == report_id)
}

/// 枚举全部在场 HID 接口路径（`GUID_DEVINTERFACE_HID`，PRESENT + DEVICEINTERFACE）。
fn enumerate_hid_interface_paths() -> Result<Vec<String>, String> {
    // SAFETY: 只读枚举；句柄集由 guard 在函数尾销毁一次。
    unsafe {
        let set = SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_HID),
            None,
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
        .map_err(|e| format!("枚举 HID 设备接口失败: {e}"))?;
        let _guard = DeviceInfoSetGuard(set);
        let mut paths = Vec::new();
        let mut index = 0u32;
        loop {
            let mut ifdata = SP_DEVICE_INTERFACE_DATA {
                cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            // 枚举结束（ERROR_NO_MORE_ITEMS）或个别项消失 → 结束循环。
            if SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVINTERFACE_HID, index, &mut ifdata)
                .is_err()
            {
                break;
            }
            paths.push(interface_detail_path(set, &ifdata)?);
            index += 1;
        }
        Ok(paths)
    }
}

/// HDEVINFO 的 RAII 守卫（枚举函数尾统一销毁）。
struct DeviceInfoSetGuard(HDEVINFO);

impl Drop for DeviceInfoSetGuard {
    fn drop(&mut self) {
        // SAFETY: set 来自 SetupDiGetClassDevsW 成功返回，仅销毁一次。
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

/// 取单个接口的设备路径（经典两段式：probe 必需大小 → DWORD 对齐缓冲填充）。
fn interface_detail_path(
    set: HDEVINFO,
    ifdata: &SP_DEVICE_INTERFACE_DATA,
) -> Result<String, String> {
    // SAFETY: set/ifdata 来自本函数枚举；probe 调用 detail=None 仅取必需大小
    // （预期以 ERROR_INSUFFICIENT_BUFFER 失败并填充 required）。
    let mut required = 0u32;
    let _ = unsafe {
        SetupDiGetDeviceInterfaceDetailW(set, ifdata, None, 0, Some(&mut required), None)
    };
    if required < 8 {
        // 至少容纳 cbSize(4) + 1 个 UTF-16 码元 + NUL(2)
        return Err("SetupDiGetDeviceInterfaceDetailW 探测大小异常".to_string());
    }
    // 物理缓冲用 u32 分配保证 4 字节对齐（cbSize 为 u32）；容量略有余量。
    let words_len = ((required as usize).div_ceil(4)) + 2;
    let mut buffer = vec![0u32; words_len];
    let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    // SAFETY: detail 指向按 SP_DEVICE_INTERFACE_DETAIL_DATA_W 布局对齐的可写缓冲；
    // cbSize 必须取本进程所见的结构体大小（Windows 契约）。
    unsafe {
        (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        SetupDiGetDeviceInterfaceDetailW(
            set,
            ifdata,
            Some(detail),
            required,
            None,
            None,
        )
        .map_err(|e| format!("读取 HID 接口路径失败: {e}"))?;
        // cbSize 之后是 NUL 结尾的 UTF-16 设备路径；按必需字节解析，不越过探测范围。
        let units = std::slice::from_raw_parts(detail.cast::<u16>(), required as usize / 2);
        let body = &units[2..];
        let end = body.iter().position(|&c| c == 0).ok_or_else(|| {
            "HID 接口路径缺少 NUL 终止符".to_string()
        })?;
        if end == 0 {
            return Err("HID 接口路径为空".to_string());
        }
        Ok(String::from_utf16_lossy(&body[..end]))
    }
}

/// 上行父链确认 USB 祖先（§4.3）：从 Raw 接口 devnode 的父开始，设备 ID 以 `USB\`
/// 开头（大小写不敏感）即确认；上限 [`MAX_ANCESTOR_DEPTH`] 层。
///
/// 父属性不存在（CR_NO_SUCH_VALUE）→ 无法确认 → Ok(false)；其余属性失败 → Err。
fn has_usb_ancestor(interface_path: &str) -> Result<bool, String> {
    let Some(first) = interface_property_bytes(interface_path, &DEVPKEY_Device_Parent)?
        .and_then(property_string)
    else {
        return Ok(false);
    };
    let mut id = first;
    for _ in 0..MAX_ANCESTOR_DEPTH {
        if device_id_is_usb(&id) {
            return Ok(true);
        }
        let wide = HSTRING::from(id.as_str());
        // SAFETY: wide 在调用期间存活；devinst 由本调用写出。
        let mut devinst = 0u32;
        let cr = unsafe { CM_Locate_DevNodeW(&mut devinst, &wide, CM_LOCATE_DEVNODE_NORMAL) };
        if cr != CR_SUCCESS {
            return Err(format!("定位父设备节点失败: CONFIGRET {}", cr.0));
        }
        match devnode_property_bytes(devinst, &DEVPKEY_Device_Parent)? {
            Some(bytes) => match property_string(bytes) {
                Some(parent) => id = parent,
                None => return Ok(false),
            },
            None => return Ok(false),
        }
    }
    Ok(false)
}

/// 设备实例 ID 是否 USB 枚举（`USB\` 前缀，大小写不敏感）。
fn device_id_is_usb(device_id: &str) -> bool {
    device_id
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("USB\\"))
}

/// 从接口路径设备段提取 VID（`\\?\HID#VID_046D&PID_..#..`，大小写不敏感）。
/// 非 HID 形态、缺 `VID_####` 或非十六进制 → None。
fn vid_from_interface_path(path: &str) -> Option<u16> {
    let rest = path.strip_prefix(r"\\?\")?;
    let bus = rest.split('#').next()?;
    if !bus.eq_ignore_ascii_case("HID") {
        return None;
    }
    let device_id = rest.split('#').nth(1)?;
    let upper = device_id.to_ascii_uppercase();
    let pos = upper.find("VID_")?;
    let hex = upper.get(pos + 4..pos + 8)?;
    u16::from_str_radix(hex, 16).ok()
}

/// 读 CM 接口属性原始字节（probe → 填充两段式）。
fn interface_property_bytes(
    interface_path: &str,
    key: &DEVPROPKEY,
) -> Result<Option<Vec<u8>>, String> {
    let wide = HSTRING::from(interface_path);
    let call = |buffer: Option<*mut u8>, size: *mut u32| {
        // SAFETY: wide/key 在调用期间存活；buffer/size 由 cm_property_buffer 提供
        // 且在调用期间有效。
        let mut property_type = DEVPROPTYPE(0);
        unsafe {
            CM_Get_Device_Interface_PropertyW(&wide, key, &mut property_type, buffer, size, 0)
        }
    };
    cm_property_buffer(call)
}

/// 读 CM 设备节点属性原始字节（probe → 填充两段式）。
fn devnode_property_bytes(devinst: u32, key: &DEVPROPKEY) -> Result<Option<Vec<u8>>, String> {
    let call = |buffer: Option<*mut u8>, size: *mut u32| {
        // SAFETY: key 在调用期间存活；buffer/size 由 cm_property_buffer 提供且在
        // 调用期间有效；devinst 来自 CM_Locate_DevNodeW。
        let mut property_type = DEVPROPTYPE(0);
        unsafe { CM_Get_DevNode_PropertyW(devinst, key, &mut property_type, buffer, size, 0) }
    };
    cm_property_buffer(call)
}

/// CM 属性读取的公共状态机：CR_NO_SUCH_VALUE → Ok(None)（属性不存在）；
/// CR_BUFFER_SMALL/CR_SUCCESS 探得大小 → 填充；其余 CONFIGRET → Err。
fn cm_property_buffer(
    mut call: impl FnMut(Option<*mut u8>, *mut u32) -> CONFIGRET,
) -> Result<Option<Vec<u8>>, String> {
    let mut size = 0u32;
    let cr = call(None, &mut size);
    if cr == CR_NO_SUCH_VALUE {
        return Ok(None);
    }
    if cr != CR_BUFFER_SMALL && cr != CR_SUCCESS {
        return Err(format!("读取设备属性失败: CONFIGRET {}", cr.0));
    }
    if size == 0 {
        return Ok(None);
    }
    let mut buffer = vec![0u8; size as usize];
    let mut filled = size;
    let cr = call(Some(buffer.as_mut_ptr()), &mut filled);
    if cr != CR_SUCCESS {
        return Err(format!("读取设备属性（填充）失败: CONFIGRET {}", cr.0));
    }
    if filled as usize > buffer.len() {
        return Err("设备属性填充长度超出缓冲（异常）".to_string());
    }
    buffer.truncate(filled as usize);
    Ok(Some(buffer))
}

/// 属性字节 → GUID（16 字节；全零视为"空 ContainerId"，无关联依据）。
fn property_guid(bytes: Vec<u8>) -> Option<[u8; 16]> {
    if bytes.len() != 16 {
        return None;
    }
    let mut guid = [0u8; 16];
    guid.copy_from_slice(&bytes);
    if guid == [0u8; 16] {
        return None;
    }
    Some(guid)
}

/// 属性字节 → NUL 终止 UTF-16 字符串（DEVPROP_TYPE_STRING）。
fn property_string(bytes: Vec<u8>) -> Option<String> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> =
        bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    if end == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&units[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    /// 真实形态样本：Logitech 鼠标 Raw 接口（MI_00）与 vendor collection（MI_02）。
    const RAW_MOUSE_PATH: &str = r"\\?\HID#VID_046D&PID_C08B&MI_00#7&2f3a3d&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    const VENDOR_PATH: &str = r"\\?\HID#VID_046D&PID_C08B&MI_02#7&2f3a3d&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";

    /// 构造合格候选事实的快捷方式（其余字段按需覆盖）。
    fn candidate(path: &str, container: [u8; 16]) -> CandidateFacts {
        CandidateFacts {
            interface_path: path.to_string(),
            container: Some(container),
            vid: Some(LOGITECH_VID),
            input_report_bytes: VENDOR_REPORT_BYTES,
            output_report_bytes: VENDOR_REPORT_BYTES,
            declares_0x11_input: true,
            declares_0x11_output: true,
        }
    }

    fn container(seed: u8) -> [u8; 16] {
        let mut g = [0u8; 16];
        g[0] = seed;
        g
    }

    fn descriptor(physical: bool, path: Option<&str>) -> MouseSourceDescriptor {
        MouseSourceDescriptor {
            source_key: path.unwrap_or("virtual:unknown").to_ascii_lowercase(),
            model: clrecoder_core::event::DeviceKey {
                kind: clrecoder_core::codes::DeviceKind::Mouse,
                vid: 0x046D,
                pid: 0xC08B,
                name: "测试鼠标".to_string(),
            },
            interface_path: path.map(str::to_string),
            physical,
        }
    }

    // ---------- vid_from_interface_path ----------

    #[test]
    fn motion_dpi_vid_parse_accepts_logitech_and_case_variants() {
        assert_eq!(vid_from_interface_path(RAW_MOUSE_PATH), Some(0x046D));
        assert_eq!(vid_from_interface_path(VENDOR_PATH), Some(0x046D));
        // 小写十六进制与总线段（实测路径大写，防御性兼容）
        assert_eq!(
            vid_from_interface_path(r"\\?\hid#vid_046d&pid_c08b&mi_00#7&2f#{}"),
            Some(0x046D)
        );
    }

    #[test]
    fn motion_dpi_vid_parse_rejects_non_hid_and_non_hex() {
        // 非 HID 形态（RDP/Root/ACPI）
        assert_eq!(vid_from_interface_path(r"\\?\RDP_MOU#0000#{}"), None);
        assert_eq!(vid_from_interface_path(r"\\?\Root#RDP_KBD#0000"), None);
        // HID 形态但无 VID
        assert_eq!(vid_from_interface_path(r"\\?\HID#VIRTUAL_MOUSE#i"), None);
        // VID 不足 4 位 / 非十六进制
        assert_eq!(vid_from_interface_path(r"\\?\HID#VID_46&PID_1#i"), None);
        assert_eq!(vid_from_interface_path(r"\\?\HID#VID_XXY_Z#i"), None);
        assert_eq!(vid_from_interface_path(""), None);
    }

    // ---------- select_vendor_candidate（唯一性纯函数） ----------

    #[test]
    fn motion_dpi_select_unique_candidate_when_exactly_one_qualifies() {
        let c = container(0x2A);
        // 不合格候选们：CAPS 19 字节（键盘等通用集合）/ 不同容器，均不得造成歧义
        let mut small = candidate(r"\\?\HID#VID_046D&PID_C08B&MI_01#7&2f3a3d&0&0002#{}", c);
        small.input_report_bytes = 19;
        let other_container = candidate(r"\\?\HID#VID_046D&PID_C08B&MI_03#7&2f3a3d&0&0003#{}", container(0xFF));
        let facts = vec![small, other_container, candidate(VENDOR_PATH, c)];
        assert_eq!(
            select_vendor_candidate(c, RAW_MOUSE_PATH, &facts),
            SelectedVendor::UniquePath(VENDOR_PATH.to_string())
        );
    }

    #[test]
    fn motion_dpi_select_ambiguous_when_two_qualify() {
        let c = container(0x2B);
        let other = r"\\?\HID#VID_046D&PID_C08B&MI_03#7&2f3a3d&0&0002#{}";
        let facts = vec![candidate(VENDOR_PATH, c), candidate(other, c)];
        assert_eq!(
            select_vendor_candidate(c, RAW_MOUSE_PATH, &facts),
            SelectedVendor::Ambiguous
        );
    }

    #[test]
    fn motion_dpi_select_none_when_no_candidate_qualifies() {
        let c = container(0x2C);
        assert_eq!(select_vendor_candidate(c, RAW_MOUSE_PATH, &[]), SelectedVendor::None);
        // 有候选但全不合格（容器不符）
        let facts = vec![candidate(VENDOR_PATH, container(0xFF))];
        assert_eq!(
            select_vendor_candidate(c, RAW_MOUSE_PATH, &facts),
            SelectedVendor::None
        );
    }

    #[test]
    fn motion_dpi_select_requires_same_container() {
        // 同型号另一只鼠标（不同 ContainerId）不得被同容器候选吸引（§4.3：不能仅按
        // 型号/VID 抓到旁边另一只鼠标）
        let facts = vec![candidate(VENDOR_PATH, container(0x99))];
        assert_eq!(
            select_vendor_candidate(container(0x2D), RAW_MOUSE_PATH, &facts),
            SelectedVendor::None
        );
    }

    #[test]
    fn motion_dpi_select_requires_logitech_vid() {
        let c = container(0x2E);
        // 同容器、同 CAPS 但 VID=1532（其他厂商）：仅凭 VID 不匹配，不入候选
        let mut other_vendor = candidate(VENDOR_PATH, c);
        other_vendor.vid = Some(0x1532);
        let facts = vec![other_vendor];
        assert_eq!(
            select_vendor_candidate(c, RAW_MOUSE_PATH, &facts),
            SelectedVendor::None
        );
    }

    #[test]
    fn motion_dpi_select_requires_20_byte_caps_and_0x11_declaration() {
        let c = container(0x2F);
        // CAPS 19 字节：不是 20/20 vendor collection
        let mut small = candidate(VENDOR_PATH, c);
        small.input_report_bytes = 19;
        assert_eq!(
            select_vendor_candidate(c, RAW_MOUSE_PATH, &[small]),
            SelectedVendor::None
        );
        // CAPS 20/20 但未声明 0x11 输出报告（通用集合伪装）
        let mut no_out = candidate(VENDOR_PATH, c);
        no_out.declares_0x11_output = false;
        assert_eq!(
            select_vendor_candidate(c, RAW_MOUSE_PATH, &[no_out]),
            SelectedVendor::None
        );
        // 未声明 0x11 输入报告同样拒绝
        let mut no_in = candidate(VENDOR_PATH, c);
        no_in.declares_0x11_input = false;
        assert_eq!(
            select_vendor_candidate(c, RAW_MOUSE_PATH, &[no_in]),
            SelectedVendor::None
        );
    }

    #[test]
    fn motion_dpi_select_excludes_raw_interface_itself() {
        // 通用鼠标输入 collection（即 Raw 接口自身）即使形状合格也不作为可写端点
        let c = container(0x30);
        let raw_as_candidate = candidate(RAW_MOUSE_PATH, c);
        assert_eq!(
            select_vendor_candidate(c, RAW_MOUSE_PATH, &[raw_as_candidate]),
            SelectedVendor::None
        );
    }

    // ---------- find_direct_endpoint descriptor 门（不触 Win32） ----------

    #[test]
    fn motion_dpi_find_rejects_non_physical_and_pathless_descriptors() {
        // virtual:unknown 固定桶（physical=false，禁自动/手动物理 DPI）
        assert!(matches!(
            find_direct_endpoint(&descriptor(false, Some(RAW_MOUSE_PATH))),
            Ok(HidEndpointMatch::Unsupported)
        ));
        // 有物理标记但无原始路径
        assert!(matches!(
            find_direct_endpoint(&descriptor(true, None)),
            Ok(HidEndpointMatch::Unsupported)
        ));
        // 非 HID 形态路径（无法提取 VID）
        assert!(matches!(
            find_direct_endpoint(&descriptor(true, Some(r"\\?\RDP_MOU#0000#{}"))),
            Ok(HidEndpointMatch::Unsupported)
        ));
    }

    // ---------- exchange 到期门（不发起 IO） ----------

    #[test]
    fn motion_dpi_exchange_rejects_expired_deadline_before_any_io() {
        let mut endpoint = HidEndpoint::for_tests();
        let request = [0u8; 20];
        // 已过期的 deadline：直接 Err，且不触碰（无效的）句柄——若触 IO 早已崩溃
        let past = Instant::now() - std::time::Duration::from_secs(1);
        let result = exchange(&mut endpoint, &request, past);
        assert!(result.is_err(), "到期必须拒绝");
        assert!(result.unwrap_err().contains("到期"), "错误须说明到期语义");
    }

    // ---------- wait_overlapped_result（命名管道注入真实未决 IO） ----------

    /// 创建唯一的 overlapped 双工命名管道服务端（测试专用，进程级唯一名）。
    fn open_test_server(name: &HSTRING) -> HANDLE {
        // SAFETY: 测试内创建的私有命名管道；句柄由调用方关闭。
        let server = unsafe {
            CreateNamedPipeW(
                name,
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_WAIT,
                1,
                1024,
                1024,
                0,
                None,
            )
        };
        assert!(!server.is_invalid(), "创建测试管道失败");
        server
    }

    /// 到期取消全链路：无客户端连接的 ConnectNamedPipe 保持未决 → 到期 CancelIoEx →
    /// **等待 IRP 真正收尾后才返回**，返回后 OVERLAPPED 不再处于未决态（迟到结果不
    /// 复用/不采信的结构保证——若提前释放 OVERLAPPED 此断言即悬空访问）。
    #[test]
    fn motion_dpi_overlapped_wait_cancels_on_deadline_and_awaits_irp_completion() {
        let name = HSTRING::from(format!(
            r"\\.\pipe\clrecoder-s3-overlapped-deadline-{}",
            std::process::id()
        ));
        // SAFETY: 句柄/事件均由本测试创建并在测试尾关闭。
        unsafe {
            let server = open_test_server(&name);
            let event = CreateEventW(None, false, false, None).expect("创建事件失败");
            let mut ov = OVERLAPPED { hEvent: event, ..Default::default() };
            // 无客户端 → ConnectNamedPipe 以 ERROR_IO_PENDING 挂起（真实未决 IO）
            let submitted = ConnectNamedPipe(server, Some(&mut ov));
            assert!(
                submitted.is_err()
                    && submitted.unwrap_err().code() == HRESULT::from_win32(ERROR_IO_PENDING.0),
                "无客户端连接时 ConnectNamedPipe 应处于未决态"
            );

            let deadline = Instant::now() + std::time::Duration::from_millis(60);
            let started = Instant::now();
            let result = wait_overlapped_result(server, &mut ov, Some(deadline), "连接");
            let elapsed = started.elapsed();

            assert!(result.is_err(), "到期必须报错（业务降级）");
            assert!(
                elapsed >= std::time::Duration::from_millis(60),
                "取消必须发生在到期之后（实际 {elapsed:?}）"
            );
            // IRP 必已收尾：再次查询不得返回 IO_INCOMPLETE（否则 OVERLAPPED 仍被
            // 驱动持有，本测试栈帧的 ov 释放即悬空——正是合同禁止的情形）
            let mut transferred = 0u32;
            let again = GetOverlappedResult(server, &ov, &mut transferred, false);
            assert!(
                again.is_err()
                    && again.unwrap_err().code() != HRESULT::from_win32(ERROR_IO_INCOMPLETE.0),
                "取消后 IRP 必须已收尾，OVERLAPPED 不得仍处于未决态"
            );

            let _ = DisconnectNamedPipe(server);
            let _ = CloseHandle(event);
            let _ = CloseHandle(server);
        }
    }

    /// 完成路径：客户端在等待期间连接 → 等待以成功收尾返回（未取消）。
    #[test]
    fn motion_dpi_overlapped_wait_returns_when_io_completes_in_time() {
        let name = HSTRING::from(format!(
            r"\\.\pipe\clrecoder-s3-overlapped-connect-{}",
            std::process::id()
        ));
        // SAFETY: 句柄/事件均由本测试创建并在测试尾关闭；client 句柄在断言后关闭。
        unsafe {
            let server = open_test_server(&name);
            let event = CreateEventW(None, false, false, None).expect("创建事件失败");
            let mut ov = OVERLAPPED { hEvent: event, ..Default::default() };
            let submitted = ConnectNamedPipe(server, Some(&mut ov));
            assert!(
                submitted.is_err()
                    && submitted.unwrap_err().code() == HRESULT::from_win32(ERROR_IO_PENDING.0),
                "无客户端连接时 ConnectNamedPipe 应处于未决态"
            );

            // 50ms 后客户端连接：等待窗口 3s 足够。HANDLE 非 Send，跨线程仅回传原始值。
            let client_name = name.clone();
            let client = std::thread::spawn(move || -> isize {
                std::thread::sleep(std::time::Duration::from_millis(50));
                // SAFETY: client_name 在线程内存活；句柄以 isize 回传给 join 侧关闭。
                let handle = CreateFileW(
                    &client_name,
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
                .expect("客户端应成功连接");
                handle.0 as isize
            });
            let result =
                wait_overlapped_result(server, &mut ov, Some(Instant::now() + std::time::Duration::from_secs(3)), "连接");
            assert!(result.is_ok(), "客户端连接后等待应以成功收尾：{result:?}");
            let client_raw = client.join().expect("客户端线程不得 panic");
            // SAFETY: 原始值来自 join 结果，指向本测试创建、尚未关闭的客户端句柄。
            let client_handle = HANDLE(client_raw as *mut core::ffi::c_void);
            assert!(!client_handle.is_invalid(), "客户端句柄必须有效");

            let _ = DisconnectNamedPipe(server);
            let _ = CloseHandle(client_handle);
            let _ = CloseHandle(event);
            let _ = CloseHandle(server);
        }
    }

    // ---------- 辅助纯函数 ----------

    #[test]
    fn motion_dpi_device_id_usb_prefix_is_case_insensitive() {
        assert!(device_id_is_usb(r"USB\VID_046D&PID_C08B&MI_00"));
        assert!(device_id_is_usb(r"usb\vid_046d&pid_c08b"));
        assert!(!device_id_is_usb(r"HID\VID_046D&PID_C08B&MI_00"));
        assert!(!device_id_is_usb(r"BTHENUM\{xxxxxxxx}"));
        assert!(!device_id_is_usb("USB")); // 不足前缀长度
        assert!(!device_id_is_usb(""));
    }

    #[test]
    fn motion_dpi_property_guid_rejects_wrong_size_and_all_zero() {
        assert_eq!(property_guid(vec![0u8; 16]), None, "全零 ContainerId 视为空");
        assert_eq!(property_guid(vec![0u8; 15]), None);
        assert_eq!(property_guid(vec![0u8; 17]), None);
        let mut g = vec![0u8; 16];
        g[0] = 0x2A;
        assert_eq!(property_guid(g), Some({
            let mut e = [0u8; 16];
            e[0] = 0x2A;
            e
        }));
    }

    #[test]
    fn motion_dpi_property_string_decodes_utf16_and_requires_nul_or_nonempty() {
        // 带 NUL 的 UTF-16LE（REG_SZ 语义）
        let mut units: Vec<u8> = "USB\\VID_046D".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        units.extend([0, 0, 0xAB, 0xCD]); // 首个 NUL 后不参与
        assert_eq!(property_string(units).as_deref(), Some(r"USB\VID_046D"));
        // 无 NUL：取全部
        let units: Vec<u8> = "USB\\X".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        assert_eq!(property_string(units).as_deref(), Some("USB\\X"));
        // 空 / 奇数字节 / 首码元即 NUL → None
        assert_eq!(property_string(Vec::new()), None);
        assert_eq!(property_string(vec![0x41]), None);
        assert_eq!(property_string(vec![0, 0, 0x41, 0]), None);
    }
}
