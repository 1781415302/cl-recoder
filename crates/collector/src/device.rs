//! hDevice → DeviceKey 解析与缓存（PLAN §4.2 DeviceKey 解析契约）。
//!
//! - hDevice → `GetRawInputDeviceInfoW(RIDI_DEVICENAME)` 取接口路径
//!   `\\?\HID#VID_%04X&PID_%04X[&REV_..&MI_..]#<实例>[#{接口GUID}]`，
//!   按 §4.2 契约正则 `VID_([0-9A-Fa-f]{4})&PID_([0-9A-Fa-f]{4})` 提取 VID/PID；
//! - 名称读取全程按 **UTF-16 码元** 计数（DEVPLAN 堆损坏修复 §4）：RIDI_DEVICENAME 的
//!   `pcbSize` 单位是字符数不是字节数；探测→对齐缓冲→有限重试由私有
//!   [`read_device_name_with`] 统一处理，生产 adapter 与回归共用；
//! - `name`：注册表 `HKLM\SYSTEM\CurrentControlSet\Enum\HID\<设备ID>\<实例ID>\FriendlyName`，
//!   取不到用 `DeviceDesc`，再取不到用 `"HID 设备 {VID:04X}:{PID:04X}"`；
//! - **hDevice==0**（precision touchpad，已文档化）**或解析失败或路径非 HID 形态（RDP/虚拟设备）**：
//!   归固定桶 `DeviceKey{kind, vid:0, pid:0, name:"未知/虚拟设备"}`，按 kind 分桶（§5.3：照常计数）；
//! - hDevice→DeviceKey 结果按句柄缓存（HashMap），热插拔后新句柄自然重解析。
//!
//! 可测性拆分：路径解析（[`parse_device_path`]）、间接字符串清理（[`clean_indirect_string`]）
//! 与解析核心（`DeviceResolver::resolve_with`，路径/名称来源可注入）均为纯函数，单测覆盖；
//! 仅注册表与 `GetRawInputDeviceInfoW` 薄封装含 IO/unsafe。

use std::collections::HashMap;
use std::sync::OnceLock;

use clrecoder_core::codes::DeviceKind;
use clrecoder_core::event::DeviceKey;
use regex::Regex;
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
};
use windows::Win32::UI::Input::{GetRawInputDeviceInfoW, RIDI_DEVICENAME};

/// 设备枚举注册表基路径（§4.2）。
const ENUM_HID_BASE: &str = r"SYSTEM\CurrentControlSet\Enum\HID";

/// §4.2 契约正则：`VID_([0-9A-Fa-f]{4})&PID_([0-9A-Fa-f]{4})`。
const VID_PID_PATTERN: &str = r"VID_([0-9A-Fa-f]{4})&PID_([0-9A-Fa-f]{4})";

/// 设备接口路径读取的 UTF-16 码元上限（DEVPLAN 堆损坏修复 §4）。
/// 探测结果超过该值视为异常，不分配、不调用填充 API。
const MAX_DEVICE_NAME_CHARS: u32 = 32_768;

/// 名称填充调用的重试上限（不含 probe）。第三次仍 TooSmall/失败则返回 None。
const MAX_DEVICE_NAME_READ_ATTEMPTS: usize = 3;

/// 未知/虚拟设备桶的固定显示名（§4.2：按 kind 分桶，keyboard/mouse 各一）。
pub(crate) const UNKNOWN_DEVICE_NAME: &str = "未知/虚拟设备";

/// 从 raw input 设备路径解析出的 HID 信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HidPathInfo {
    /// Vendor ID
    pub(crate) vid: u16,
    /// Product ID
    pub(crate) pid: u16,
    /// 设备 ID 段，如 `VID_04D9&PID_0169&REV_0100&MI_00`
    pub(crate) device_id: String,
    /// 实例 ID 段，如 `7&2f3a3d&0&0000`；缺失（或第 3 段实为接口 GUID）时为 `None`，
    /// 注册表取名将枚举设备键子键兜底。
    pub(crate) instance_id: Option<String>,
}

/// 未知/虚拟设备固定桶（§4.2：hDevice==0、解析失败、非 HID 形态时按 kind 分桶）。
pub(crate) fn unknown_device(kind: DeviceKind) -> DeviceKey {
    DeviceKey { kind, vid: 0, pid: 0, name: UNKNOWN_DEVICE_NAME.to_string() }
}

