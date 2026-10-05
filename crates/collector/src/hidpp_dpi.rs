//! 有限 HID++ 编解码与只读当前 DPI 探测（motion-dpi §4.3）。
//!
//! 支持范围（§1/§4.3 锁定，不得自行扩大）：USB 直连 Logitech、HID++ 2.0、feature
//! 0x2201 AdjustableDpi、单传感器；device index 固定 0xFF、software id 固定 0x0B、
//! long report 0x11 总 20 字节。**只读**：不发送 setSensorDpi 等任何设置命令、不做
//! 型号猜值；接收器（GetDeviceType=7）、多传感器、缺 feature、非 USB 直连、CAPS
//! 不符均判 Unsupported（走手动），transport/协议/超时失败判 Unavailable（稍后重试）。
//!
//! 探测序列（§4.3"自动读取的实际实现"，依据 Logitech cpg-docs IRoot 规格与
//! lekensteyn x2201 文档）：
//! 1. [`hid_transport::find_direct_endpoint`]：唯一关联；Err→Unavailable，枚举判定直通；
//! 2. Root ping（function 1，参数 [0,0,nonce]）：nonce 回声 + protocolMajor≥2 且非
//!    0x8F（官方 IRoot 规格：0x02=2.0 旧版、0x04=2.0、0x8F=HID++ 1.0——仅按"≥2"
//!    会放行 1.0 标记值，故一并排除）；
//! 3. getFeature(0x0005) → GetDeviceType（function 2）：Mouse(3) 才继续；
//!    Receiver(7)/其他类型/缺 feature/无法确认均 Unsupported；
//! 4. getFeature(0x2201)：index 0 → Unsupported；
//! 5. getSensorCount（function 0）必须为 1（多传感器拒绝）；
//! 6. getSensorDpi(sensorIdx=0)（function 2，参数 [0]）：payload[0]=0 回声、
//!    payload[1..3] 大端**当前值**（不是 defaultDpi），1..=57343 才 Available。
//!
//! `timeout`（生产 1500ms，由 S4 传入）是业务结果接受期限与"停止发后续查询"的期限：
//! 每个查询发起前检查到期即降级 Unavailable；IO 取消与资源持有由 hid_transport 负责。
//!
//! 可测性拆分：编解码（[`encode_request`]/[`decode_response`]）与探测序列
//! （[`probe_with`]，transport 可注入）为纯/可注入函数，单测覆盖全部分支；
//! 生产 transport 是 [`hid_transport`] 的薄适配（[`RealTransport`]），测试用脚本化
//! 假 transport——禁止硬件写配置，不触真实 HID。
//!
//! S3 交付态：本模块公开入口在 S4（mouse_dpi/motion_runtime 接线）前无 crate 内调用
//! 方，临时允许 dead_code 以维持零警告基线，S4 接线后移除。
#![allow(dead_code)]

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, Instant};

use clrecoder_core::motion::MouseSourceDescriptor;

use crate::hid_transport::{self, HidEndpoint, HidEndpointMatch};

/// long report ID（§4.3：请求一律 0x11、总 20 字节）。
pub const REPORT_LONG: u8 = 0x11;
/// short report ID（§4.3：接受 0x10 的 7 字节有效帧，可能由 20 字节 Windows 缓冲承载）。
pub const REPORT_SHORT: u8 = 0x10;

/// 固定 device index（§4.3：不试其他 device index）。
const DEVICE_INDEX: u8 = 0xFF;
/// 固定 software id（§4.3）。
const SOFTWARE_ID: u8 = 0x0B;
/// Root feature（0x0000）。
const ROOT_FEATURE: u8 = 0x00;
/// Root function 0：getFeature。
const ROOT_FN_GET_FEATURE: u8 = 0x0;
/// Root function 1：ping（官方 IRoot 规格名 GetProtocolVersion）。
const ROOT_FN_PING: u8 = 0x1;
/// Device Name and Type feature（0x0005）。
const FEATURE_DEVICE_TYPE: u16 = 0x0005;
/// GetDeviceType 的 function index。
const DEVICE_TYPE_FN: u8 = 0x2;
/// DeviceType 枚举：Mouse（厂商 0x0005 规格枚举）。
const DEVICE_TYPE_MOUSE: u8 = 0x03;
/// DeviceType 枚举：Receiver——ContainerID/0xFF 不能排除接收器，由本查询排除。
const DEVICE_TYPE_RECEIVER: u8 = 0x07;
/// AdjustableDpi feature（0x2201）。
const FEATURE_ADJUSTABLE_DPI: u16 = 0x2201;
/// getSensorCount 的 function index。
const SENSOR_COUNT_FN: u8 = 0x0;
/// getSensorDpi 的 function index。
const SENSOR_DPI_FN: u8 = 0x2;
/// 可采信 DPI 下界（§4.3/x2201：1..57343）。
const DPI_MIN: u32 = 1;
/// 可采信 DPI 上界（0xDFFF）。
const DPI_MAX: u32 = 57343;
/// 官方 IRoot 规格中 HID++ 1.0 的 protocolMajor 标记值（0x8F=143，仅按 ≥2 会被放行）。
const PROTOCOL_MAJOR_1_0: u32 = 0x8F;
/// 同一查询在通知穿插下的最大交换次数（只读幂等，有界重发）。
const MAX_ASK_ATTEMPTS: usize = 3;

/// HID++ 请求（§4.3 类型归属固定）：编码为 long report 20 字节帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidppRequest {
    /// feature index（响应按它回显匹配）
    pub feature: u8,
    /// function（0..16，编入第 3 字节高 4 位）
    pub function: u8,
    /// software id（0..16，编入第 3 字节低 4 位；本协议固定 0x0B）
    pub sw_id: u8,
    /// 参数（从第 4 字节起，≤16 字节，余下补 0）
    pub params: Vec<u8>,
}

/// DPI 探测结果（§4.3 类型归属固定）：Unavailable 才随时间重试；
/// Unsupported/Ambiguous 是判定，同连接不重探。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiProbeResult {
    /// 读到当前 DPI（传感器当前档位值，不是 defaultDpi/最大值）
    Available(u32),
    /// 设备在支持范围外（接收器/多传感器/缺 feature/非 2.0 等）——走手动
    Unsupported,
    /// 同 ContainerID 命中多个候选 vendor collection——拒绝采信
    Ambiguous,
    /// transport/IO/协议偏差/超时——业务降级，不影响原输入，稍后重试
    Unavailable,
}

