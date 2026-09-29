//! keylabel —— 键帽显示名：scancode → 当前布局键名（PLAN §3：`key_label(sc: u16) -> String`，带 LRU 缓存）。
//!
//! **只属于 GUI 展示层**（§2.5：禁止下沉到 collector/store；collector 不做任何键名翻译）。
//! 实现：`GetKeyNameTextW`（当前布局的键名）+ `MapVirtualKeyExW`（扩展键校验/回查），
//! 即任务契约指定的 "MapVirtualKeyExW + GetKeyNameTextW" 组合：
//!
//! 1. lParam 组装：bit16-23 = MakeCode（低 8 位），bit24 = E0 扩展键标志；
//! 2. `GetKeyNameTextW` 直接取名；
//! 3. 取不到时用 `MapVirtualKeyExW(MAPVK_VSC_TO_VK_EX, 当前布局)` 求 VK，再用
//!    `MAPVK_VK_TO_VSC` 回查规范扫描码重试一次（处理个别扩展标志不匹配的键）；
//! 4. 仍取不到 → 兜底 `键 0x{sc:X}`（与 core::qtkeys 兜底风格一致），绝不 panic。
//!
//! 特例：Pause（规范化码 `0xE11D`，E1 1D 45 序列）——`GetKeyNameTextW` 只认 0x45
//! （Windows 对 Pause 注册的扫描码），对 0x1D+E1 不出名字；按 0x45 查询，空则回 "Pause"。
//!
//! LRU 缓存：键名只随布局/语言变化，进程内缓存 512 条（容量足够覆盖常用键），
//! 命中零系统调用——`get_key_daily`/`get_top_keys`/导出全量填充 label 的热路径依赖它。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use clrecoder_core::codes::{mods, DeviceKind, PAUSE_SCANCODE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, GetKeyNameTextW, MapVirtualKeyExW, MAPVK_VK_TO_VSC, MAPVK_VSC_TO_VK_EX,
};

/// LRU 容量：覆盖全部常用扫描码（<256 基础码 + E0 前缀扩展码）绰绰有余。
const CACHE_CAP: usize = 512;

/// 简易 LRU：HashMap 存值 + VecDeque 维护访问序（get/put 均移到队尾，淘汰从队头）。
#[derive(Default)]
struct Lru {
    map: HashMap<u16, String>,
    order: VecDeque<u16>,
}

impl Lru {
    /// 命中并提升到队尾。
    fn get(&mut self, k: u16) -> Option<String> {
        if !self.map.contains_key(&k) {
            return None;
        }
        if let Some(pos) = self.order.iter().position(|&x| x == k) {
            self.order.remove(pos);
        }
        self.order.push_back(k);
        self.map.get(&k).cloned()
    }

    /// 插入并提升到队尾；超容量淘汰队头。
    fn put(&mut self, k: u16, v: String) {
        if self.map.insert(k, v).is_none() {
            self.order.push_back(k);
        }
        while self.map.len() > CACHE_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            } else {
                self.map.clear();
                break;
            }
        }
    }
}

fn cache() -> &'static Mutex<Lru> {
    static CACHE: OnceLock<Mutex<Lru>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Lru::default()))
}

/// 归一化 scancode → 当前布局键帽显示名（PLAN §3 契约，带 LRU 缓存）。
///
/// 布局无关地保证：`0x1D`（Ctrl）→ "Ctrl" 类输出；`0xE11D`（Pause）→ "Pause"；
/// 未知/取不到 → `键 0x{sc:X}`，绝不 panic、绝不返回空串。
#[must_use]
pub fn key_label(sc: u16) -> String {
    if let Some(hit) = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(sc) {
        return hit;
    }
    let label = query_key_name(sc);
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .put(sc, label.clone());
    label
}

/// 兜底标签（与 core::qtkeys 的兜底风格一致）。
fn fallback_label(sc: u16) -> String {
    format!("键 0x{sc:X}")
}