/// 解析 raw input 设备接口路径（§4.2）。
///
/// 样本：`\\?\HID#VID_04D9&PID_0169&REV_0100&MI_00#7&2f3a3d&0&0000#{884b96c3-…}`。
/// 段结构：`<总线>#<设备ID>#<实例ID>[#{接口GUID}]`。总线非 `HID`（RDP/Root/ACPI 等虚拟或
/// 非 HID 形态）、缺设备段或设备段无 VID/PID → `None`（调用方归"未知/虚拟设备"桶）。
pub(crate) fn parse_device_path(path: &str) -> Option<HidPathInfo> {
    let rest = path.strip_prefix(r"\\?\")?;
    let mut segs = rest.split('#');
    let bus = segs.next()?;
    // 总线段不区分大小写（实测路径为大写 HID；RDP/虚拟设备为 RDP_MOU / Root#RDP_KBD 等形态）
    if !bus.eq_ignore_ascii_case("HID") {
        return None;
    }
    let device_id = segs.next().filter(|s| !s.is_empty())?;
    // 第 3 段为实例 ID；若以 '{' 开头则实为接口 GUID（路径无实例段的罕见形态），不得误用
    let instance_id = segs
        .next()
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .map(str::to_string);
    let (vid, pid) = extract_vid_pid(device_id)?;
    Some(HidPathInfo { vid, pid, device_id: device_id.to_string(), instance_id })
}

/// §4.2 契约正则提取 VID/PID（u16 十六进制，恰好 4 位）。
fn extract_vid_pid(device_id: &str) -> Option<(u16, u16)> {
    static VID_PID_RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = VID_PID_RE
        .get_or_init(|| Regex::new(VID_PID_PATTERN).ok())
        .as_ref()?;
    let caps = re.captures(device_id)?;
    let vid = u16::from_str_radix(caps.get(1)?.as_str(), 16).ok()?;
    let pid = u16::from_str_radix(caps.get(2)?.as_str(), 16).ok()?;
    Some((vid, pid))
}

/// hDevice → DeviceKey 解析器（§4.2：结果按句柄+种类缓存，热插拔后新句柄自然重解析；
/// 缓存键含 kind，防御同一句柄被不同 RIM 类型事件引用的假想情形）。
pub(crate) struct DeviceResolver {
    cache: HashMap<(isize, DeviceKind), DeviceKey>,
}

impl Default for DeviceResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceResolver {
    /// 创建空缓存解析器。
    pub(crate) fn new() -> Self {
        Self { cache: HashMap::new() }
    }

    /// 解析（带缓存）。hDevice==0 / 取路径失败 / 非 HID 形态 / 注册表与兜底均失败时
    /// 分别落"未知/虚拟设备"桶或 `"HID 设备 {VID:04X}:{PID:04X}"` 名（§4.2）。
    pub(crate) fn resolve(&mut self, hdevice: HANDLE, kind: DeviceKind) -> DeviceKey {
        self.resolve_with(hdevice, kind, || raw_input_device_name(hdevice), registry_device_name)
    }

    /// 移除该原生句柄的**所有 kind** 缓存（§4.3：设备拔出通知时由 raw_input 同步调用——
    /// Windows 可能把回收后的句柄值复用给另一台设备，失效前缓存的 DeviceKey 会张冠李戴）。
    /// 未知句柄调用无副作用。
    pub fn forget_handle(&mut self, hdevice: HANDLE) {
        let key = hdevice.0 as isize;
        for kind in [DeviceKind::Keyboard, DeviceKind::Mouse, DeviceKind::Gamepad] {
            self.cache.remove(&(key, kind));
        }
    }

    /// 可注入路径/名称来源的解析核心：生产路径与单测共用同一逻辑，
    /// 单测注入路径样本与名称桩，不触真实 IO。
    pub(crate) fn resolve_with(
        &mut self,
        hdevice: HANDLE,
        kind: DeviceKind,
        path_provider: impl FnOnce() -> Option<String>,
        name_provider: impl FnOnce(&HidPathInfo) -> Option<String>,
    ) -> DeviceKey {
        let key = (hdevice.0 as isize, kind);
        if let Some(d) = self.cache.get(&key) {
            return d.clone();
        }
        let d = resolve_uncached(hdevice, kind, path_provider, name_provider);
        self.cache.insert(key, d.clone());
        d
    }
}

/// 无缓存的解析流程（§4.2 逐条兜底）。
fn resolve_uncached(
    hdevice: HANDLE,
    kind: DeviceKind,
    path_provider: impl FnOnce() -> Option<String>,
    name_provider: impl FnOnce(&HidPathInfo) -> Option<String>,
) -> DeviceKey {
    // hDevice==0（precision touchpad 等已文档化情形）→ 固定桶
    if hdevice.is_invalid() {
        return unknown_device(kind);
    }
    let Some(path) = path_provider() else {
        return unknown_device(kind);
    };
    let Some(info) = parse_device_path(&path) else {
        // 非 HID 形态（RDP/虚拟设备）或无法提取 VID/PID → 固定桶
        return unknown_device(kind);
    };
    let name = name_provider(&info)
        .unwrap_or_else(|| format!("HID 设备 {:04X}:{:04X}", info.vid, info.pid));
    DeviceKey { kind, vid: info.vid, pid: info.pid, name }
}

/// 注册表取显示名：实例键 `FriendlyName` → `DeviceDesc`；实例键打不开时枚举设备键
/// 子键兜底（实例 ID 与注册表不一致的罕见场景）。
/// 返回 `None` 时由调用方落 `"HID 设备 {VID:04X}:{PID:04X}"`（§4.2 兜底链最后一级）。
fn registry_device_name(info: &HidPathInfo) -> Option<String> {
    if let Some(instance) = &info.instance_id {
        let subkey = format!(r"{ENUM_HID_BASE}\{}\{}", info.device_id, instance);
        if let Some(name) = read_name_from_key(&subkey) {
            return Some(name);
        }
    }
    let device_subkey = format!(r"{ENUM_HID_BASE}\{}", info.device_id);
    for instance in enum_subkeys(&device_subkey) {
        let subkey = format!(r"{device_subkey}\{instance}");
        if let Some(name) = read_name_from_key(&subkey) {
            return Some(name);
        }
    }
    None
}

/// 打开单个实例键并读 `FriendlyName` → `DeviceDesc`。
fn read_name_from_key(subkey: &str) -> Option<String> {
    unsafe {
        let mut hkey = HKEY::default();
        let status = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(subkey),
            None,
            KEY_READ | KEY_WOW64_64KEY,
            &mut hkey,
        );
        if status != ERROR_SUCCESS {
            return None;
        }
        let result = read_friendly_or_desc(hkey);
        let _ = RegCloseKey(hkey);
        result
    }
}

/// 同一键内按 §4.2 顺序取值：FriendlyName 优先，DeviceDesc 兜底。
fn read_friendly_or_desc(hkey: HKEY) -> Option<String> {
    for value in ["FriendlyName", "DeviceDesc"] {
        if let Some(name) = read_sz_value(hkey, value) {
            return Some(clean_indirect_string(name));
        }
    }
    None
}

/// 读取 REG_SZ（兼容 REG_EXPAND_SZ，不展开环境变量）字符串值。
fn read_sz_value(hkey: HKEY, value_name: &str) -> Option<String> {
    unsafe {
        let name = HSTRING::from(value_name);
        let mut vtype = REG_VALUE_TYPE::default();
        let mut size: u32 = 0;
        // 先探大小（lpdata=None 时返回所需字节数）
        let status = RegQueryValueExW(hkey, &name, None, Some(&mut vtype), None, Some(&mut size));
        if status != ERROR_SUCCESS || size == 0 {
            return None;
        }
        if vtype != REG_SZ && vtype != REG_EXPAND_SZ {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let mut read = size;
        let status = RegQueryValueExW(
            hkey,
            &name,
            None,
            Some(&mut vtype),
            Some(buf.as_mut_ptr()),
            Some(&mut read),
        );
        if status != ERROR_SUCCESS {
            return None;
        }
        buf.truncate(read as usize);
        // REG_SZ 不保证以 NUL 结尾：按 UTF-16LE 解析并截断到首个 NUL
        let units: Vec<u16> =
            buf.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]))
    }
}