/// 编码请求（§4.3）：第 0 字节 report（0x11）、第 1 字节 0xFF、第 2 字节 feature、
/// 第 3 字节 (function<<4)|sw_id，params 从 4 开始、余下补 0。
pub fn encode_request(request: &HidppRequest) -> Result<[u8; 20], String> {
    if request.function > 0x0F {
        return Err(format!("HID++ function 超出 4 位域: {}", request.function));
    }
    if request.sw_id > 0x0F {
        return Err(format!("HID++ software id 超出 4 位域: {}", request.sw_id));
    }
    if request.params.len() > 16 {
        return Err(format!(
            "HID++ 参数超出 long report 容量: {} > 16",
            request.params.len()
        ));
    }
    let mut frame = [0u8; 20];
    frame[0] = REPORT_LONG;
    frame[1] = DEVICE_INDEX;
    frame[2] = request.feature;
    frame[3] = (request.function << 4) | request.sw_id;
    frame[4..4 + request.params.len()].copy_from_slice(&request.params);
    Ok(frame)
}

/// 解码响应（§4.3）：
/// - 接受 0x10 的 7 字节有效帧（可能由 20 字节 Windows 缓冲承载）与 0x11 的 20 字节
///   帧，按 report ID 校验最小字节数（短包 → Err）；
/// - 按 device/feature/function/sw_id 匹配（device 固定 0xFF），ping 另校验 nonce 回声；
/// - 其他响应/通知（身份不符、其他 report ID、**ping nonce 失配**——如同 sw_id 外来
///   ping 的应答）返回 None，由探测层的有界重发消化；
/// - 协议错误帧（function 半字节 0xF——本协议不发送该 function）→ Err。
pub fn decode_response(request: &HidppRequest, bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let Some(&report) = bytes.first() else {
        return Err("HID++ 响应为空".to_string());
    };
    let params: &[u8] = match report {
        REPORT_SHORT => {
            if bytes.len() < 7 {
                return Err(format!("HID++ 短帧不足 7 字节: {}", bytes.len()));
            }
            &bytes[4..7]
        }
        REPORT_LONG => {
            if bytes.len() < 20 {
                return Err(format!("HID++ 长帧不足 20 字节: {}", bytes.len()));
            }
            &bytes[4..20]
        }
        _ => return Ok(None), // 其他 report（vendor 通知等）→ 其他响应
    };
    let device = bytes[1];
    let feature = bytes[2];
    let function = bytes[3] >> 4;
    let sw_id = bytes[3] & 0x0F;
    // 协议错误帧：function 半字节 0xF，feature/sw_id 仍回显请求方身份
    if function == 0x0F {
        if device == DEVICE_INDEX && feature == request.feature && sw_id == request.sw_id {
            let code = params.first().copied().unwrap_or(0);
            return Err(format!(
                "HID++ 协议错误帧: feature 0x{:02X} 错误码 0x{code:02X}",
                request.feature
            ));
        }
        return Ok(None); // 非本请求的错误帧 → 其他响应
    }
    if device != DEVICE_INDEX
        || feature != request.feature
        || function != request.function
        || sw_id != request.sw_id
    {
        return Ok(None);
    }
    // ping（Root function 1）额外校验 nonce 回声（请求参数第 3 字节 ↔ 响应参数第 3 字节）：
    // 回声不符不落入"协议 error"，归"其他响应"→ None（同 sw_id 外来 ping 应答恰是
    // ask() 有界重发针对的场景），持续失配时由重试上限自然降级。
    if request.feature == ROOT_FEATURE && request.function == ROOT_FN_PING {
        let expected = request.params.get(2).copied().unwrap_or(0);
        let echoed = params.get(2).copied().unwrap_or(0);
        if echoed != expected {
            return Ok(None);
        }
    }
    Ok(Some(params.to_vec()))
}

/// 探测当前 DPI（§4.3 公开入口）：唯一关联后按支持范围逐级确认，读取传感器当前值。
///
/// 生产 timeout 为 1500ms（由 S4 传入）；transport Err 一律映射 Unavailable，
/// 不解析中文字符串（§4.3）。
pub fn probe_current_dpi(descriptor: &MouseSourceDescriptor, timeout: Duration) -> DpiProbeResult {
    probe_with(descriptor, timeout, &mut RealTransport)
}

/// transport 抽象：生产实现转发 [`hid_transport`]；单测注入脚本化假 transport。
pub(crate) trait DpiTransport {
    /// 关联唯一 vendor collection 端点。
    fn find_direct_endpoint(
        &mut self,
        descriptor: &MouseSourceDescriptor,
    ) -> Result<HidEndpointMatch, String>;
    /// 一次 20 字节请求/响应交换（到期/取消语义由实现保证）。
    fn exchange(
        &mut self,
        endpoint: &mut HidEndpoint,
        request: &[u8; 20],
        deadline: Instant,
    ) -> Result<Vec<u8>, String>;
}

/// 生产 transport（薄适配，无状态）。
struct RealTransport;

impl DpiTransport for RealTransport {
    fn find_direct_endpoint(
        &mut self,
        descriptor: &MouseSourceDescriptor,
    ) -> Result<HidEndpointMatch, String> {
        hid_transport::find_direct_endpoint(descriptor)
    }

    fn exchange(
        &mut self,
        endpoint: &mut HidEndpoint,
        request: &[u8; 20],
        deadline: Instant,
    ) -> Result<Vec<u8>, String> {
        hid_transport::exchange(endpoint, request, deadline)
    }
}

