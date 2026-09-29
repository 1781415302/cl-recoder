//! code 空间契约（PLAN §4.1 逐字对齐）——全系统最重要的契约。
//!
//! **唯一性边界**：`code` 用 `u16` 表达，但其唯一性只在设备种类内成立
//! （`MouseButton` 1..=9 与 `GamepadButton` 1..=17 值域重叠，键盘 scancode 低段也有重叠）；
//! 消歧靠 `input_daily.device_id → devices.kind`，**禁止按 code 值域判断设备种类**。

use serde::{Deserialize, Serialize};

/// 设备种类。serde 序列化为小写字符串：`"keyboard"` | `"mouse"` | `"gamepad"`
/// （与 store DDL 的 `devices.kind` CHECK 约束、GUI TS 契约逐字对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    /// 键盘（含 Consumer Control 页的媒体键，按键盘统计）
    Keyboard,
    /// 鼠标（左/右/中/X1/X2 + 滚轮四向，与手柄完全分开）
    Mouse,
    /// 手柄（独立于鼠标统计）
    Gamepad,
}

/// 鼠标按键 code 空间（1..=9）。滚轮刻度由采集层折算成按键事件（§4.6 累计器）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum MouseButton {
    /// 左键
    Left = 1,
    /// 右键
    Right = 2,
    /// 中键
    Middle = 3,
    /// 侧键 X1（后退）
    X1 = 4,
    /// 侧键 X2（前进）
    X2 = 5,
    /// 滚轮上（采集层累计 120 delta 折算 1 刻度）
    WheelUp = 6,
    /// 滚轮下
    WheelDown = 7,
    /// 滚轮左（水平滚轮）
    WheelLeft = 8,
    /// 滚轮右
    WheelRight = 9,
}

impl From<MouseButton> for u16 {
    /// 写库用 code（`input_daily.code`）。
    fn from(b: MouseButton) -> Self {
        b as u16
    }
}

/// 手柄按键 code 空间（1..=17）。与 `MouseButton` 值域重叠——种类消歧靠 `devices.kind`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum GamepadButton {
    /// 南（A / 叉）
    South = 1,
    /// 东（B / 圈）
    East = 2,
    /// 北（Y / 三角）
    North = 3,
    /// 西（X / 方块）
    West = 4,
    /// 左Trigger（LT，模拟量上穿 0.33 计 1 次）
    LeftTrigger = 5,
    /// 左Trigger2（LT 深行程）
    LeftTrigger2 = 6,
    /// 右Trigger（RT）
    RightTrigger = 7,
    /// 右Trigger2（RT 深行程）
    RightTrigger2 = 8,
    /// 选择键（Back/Share）
    Select = 9,
    /// 开始键（Start/Options）
    Start = 10,
    /// 模式键（Guide/PS）
    Mode = 11,
    /// 左摇杆按下
    LeftThumb = 12,
    /// 右摇杆按下
    RightThumb = 13,
    /// 十字键上
    DPadUp = 14,
    /// 十字键下
    DPadDown = 15,
    /// 十字键左
    DPadLeft = 16,
    /// 十字键右
    DPadRight = 17,
}

impl From<GamepadButton> for u16 {
    /// 写库用 code（`input_daily.code`）。
    fn from(b: GamepadButton) -> Self {
        b as u16
    }
}

/// 修饰键位掩码（L/R 合并；组合键统计 `combo_daily.mods` 的取值域）。
pub mod mods {
    /// Ctrl（左/右任一）
    pub const CTRL: u8 = 1;
    /// Shift（左/右任一）
    pub const SHIFT: u8 = 2;
    /// Alt（左/右任一）
    pub const ALT: u8 = 4;
    /// Win（左/右任一）
    pub const WIN: u8 = 8;
}

/// VK_PAUSE（Winuser.h）。
const VK_PAUSE: u16 = 0x13;

