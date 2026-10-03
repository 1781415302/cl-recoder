//! clrecoder-engine —— 键盘统计**纯函数**状态机（PLAN §1/§2.2/§4.3）。
//!
//! 职责边界：只处理键盘事件（sc + down/up），不依赖 windows crate、不做任何 IO、
//! 不碰鼠标/手柄事件；唯一依赖是 [`clrecoder_core`] 的修饰键位掩码契约
//! （`codes::modifier_bit` / `codes::mods`）。
//!
//! 统计语义（PLAN §4.3 五条规则 + correctness-v2 §4.2 来源维度，逐字实现）：
//! 1) down 且 held 中已存在该 `(source, sc)` → 自动重复，不产计数，held 不变；
//! 2) down：插入 held；该键计数 `Key{sc}` 恒产出（含修饰键自身，修饰键也磨损）；
//! 3) down 且该键非修饰键且 mods_held != 0 → 产出 `Combo{mods: mods_held, code: sc}`；
//! 4) up：从 held 移除该 `(source, sc)`（不存在则忽略）；修饰键按"物理 sc 集合"跟踪，
//!    L/R 合并位仅在两侧都抬起后清零（mods_held 每次由 held 中的修饰键重算）；
//! 5) mods_held = OR(held 中所有键的 modifier_bit)。
//!
//! 来源维度（correctness-v2 §4.2，F3/F5 的状态机基础）：held 的逻辑键是 `(source, sc)`——
//! 同一来源的重复 make 判为自动重复，不同来源的同码按下各自独立计数；修饰位始终取
//! **所有** held 项的位或，[`Engine::remove_source`] / [`Engine::clear_sources`]
//! （设备拔出/重置生命周期）后重算，且移除一个来源不影响另一个仍按住的同码修饰键。
//! [`Engine::on_key`] 保留为 source=0 的单来源兼容包装，生产 aggregator 必须走
//! [`Engine::on_key_from`]。
//!
//! "今天"不属于引擎状态——日期按调用方（aggregator）参数入桶，跨天自然开新桶。

use std::collections::HashSet;

use clrecoder_core::codes::modifier_bit;
use clrecoder_core::event::InputSourceId;

/// 单来源兼容入口的保留来源 ID：[`Engine::on_key`] 路由到它（真实输入用正 ID）。
const COMPAT_SOURCE: InputSourceId = InputSourceId(0);

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

/// 键盘统计纯状态机（PLAN §4.3 + correctness-v2 §4.2）。
///
/// 按下状态以 `(来源, 物理 scancode)` 集合跟踪（`held`），修饰键位掩码 `mods_held`
/// 每次 held 变动后由 held 中的修饰键重算——左右修饰键天然等价（L/R 合并位）。
/// 引擎不存日期、不存设备；调用方对每个 `(来源, 键盘事件)` 调一次
/// [`Engine::on_key_from`]（单键盘兼容路径用 [`Engine::on_key`]，等价于来源 0）。
#[derive(Debug, Clone, Default)]
pub struct Engine {
    /// 当前按下的 `(来源, scancode)` 集合（修饰键与非修饰键统一跟踪；
    /// 来源维度实现同码跨键盘独立计数——F3/F5 的状态机基础）。
    held: HashSet<(InputSourceId, u16)>,
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
    /// 单来源兼容入口：等价于 `on_key_from(InputSourceId(0), sc, down)`——已有单键盘
    /// 调用方与测试无需改动；生产 aggregator 必须改走 [`Engine::on_key_from`]。
    ///
    /// `sc` 必须是采集层经 `core::codes::normalize_scancode` 归一化后的码值
    /// （如 Pause 的 E1 双事件归一化为同一码 0xE11D，由本状态表天然去重）。
    pub fn on_key(&mut self, sc: u16, down: bool) -> EngineOut {
        self.on_key_from(COMPAT_SOURCE, sc, down)
    }