/// 单次查询（无缓存）。流程见模块注释 1-4。
fn query_key_name(sc: u16) -> String {
    // Pause 特例：0xE11D → 用 0x45（Windows 对 Pause 注册的扫描码）查询
    if sc == PAUSE_SCANCODE {
        let name = get_key_name_text(0x45, false);
        return if name.is_empty() { "Pause".to_string() } else { name };
    }
    let make = u32::from(sc & 0xFF);
    if make == 0 {
        return fallback_label(sc); // code 0 不对应物理键
    }
    let e0 = sc & 0xE000 != 0;

    // 第 1 次：GetKeyNameTextW 直查
    let name = get_key_name_text(make, e0);
    if !name.is_empty() {
        return name;
    }
    // 第 2 次：MapVirtualKeyExW 求 VK（扩展键在 uCode 里以 0x100 位表达），
    // 用 VK 回查规范扫描码再取一次名——兜个别键扩展标志/扫描码表达不一致的情况。
    let hkl = unsafe { GetKeyboardLayout(0) }; // GUI 线程 = 当前用户布局
    let sc_for_map = make | (u32::from(e0) * 0x100);
    let vk = unsafe { MapVirtualKeyExW(sc_for_map, MAPVK_VSC_TO_VK_EX, Some(hkl)) };
    if vk != 0 {
        let canonical = unsafe { MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC, Some(hkl)) };
        if canonical != 0 && (canonical & 0xFF) != 0 {
            let name2 = get_key_name_text(canonical & 0xFF, e0);
            if !name2.is_empty() {
                return name2;
            }
            // 个别键（如部分扩展键）规范码带扩展位时需重试：canonical 可能是 0x100|make 形态
            let name3 = get_key_name_text(canonical & 0xFF, canonical & 0x100 != 0 || e0);
            if !name3.is_empty() {
                return name3;
            }
        }
    }
    fallback_label(sc)
}

/// `GetKeyNameTextW` 单次调用：MakeCode + E0 标志 → 键名（失败/无名返回空串）。
fn get_key_name_text(make: u32, extended: bool) -> String {
    // lParam：bit16-23 扫描码，bit24 扩展键标志（Winuser.h）
    let mut lparam: i32 = ((make & 0xFF) << 16) as i32;
    if extended {
        lparam |= 0x0100_0000;
    }
    let mut buf = [0u16; 64];
    let n = unsafe { GetKeyNameTextW(lparam, &mut buf) };
    if n <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..n as usize]).trim().to_string()
    }
}

/// 鼠标按键 code → 显示名（`input_daily` 的鼠标 code 空间 1..=9，§4.1 MouseButton）。
/// 导出/逐日明细里非键盘设备的 label 填充用（GUI 展示层职责）。
#[must_use]
pub fn mouse_button_label(code: u16) -> String {
    match code {
        1 => "左键".into(),
        2 => "右键".into(),
        3 => "中键".into(),
        4 => "侧键X1（后退）".into(),
        5 => "侧键X2（前进）".into(),
        6 => "滚轮上".into(),
        7 => "滚轮下".into(),
        8 => "滚轮左".into(),
        9 => "滚轮右".into(),
        other => format!("按钮 {other}"),
    }
}

/// 手柄按键 code → 显示名（`input_daily` 的手柄 code 空间 1..=17，§4.1 GamepadButton）。
#[must_use]
pub fn gamepad_button_label(code: u16) -> String {
    match code {
        1 => "A（南）".into(),
        2 => "B（东）".into(),
        3 => "X（北）".into(),
        4 => "Y（西）".into(),
        5 => "LT".into(),
        6 => "LT2".into(),
        7 => "RT".into(),
        8 => "RT2".into(),
        9 => "Select".into(),
        10 => "Start".into(),
        11 => "Mode（Guide）".into(),
        12 => "左摇杆按下".into(),
        13 => "右摇杆按下".into(),
        14 => "十字上".into(),
        15 => "十字下".into(),
        16 => "十字左".into(),
        17 => "十字右".into(),
        other => format!("按钮 {other}"),
    }
}

/// 设备种类 + code → 显示名：键盘走布局相关 [`key_label`]，鼠标/手柄走静态展示名。
/// `get_key_daily` / Top-N / 导出统一入口（§4.7："label 由 GUI keylabel 补"）。
#[must_use]
pub fn code_label(kind: DeviceKind, code: u16) -> String {
    match kind {
        DeviceKind::Keyboard => key_label(code),
        DeviceKind::Mouse => mouse_button_label(code),
        DeviceKind::Gamepad => gamepad_button_label(code),
    }
}

/// 修饰键位掩码 → "Ctrl"/"Shift"/"Alt"/"Win" 名（固定顺序 Ctrl+Shift+Alt+Win，§4.7 示例 "Ctrl+Shift+T"）。
#[must_use]
pub fn mod_names(mods_bits: u8) -> Vec<&'static str> {
    let mut v = Vec::new();
    if mods_bits & mods::CTRL != 0 {
        v.push("Ctrl");
    }
    if mods_bits & mods::SHIFT != 0 {
        v.push("Shift");
    }
    if mods_bits & mods::ALT != 0 {
        v.push("Alt");
    }
    if mods_bits & mods::WIN != 0 {
        v.push("Win");
    }
    v
}