/// KEYBOARD_OVERRUN_MAKE_CODE（键盘缓冲溢出标志扫描码，Winuser.h）。
const KEYBOARD_OVERRUN_MAKE_CODE: u8 = 0xFF;

/// Pause 键的归一化码：`E1 1D 45` 序列与 VK_PAUSE 的 0x45 伴随事件统一成此码。
pub const PAUSE_SCANCODE: u16 = 0xE11D;

/// 键盘归一化 scancode：`u16 = MakeCode | (E0 ? 0xE000 : 0) | (E1 ? 0xE100 : 0)`（PLAN §4.1）。
///
/// 非显然规则（必须实现，禁止简化）：
/// - **E1 前缀**（Pause 键 `E1 1D 45` 序列的首事件：MakeCode=0x1D + E1 标志）与
///   **VKey==VK_PAUSE(0x13) 的 0x45 伴随事件**都归一化为 [`PAUSE_SCANCODE`]（0xE11D），
///   由 Engine 的按下状态表天然去重成 1 次计数；
/// - **MakeCode==0xFF**（KEYBOARD_OVERRUN_MAKE_CODE，键盘缓冲溢出）→ 丢弃（返回 `None`）；
/// - **MakeCode==0 且 VKey!=0** 的情况：由采集层（raw_input）先用
///   `MapVirtualKeyW(VKey, MAPVK_VK_TO_VSC_EX)` 解析出 scancode 后再调用本函数。
///   本 crate 禁止依赖 windows API，因此本函数保持**纯函数**；若 make==0 仍然到达此处
///   （防御兜底），返回 `None` 丢弃。
///
/// # 参数
/// - `make`：RAWKEYBOARD.MakeCode 的低 8 位
/// - `e0`：`RAWKEYBOARD.Flags & RI_KEY_E0`
/// - `e1`：`RAWKEYBOARD.Flags & RI_KEY_E1`
/// - `vkey`：RAWKEYBOARD.VKey
#[must_use]
pub fn normalize_scancode(make: u8, e0: bool, e1: bool, vkey: u16) -> Option<u16> {
    // 键盘缓冲溢出事件：不对应任何物理键，丢弃。
    if make == KEYBOARD_OVERRUN_MAKE_CODE {
        return None;
    }
    // Pause 的 0x45 伴随事件（无前缀标志、VKey==VK_PAUSE）：与 E1 1D 首事件归一化为同一码，
    // Engine 按下状态表据此把两次 WM_INPUT 去重为 1 次计数。
    // 注意顺序：必须先于通用公式，否则 e1=true 时 0x45 会错误归一化为 0xE145。
    if make == 0x45 && vkey == VK_PAUSE {
        return Some(PAUSE_SCANCODE);
    }
    // MakeCode==0：采集层应先用 MapVirtualKeyW 解析（本函数无 OS 依赖，保持纯函数）；
    // 防御兜底：解析后仍为 0（或 VKey 也为 0 的垃圾事件）一律丢弃。
    if make == 0 {
        return None;
    }
    let mut sc = make as u16;
    if e0 {
        sc |= 0xE000;
    }
    if e1 {
        sc |= 0xE100;
    }
    Some(sc)
}