    /// 输入一个带来源的键盘事件（归一化 scancode + 按下/抬起边沿），输出 0..=2 条计数。
    ///
    /// `source` 为 collector 进程内单调分配的连接 ID（0 保留给 [`Engine::on_key`]）；
    /// 按下状态按 `(source, sc)` 去重——同一来源的重复 make 判为自动重复，
    /// 不同来源的同码按下各自独立计数（F3/F5）。修饰位始终取**所有**来源
    /// held 项的位或，`up` / `remove_source` / `clear_sources` 后重算。
    ///
    /// `sc` 必须是采集层经 `core::codes::normalize_scancode` 归一化后的码值。
    pub fn on_key_from(&mut self, source: InputSourceId, sc: u16, down: bool) -> EngineOut {
        if down {
            // 规则 1：自动重复——held 已含该 (source, sc)：不产计数，held/mods 均不变。
            if self.held.contains(&(source, sc)) {
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
            self.held.insert((source, sc));
            self.mods_held = self.recompute_mods_held();
            out
        } else {
            // 规则 4：up——从 held 移除该 (source, sc)（不存在则忽略），不产任何计数。
            self.held.remove(&(source, sc));
            self.mods_held = self.recompute_mods_held();
            EngineOut::default()
        }
    }

    /// 移除一个输入来源的全部按下状态（设备拔出/失联的生命周期事件）：
    /// 该来源按住的键视为全部抬起，mods_held 由剩余 held 重算——
    /// 不影响其他来源仍按住的同码修饰键（correctness-v2 §4.2）。
    pub fn remove_source(&mut self, source: InputSourceId) {
        self.held.retain(|&(s, _)| s != source);
        self.mods_held = self.recompute_mods_held();
    }

    /// 清空全部来源的按下状态（键盘来源整体重置的生命周期事件）：
    /// mods_held 归零，此后各来源的 down 重新独立计数。
    pub fn clear_sources(&mut self) {
        self.held.clear();
        self.mods_held = self.recompute_mods_held();
    }

    /// 规则 4/5：mods_held 每次由 held 中的修饰键重算——
    /// 左右合并位（如 0x1D 与 0xE01D 同为 CTRL）仅在两侧都抬起后自然清零。
    fn recompute_mods_held(&self) -> u8 {
        self.held
            .iter()
            .filter_map(|&(_, sc)| modifier_bit(sc))
            .fold(0, |acc, bit| acc | bit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::codes::{mods, normalize_scancode};
    use clrecoder_core::event::InputSourceId;

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

    // ==================================================================
    // correctness-v2 §4.2：来源维度（F3/F5 状态机基础）
    // 每个测试对应行为表一行；真实输入使用正 ID，0 仅限 on_key 兼容入口。
    // ==================================================================

    /// 行为表第 1 行：`(101,A,down)`、`(102,A,down)` → 两个 key=Some(0x1E)。
    /// 同码双键盘各自独立计数（型号相同也按连接隔离——Writer 侧再按型号合并）。
    #[test]
    fn correctness_v2_same_scancode_two_sources_count_independently() {
        let mut e = Engine::new();
        assert_eq!(
            e.on_key_from(InputSourceId(101), 0x1E, true),
            EngineOut { key: Some(0x1E), combo: None }
        );
        assert_eq!(
            e.on_key_from(InputSourceId(102), 0x1E, true),
            EngineOut { key: Some(0x1E), combo: None }
        );
        // 101 自身的重复 make 仍被去重——隔离只在来源之间，不破坏规则 1
        assert_no_counts(e.on_key_from(InputSourceId(101), 0x1E, true));
    }

    /// 行为表第 2 行：`(101,A,down)` 重复三次 → 仅第一条有 Key。
    #[test]
    fn correctness_v2_repeat_dedup_is_per_source() {
        let mut e = Engine::new();
        assert_eq!(e.on_key_from(InputSourceId(101), 0x1E, true).key, Some(0x1E));
        assert_no_counts(e.on_key_from(InputSourceId(101), 0x1E, true));
        assert_no_counts(e.on_key_from(InputSourceId(101), 0x1E, true));
        // 101 的按下状态不影响 102 的同码 down（去重键是 (source, sc)）
        assert_eq!(e.on_key_from(InputSourceId(102), 0x1E, true).key, Some(0x1E));
    }

    /// 行为表第 3 行：101 Ctrl down，102 C down → 第二条 combo=Some((1,0x2E))。
    /// 修饰位跨来源位或（mods::CTRL == 1）。
    #[test]
    fn correctness_v2_cross_keyboard_ctrl_combo() {
        let mut e = Engine::new();
        assert_eq!(e.on_key_from(InputSourceId(101), 0x1D, true).key, Some(0x1D));
        let c = e.on_key_from(InputSourceId(102), 0x2E, true);
        assert_eq!(c, EngineOut { key: Some(0x2E), combo: Some((mods::CTRL, 0x2E)) });
        // 行为表逐字锚定：mods 值就是 1
        assert_eq!(c.combo, Some((1, 0x2E)));
    }

    /// 行为表第 4 行：101/102 Ctrl down，101 Ctrl up，102 C down → C 仍有 Ctrl+C。
    /// up 只摘除本来源的 (source, sc)，另一来源的同码修饰键保留。
    #[test]
    fn correctness_v2_ctrl_up_on_one_source_keeps_other_sources_mod() {
        let mut e = Engine::new();
        assert!(e.on_key_from(InputSourceId(101), 0x1D, true).key.is_some());
        assert!(e.on_key_from(InputSourceId(102), 0x1D, true).key.is_some());
        assert_no_counts(e.on_key_from(InputSourceId(101), 0x1D, false));
        assert_eq!(
            e.on_key_from(InputSourceId(102), 0x2E, true),
            EngineOut { key: Some(0x2E), combo: Some((mods::CTRL, 0x2E)) }
        );
    }

    /// 行为表第 5 行：101 Ctrl down，remove_source(101)，102 C down → C 无 Combo。
    #[test]
    fn correctness_v2_remove_source_drops_only_that_source_state() {
        let mut e = Engine::new();
        assert!(e.on_key_from(InputSourceId(101), 0x1D, true).key.is_some());
        e.remove_source(InputSourceId(101));
        assert_eq!(
            e.on_key_from(InputSourceId(102), 0x2E, true),
            EngineOut { key: Some(0x2E), combo: None }
        );
    }

    /// §4.2 非显然决策：移除一个来源不能影响另一个仍按住的同码修饰键
    /// （101/102 都按住 Ctrl，移除 101 后 102 的 C 仍产 Ctrl+C）。
    #[test]
    fn correctness_v2_remove_source_spares_other_source_same_modifier() {
        let mut e = Engine::new();
        assert!(e.on_key_from(InputSourceId(101), 0x1D, true).key.is_some());
        assert!(e.on_key_from(InputSourceId(102), 0x1D, true).key.is_some());
        e.remove_source(InputSourceId(101));
        assert_eq!(
            e.on_key_from(InputSourceId(102), 0x2E, true),
            EngineOut { key: Some(0x2E), combo: Some((mods::CTRL, 0x2E)) }
        );
    }

    /// 行为表第 6 行：101 Ctrl down → clear_sources → 101 A down
    /// → A 重新计数、无旧 Ctrl。
    #[test]
    fn correctness_v2_clear_sources_allows_fresh_count_without_stale_mods() {
        let mut e = Engine::new();
        assert!(e.on_key_from(InputSourceId(101), 0x1D, true).key.is_some());
        // clear 前确立旧状态：A 带旧 Ctrl 组合
        assert_eq!(
            e.on_key_from(InputSourceId(101), 0x1E, true),
            EngineOut { key: Some(0x1E), combo: Some((mods::CTRL, 0x1E)) }
        );
        e.clear_sources();
        // 同一来源的同码 down 重新计 1 次（held 已清空），且不带 clear 前按住的 Ctrl
        assert_eq!(
            e.on_key_from(InputSourceId(101), 0x1E, true),
            EngineOut { key: Some(0x1E), combo: None }
        );
    }

    /// 兼容入口：`on_key` 与 `on_key_from(InputSourceId(0), …)` 共享同一按下状态
    /// （source=0 专用于单来源兼容路径，真实输入的正 ID 与之隔离）。
    #[test]
    fn correctness_v2_on_key_compat_wrapper_routes_to_source_zero() {
        let mut e = Engine::new();
        assert_eq!(e.on_key(0x1E, true), EngineOut { key: Some(0x1E), combo: None });
        // source=0 的同码 down 判为自动重复（两条路径等价）
        assert_no_counts(e.on_key_from(InputSourceId(0), 0x1E, true));
        assert_no_counts(e.on_key(0x1E, true));
        // 正 ID 来源与 source=0 互不干扰
        assert_eq!(
            e.on_key_from(InputSourceId(1), 0x1E, true),
            EngineOut { key: Some(0x1E), combo: None }
        );
    }

    /// 空状态上的生命周期操作是无害 no-op：不 panic、不污染后续计数。
    #[test]
    fn correctness_v2_remove_and_clear_on_empty_state_are_noops() {
        let mut e = Engine::new();
        e.remove_source(InputSourceId(101));
        e.clear_sources();
        assert_eq!(
            e.on_key_from(InputSourceId(101), 0x1E, true),
            EngineOut { key: Some(0x1E), combo: None }
        );
    }
}