/// 组合键显示名：`combo_label(mods, code)` = 修饰名按序拼接 + 非修饰键名（如 "Ctrl+Shift+T"）。
#[must_use]
pub fn combo_label(mods_bits: u8, code: u16) -> String {
    let mut parts: Vec<String> = mod_names(mods_bits).into_iter().map(String::from).collect();
    parts.push(key_label(code));
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::codes::mods;

    /// 验收点（§8-S10）：keylabel 对 0x1D → "Ctrl" 类输出（非兜底、非空）。
    /// 标准布局（含 zh-CN）下 GetKeyNameTextW 返回 "Ctrl"；若系统布局极度特殊，
    /// 退而求其次断言"非空且不是兜底格式"。
    #[test]
    fn ctrl_scancode_yields_ctrl_like_label() {
        let l = key_label(0x1D);
        assert!(!l.is_empty());
        assert_ne!(l, fallback_label(0x1D), "0x1D 必须解析出真名，不能落到兜底: {l}");
        assert!(l.contains("Ctrl"), "0x1D 应为 Ctrl 类输出，实际: {l}");
    }

    /// 常用键非空且非兜底：'A'(0x1E)、空格(0x39)、回车(0x1C)、右Ctrl(0xE01D)。
    #[test]
    fn common_scancodes_resolve() {
        for sc in [0x1Eu16, 0x39, 0x1C, 0xE01D] {
            let l = key_label(sc);
            assert!(!l.is_empty(), "0x{sc:X} 标签为空");
            assert_ne!(l, fallback_label(sc), "0x{sc:X} 不应落到兜底: {l}");
        }
        // QWERTY 布局下 'A' 键显示为 "A"（zh-CN/en-US 通用）
        assert_eq!(key_label(0x1E), "A");
    }

    /// Pause 特例：0xE11D → 恒 "Pause"（GetKeyNameText 按 0x45 查询，空则字面兜底）。
    #[test]
    fn pause_key_is_guaranteed() {
        assert_eq!(key_label(0xE11D), "Pause");
    }

    /// LRU 缓存正确性：同一码两次调用结果一致（第二次为缓存命中路径）。
    #[test]
    fn cache_returns_consistent_results() {
        let a = key_label(0x2A);
        let b = key_label(0x2A);
        assert_eq!(a, b);
        assert_eq!(a, "Shift", "左 Shift（QWERTY）应为 Shift，实际: {a}");
    }

    /// code 0 与合法但无名扫描码：非空、不 panic（兜底路径）。
    #[test]
    fn fallback_for_unnamed_codes() {
        assert_eq!(key_label(0x0000), fallback_label(0x0000));
        let odd = key_label(0x0063); // 多数布局无此码 → 兜底或真名，二者皆合法
        assert!(!odd.is_empty());
    }

    /// 鼠标/手柄静态展示名（导出与逐日明细用）。
    #[test]
    fn mouse_and_gamepad_labels() {
        assert_eq!(mouse_button_label(1), "左键");
        assert_eq!(mouse_button_label(9), "滚轮右");
        assert_eq!(mouse_button_label(42), "按钮 42");
        assert_eq!(gamepad_button_label(1), "A（南）");
        assert_eq!(gamepad_button_label(17), "十字右");
        assert_eq!(gamepad_button_label(99), "按钮 99");
        assert_eq!(code_label(DeviceKind::Mouse, 6), "滚轮上");
        assert_eq!(code_label(DeviceKind::Gamepad, 11), "Mode（Guide）");
        assert_eq!(code_label(DeviceKind::Keyboard, 0x1E), "A");
    }

    /// 组合键标签：固定顺序 Ctrl+Shift+Alt+Win + 键名（§4.7 "Ctrl+Shift+T" 形状）。
    #[test]
    fn combo_label_format() {
        assert!(combo_label(mods::CTRL, 0x14).starts_with("Ctrl+"), "{}", combo_label(mods::CTRL, 0x14));
        let cs = combo_label(mods::CTRL | mods::SHIFT, 0x14);
        assert!(cs.starts_with("Ctrl+Shift+"), "{cs}");
        assert!(cs.ends_with(key_label(0x14).as_str()), "{cs}");
        // 位序无关：mods 组合里各修饰名都出现
        let all = combo_label(mods::CTRL | mods::SHIFT | mods::ALT | mods::WIN, 0x1E);
        for name in ["Ctrl", "Shift", "Alt", "Win"] {
            assert!(all.contains(name), "{all} 缺 {name}");
        }
        // L/R 合并位：CTRL|SHIFT 不产生重复段
        assert_eq!(all.matches('+').count(), 4, "{all}");
    }
}