/// 修饰键 scancode 集合（布局无关，固定集合）：返回对应 [`mods`] 位（位或语义）。
///
/// | scancode | 修饰键 |
/// |---|---|
/// | `0x2A` / `0x36` / `0xE036` | Shift |
/// | `0x1D` / `0xE01D` | Ctrl |
/// | `0x38` / `0xE038` | Alt |
/// | `0xE05B` / `0xE05C` | Win |
///
/// 非 0 返回值之间可用位或组合；引擎（engine）据此从"物理 sc 集合"重算 mods_held。
#[must_use]
pub fn modifier_bit(sc: u16) -> Option<u8> {
    match sc {
        0x2A | 0x36 | 0xE036 => Some(mods::SHIFT),
        0x1D | 0xE01D => Some(mods::CTRL),
        0x38 | 0xE038 => Some(mods::ALT),
        0xE05B | 0xE05C => Some(mods::WIN),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- DeviceKind serde 小写 ----------

    #[test]
    fn device_kind_serde_lowercase() {
        assert_eq!(serde_json::to_string(&DeviceKind::Keyboard).unwrap(), r#""keyboard""#);
        assert_eq!(serde_json::to_string(&DeviceKind::Mouse).unwrap(), r#""mouse""#);
        assert_eq!(serde_json::to_string(&DeviceKind::Gamepad).unwrap(), r#""gamepad""#);
        let k: DeviceKind = serde_json::from_str(r#""keyboard""#).unwrap();
        assert_eq!(k, DeviceKind::Keyboard);
        let g: DeviceKind = serde_json::from_str(r#""gamepad""#).unwrap();
        assert_eq!(g, DeviceKind::Gamepad);
        // 非法值必须报错（DDL CHECK(kind IN ...) 的 Rust 侧对应）
        assert!(serde_json::from_str::<DeviceKind>(r#""joystick""#).is_err());
    }

    // ---------- code 值域（与 §4.1 逐字对齐）----------

    #[test]
    fn mouse_button_discriminants() {
        assert_eq!(MouseButton::Left as u16, 1);
        assert_eq!(MouseButton::Right as u16, 2);
        assert_eq!(MouseButton::Middle as u16, 3);
        assert_eq!(MouseButton::X1 as u16, 4);
        assert_eq!(MouseButton::X2 as u16, 5);
        assert_eq!(MouseButton::WheelUp as u16, 6);
        assert_eq!(MouseButton::WheelDown as u16, 7);
        assert_eq!(MouseButton::WheelLeft as u16, 8);
        assert_eq!(MouseButton::WheelRight as u16, 9);
    }

    #[test]
    fn gamepad_button_discriminants() {
        let expect: [(GamepadButton, u16); 17] = [
            (GamepadButton::South, 1),
            (GamepadButton::East, 2),
            (GamepadButton::North, 3),
            (GamepadButton::West, 4),
            (GamepadButton::LeftTrigger, 5),
            (GamepadButton::LeftTrigger2, 6),
            (GamepadButton::RightTrigger, 7),
            (GamepadButton::RightTrigger2, 8),
            (GamepadButton::Select, 9),
            (GamepadButton::Start, 10),
            (GamepadButton::Mode, 11),
            (GamepadButton::LeftThumb, 12),
            (GamepadButton::RightThumb, 13),
            (GamepadButton::DPadUp, 14),
            (GamepadButton::DPadDown, 15),
            (GamepadButton::DPadLeft, 16),
            (GamepadButton::DPadRight, 17),
        ];
        for (b, code) in expect {
            assert_eq!(b as u16, code);
            assert_eq!(u16::from(b), code);
        }
    }

    #[test]
    fn mods_bit_constants() {
        assert_eq!(mods::CTRL, 1);
        assert_eq!(mods::SHIFT, 2);
        assert_eq!(mods::ALT, 4);
        assert_eq!(mods::WIN, 8);
        // 四个修饰键全组合仍在 u8 内且互不重叠
        assert_eq!(mods::CTRL | mods::SHIFT | mods::ALT | mods::WIN, 0b1111);
    }

    // ---------- normalize_scancode ----------

    #[test]
    fn normalize_plain_scancode() {
        // 'A' 键：MakeCode=0x1E，无前缀
        assert_eq!(normalize_scancode(0x1E, false, false, 0x41), Some(0x1E));
        // 空格：0x39
        assert_eq!(normalize_scancode(0x39, false, false, 0x20), Some(0x39));
    }

    #[test]
    fn normalize_e0_prefix() {
        // 左 Ctrl=0x1D，右 Ctrl=E0 1D
        assert_eq!(normalize_scancode(0x1D, false, false, 0x11), Some(0x1D));
        assert_eq!(normalize_scancode(0x1D, true, false, 0x11), Some(0xE01D));
        // 左 Win=E0 5B
        assert_eq!(normalize_scancode(0x5B, true, false, 0x5B), Some(0xE05B));
        // NumLock=E0 45（不得被 Pause 伴随事件规则误伤）
        assert_eq!(normalize_scancode(0x45, true, false, 0x90), Some(0xE045));
    }

    #[test]
    fn normalize_pause_e1_and_companion_dedupe_to_same_code() {
        // Pause 首事件：MakeCode=0x1D + E1 标志 → 通用公式
        assert_eq!(normalize_scancode(0x1D, false, true, 0x13), Some(0xE11D));
        // Pause 伴随事件：MakeCode=0x45、无前缀标志、VKey==VK_PAUSE(0x13) → 特判归一化
        assert_eq!(normalize_scancode(0x45, false, false, 0x13), Some(0xE11D));
        // 伴随事件即使带残留 E1/E0 标志也归一化为同一码（特判先于通用公式）
        assert_eq!(normalize_scancode(0x45, false, true, 0x13), Some(0xE11D));
        assert_eq!(normalize_scancode(0x45, true, false, 0x13), Some(0xE11D));
        // 两条路径产物相同 → Engine 按下状态表天然去重成 1 次计数
        assert_eq!(
            normalize_scancode(0x1D, false, true, 0x13),
            normalize_scancode(0x45, false, false, 0x13)
        );
    }

    #[test]
    fn normalize_overrun_make_code_discarded() {
        assert_eq!(normalize_scancode(0xFF, false, false, 0x00), None);
        assert_eq!(normalize_scancode(0xFF, true, false, 0x13), None);
    }

    #[test]
    fn normalize_zero_make_code_defensive_drop() {
        // 契约：MakeCode==0 且 VKey!=0 由采集层先 MapVirtualKeyW 解析后再进 core；
        // 这里验证纯函数兜底——解析后仍为 0 的一律丢弃，绝不产出 code 0 进库。
        assert_eq!(normalize_scancode(0x00, false, false, 0x41), None);
        assert_eq!(normalize_scancode(0x00, false, false, 0x00), None);
    }

    // ---------- modifier_bit ----------

    #[test]
    fn modifier_bit_fixed_set() {
        // Shift：0x2A / 0x36 / 0xE036
        assert_eq!(modifier_bit(0x2A), Some(mods::SHIFT));
        assert_eq!(modifier_bit(0x36), Some(mods::SHIFT));
        assert_eq!(modifier_bit(0xE036), Some(mods::SHIFT));
        // Ctrl：0x1D / 0xE01D（左右等价，L/R 合并位）
        assert_eq!(modifier_bit(0x1D), Some(mods::CTRL));
        assert_eq!(modifier_bit(0xE01D), Some(mods::CTRL));
        // Alt：0x38 / 0xE038
        assert_eq!(modifier_bit(0x38), Some(mods::ALT));
        assert_eq!(modifier_bit(0xE038), Some(mods::ALT));
        // Win：0xE05B / 0xE05C
        assert_eq!(modifier_bit(0xE05B), Some(mods::WIN));
        assert_eq!(modifier_bit(0xE05C), Some(mods::WIN));
    }

    #[test]
    fn modifier_bit_rejects_non_modifiers() {
        assert_eq!(modifier_bit(0x1E), None); // 'A'
        assert_eq!(modifier_bit(0x46), None); // ScrollLock
        assert_eq!(modifier_bit(0xE05D), None); // Application 键
        assert_eq!(modifier_bit(0xE11D), None); // Pause（非修饰键）
        assert_eq!(modifier_bit(0x2B), None); // '\'
        assert_eq!(modifier_bit(0x00), None);
    }
}