/// 枚举注册表键的子键名（上限 32，防异常键面拖慢解析；失败/无子键返回空）。
fn enum_subkeys(subkey: &str) -> Vec<String> {
    const MAX_SUBKEYS: u32 = 32;
    unsafe {
        let mut hkey = HKEY::default();
        let status = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(subkey),
            None,
            KEY_READ | KEY_WOW64_64KEY,
            &mut hkey,
        );
        if status != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut names = Vec::new();
        for index in 0..MAX_SUBKEYS {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            let status = RegEnumKeyExW(
                hkey,
                index,
                Some(PWSTR(buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            );
            if status != ERROR_SUCCESS || len == 0 {
                break;
            }
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        let _ = RegCloseKey(hkey);
        names
    }
}

/// `DeviceDesc` 常为间接字符串 `@<inf>,%<token%>;<显示文本>`（展开 token 需 SetupAPI，
/// 不在依赖白名单内）——按 §9.2 展示兜底：取分号后的显示文本；非间接字符串原样保留。
fn clean_indirect_string(s: String) -> String {
    if s.starts_with('@') {
        if let Some(pos) = s.find(';') {
            let tail = &s[pos + 1..];
            if !tail.is_empty() {
                return tail.to_string();
            }
        }
    }
    s
}

/// `GetRawInputDeviceInfoW(RIDI_DEVICENAME)` 的一次填充结果（DEVPLAN 堆损坏修复 §4）。
///
/// 长度单位一律是 **UTF-16 码元**，不是字节数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceNameRead {
    /// API 成功；`used_chars` 为该次输出 `pcbSize`（字符数），只允许落在 `(0, 输入切片len]`。
    Complete { used_chars: u32 },
    /// `ERROR_INSUFFICIENT_BUFFER`；`required_chars` 为该次输出 `pcbSize`。
    TooSmall { required_chars: u32 },
    /// 其它失败 / 设备拔出等；调用方返回 None，不升级为进程级失败。
    Failed,
}

/// DWORD 对齐的拥有型设备名缓冲区（DEVPLAN 堆损坏修复 §4）。
///
/// 需要 N 个 UTF-16 码元时：底层 `Vec<u32>` 元素数为 `ceil(N/2)`（存储换算），
/// 可写 UTF-16 视图的**逻辑长度严格为 N**，不暴露对齐 padding。
/// 禁止把 RIDI_DEVICENAME 所需字符数除以 2 作为 u16 元素数。
struct AlignedNameBuffer {
    /// 零初始化的 DWORD 对齐底层存储。
    words: Vec<u32>,
    /// 对外可写 UTF-16 视图的逻辑码元数。
    logical_chars: usize,
}

impl AlignedNameBuffer {
    /// 分配恰好容纳 `chars` 个 UTF-16 码元的零初始化对齐缓冲。
    ///
    /// `chars == 0` 或超过 [`MAX_DEVICE_NAME_CHARS`] 时返回 None。
    fn new(chars: u32) -> Option<Self> {
        if chars == 0 || chars > MAX_DEVICE_NAME_CHARS {
            return None;
        }
        // 底层 u32 数 = ceil(chars/2)；这是存储换算，不是把字符数当成 u16 元素数
        let words = (chars as usize).div_ceil(2);
        Some(Self { words: vec![0u32; words], logical_chars: chars as usize })
    }

    /// 暴露逻辑长度为 N 的可写 UTF-16 视图（不含对齐 padding）。
    fn as_utf16_mut(&mut self) -> &mut [u16] {
        // SAFETY:
        // - 对齐：`Vec<u32>` 元素按 4 字节对齐，强转为 `*mut u16` 后仍满足 u16 对齐；
        // - 初始化：底层 `Vec<u32>` 以 0 整块初始化，整个物理区可读可写；
        // - 长度：`logical_chars == N`，且 `words.len() * 2 >= N`（ceil(N/2)*2 >= N），
        //   视图严格取前 N 个码元，不越出已分配区、不暴露 padding；
        // - 独占：`&mut self` 保证调用期间不与底层 `words` 同时使用；调用方（Win32
        //   adapter）在 API 返回前不得释放/移动/resize 该缓冲。
        unsafe {
            std::slice::from_raw_parts_mut(self.words.as_mut_ptr() as *mut u16, self.logical_chars)
        }
    }
}

/// 有界读取状态机（DEVPLAN 堆损坏修复 §4）：probe → 分配 → 有限重试填充 → 有限范围解析。
///
/// - probe 失败 / 0 / 超限：None，不分配、不调用 read；
/// - Complete：先校验边界，再在已分配区域内找 NUL；缺 NUL / 空名 → None；
/// - TooSmall：仅当 `current < required <= MAX` 才扩容重试，最多
///   [`MAX_DEVICE_NAME_READ_ATTEMPTS`] 次填充调用；
/// - Failed / 异常长度 / 无增长：None。
fn read_device_name_with(
    mut probe: impl FnMut() -> Option<u32>,
    mut read: impl FnMut(&mut [u16]) -> DeviceNameRead,
) -> Option<String> {
    let mut current_chars = probe()?;
    if current_chars == 0 || current_chars > MAX_DEVICE_NAME_CHARS {
        return None;
    }
    for _ in 0..MAX_DEVICE_NAME_READ_ATTEMPTS {
        // 每次尝试分配全新对齐缓冲；不解析失败调用留下的部分内容
        let mut buffer = AlignedNameBuffer::new(current_chars)?;
        let view = buffer.as_utf16_mut();
        match read(view) {
            DeviceNameRead::Complete { used_chars } => {
                if used_chars == 0 || used_chars as usize > view.len() {
                    return None;
                }
                let units = &view[..used_chars as usize];
                // 在已分配、初始化区域内找 NUL；缺 NUL → None（不做无界 PWSTR 扫描）
                let end = units.iter().position(|&c| c == 0)?;
                if end == 0 {
                    return None;
                }
                return Some(String::from_utf16_lossy(&units[..end]));
            }
            DeviceNameRead::TooSmall { required_chars } => {
                // 仅允许有界增长；无增长或超过上限立即失败
                if required_chars <= current_chars || required_chars > MAX_DEVICE_NAME_CHARS {
                    return None;
                }
                current_chars = required_chars;
            }
            DeviceNameRead::Failed => return None,
        }
    }
    None
}

/// `GetRawInputDeviceInfoW(RIDI_DEVICENAME)` 取设备接口路径（unsafe 集中于此薄封装）。
///
/// 长度单位：RIDI_DEVICENAME 的 `pcbSize` 是 **UTF-16 码元数**（含终止空间），不是字节数。
/// 分配/重试/解析统一走 [`read_device_name_with`]，生产与回归共用同一 helper。
fn raw_input_device_name(hdevice: HANDLE) -> Option<String> {
    read_device_name_with(
        || unsafe {
            // SAFETY: hdevice 来自 raw input 事件，调用期间保持有效；探测调用 pData=None。
            let mut size: u32 = 0;
            let n = GetRawInputDeviceInfoW(Some(hdevice), RIDI_DEVICENAME, None, &mut size);
            // UINT_MAX 仅代表失败；size 单位为 UTF-16 码元数（含终止空间）
            if n == u32::MAX {
                None
            } else {
                Some(size)
            }
        },
        |buf| unsafe {
            // SAFETY:
            // - buf 来自 AlignedNameBuffer::as_utf16_mut，逻辑长度 N 已零初始化；
            // - pcbSize 只以切片 len 设置输入容量；指针指向该切片有效存储；
            // - API 完成前拥有型缓冲区不释放、不移动、不 resize；此处不同时借用 words。
            let mut pcb_size = buf.len() as u32;
            let n = GetRawInputDeviceInfoW(
                Some(hdevice),
                RIDI_DEVICENAME,
                Some(buf.as_mut_ptr().cast::<core::ffi::c_void>()),
                &mut pcb_size,
            );
            if n == u32::MAX {
                // 立即读取 GetLastError；不得把“bytes copied”再用于除以 2
                // SAFETY: 紧跟失败的 Win32 调用，读取线程 last-error 合法。
                if GetLastError() == ERROR_INSUFFICIENT_BUFFER {
                    DeviceNameRead::TooSmall { required_chars: pcb_size }
                } else {
                    DeviceNameRead::Failed
                }
            } else {
                DeviceNameRead::Complete { used_chars: pcb_size }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use windows::Win32::UI::Input::{
        GetRawInputDeviceList, RAWINPUTDEVICELIST, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
    };

    /// 真实形态样本（键盘集合：含 REV/MI 与接口 GUID 后缀）。
    const KEYBOARD_PATH: &str = r"\\?\HID#VID_04D9&PID_0169&REV_0100&MI_00#7&2f3a3d&0&0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";
    /// 真实形态样本（鼠标集合 + 小写十六进制）。
    const MOUSE_PATH: &str = r"\\?\HID#VID_1532&PID_0045&MI_01#8&1a2b3c&0&0001";

    fn handle(v: isize) -> HANDLE {
        HANDLE(v as *mut core::ffi::c_void)
    }

    // ---------- parse_device_path ----------

    #[test]
    fn parse_real_keyboard_path_with_rev_mi_and_interface_guid() {
        let info = parse_device_path(KEYBOARD_PATH).expect("真实键盘路径必须可解析");
        assert_eq!(info.vid, 0x04D9);
        assert_eq!(info.pid, 0x0169);
        assert_eq!(info.device_id, "VID_04D9&PID_0169&REV_0100&MI_00");
        assert_eq!(info.instance_id.as_deref(), Some("7&2f3a3d&0&0000"));
    }

    #[test]
    fn parse_real_mouse_path_and_mixed_hex_case() {
        let info = parse_device_path(MOUSE_PATH).expect("真实鼠标路径必须可解析");
        assert_eq!(info.vid, 0x1532);
        assert_eq!(info.pid, 0x0045);
        assert_eq!(info.instance_id.as_deref(), Some("8&1a2b3c&0&0001"));
        // 契约正则前缀字面大写（与真实路径一致），十六进制位大小写均可提取
        let lower = parse_device_path(r"\\?\HID#VID_04d9&PID_0169#i").expect("小写十六进制必须可解析");
        assert_eq!((lower.vid, lower.pid), (0x04D9, 0x0169));
    }

    #[test]
    fn parse_instance_optional_and_interface_guid_not_treated_as_instance() {
        // 无实例段：第 3 段是接口 GUID，不得误认为实例 ID
        let info = parse_device_path(r"\\?\HID#VID_04D9&PID_0169#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}")
            .expect("无实例段路径仍应解析出 VID/PID");
        assert_eq!(info.instance_id, None);
        // 完全无第 3 段
        let bare = parse_device_path(r"\\?\HID#VID_04D9&PID_0169").expect("两段路径应可解析");
        assert_eq!(bare.instance_id, None);
    }

    #[test]
    fn parse_rejects_non_hid_and_malformed_paths() {
        // RDP / 虚拟设备 / ACPI 等非 HID 形态（§4.2 → "未知/虚拟设备"桶）
        assert_eq!(
            parse_device_path(r"\\?\RDP_MOU#0000#{6f1b8e80-b74f-4a12-9c9d-000000000000}"),
            None
        );
        assert_eq!(
            parse_device_path(r"\\?\Root#RDP_KBD#0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}"),
            None
        );
        assert_eq!(
            parse_device_path(r"\\?\ACPI#PNP0303#4&2f3a3d&0#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}"),
            None
        );
        // HID 形态但无 VID/PID
        assert_eq!(parse_device_path(r"\\?\HID#VIRTUAL_KEYBOARD#instance"), None);
        // 空设备段 / 垃圾输入
        assert_eq!(parse_device_path(r"\\?\HID##instance"), None);
        assert_eq!(parse_device_path("not a device path"), None);
        assert_eq!(parse_device_path(""), None);
    }

    // ---------- clean_indirect_string ----------

    #[test]
    fn indirect_device_desc_is_trimmed_to_display_text() {
        assert_eq!(
            clean_indirect_string("@oem13.inf,%hid_mouse%;HID-compliant mouse".to_string()),
            "HID-compliant mouse"
        );
        assert_eq!(
            clean_indirect_string("@hidserv.inf,%usbinputdevice%;USB Input Device".to_string()),
            "USB Input Device"
        );
        // 普通字符串原样保留
        assert_eq!(
            clean_indirect_string("Logitech G502 HERO".to_string()),
            "Logitech G502 HERO"
        );
        // 有 @ 无分号 → 原样
        assert_eq!(
            clean_indirect_string("@only_prefix_no_semicolon".to_string()),
            "@only_prefix_no_semicolon"
        );
    }

    // ---------- DeviceResolver（注入桩，不触真实 IO） ----------

    #[test]
    fn resolver_zero_handle_goes_to_unknown_bucket_by_kind() {
        let mut r = DeviceResolver::new();
        let kb = r.resolve_with(
            HANDLE::default(),
            DeviceKind::Keyboard,
            || panic!("hDevice==0 不应查询设备路径"),
            |_: &HidPathInfo| panic!("hDevice==0 不应查询注册表"),
        );
        assert_eq!(
            kb,
            DeviceKey {
                kind: DeviceKind::Keyboard,
                vid: 0,
                pid: 0,
                name: "未知/虚拟设备".to_string()
            }
        );
        // mouse 独立分桶（§4.2：keyboard/mouse 各一）
        let mouse = r.resolve_with(
            HANDLE::default(),
            DeviceKind::Mouse,
            || panic!("hDevice==0 不应查询设备路径"),
            |_: &HidPathInfo| panic!("hDevice==0 不应查询注册表"),
        );
        assert_eq!(mouse.kind, DeviceKind::Mouse);
        assert_eq!(mouse.name, "未知/虚拟设备");
    }

    #[test]
    fn resolver_buckets_non_hid_paths_without_touching_registry() {
        let mut r = DeviceResolver::new();
        for path in [
            r"\\?\RDP_MOU#0000#{6f1b8e80-b74f-4a12-9c9d-000000000000}",
            r"\\?\Root#RDP_KBD#0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}",
        ] {
            let d = r.resolve_with(
                handle(0x42),
                DeviceKind::Mouse,
                || Some(path.to_string()),
                |_: &HidPathInfo| panic!("非 HID 路径不应查注册表"),
            );
            assert_eq!(d, unknown_device(DeviceKind::Mouse));
        }
        // 取路径失败同样落桶
        let d = r.resolve_with(handle(0x43), DeviceKind::Keyboard, || None, |_: &HidPathInfo| {
            panic!("无路径不应查注册表")
        });
        assert_eq!(d, unknown_device(DeviceKind::Keyboard));
    }

    #[test]
    fn resolver_falls_back_to_vid_pid_name_when_registry_misses() {
        let mut r = DeviceResolver::new();
        let d = r.resolve_with(
            handle(0x100),
            DeviceKind::Keyboard,
            || Some(KEYBOARD_PATH.to_string()),
            |_| None, // 注册表无 FriendlyName/DeviceDesc
        );
        assert_eq!(d.vid, 0x04D9);
        assert_eq!(d.pid, 0x0169);
        assert_eq!(d.name, "HID 设备 04D9:0169");
    }

    #[test]
    fn resolver_uses_registry_name_and_caches_per_handle_and_kind() {
        let mut r = DeviceResolver::new();
        let calls = Cell::new(0u32);
        let d1 = r.resolve_with(
            handle(0xABC),
            DeviceKind::Keyboard,
            || {
                calls.set(calls.get() + 1);
                Some(KEYBOARD_PATH.to_string())
            },
            |_| Some("测试键盘".to_string()),
        );
        assert_eq!(d1.name, "测试键盘");
        assert_eq!(calls.get(), 1);

        // 同句柄同种类：命中缓存，不再触发路径/注册表查询
        let d2 = r.resolve_with(
            handle(0xABC),
            DeviceKind::Keyboard,
            || {
                calls.set(calls.get() + 1);
                panic!("缓存命中不应再次查询路径");
            },
            |_: &HidPathInfo| panic!("缓存命中不应再次查询注册表"),
        );
        assert_eq!(d2, d1);
        assert_eq!(calls.get(), 1);

        // 同句柄不同种类：独立缓存条目（防御性），且注册表名缺失时走 VID/PID 兜底名
        let d3 = r.resolve_with(
            handle(0xABC),
            DeviceKind::Mouse,
            || {
                calls.set(calls.get() + 1);
                Some(KEYBOARD_PATH.to_string())
            },
            |_| None,
        );
        assert_eq!(d3.kind, DeviceKind::Mouse);
        assert_eq!(d3.name, "HID 设备 04D9:0169");
        assert_eq!(calls.get(), 2);
    }

    /// §4.3：forget_handle 失效指定句柄缓存——拔出后句柄值可能被系统复用给另一台设备，
    /// 失效后必须重新解析；未知句柄调用无副作用（既有缓存不受影响）。
    #[test]
    fn correctness_v2_forget_handle_invalidates_cache_and_unknown_handle_is_noop() {
        let mut r = DeviceResolver::new();
        let calls = Cell::new(0u32);
        let d1 = r.resolve_with(
            handle(0xABC),
            DeviceKind::Keyboard,
            || {
                calls.set(calls.get() + 1);
                Some(KEYBOARD_PATH.to_string())
            },
            |_| Some("旧键盘".to_string()),
        );
        assert_eq!(calls.get(), 1);

        // 未知句柄 forget：无副作用——已缓存句柄仍命中缓存
        r.forget_handle(handle(0xDEAD));
        let d2 = r.resolve_with(
            handle(0xABC),
            DeviceKind::Keyboard,
            || panic!("未知句柄 forget 不应影响已有缓存"),
            |_: &HidPathInfo| panic!("缓存命中不应再次查询注册表"),
        );
        assert_eq!(d2, d1);
        assert_eq!(calls.get(), 1);

        // forget 指定句柄：缓存被移除，重新解析（模拟句柄复用后设备变化的场景）
        r.forget_handle(handle(0xABC));
        let d3 = r.resolve_with(
            handle(0xABC),
            DeviceKind::Keyboard,
            || {
                calls.set(calls.get() + 1);
                Some(MOUSE_PATH.to_string())
            },
            |_| Some("新键盘".to_string()),
        );
        assert_eq!(calls.get(), 2, "forget_handle 后必须重新解析");
        assert_ne!(d3, d1);
        assert_eq!((d3.vid, d3.pid, d3.name.as_str()), (0x1532, 0x0045, "新键盘"));
    }

    /// §4.3：forget_handle 移除该句柄**所有 kind** 的缓存条目（同句柄 keyboard/mouse 一起失效）。
    #[test]
    fn correctness_v2_forget_handle_clears_every_kind_entry() {
        let mut r = DeviceResolver::new();
        let kb_calls = Cell::new(0u32);
        let mouse_calls = Cell::new(0u32);
        let _ = r.resolve_with(
            handle(0xABC),
            DeviceKind::Keyboard,
            || {
                kb_calls.set(kb_calls.get() + 1);
                Some(KEYBOARD_PATH.to_string())
            },
            |_| None,
        );
        let _ = r.resolve_with(
            handle(0xABC),
            DeviceKind::Mouse,
            || {
                mouse_calls.set(mouse_calls.get() + 1);
                Some(MOUSE_PATH.to_string())
            },
            |_| None,
        );
        r.forget_handle(handle(0xABC));
        // 两个 kind 都必须重新解析（各查一次路径）
        let _ = r.resolve_with(
            handle(0xABC),
            DeviceKind::Keyboard,
            || {
                kb_calls.set(kb_calls.get() + 1);
                Some(KEYBOARD_PATH.to_string())
            },
            |_| None,
        );
        let _ = r.resolve_with(
            handle(0xABC),
            DeviceKind::Mouse,
            || {
                mouse_calls.set(mouse_calls.get() + 1);
                Some(MOUSE_PATH.to_string())
            },
            |_| None,
        );
        assert_eq!(kb_calls.get(), 2, "keyboard 缓存应被失效");
        assert_eq!(mouse_calls.get(), 2, "mouse 缓存应被失效");
    }

    // ---------- collector_heap_*（DEVPLAN 堆损坏修复 §8 S1 确定性回归） ----------

    /// 构造去重的非 NUL UTF-16 序列（CJK 区，避免与终止符混淆）。
    fn cjk_units(len: usize) -> Vec<u16> {
        (0..len).map(|i| 0x4E00u16.wrapping_add(i as u16)).collect()
    }

    /// 本机证据样本 + 偶数长度：read 必须拿到完整 N 码元视图，末 NUL 可写、
    /// 地址 %4==0、名称不截断；若恢复旧 `N/2` 分配，下列断言应失败且不产生真正越界。
    #[test]
    fn collector_heap_full_capacity_view_alignment_and_no_truncation() {
        // 证据 JSON 中的真实所需码元数 + 偶数/奇数补充样本
        for n in [79u32, 89, 95, 70, 80, 88, 94, 32, 3, 2] {
            let mut buffer = AlignedNameBuffer::new(n).expect("合法容量必须可分配");
            // 若恢复旧 size/2+1 分配，逻辑视图不会是完整 N 码元
            assert_eq!(buffer.logical_chars, n as usize, "逻辑长度必须严格为 N");
            assert_eq!(
                buffer.words.len(),
                (n as usize).div_ceil(2),
                "底层 u32 数必须是 ceil(N/2)，不是把字符数/2 当 u16 元素数"
            );
            let view = buffer.as_utf16_mut();
            assert_eq!(view.len(), n as usize, "read 必须看到完整 N 码元可写视图");
            assert_eq!(view.as_ptr() as usize % 4, 0, "缓冲区地址必须满足 DWORD 对齐");
            assert!(
                buffer.words.len() * 2 >= n as usize,
                "物理 u16 容量必须覆盖 N 码元"
            );

            // 名称占 N-1 码元 + 末 NUL；旧缺陷把字符数/2+1 当 u16 元素数，
            // 对 N>=3 的样本容量不足，read 视图断言已覆盖该反例
            if n >= 3 {
                assert!((n as usize) / 2 + 1 < n as usize);
            }
            let name_units = cjk_units((n - 1) as usize);
            let expected = String::from_utf16_lossy(&name_units);

            let result = read_device_name_with(
                || Some(n),
                |buf| {
                    assert_eq!(buf.len(), n as usize, "read 容量必须是完整 N 码元");
                    assert_eq!(buf.as_ptr() as usize % 4, 0, "read 指针必须 DWORD 对齐");
                    let name_len = buf.len() - 1;
                    buf[..name_len].copy_from_slice(&name_units);
                    buf[name_len] = 0;
                    DeviceNameRead::Complete { used_chars: buf.len() as u32 }
                },
            );
            assert_eq!(result.as_deref(), Some(expected.as_str()), "名称不得截断（N={n}）");
        }
    }

    /// 中文、非 BMP 代理对、首个 NUL 后的内容不参与结果。
    #[test]
    fn collector_heap_unicode_chinese_and_surrogate_pair_complete() {
        let mut payload: Vec<u16> = Vec::new();
        payload.extend("键盘".encode_utf16());
        // 非 BMP：char::encode_utf16 写入缓冲区并返回码元切片（代理对）
        let mut surrogate = [0u16; 2];
        let encoded = '\u{1F600}'.encode_utf16(&mut surrogate);
        payload.extend_from_slice(encoded);
        payload.push(0); // 首个 NUL
        payload.extend("TRAILING".encode_utf16()); // 不得参与结果
        let first_nul = payload.iter().position(|&c| c == 0).expect("payload 必须含 NUL");
        let expected = String::from_utf16_lossy(&payload[..first_nul]);
        assert!(expected.contains("键盘"));
        assert!(expected.contains('\u{1F600}'));

        let result = read_device_name_with(
            || Some(payload.len() as u32),
            |buf| {
                assert_eq!(buf.len(), payload.len());
                buf.copy_from_slice(&payload);
                DeviceNameRead::Complete { used_chars: buf.len() as u32 }
            },
        );
        let got = result.expect("完整 Unicode 名称应成功解码");
        assert_eq!(got, expected);
        assert!(!got.contains("TRAILING"), "首个 NUL 后的内容不得参与结果");
        // 代理对按 UTF-16 码元计容量，解码后仍是单个非 BMP 字符
        assert_eq!(got.chars().filter(|c| !c.is_ascii()).count(), 3); // 键+盘+😀
    }

    /// probe 失败/0/超限时不调用 read；上限本身可接受。
    #[test]
    fn collector_heap_probe_failure_zero_over_limit_skips_read() {
        let mut calls = 0u32;
        let r = read_device_name_with(
            || {
                calls += 1;
                None
            },
            |_| panic!("probe 失败不应调用 read"),
        );
        assert_eq!(r, None);
        assert_eq!(calls, 1);

        let r = read_device_name_with(
            || Some(0),
            |_| panic!("probe=0 不应调用 read"),
        );
        assert_eq!(r, None);

        let r = read_device_name_with(
            || Some(MAX_DEVICE_NAME_CHARS + 1),
            |_| panic!("probe 超限不应调用 read"),
        );
        assert_eq!(r, None);
        assert!(AlignedNameBuffer::new(MAX_DEVICE_NAME_CHARS + 1).is_none());
        assert!(AlignedNameBuffer::new(0).is_none());

        // 上限本身可接受：可分配、可调用 read
        let r = read_device_name_with(
            || Some(MAX_DEVICE_NAME_CHARS),
            |buf| {
                assert_eq!(buf.len(), MAX_DEVICE_NAME_CHARS as usize);
                buf[0] = b'A' as u16;
                buf[1] = 0;
                DeviceNameRead::Complete { used_chars: 2 }
            },
        );
        assert_eq!(r.as_deref(), Some("A"));
        assert!(AlignedNameBuffer::new(MAX_DEVICE_NAME_CHARS).is_some());
    }

    /// ERROR_INSUFFICIENT_BUFFER 增长重试；3 次封顶；无增长/超限请求立即失败。
    #[test]
    fn collector_heap_insufficient_buffer_growth_retry_and_attempt_cap() {
        // 首次 TooSmall{95}，二次成功；容量序列 79→95，且扩容后不得复用脏内容
        let mut lens = Vec::new();
        let mut attempt = 0usize;
        let result = read_device_name_with(
            || Some(79),
            |buf| {
                lens.push(buf.len());
                attempt += 1;
                match attempt {
                    1 => {
                        buf[0] = 0xDEAD; // 模拟失败调用留下的部分内容
                        DeviceNameRead::TooSmall { required_chars: 95 }
                    }
                    _ => {
                        assert_eq!(buf.len(), 95);
                        assert_eq!(buf[0], 0, "TooSmall 后不得复用失败调用的部分内容");
                        let name_units = cjk_units(94);
                        buf[..94].copy_from_slice(&name_units);
                        buf[94] = 0;
                        DeviceNameRead::Complete { used_chars: 95 }
                    }
                }
            },
        );
        assert_eq!(lens, vec![79, 95], "容量序列必须是 79→95");
        let got = result.expect("增长重试后应成功");
        assert_eq!(got.chars().count(), 94, "名称不得因扩容被截断");

        // 连续增长 TooSmall：填充调用恰好 3 次，无第四次
        let mut attempt = 0usize;
        let mut current = 79u32;
        let result = read_device_name_with(
            || Some(79),
            |_| {
                attempt += 1;
                assert!(attempt <= MAX_DEVICE_NAME_READ_ATTEMPTS, "填充调用不得超过 3 次");
                current += 20;
                DeviceNameRead::TooSmall { required_chars: current }
            },
        );
        assert_eq!(result, None);
        assert_eq!(attempt, MAX_DEVICE_NAME_READ_ATTEMPTS, "应恰好 3 次封顶");

        // 无增长的 TooSmall：立即失败
        let mut attempt = 0usize;
        let result = read_device_name_with(
            || Some(79),
            |_| {
                attempt += 1;
                DeviceNameRead::TooSmall { required_chars: 79 }
            },
        );
        assert_eq!(result, None);
        assert_eq!(attempt, 1, "无增长应立即失败");

        // 超限请求：立即失败
        let mut attempt = 0usize;
        let result = read_device_name_with(
            || Some(79),
            |_| {
                attempt += 1;
                DeviceNameRead::TooSmall { required_chars: MAX_DEVICE_NAME_CHARS + 1 }
            },
        );
        assert_eq!(result, None);
        assert_eq!(attempt, 1, "超限请求应立即失败");
    }

    /// Complete 长度 0/超容量/缺NUL/空名失败；普通读取错误失败。
    #[test]
    fn collector_heap_complete_invalid_lengths_and_missing_nul_fail() {
        // used_chars = 0
        let r = read_device_name_with(
            || Some(16),
            |buf| {
                let _ = buf;
                DeviceNameRead::Complete { used_chars: 0 }
            },
        );
        assert_eq!(r, None);

        // used_chars 超过输入容量：不切片越界、不 panic
        let r = read_device_name_with(
            || Some(16),
            |buf| DeviceNameRead::Complete { used_chars: buf.len() as u32 + 1 },
        );
        assert_eq!(r, None);

        // 缺 NUL：已分配区域内无终止符 → None
        let r = read_device_name_with(
            || Some(8),
            |buf| {
                for c in buf.iter_mut() {
                    *c = 0x41;
                }
                DeviceNameRead::Complete { used_chars: buf.len() as u32 }
            },
        );
        assert_eq!(r, None);

        // 空名称：首码元即 NUL
        let r = read_device_name_with(
            || Some(8),
            |buf| {
                buf[0] = 0;
                DeviceNameRead::Complete { used_chars: 4 }
            },
        );
        assert_eq!(r, None);

        // 普通读取错误 → None，不升级为进程级失败
        let r = read_device_name_with(
            || Some(16),
            |buf| {
                let _ = buf;
                DeviceNameRead::Failed
            },
        );
        assert_eq!(r, None);

        // Failed 后不扩容、不重试
        let mut attempt = 0usize;
        let r = read_device_name_with(
            || Some(16),
            |_| {
                attempt += 1;
                DeviceNameRead::Failed
            },
        );
        assert_eq!(r, None);
        assert_eq!(attempt, 1);
    }

    /// 独立参考读取（S2 native 冒烟共用）：足够大的 DWORD 对齐缓冲 + 尾部 canary。
    /// 只读查询，不注册 Raw Input、不启动采集、不输出设备路径。
    fn reference_device_name(hdevice: HANDLE) -> Option<String> {
        unsafe {
            // SAFETY: hdevice 来自 GetRawInputDeviceList 枚举；探测 pData=None。
            let mut probe: u32 = 0;
            let n = GetRawInputDeviceInfoW(Some(hdevice), RIDI_DEVICENAME, None, &mut probe);
            if n == u32::MAX || probe == 0 || probe > MAX_DEVICE_NAME_CHARS {
                return None;
            }
            // 物理缓冲远大于探测码元数，尾部留 canary；Vec<u32> 保证 4 字节对齐
            let logical = probe as usize;
            let padding = 128usize;
            let total_chars = logical + padding + 16;
            let words_len = total_chars.div_ceil(2) + 2;
            let mut words = vec![0u32; words_len];
            let base = words.as_mut_ptr() as *mut u16;
            let canary_at = logical + padding;
            let capacity_u16 = words_len * 2;
            assert!(canary_at + 2 <= capacity_u16);
            *base.add(canary_at) = 0xC0DE;
            *base.add(canary_at + 1) = 0xDEAD;

            // SAFETY: base 指向 words 的独占可写存储；API 期间 words 不释放/移动。
            let mut pcb_size = probe;
            let n = GetRawInputDeviceInfoW(
                Some(hdevice),
                RIDI_DEVICENAME,
                Some(base.cast::<core::ffi::c_void>()),
                &mut pcb_size,
            );
            if n == u32::MAX {
                return None;
            }
            assert_eq!(*base.add(canary_at), 0xC0DE, "参考缓冲尾部 canary 被破坏");
            assert_eq!(*base.add(canary_at + 1), 0xDEAD, "参考缓冲尾部 canary 被破坏");
            // 缓冲足够大时，输出码元数不应超过探测值；超出视为异常
            if pcb_size == 0 || pcb_size as usize > logical {
                return None;
            }
            // SAFETY: 前 pcb_size 个码元位于已分配、已初始化的 words 存储内。
            let slice = std::slice::from_raw_parts(base, pcb_size as usize);
            let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
            if end == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&slice[..end]))
        }
    }

    /// S2 关卡用：真实 Win32 只读冒烟。默认 ignored；S2 以 --ignored 执行。
    ///
    /// 仅枚举 GetRawInputDeviceList 并调用修复后的 [`raw_input_device_name`]，
    /// 与独立正确容量参考读取对照；不启动 collector、不输出设备路径。
    #[test]
    #[ignore = "真实 Win32 只读验证（S2 关卡）；默认 cargo test 不执行"]
    fn collector_heap_native_name_read_smoke() {
        unsafe {
            // SAFETY: GetRawInputDeviceList 只读枚举；cbSize 取自本 crate 使用的类型布局。
            let cb_size = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
            let mut count: u32 = 0;
            let n = GetRawInputDeviceList(None, &mut count, cb_size);
            if n == u32::MAX || count == 0 {
                println!(
                    "collector_heap_native_name_read_smoke: 环境无法覆盖 native 关卡（枚举失败或无设备）"
                );
                return;
            }
            let mut devices = vec![RAWINPUTDEVICELIST::default(); count as usize];
            // SAFETY: devices 按 count 分配，API 期间不释放/移动。
            let n = GetRawInputDeviceList(Some(devices.as_mut_ptr()), &mut count, cb_size);
            if n == u32::MAX {
                println!("collector_heap_native_name_read_smoke: 枚举读取失败");
                return;
            }
            devices.truncate(count as usize);

            let mut compared = 0usize;
            let mut skipped = 0usize;
            let mut capacities: Vec<u32> = Vec::new();
            for (index, device) in devices.iter().enumerate() {
                if device.dwType != RIM_TYPEMOUSE && device.dwType != RIM_TYPEKEYBOARD {
                    continue;
                }
                let kind = if device.dwType == RIM_TYPEMOUSE { "mouse" } else { "keyboard" };
                // 两次调用之间设备可能拔出：该项跳过并计数
                match (raw_input_device_name(device.hDevice), reference_device_name(device.hDevice))
                {
                    (Some(production), Some(reference)) => {
                        if production != reference {
                            panic!(
                                "生产 wrapper 与独立参考读取不一致（设备 #{index} {kind}，字符数 {} vs {}）",
                                production.chars().count(),
                                reference.chars().count()
                            );
                        }
                        capacities.push(production.chars().count() as u32 + 1);
                        println!(
                            "  设备[{index}] {kind}: 名称字符数={} 一致（canary 保持）",
                            production.chars().count()
                        );
                        compared += 1;
                    }
                    _ => {
                        skipped += 1;
                        println!("  设备[{index}] {kind}: 枚举/读取期间拔出或不可读，跳过");
                    }
                }
            }
            println!(
                "collector_heap_native_name_read_smoke: 枚举 {} 个；对照成功 {}，跳过 {}；容量样本={:?}",
                devices.len(),
                compared,
                skipped,
                capacities
            );
            if compared == 0 {
                println!(
                    "collector_heap_native_name_read_smoke: 没有任何成功键盘/鼠标对照，环境无法覆盖 native 关卡"
                );
            }
        }
    }
}
