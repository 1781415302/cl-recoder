//! clrecoder-engine —— 键盘统计**纯函数**状态机（PLAN §1/§2.2/§4.3）。
//!
//! 职责边界：只处理键盘事件（sc + down/up），不依赖 windows crate、不做任何 IO、
//! 不碰鼠标/手柄事件；唯一依赖是 [`clrecoder_core`] 的修饰键位掩码契约
//! （`codes::modifier_bit` / `codes::mods`）。
//!
//! 统计语义（§4.3 五条规则，逐字实现）：
//! 1) down 且 held 中已存在该 sc → 自动重复，不产计数，held 不变；
//! 2) down：插入 held；该键计数 `Key{sc}` 恒产出（含修饰键自身，修饰键也磨损）；
//! 3) down 且该键非修饰键且 mods_held != 0 → 产出 `Combo{mods: mods_held, code: sc}`；
//! 4) up：从 held 移除（不存在则忽略）；修饰键按"物理 sc 集合"跟踪，
//!    L/R 合并位仅在两侧都抬起后清零（mods_held 每次由 held 中的修饰键重算）；
//! 5) mods_held = OR(held 中所有键的 modifier_bit)。
//!
//! "今天"不属于引擎状态——日期按调用方（aggregator）参数入桶，跨天自然开新桶。

use std::collections::HashSet;

use clrecoder_core::codes::modifier_bit;

/// 一次键盘事件的产出：0..=2 条计数（PLAN §4.3 契约类型，字段逐字对齐）。
///
/// - [`EngineOut::key`]：物理按键计数（写 `input_daily.code`），down 恒产出（含修饰键自身）；
/// - [`EngineOut::combo`]：组合键计数 `(mods, code)`（写 `combo_daily.mods` / `combo_daily.code`），
///   仅当 down 的是**非修饰键**且按下时有修饰键处于按住状态才产出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[must_use]
pub struct EngineOut {
    /// 按键计数：`Some(sc)` 表示该物理键按下计 1 次。
    pub key: Option<u16>,
    /// 组合键计数：`Some((mods, code))`，mods 为 [`clrecoder_core::codes::mods`] 位掩码。
    pub combo: Option<(u8, u16)>,
}

/// 键盘统计纯状态机（PLAN §4.3）。
///
/// 按下状态以"物理 scancode 集合"跟踪（`held`），修饰键位掩码 `mods_held`
/// 每次 held 变动后由 held 中的修饰键重算——左右修饰键天然等价（L/R 合并位）。
/// 引擎不存日期、不存设备；调用方对每个 `(设备, 键盘事件)` 调一次 [`Engine::on_key`]。
#[derive(Debug, Clone, Default)]
pub struct Engine {
    /// 当前按下的物理 scancode 集合（修饰键与非修饰键统一跟踪）。
    held: HashSet<u16>,
    /// mods_held = OR(held 中所有键的 modifier_bit)（§4.3 规则 5，held 变动后重算）。
    mods_held: u8,
}