/// 可注入 transport 的探测核心（单测与未来 fixture 共用）。
pub(crate) fn probe_with(
    descriptor: &MouseSourceDescriptor,
    timeout: Duration,
    transport: &mut dyn DpiTransport,
) -> DpiProbeResult {
    let deadline = Instant::now() + timeout;
    // 到期即降级：不触 transport、不发任何查询（"停止发后续查询"的最严格形式）。
    if Instant::now() >= deadline {
        return DpiProbeResult::Unavailable;
    }
    let mut endpoint = match transport.find_direct_endpoint(descriptor) {
        Err(_) => return DpiProbeResult::Unavailable, // transport Err 映射 Unavailable
        Ok(HidEndpointMatch::Unsupported) => return DpiProbeResult::Unsupported,
        Ok(HidEndpointMatch::Ambiguous) => return DpiProbeResult::Ambiguous,
        Ok(HidEndpointMatch::Unique(endpoint)) => endpoint,
    };
    probe_sequence(&mut endpoint, deadline, transport)
}

/// 逐级确认 + 读取（§4.3 序列）。错误映射约定：
/// - 交换失败/超时/协议错误帧/nonce 不符：ping 与传感器查询 → Unavailable（§5
///   "协议偏差降为 Unavailable"）；资格确认（getFeature/GetDeviceType）→ Unsupported
///   （§4.3"缺 feature 或无法确认均 Unsupported"）；
/// - major<2 / DeviceType≠Mouse / 传感器数≠1 → Unsupported（支持范围外）；
/// - 当前 DPI 读数越界或回声异常 → Unavailable（读数不可采信，稍后重试）。
fn probe_sequence(
    endpoint: &mut HidEndpoint,
    deadline: Instant,
    transport: &mut dyn DpiTransport,
) -> DpiProbeResult {
    // 1) Root ping：nonce 回声 + HID++ 2.0
    let nonce = fresh_nonce();
    let ping = HidppRequest {
        feature: ROOT_FEATURE,
        function: ROOT_FN_PING,
        sw_id: SOFTWARE_ID,
        params: vec![0, 0, nonce],
    };
    let Some(reply) = ask(endpoint, &ping, deadline, transport) else {
        return DpiProbeResult::Unavailable;
    };
    let major = reply.first().copied().unwrap_or(0) as u32;
    if major < 2 || major == PROTOCOL_MAJOR_1_0 {
        return DpiProbeResult::Unsupported; // 非 HID++ 2.0（0x8F 为 1.0 标记）
    }

    // 2) getFeature(0x0005) → feature index
    let Some(type_index) = get_feature_index(endpoint, FEATURE_DEVICE_TYPE, deadline, transport)
    else {
        // 到期属于业务降级（Unavailable）；未到期的失败才是"缺 feature/无法确认"
        return if Instant::now() >= deadline {
            DpiProbeResult::Unavailable
        } else {
            DpiProbeResult::Unsupported
        };
    };
    if type_index == 0 {
        return DpiProbeResult::Unsupported;
    }

    // 3) GetDeviceType（function 2，无参数）：必须 Mouse(3)；Receiver(7)/其他/空回复均不支持
    let type_request = HidppRequest {
        feature: type_index,
        function: DEVICE_TYPE_FN,
        sw_id: SOFTWARE_ID,
        params: Vec::new(),
    };
    let Some(type_reply) = ask(endpoint, &type_request, deadline, transport) else {
        return if Instant::now() >= deadline {
            DpiProbeResult::Unavailable
        } else {
            DpiProbeResult::Unsupported // 无法确认
        };
    };
    match type_reply.first().copied() {
        Some(DEVICE_TYPE_MOUSE) => {}
        _ => return DpiProbeResult::Unsupported,
    }

    // 4) getFeature(0x2201) → feature index（0 = 设备不支持 AdjustableDpi）
    let Some(dpi_index) =
        get_feature_index(endpoint, FEATURE_ADJUSTABLE_DPI, deadline, transport)
    else {
        return if Instant::now() >= deadline {
            DpiProbeResult::Unavailable
        } else {
            DpiProbeResult::Unsupported
        };
    };
    if dpi_index == 0 {
        return DpiProbeResult::Unsupported;
    }

    // 5) getSensorCount（function 0）：必须恰为 1（多传感器/无传感器拒绝）
    let count_request = HidppRequest {
        feature: dpi_index,
        function: SENSOR_COUNT_FN,
        sw_id: SOFTWARE_ID,
        params: Vec::new(),
    };
    let Some(count_reply) = ask(endpoint, &count_request, deadline, transport) else {
        return DpiProbeResult::Unavailable; // 0x2201 已确认在场，异常按协议偏差降级
    };
    if count_reply.first().copied() != Some(1) {
        return DpiProbeResult::Unsupported;
    }

    // 6) getSensorDpi(sensorIdx=0)（function 2，参数 [0]）：取当前值，不取 defaultDpi
    let dpi_request = HidppRequest {
        feature: dpi_index,
        function: SENSOR_DPI_FN,
        sw_id: SOFTWARE_ID,
        params: vec![0],
    };
    let Some(payload) = ask(endpoint, &dpi_request, deadline, transport) else {
        return DpiProbeResult::Unavailable;
    };
    // payload = [sensorIdx 回声, 当前 DPI 大端两字节, default DPI 大端两字节]
    // 例：[0, 0x03, 0x20, 0x01, 0x90] 当前 800、默认 400——只返回 800。
    if payload.first().copied() != Some(0) || payload.len() < 5 {
        return DpiProbeResult::Unavailable; // 回声/长度异常 → 协议偏差
    }
    let value = u16::from_be_bytes([payload[1], payload[2]]) as u32;
    if (DPI_MIN..=DPI_MAX).contains(&value) {
        DpiProbeResult::Available(value)
    } else {
        DpiProbeResult::Unavailable
    }
}

/// 发送一个只读查询并取回匹配的响应参数（§4.3）：
/// - 每次交换发起前检查业务期限，到期立即停发后续查询 → None；
/// - transport Err（含到期取消）→ None；协议错误帧/nonce 不符 → None；
/// - None 响应（通知穿插等）→ 有界重发同一只读查询（幂等），最多 [`MAX_ASK_ATTEMPTS`] 次。
fn ask(
    endpoint: &mut HidEndpoint,
    request: &HidppRequest,
    deadline: Instant,
    transport: &mut dyn DpiTransport,
) -> Option<Vec<u8>> {
    let wire = encode_request(request).ok()?;
    for _ in 0..MAX_ASK_ATTEMPTS {
        if Instant::now() >= deadline {
            return None; // 停止发后续查询（1500ms 业务期限语义）
        }
        match transport.exchange(endpoint, &wire, deadline) {
            Err(_) => return None,
            Ok(bytes) => match decode_response(request, &bytes) {
                Ok(Some(params)) => return Some(params),
                Ok(None) => continue, // 通知/他人响应：重发同一只读查询
                Err(_) => return None,
            },
        }
    }
    None
}

