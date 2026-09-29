//! hDevice → DeviceKey 解析与缓存（PLAN §4.2 DeviceKey 解析契约）。
//!
//! - hDevice → `GetRawInputDeviceInfoW(RIDI_DEVICENAME)` 取接口路径
//!   `\\?\HID#VID_%04X&PID_%04X[&REV_..&MI_..]#<实例>[#{接口GUID}]`，
//!   按 §4.2 契约正则 `VID_([0-9A-Fa-f]{4})&PID_([0-9A-Fa-f]{4})` 提取 VID/PID；
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
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
};
use windows::Win32::UI::Input::{GetRawInputDeviceInfoW, RIDI_DEVICENAME};

/// 设备枚举注册表基路径（§4.2）。
const ENUM_HID_BASE: &str = r"SYSTEM\CurrentControlSet\Enum\HID";

/// §4.2 契约正则：`VID_([0-9A-Fa-f]{4})&PID_([0-9A-Fa-f]{4})`。
const VID_PID_PATTERN: &str = r"VID_([0-9A-Fa-f]{4})&PID_([0-9A-Fa-f]{4})";

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

/// `GetRawInputDeviceInfoW(RIDI_DEVICENAME)` 取设备接口路径（unsafe 集中于此薄封装）。
fn raw_input_device_name(hdevice: HANDLE) -> Option<String> {
    unsafe {
        let mut size: u32 = 0;
        let n = GetRawInputDeviceInfoW(Some(hdevice), RIDI_DEVICENAME, None, &mut size);
        if n == u32::MAX || size == 0 {
            return None;
        }
        // pcbSize 语义为字节；按字符数预留（+1 冗余），不依赖返回值语义差异
        let mut buf = vec![0u16; size as usize / 2 + 1];
        let mut read = size;
        let n = GetRawInputDeviceInfoW(
            Some(hdevice),
            RIDI_DEVICENAME,
            Some(buf.as_mut_ptr().cast::<core::ffi::c_void>()),
            &mut read,
        );
        if n == u32::MAX {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

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
}