impl Engine {
    /// 创建空状态引擎（无键按下、无修饰键）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            held: HashSet::new(),
            mods_held: 0,
        }
    }

    /// 输入一个键盘事件（归一化 scancode + 按下/抬起边沿），输出 0..=2 条计数。
    ///
    /// `sc` 必须是采集层经 `core::codes::normalize_scancode` 归一化后的码值
    /// （如 Pause 的 E1 双事件归一化为同一码 0xE11D，由本状态表天然去重）。
    pub fn on_key(&mut self, sc: u16, down: bool) -> EngineOut {
        if down {
            // 规则 1：自动重复——held 已含该 sc：不产计数，held/mods 均不变。
            if self.held.contains(&sc) {
                return EngineOut::default();
            }
            let is_modifier = modifier_bit(sc).is_some();
            // 规则 2/3：down 恒产 Key；Combo 只在"非修饰键 + 有修饰键按住"时产出。
            // 此处 mods_held 是插入前的值；因 Combo 只对非修饰键产出，而插入
            // 非修饰键不改写 mods_held，故"插入前 = 插入后"，无歧义。
            let out = EngineOut {
                key: Some(sc),
                combo: if !is_modifier && self.mods_held != 0 {
                    Some((self.mods_held, sc))
                } else {
                    None
                },
            };
            // 规则 2：插入 held；规则 5：mods_held 由 held 重算。
            self.held.insert(sc);
            self.mods_held = self.recompute_mods_held();
            out
        } else {
            // 规则 4：up——从 held 移除（不存在则忽略），不产任何计数。
            self.held.remove(&sc);
            self.mods_held = self.recompute_mods_held();
            EngineOut::default()
        }
    }

    /// 规则 4/5：mods_held 每次由 held 中的修饰键重算——
    /// 左右合并位（如 0x1D 与 0xE01D 同为 CTRL）仅在两侧都抬起后自然清零。
    fn recompute_mods_held(&self) -> u8 {
        self.held
            .iter()
            .filter_map(|&sc| modifier_bit(sc))
            .fold(0, |acc, bit| acc | bit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::codes::{mods, normalize_scancode};

    /// 断言"无任何计数"产出（自动重复 / up 事件的预期形状）。
    fn assert_no_counts(out: EngineOut) {
        assert_eq!(out, EngineOut { key: None, combo: None });
    }

    // ------------------------------------------------------------------
    // 组 1：repeat 去重——按住 'A' 产生 N 个重复 make → 恰好 1 个 Key
    // ------------------------------------------------------------------

    #[test]
    fn auto_repeat_produces_exactly_one_key() {
        let mut e = Engine::new();
        // 首次 down：恰好 1 个 Key
        assert_eq!(e.on_key(0x1E, true), EngineOut { key: Some(0x1E), combo: None });
        // 按住期间 OS 自动重复 9 个 make：全部去重，held 不变
        for _ in 0..9 {
            assert_no_counts(e.on_key(0x1E, true));
        }
        // 抬起（不产计数）后再按下：重新计 1 次——证明去重靠按下状态表而非计数上限
        assert_no_counts(e.on_key(0x1E, false));
        assert_eq!(e.on_key(0x1E, true), EngineOut { key: Some(0x1E), combo: None });
    }

    // ------------------------------------------------------------------
    // 组 2：Ctrl+C 连按两次 → 2 个 Combo + 2 个 Key(C)
    // ------------------------------------------------------------------

    #[test]
    fn ctrl_c_pressed_twice_yields_two_combos_and_two_keys() {
        let mut e = Engine::new();
        // Ctrl 按下：只有 Key(Ctrl)，无 Combo（修饰键自身不触发组合键）
        assert_eq!(e.on_key(0x1D, true), EngineOut { key: Some(0x1D), combo: None });
        // 第一次 Ctrl+C
        assert_eq!(
            e.on_key(0x2E, true),
            EngineOut { key: Some(0x2E), combo: Some((mods::CTRL, 0x2E)) }
        );
        assert_no_counts(e.on_key(0x2E, false));
        // 第二次 Ctrl+C（Ctrl 全程按住）
        assert_eq!(
            e.on_key(0x2E, true),
            EngineOut { key: Some(0x2E), combo: Some((mods::CTRL, 0x2E)) }
        );
        assert_no_counts(e.on_key(0x2E, false));
        assert_no_counts(e.on_key(0x1D, false));
    }

    // ------------------------------------------------------------------
    // 组 3：Ctrl 按住时依次按 C、V → 2 个 Combo（mods 相同）
    // ------------------------------------------------------------------

    #[test]
    fn ctrl_held_then_c_then_v_yields_two_combos_with_same_mods() {
        let mut e = Engine::new();
        assert_eq!(e.on_key(0x1D, true).key, Some(0x1D));
        let c = e.on_key(0x2E, true);
        assert_eq!(c.combo, Some((mods::CTRL, 0x2E)));
        assert_no_counts(e.on_key(0x2E, false));
        let v = e.on_key(0x2F, true);
        assert_eq!(v.combo, Some((mods::CTRL, 0x2F)));
        assert_no_counts(e.on_key(0x2F, false));
        // 两个 Combo 的 mods 位掩码相同
        assert_eq!(c.combo.map(|(m, _)| m), v.combo.map(|(m, _)| m));
    }

    // ------------------------------------------------------------------
    // 组 4：Shift+A 松开 Shift 后再按 A → 第二个 A 无 Combo
    // ------------------------------------------------------------------

    #[test]
    fn shift_a_then_release_shift_then_plain_a_has_no_combo() {
        let mut e = Engine::new();
        assert_eq!(e.on_key(0x2A, true).key, Some(0x2A));
        // Shift+A：有 Combo
        assert_eq!(
            e.on_key(0x1E, true),
            EngineOut { key: Some(0x1E), combo: Some((mods::SHIFT, 0x1E)) }
        );
        assert_no_counts(e.on_key(0x1E, false));
        // 松开 Shift：无计数
        assert_no_counts(e.on_key(0x2A, false));
        // 再按 A：mods 已清零 → 无 Combo
        assert_eq!(e.on_key(0x1E, true), EngineOut { key: Some(0x1E), combo: None });
    }

    // ------------------------------------------------------------------
    // 组 5：Pause 的 E1 双事件 → 1 个 Key(0xE11D)
    // ------------------------------------------------------------------

    #[test]
    fn pause_e1_double_event_counts_once() {
        let mut e = Engine::new();
        // 采集层两条 WM_INPUT 经 core 契约归一化为同一码
        let first = normalize_scancode(0x1D, false, true, 0x13).unwrap(); // E1 1D 首事件
        let second = normalize_scancode(0x45, false, false, 0x13).unwrap(); // 0x45 伴随事件
        assert_eq!(first, 0xE11D);
        assert_eq!(second, 0xE11D);
        // 两条 down 只产 1 个 Key(0xE11D)：第二条命中按下状态表去重
        assert_eq!(e.on_key(first, true), EngineOut { key: Some(0xE11D), combo: None });
        assert_no_counts(e.on_key(second, true));
        // 抬起后状态完全复位：再按 Pause 仍计 1
        assert_no_counts(e.on_key(0xE11D, false));
        assert_eq!(e.on_key(first, true), EngineOut { key: Some(0xE11D), combo: None });
    }

    // ------------------------------------------------------------------
    // 组 6：右侧 Ctrl(0xE01D)+X 与左侧等价（mods 同为 CTRL）；L/R 合并位两侧都抬起才清零
    // ------------------------------------------------------------------

    #[test]
    fn right_ctrl_equals_left_ctrl() {
        let mut e = Engine::new();
        // 右 Ctrl（E0 1D）+ X
        assert_eq!(e.on_key(0xE01D, true), EngineOut { key: Some(0xE01D), combo: None });
        assert_eq!(
            e.on_key(0x2D, true),
            EngineOut { key: Some(0x2D), combo: Some((mods::CTRL, 0x2D)) }
        );
        assert_no_counts(e.on_key(0x2D, false));
        assert_no_counts(e.on_key(0xE01D, false));
        // 左 Ctrl + X：mods 与右侧完全相同
        assert_eq!(e.on_key(0x1D, true).key, Some(0x1D));
        assert_eq!(
            e.on_key(0x2D, true),
            EngineOut { key: Some(0x2D), combo: Some((mods::CTRL, 0x2D)) }
        );
        assert_no_counts(e.on_key(0x2D, false));
        // L/R 合并位语义：左右都按下，抬起一侧后 CTRL 位仍在（另一侧还按着）
        assert_eq!(e.on_key(0xE01D, true), EngineOut { key: Some(0xE01D), combo: None });
        assert_no_counts(e.on_key(0xE01D, false)); // 只抬起右侧
        assert_eq!(
            e.on_key(0x2D, true),
            EngineOut { key: Some(0x2D), combo: Some((mods::CTRL, 0x2D)) }
        );
        assert_no_counts(e.on_key(0x2D, false));
        // 两侧都抬起 → CTRL 位清零
        assert_no_counts(e.on_key(0x1D, false));
        assert_eq!(e.on_key(0x2D, true), EngineOut { key: Some(0x2D), combo: None });
    }

    // ------------------------------------------------------------------
    // 组 7：纯修饰键按下产 Key、不产 Combo（全部修饰键 sc 逐个验证）
    // ------------------------------------------------------------------

    #[test]
    fn modifier_keys_alone_produce_key_but_never_combo() {
        let mut e = Engine::new();
        // core 契约的全修饰键集合：LShift/RShift/RShift(E0)/LCtrl/RCtrl/LAlt/RAlt/LWin/RWin
        for sc in [0x2Au16, 0x36, 0xE036, 0x1D, 0xE01D, 0x38, 0xE038, 0xE05B, 0xE05C] {
            assert_eq!(
                e.on_key(sc, true),
                EngineOut { key: Some(sc), combo: None },
                "sc=0x{sc:04X} 应只产 Key"
            );
            assert_no_counts(e.on_key(sc, false));
        }
        // 修饰键之间叠加也绝不产 Combo：Ctrl 按住时按 Shift，只有 Key(Shift)
        assert_eq!(e.on_key(0x1D, true), EngineOut { key: Some(0x1D), combo: None });
        assert_eq!(e.on_key(0x2A, true), EngineOut { key: Some(0x2A), combo: None });
    }

    // ------------------------------------------------------------------
    // 补充边界（契约内行为的额外锚定）
    // ------------------------------------------------------------------

    #[test]
    fn up_without_down_is_ignored_and_state_stays_consistent() {
        let mut e = Engine::new();
        // 幽灵 up（如采集启动前已按下的键）：忽略、不 panic、不产计数
        assert_no_counts(e.on_key(0x1E, false));
        // 之后的正常按下仍计 1，说明幽灵 up 未污染状态
        assert_eq!(e.on_key(0x1E, true), EngineOut { key: Some(0x1E), combo: None });
    }

    #[test]
    fn modifier_auto_repeat_is_deduped_like_plain_keys() {
        let mut e = Engine::new();
        assert_eq!(e.on_key(0x2A, true).key, Some(0x2A));
        // 修饰键的自动重复 make 同样被规则 1 去重，且不影响 mods 状态
        assert_no_counts(e.on_key(0x2A, true));
        assert_eq!(
            e.on_key(0x1E, true),
            EngineOut { key: Some(0x1E), combo: Some((mods::SHIFT, 0x1E)) }
        );
    }

    #[test]
    fn multiple_modifiers_or_together_in_combo_mods() {
        let mut e = Engine::new();
        assert_eq!(e.on_key(0x1D, true).key, Some(0x1D));
        assert_eq!(e.on_key(0x2A, true).key, Some(0x2A));
        // Ctrl+Shift+T：mods = CTRL|SHIFT 位或
        assert_eq!(
            e.on_key(0x14, true),
            EngineOut { key: Some(0x14), combo: Some((mods::CTRL | mods::SHIFT, 0x14)) }
        );
        assert_no_counts(e.on_key(0x14, false));
        // 抬起 Ctrl 后仅剩 Shift：T 的 Combo mods 退化为 SHIFT
        assert_no_counts(e.on_key(0x1D, false));
        assert_eq!(
            e.on_key(0x14, true),
            EngineOut { key: Some(0x14), combo: Some((mods::SHIFT, 0x14)) }
        );
    }
}