/// Root getFeature（function 0，参数 [featureID 大端两字节, 0]）→ feature index。
/// 返回 None = 查询失败（由调用方按阶段映射）；index 0 = 设备不支持该 feature。
fn get_feature_index(
    endpoint: &mut HidEndpoint,
    feature_id: u16,
    deadline: Instant,
    transport: &mut dyn DpiTransport,
) -> Option<u8> {
    let request = HidppRequest {
        feature: ROOT_FEATURE,
        function: ROOT_FN_GET_FEATURE,
        sw_id: SOFTWARE_ID,
        params: vec![(feature_id >> 8) as u8, (feature_id & 0xFF) as u8, 0],
    };
    let reply = ask(endpoint, &request, deadline, transport)?;
    reply.first().copied()
}

/// 本次探测的 ping nonce（RandomState 派生；只要求与请求一一对应、不固定复用）。
fn fresh_nonce() -> u8 {
    RandomState::new().build_hasher().finish() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::codes::DeviceKind;
    use clrecoder_core::event::DeviceKey;
    use std::collections::VecDeque;

    /// 脚本化假 transport（§8-S3：禁止硬件写配置，不触真实 HID）。
    struct FakeTransport {
        find: Result<HidEndpointMatchKind, String>,
        script: VecDeque<ExchangeOutcome>,
        /// 已发出的请求记录（验证"到期停止发后续查询"与查询序列）
        requests: Vec<[u8; 20]>,
        find_calls: usize,
        /// 下一次 exchange 前 burn 的时间（模拟真实 IO 耗时导致跨过 deadline）
        burn_next: Option<Duration>,
    }

    /// find 结果的值形状（Unique 用测试端点在 trait 方法里物化）。
    enum HidEndpointMatchKind {
        Unsupported,
        Ambiguous,
        Unique,
    }

    enum ExchangeOutcome {
        Reply(Vec<u8>),
        /// ping 动态应答：回声**实际请求**中的 nonce（major/minor 由脚本给定）
        PingReply { major: u8, minor: u8 },
        Fail(String),
    }

    impl FakeTransport {
        fn unique() -> Self {
            Self { find: Ok(HidEndpointMatchKind::Unique), script: VecDeque::new(), requests: Vec::new(), find_calls: 0, burn_next: None }
        }

        fn with_find(find: Result<HidEndpointMatchKind, String>) -> Self {
            Self { find, script: VecDeque::new(), requests: Vec::new(), find_calls: 0, burn_next: None }
        }

        fn push(mut self, outcome: ExchangeOutcome) -> Self {
            self.script.push_back(outcome);
            self
        }

        fn ping_ok(self, major: u8) -> Self {
            self.push(ExchangeOutcome::PingReply { major, minor: 0x04 })
        }

        fn reply(self, frame: Vec<u8>) -> Self {
            self.push(ExchangeOutcome::Reply(frame))
        }

        fn fail(self, message: &str) -> Self {
            self.push(ExchangeOutcome::Fail(message.to_string()))
        }

        fn burn_next(mut self, delay: Duration) -> Self {
            self.burn_next = Some(delay);
            self
        }

        /// 构造 20 字节 0x11 响应帧（device 0xFF、给定 feature/function/sw_id）。
        fn frame(feature: u8, function: u8, params: &[u8]) -> Vec<u8> {
            let mut frame = vec![0u8; 20];
            frame[0] = REPORT_LONG;
            frame[1] = DEVICE_INDEX;
            frame[2] = feature;
            frame[3] = (function << 4) | SOFTWARE_ID;
            frame[4..4 + params.len()].copy_from_slice(params);
            frame
        }

        /// 静态 ping 帧（nonce 由测试显式给定；用于 nonce 失配/通知穿插场景）。
        fn static_ping_frame(major: u8, nonce: u8) -> Vec<u8> {
            Self::frame(ROOT_FEATURE, ROOT_FN_PING, &[major, 0x04, nonce])
        }

        fn feature_index_reply(index: u8) -> Vec<u8> {
            Self::frame(ROOT_FEATURE, ROOT_FN_GET_FEATURE, &[index, 0, 0])
        }

        fn device_type_reply(feature_index: u8, device_type: u8) -> Vec<u8> {
            Self::frame(feature_index, DEVICE_TYPE_FN, &[device_type, 0, 0])
        }

        fn sensor_count_reply(feature_index: u8, count: u8) -> Vec<u8> {
            Self::frame(feature_index, SENSOR_COUNT_FN, &[count, 0, 0])
        }

        fn sensor_dpi_reply(feature_index: u8, current: u16, default: u16, sensor_echo: u8) -> Vec<u8> {
            Self::frame(
                feature_index,
                SENSOR_DPI_FN,
                &[sensor_echo, (current >> 8) as u8, (current & 0xFF) as u8, (default >> 8) as u8, (default & 0xFF) as u8],
            )
        }

        /// 标准成功序列（ping→getFeature05→type→getFeature22→count→dpi，当前 800/默认 400）。
        /// 0x0005/0x2201 的 index 与 getFeature 应答一致（0x05/0x22）。
        fn script_happy(self) -> Self {
            self.ping_ok(0x04)
                .reply(Self::feature_index_reply(0x05))
                .reply(Self::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
                .reply(Self::feature_index_reply(0x22))
                .reply(Self::sensor_count_reply(0x22, 1))
                .reply(Self::sensor_dpi_reply(0x22, 800, 400, 0))
        }
    }

    impl DpiTransport for FakeTransport {
        fn find_direct_endpoint(
            &mut self,
            _descriptor: &MouseSourceDescriptor,
        ) -> Result<HidEndpointMatch, String> {
            self.find_calls += 1;
            match &self.find {
                Err(message) => Err(message.clone()),
                Ok(HidEndpointMatchKind::Unsupported) => Ok(HidEndpointMatch::Unsupported),
                Ok(HidEndpointMatchKind::Ambiguous) => Ok(HidEndpointMatch::Ambiguous),
                Ok(HidEndpointMatchKind::Unique) => Ok(HidEndpointMatch::Unique(HidEndpoint::for_tests())),
            }
        }

        fn exchange(
            &mut self,
            _endpoint: &mut HidEndpoint,
            request: &[u8; 20],
            _deadline: Instant,
        ) -> Result<Vec<u8>, String> {
            if let Some(delay) = self.burn_next.take() {
                std::thread::sleep(delay);
            }
            self.requests.push(*request);
            match self.script.pop_front() {
                Some(ExchangeOutcome::Reply(bytes)) => Ok(bytes),
                Some(ExchangeOutcome::PingReply { major, minor }) => {
                    // 回声请求中的 nonce（wire 第 6 字节 = ping 请求参数第 3 字节）
                    let nonce = request[6];
                    Ok(Self::frame(ROOT_FEATURE, ROOT_FN_PING, &[major, minor, nonce]))
                }
                Some(ExchangeOutcome::Fail(message)) => Err(message),
                None => panic!("假 transport 脚本耗尽——探测发出了脚本外的查询: {request:02X?}"),
            }
        }
    }

    fn descriptor() -> MouseSourceDescriptor {
        MouseSourceDescriptor {
            source_key: r"\\?\hid#vid_046d&pid_c08b&mi_00#7&2f3a3d&0&0000".to_string(),
            model: DeviceKey {
                kind: DeviceKind::Mouse,
                vid: 0x046D,
                pid: 0xC08B,
                name: "测试鼠标".to_string(),
            },
            interface_path: Some(
                r"\\?\HID#VID_046D&PID_C08B&MI_00#7&2f3a3d&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}"
                    .to_string(),
            ),
            physical: true,
        }
    }

    fn probe(fake: &mut FakeTransport, timeout: Duration) -> DpiProbeResult {
        probe_with(&descriptor(), timeout, fake)
    }

    // ---------- encode_request ----------

    #[test]
    fn motion_dpi_encode_layout_bytes_params_and_zero_padding() {
        let request = HidppRequest {
            feature: 0x22,
            function: 0x2,
            sw_id: SOFTWARE_ID,
            params: vec![0x00],
        };
        let frame = encode_request(&request).expect("合法请求必须可编码");
        assert_eq!(frame[0], REPORT_LONG);
        assert_eq!(frame[1], DEVICE_INDEX);
        assert_eq!(frame[2], 0x22);
        assert_eq!(frame[3], (0x2 << 4) | 0x0B);
        assert_eq!(frame[4], 0x00);
        assert!(frame[5..].iter().all(|&b| b == 0), "余下必须补 0");

        // getFeature 参数：featureID 大端两字节 + 0（0x2201）
        let get_feature = HidppRequest {
            feature: ROOT_FEATURE,
            function: ROOT_FN_GET_FEATURE,
            sw_id: SOFTWARE_ID,
            params: vec![0x22, 0x01, 0x00],
        };
        let frame = encode_request(&get_feature).expect("合法请求必须可编码");
        assert_eq!(&frame[4..7], &[0x22, 0x01, 0x00]);

        // ping 参数 [0,0,nonce]；满 16 字节参数合法
        let full = HidppRequest {
            feature: 0x01,
            function: 0xF,
            sw_id: 0xE,
            params: (0u8..16).collect(),
        };
        let frame = encode_request(&full).expect("16 字节参数必须可编码");
        assert_eq!(frame[3], 0xFE);
        assert_eq!(&frame[4..20], &{
            let mut expected = [0u8; 16];
            for (i, slot) in expected.iter_mut().enumerate() {
                *slot = i as u8;
            }
            expected
        });
    }

    #[test]
    fn motion_dpi_encode_rejects_oversized_function_swid_and_params() {
        let base = |function: u8, sw_id: u8, params: Vec<u8>| HidppRequest {
            feature: 0x01,
            function,
            sw_id,
            params,
        };
        assert!(encode_request(&base(0x10, 0x0B, vec![])).is_err(), "function 16 越界");
        assert!(encode_request(&base(0x2, 0x10, vec![])).is_err(), "sw_id 16 越界");
        assert!(encode_request(&base(0x2, 0x0B, vec![0u8; 17])).is_err(), "17 字节参数越界");
        assert!(encode_request(&base(0x2, 0x0B, vec![0u8; 16])).is_ok());
    }

    // ---------- decode_response ----------

    #[test]
    fn motion_dpi_decode_accepts_long_20_and_short_7_with_padding() {
        let request = HidppRequest {
            feature: 0x22,
            function: 0x2,
            sw_id: SOFTWARE_ID,
            params: vec![0x00],
        };
        // 0x11 的 20 字节帧
        let long = FakeTransport::frame(0x22, 0x2, &[0x00, 0x03, 0x20, 0x01, 0x90]);
        assert_eq!(
            decode_response(&request, &long).expect("合法长帧必须解码"),
            Some(vec![0x00, 0x03, 0x20, 0x01, 0x90, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        );
        // 0x10 的 7 字节帧
        let short = vec![REPORT_SHORT, DEVICE_INDEX, 0x22, (0x2 << 4) | SOFTWARE_ID, 0x05, 0x00, 0x00];
        assert_eq!(
            decode_response(&request, &short).expect("合法短帧必须解码"),
            Some(vec![0x05, 0x00, 0x00])
        );
        // 0x10 由 20 字节 Windows 缓冲承载：第 7 字节后是填充，不影响解码
        let mut padded = short.clone();
        padded.resize(20, 0xEE);
        assert_eq!(
            decode_response(&request, &padded).expect("带填充的短帧必须解码"),
            Some(vec![0x05, 0x00, 0x00])
        );
    }

    #[test]
    fn motion_dpi_decode_rejects_short_packets_and_empty() {
        let request = HidppRequest { feature: 0x22, function: 0x2, sw_id: SOFTWARE_ID, params: vec![] };
        // 短帧不足 7 字节
        let short = vec![REPORT_SHORT, DEVICE_INDEX, 0x22, (0x2 << 4) | SOFTWARE_ID, 0x01, 0x02];
        assert!(decode_response(&request, &short).is_err(), "6 字节短帧必须拒绝");
        // 长帧不足 20 字节
        let mut long = FakeTransport::frame(0x22, 0x2, &[0x00]);
        long.truncate(19);
        assert!(decode_response(&request, &long).is_err(), "19 字节长帧必须拒绝");
        // 空响应
        assert!(decode_response(&request, &[]).is_err(), "空响应必须拒绝");
    }

    #[test]
    fn motion_dpi_decode_ignores_mismatched_identity_and_other_reports() {
        let request = HidppRequest { feature: 0x22, function: 0x2, sw_id: SOFTWARE_ID, params: vec![] };
        let frame = FakeTransport::frame(0x22, 0x2, &[0x01]);
        // 错 sw_id（他人请求的响应）→ None
        let mut wrong_sw = frame.clone();
        wrong_sw[3] = (0x2 << 4) | 0x05;
        assert_eq!(decode_response(&request, &wrong_sw), Ok(None), "错 sw_id → 其他响应");
        // 错 feature → None
        let wrong_feature = FakeTransport::frame(0x21, 0x2, &[0x01]);
        assert_eq!(decode_response(&request, &wrong_feature), Ok(None), "错 feature → 其他响应");
        // 错 function → None
        let wrong_function = FakeTransport::frame(0x22, 0x1, &[0x01]);
        assert_eq!(decode_response(&request, &wrong_function), Ok(None), "错 function → 其他响应");
        // 错 device → None
        let mut wrong_device = frame.clone();
        wrong_device[1] = 0x01;
        assert_eq!(decode_response(&request, &wrong_device), Ok(None), "错 device → 其他响应");
        // 其他 report ID（vendor 通知等）→ None
        let mut other_report = frame.clone();
        other_report[0] = 0x12;
        assert_eq!(decode_response(&request, &other_report), Ok(None), "其他 report → 其他响应");
    }

    #[test]
    fn motion_dpi_decode_flags_protocol_error_frames() {
        let request = HidppRequest { feature: 0x22, function: 0x2, sw_id: SOFTWARE_ID, params: vec![] };
        // function 半字节 0xF + 错误码 0x02 → Err
        let mut error_frame = FakeTransport::frame(0x22, 0xF, &[0x02]);
        error_frame[3] = (0xF << 4) | SOFTWARE_ID;
        let result = decode_response(&request, &error_frame);
        assert!(result.is_err(), "协议错误帧必须 Err");
        assert!(result.unwrap_err().contains("0x02"), "错误信息须含错误码");
        // 他人请求的错误帧（feature 不符）→ None
        let mut other_error = error_frame.clone();
        other_error[2] = 0x11;
        other_error[3] = (0xF << 4) | SOFTWARE_ID;
        assert_eq!(decode_response(&request, &other_error), Ok(None));
    }

    #[test]
    fn motion_dpi_decode_ping_validates_nonce_echo() {
        let ping = HidppRequest {
            feature: ROOT_FEATURE,
            function: ROOT_FN_PING,
            sw_id: SOFTWARE_ID,
            params: vec![0, 0, 0x5A],
        };
        // 回声一致 → Some(16 字节参数，含尾部补零)：[major, minor, nonce, 0…]
        let reply = FakeTransport::static_ping_frame(0x04, 0x5A);
        let mut expected = vec![0x04, 0x04, 0x5A];
        expected.resize(16, 0);
        assert_eq!(
            decode_response(&ping, &reply).expect("nonce 一致的 ping 必须解码"),
            Some(expected)
        );
        // 回声不一致（同 sw_id 外来 ping 应答）→ None（"其他响应"，由 ask 有界重发消化）
        let mismatched = FakeTransport::static_ping_frame(0x04, 0x77);
        assert_eq!(
            decode_response(&ping, &mismatched),
            Ok(None),
            "错 nonce 必须按其他响应返回 None"
        );
    }

    // ---------- probe_with（假 transport，全流程） ----------

    #[test]
    fn motion_dpi_probe_happy_path_returns_current_800_not_default_400() {
        let mut fake = FakeTransport::unique().script_happy();
        let result = probe(&mut fake, Duration::from_secs(2));
        assert_eq!(result, DpiProbeResult::Available(800), "只能返回当前值 800，不是默认 400");
        assert_eq!(fake.find_calls, 1);
        assert_eq!(fake.requests.len(), 6, "恰好 6 个只读查询");
        // 最后一个查询是 getSensorDpi(sensorIdx=0)：function 2、参数 [0]。
        // fixture 中 0x2201 的 index 是 0x22（见 script_happy 的 getFeature 应答）。
        let last = &fake.requests[5];
        assert_eq!(last[2], 0x22);
        assert_eq!(last[3], (SENSOR_DPI_FN << 4) | SOFTWARE_ID);
        assert_eq!(last[4], 0x00, "sensorIdx=0");
        // 不发任何设置命令：全部请求均为只读查询（feature 0x00 ping/getFeature、
        // 0x05 function2、0x22 function0/function2）
        for request in &fake.requests {
            assert_eq!(request[0], REPORT_LONG);
            assert_eq!(request[1], DEVICE_INDEX);
            let function = request[3] >> 4;
            assert!(
                (request[2] == ROOT_FEATURE && (function == ROOT_FN_PING || function == ROOT_FN_GET_FEATURE))
                    || (request[2] == 0x05 && function == DEVICE_TYPE_FN)
                    || (request[2] == 0x22
                        && (function == SENSOR_COUNT_FN || function == SENSOR_DPI_FN)),
                "发出了脚本外的查询类别: {request:02X?}"
            );
        }
    }

    #[test]
    fn motion_dpi_probe_rejects_receiver_device_type_and_stops_queries() {
        let mut fake = FakeTransport::unique()
            .ping_ok(0x04)
            .reply(FakeTransport::feature_index_reply(0x05))
            .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_RECEIVER));
        let result = probe(&mut fake, Duration::from_secs(2));
        assert_eq!(result, DpiProbeResult::Unsupported, "Receiver(type=7) 必须 Unsupported");
        assert_eq!(fake.requests.len(), 3, "GetDeviceType 拒绝后不得发后续查询");
    }

    #[test]
    fn motion_dpi_probe_rejects_other_device_types_as_unsupported() {
        // 键盘/触控板等其余类型同样不在支持范围
        for device_type in [0x00u8, 0x01, 0x02, 0x04, 0x05, 0x06] {
            let mut fake = FakeTransport::unique()
                .ping_ok(0x04)
                .reply(FakeTransport::feature_index_reply(0x05))
                .reply(FakeTransport::device_type_reply(0x05, device_type));
            assert_eq!(
                probe(&mut fake, Duration::from_secs(2)),
                DpiProbeResult::Unsupported,
                "DeviceType {device_type} 必须 Unsupported"
            );
        }
    }

    #[test]
    fn motion_dpi_probe_rejects_multi_sensor_counts() {
        for count in [0u8, 2, 3] {
            let mut fake = FakeTransport::unique()
                .ping_ok(0x04)
                .reply(FakeTransport::feature_index_reply(0x05))
                .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
                .reply(FakeTransport::feature_index_reply(0x22))
                .reply(FakeTransport::sensor_count_reply(0x22, count));
            assert_eq!(
                probe(&mut fake, Duration::from_secs(2)),
                DpiProbeResult::Unsupported,
                "传感器数 {count} 必须 Unsupported（必须恰为 1）"
            );
        }
    }

    #[test]
    fn motion_dpi_probe_rejects_missing_2201_feature() {
        let mut fake = FakeTransport::unique()
            .ping_ok(0x04)
            .reply(FakeTransport::feature_index_reply(0x05))
            .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
            .reply(FakeTransport::feature_index_reply(0)); // 0x2201 缺失（index 0）
        assert_eq!(probe(&mut fake, Duration::from_secs(2)), DpiProbeResult::Unsupported);
        assert_eq!(fake.requests.len(), 4);
    }

    #[test]
    fn motion_dpi_probe_rejects_non_hidpp20_protocol_majors() {
        // major 1（旧 1.0 数值）与 0x8F（官方 IRoot 规格的 1.0 标记）都必须 Unsupported
        for major in [0x01u8, 0x8F] {
            let mut fake = FakeTransport::unique().ping_ok(major);
            assert_eq!(
                probe(&mut fake, Duration::from_secs(2)),
                DpiProbeResult::Unsupported,
                "protocolMajor 0x{major:02X} 必须 Unsupported"
            );
            assert_eq!(fake.requests.len(), 1);
        }
        // 0x02（2.0 旧版）与 0x04（2.0）允许进入后续资格确认
        for major in [0x02u8, 0x04] {
            let mut fake = FakeTransport::unique()
                .ping_ok(major)
                .reply(FakeTransport::feature_index_reply(0)); // 0x0005 缺失即止
            assert_eq!(
                probe(&mut fake, Duration::from_secs(2)),
                DpiProbeResult::Unsupported,
                "protocolMajor 0x{major:02X} 是 2.0，应继续资格确认"
            );
            assert_eq!(fake.requests.len(), 2);
        }
    }

    #[test]
    fn motion_dpi_probe_maps_transport_find_results_directly() {
        // Err → Unavailable（不解析中文字符串）
        let mut fake = FakeTransport::with_find(Err("打开 vendor collection 失败: 测试".to_string()));
        assert_eq!(probe(&mut fake, Duration::from_secs(2)), DpiProbeResult::Unavailable);
        assert_eq!(fake.find_calls, 1);
        // Unsupported/Ambiguous 判定直通
        let mut fake = FakeTransport::with_find(Ok(HidEndpointMatchKind::Unsupported));
        assert_eq!(probe(&mut fake, Duration::from_secs(2)), DpiProbeResult::Unsupported);
        let mut fake = FakeTransport::with_find(Ok(HidEndpointMatchKind::Ambiguous));
        assert_eq!(probe(&mut fake, Duration::from_secs(2)), DpiProbeResult::Ambiguous);
    }

    #[test]
    fn motion_dpi_probe_degrades_exchange_failure_and_error_frames_to_unavailable() {
        // 交换失败（transport/IO/到期取消）→ Unavailable
        let mut fake = FakeTransport::unique().fail("HID++ 读取超出业务期限（已取消）");
        assert_eq!(probe(&mut fake, Duration::from_secs(2)), DpiProbeResult::Unavailable);

        // 0x2201 在场后收到协议错误帧 → Unavailable（协议偏差降级，不判 Unsupported）
        let mut error_frame = FakeTransport::frame(0x22, 0xF, &[0x02]);
        error_frame[3] = (0xF << 4) | SOFTWARE_ID;
        let mut error_at_dpi = FakeTransport::unique()
            .ping_ok(0x04)
            .reply(FakeTransport::feature_index_reply(0x05))
            .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
            .reply(FakeTransport::feature_index_reply(0x22))
            .reply(FakeTransport::sensor_count_reply(0x22, 1))
            .reply(error_frame);
        assert_eq!(probe(&mut error_at_dpi, Duration::from_secs(2)), DpiProbeResult::Unavailable);

        // getFeature/GetDeviceType 阶段的错误帧 → Unsupported（缺 feature/无法确认）
        let mut feature_error = FakeTransport::frame(ROOT_FEATURE, 0xF, &[0x09]);
        feature_error[3] = (0xF << 4) | SOFTWARE_ID;
        let mut error_at_feature = FakeTransport::unique()
            .ping_ok(0x04)
            .reply(feature_error);
        assert_eq!(probe(&mut error_at_feature, Duration::from_secs(2)), DpiProbeResult::Unsupported);
    }

    #[test]
    fn motion_dpi_probe_recovers_from_stray_wrong_nonce_ping_reply() {
        // 穿插一帧错 nonce 的 ping 应答（如同 sw_id 外来 ping）：decode 归"其他响应"
        // → None → ask() 有界重发消化，探测在本轮内恢复继续。
        let mut fake = FakeTransport::unique()
            .reply(FakeTransport::static_ping_frame(0x04, 0x42))
            .ping_ok(0x04)
            .reply(FakeTransport::feature_index_reply(0x05))
            .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
            .reply(FakeTransport::feature_index_reply(0x22))
            .reply(FakeTransport::sensor_count_reply(0x22, 1))
            .reply(FakeTransport::sensor_dpi_reply(0x22, 800, 400, 0));
        assert_eq!(
            probe(&mut fake, Duration::from_secs(2)),
            DpiProbeResult::Available(800),
            "单次错 nonce 穿插应在本轮内恢复"
        );
        assert_eq!(fake.requests.len(), 7, "ping 重发一次");
    }

    #[test]
    fn motion_dpi_probe_persistent_wrong_nonce_degrades_to_unavailable_after_bounded_retries() {
        // 持续错 nonce（每轮重发都拿到别人应答）：恰好 MAX_ASK_ATTEMPTS 次交换后
        // 本轮降级 Unavailable（§4.3 有界重试，不无限发查询）。
        let mut fake = FakeTransport::unique()
            .reply(FakeTransport::static_ping_frame(0x04, 0x42))
            .reply(FakeTransport::static_ping_frame(0x04, 0x42))
            .reply(FakeTransport::static_ping_frame(0x04, 0x42));
        assert_eq!(
            probe(&mut fake, Duration::from_secs(2)),
            DpiProbeResult::Unavailable,
            "持续错 nonce 最终降级 Unavailable"
        );
        assert_eq!(
            fake.requests.len(),
            MAX_ASK_ATTEMPTS,
            "重发次数必须有界（不超过 MAX_ASK_ATTEMPTS）"
        );
    }

    #[test]
    fn motion_dpi_probe_retries_after_interleaved_notification_within_bounds() {
        // 通知穿插（身份不符 → None）：重发同一只读查询后成功
        let mut notification = FakeTransport::static_ping_frame(0x04, 0);
        notification[3] = (ROOT_FN_PING << 4) | 0x05; // 他人 sw_id → None
        let mut fake = FakeTransport::unique()
            .reply(notification)
            .ping_ok(0x04)
            .reply(FakeTransport::feature_index_reply(0x05))
            .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
            .reply(FakeTransport::feature_index_reply(0x22))
            .reply(FakeTransport::sensor_count_reply(0x22, 1))
            .reply(FakeTransport::sensor_dpi_reply(0x22, 1600, 800, 0));
        assert_eq!(probe(&mut fake, Duration::from_secs(2)), DpiProbeResult::Available(1600));
        assert_eq!(fake.requests.len(), 7, "ping 重发一次");
    }

    #[test]
    fn motion_dpi_probe_dpi_value_bounds_1_to_57343() {
        for (value, expected) in [
            (1u16, Some(1u32)),
            (100, Some(100)),
            (57343, Some(57343)),
            (0, None),
        ] {
            let mut fake = FakeTransport::unique()
                .ping_ok(0x04)
                .reply(FakeTransport::feature_index_reply(0x05))
                .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
                .reply(FakeTransport::feature_index_reply(0x22))
                .reply(FakeTransport::sensor_count_reply(0x22, 1))
                .reply(FakeTransport::sensor_dpi_reply(0x22, value, 400, 0));
            let result = probe(&mut fake, Duration::from_secs(2));
            assert_eq!(
                result,
                expected.map_or(DpiProbeResult::Unavailable, DpiProbeResult::Available),
                "DPI {value} 判定错误"
            );
        }
        // 上界之外（0xE000=57344 起 per x2201 未定义；以 60000 为代表）
        let mut fake = FakeTransport::unique()
            .ping_ok(0x04)
            .reply(FakeTransport::feature_index_reply(0x05))
            .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
            .reply(FakeTransport::feature_index_reply(0x22))
            .reply(FakeTransport::sensor_count_reply(0x22, 1))
            .reply(FakeTransport::sensor_dpi_reply(0x22, 60000, 400, 0));
        assert_eq!(probe(&mut fake, Duration::from_secs(2)), DpiProbeResult::Unavailable);
    }

    #[test]
    fn motion_dpi_probe_rejects_wrong_sensor_echo_and_short_payload() {
        // sensorIdx 回声非 0（协议偏差）→ Unavailable
        let mut wrong_echo = FakeTransport::unique()
            .ping_ok(0x04)
            .reply(FakeTransport::feature_index_reply(0x05))
            .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
            .reply(FakeTransport::feature_index_reply(0x22))
            .reply(FakeTransport::sensor_count_reply(0x22, 1))
            .reply(FakeTransport::sensor_dpi_reply(0x22, 800, 400, 1));
        assert_eq!(probe(&mut wrong_echo, Duration::from_secs(2)), DpiProbeResult::Unavailable);

        // payload 不足 5 字节（0x10 短帧只容 3 字节参数）→ Unavailable
        let short_dpi = vec![
            REPORT_SHORT,
            DEVICE_INDEX,
            0x22,
            (SENSOR_DPI_FN << 4) | SOFTWARE_ID,
            0x00,
            0x03,
            0x20,
        ];
        let mut short_payload = FakeTransport::unique()
            .ping_ok(0x04)
            .reply(FakeTransport::feature_index_reply(0x05))
            .reply(FakeTransport::device_type_reply(0x05, DEVICE_TYPE_MOUSE))
            .reply(FakeTransport::feature_index_reply(0x22))
            .reply(FakeTransport::sensor_count_reply(0x22, 1))
            .reply(short_dpi);
        assert_eq!(probe(&mut short_payload, Duration::from_secs(2)), DpiProbeResult::Unavailable);
    }

    #[test]
    fn motion_dpi_probe_stops_sending_queries_once_deadline_has_passed() {
        // timeout=0：deadline 已过 → 不触 transport、不发任何查询
        let mut fake = FakeTransport::unique().script_happy();
        assert_eq!(probe(&mut fake, Duration::ZERO), DpiProbeResult::Unavailable);
        assert_eq!(fake.find_calls, 0, "到期后不得触 transport");
        assert!(fake.requests.is_empty(), "到期后不得发任何查询");

        // 中途到期：ping 正常返回但 exchange 耗尽窗口 → 立即停发后续查询
        let mut fake = FakeTransport::unique()
            .ping_ok(0x04)
            .burn_next(Duration::from_millis(80));
        let started = Instant::now();
        let result = probe(&mut fake, Duration::from_millis(30));
        assert_eq!(result, DpiProbeResult::Unavailable, "到期业务降级");
        assert_eq!(fake.requests.len(), 1, "到期后停止发后续查询");
        assert!(
            started.elapsed() >= Duration::from_millis(30),
            "降级必须发生在到期之后"
        );
    }
}
