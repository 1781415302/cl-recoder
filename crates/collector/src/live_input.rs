//! 实时输入内存状态机（DEVPLAN-input-live-trigger §4.3）。
//!
//! 职责：每来源 held、按下边沿序号、RAW 心跳、会话门（capture epoch）、完整快照。
//! 禁止：SQL、网络传输、GUI 回调、统计计数、DPI/设备解析、序列化（快照复制后由
//! live_server 外层序列化）。
//!
//! 内部 `capture_epoch` 拒绝跨会话在途帧；线上 `reset_epoch` 供前端清显示基线——
//! 二者不是同一个承诺值。共享锁内只做内存态更新。

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use clrecoder_core::codes::{GamepadButton, PAUSE_SCANCODE};
use clrecoder_core::event::{DeviceKey, InputSourceId};
use clrecoder_core::live_input::{
    LiveCaptureState, LiveDigitalState, LiveGamepadAnalog, LivePoint, LivePress, LiveSnapshot,
    LiveSource, LiveSourceAvailability, LIVE_SOURCE_STALE_MS,
};
use clrecoder_core::motion::{MotionConnectionId, StickPoint};

/// 会话门代际令牌：生产者在原生取样/解包**前**捕获；发布时与当前 `capture_epoch` 比对。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LiveCaptureToken {
    /// 捕获时的内部 capture epoch
    pub epoch: u64,
}

/// 鼠标一包实时边沿（codes 1..5）与滚轮 pulse（codes 6..9）。
///
/// 包内同时 down/up 时按 down→up 排列；滚轮非零 raw delta 产生一次方向 pulse。
/// 不改统计滚轮累计器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveMousePacket {
    /// 按键边沿 `(code, down)`，code ∈ 1..=5
    pub edges: Vec<(u16, bool)>,
    /// 滚轮方向 pulse，code ∈ 6..=9
    pub wheel_codes: Vec<u16>,
}

/// 同次 XInput 采样的全部实时字段（数字 + 模拟）。
#[derive(Debug, Clone, PartialEq)]
pub struct LiveGamepadSample {
    /// XInput 槽位 0..3
    pub slot: u8,
    /// 连接代际
    pub connection: MotionConnectionId,
    /// 型号身份
    pub device: DeviceKey,
    /// `XINPUT_GAMEPAD.wButtons` 位掩码
    pub buttons: u16,
    /// 左摇杆（已归一化，Y 正向上）
    pub left: StickPoint,
    /// 右摇杆
    pub right: StickPoint,
    /// LT = raw/255
    pub left_trigger: f64,
    /// RT = raw/255
    pub right_trigger: f64,
}

/// 内存实时状态机（线程安全；内部状态私有）。
#[derive(Debug)]
pub struct LiveInputRuntime {
    inner: Mutex<Inner>,
}

/// Pause 伴随 make 去重窗口（§4.1：30ms 内同来源）。
const PAUSE_DEDUP: Duration = Duration::from_millis(30);

/// LT/RT 数字 held/pulse 门（§4.1：raw/255 >= 0.33；模拟填充仍用连续原值）。
const TRIGGER_DIGITAL_GATE: f64 = 0.33;

/// Guide / Mode 码：仅出现在 `unsupported_codes`，无 held/pulse。
const GUIDE_CODE: u16 = GamepadButton::Mode as u16;

// XINPUT_GAMEPAD.wButtons（learn.microsoft.com XINPUT_GAMEPAD）
const XINPUT_DPAD_UP: u16 = 0x0001;
const XINPUT_DPAD_DOWN: u16 = 0x0002;
const XINPUT_DPAD_LEFT: u16 = 0x0004;
const XINPUT_DPAD_RIGHT: u16 = 0x0008;
const XINPUT_START: u16 = 0x0010;
const XINPUT_BACK: u16 = 0x0020;
const XINPUT_LEFT_THUMB: u16 = 0x0040;
const XINPUT_RIGHT_THUMB: u16 = 0x0080;
const XINPUT_LEFT_SHOULDER: u16 = 0x0100;
const XINPUT_RIGHT_SHOULDER: u16 = 0x0200;
const XINPUT_A: u16 = 0x1000;
const XINPUT_B: u16 = 0x2000;
const XINPUT_X: u16 = 0x4000;
const XINPUT_Y: u16 = 0x8000;

#[derive(Debug)]
struct Inner {
    session_id: String,
    capture_state: LiveCaptureState,
    /// 内部会话门代际；`set_capture_state` 每次 +1
    capture_epoch: u64,
    /// 线上复位代际；会话复位与 RAW 破坏性过期清态时 +1
    reset_epoch: u64,
    /// 快照序号；每次 `snapshot` +1 后写入
    sample_seq: u64,
    raw_last_touch: Option<Instant>,
    /// 已对当前 `raw_last_touch` 做过破坏性清态（每断段一次）
    raw_stale_cleared: bool,
    keyboards: HashMap<InputSourceId, KeyboardSource>,
    mice: HashMap<InputSourceId, MouseSource>,
    gamepads: HashMap<u8, GamepadSource>,
}

#[derive(Debug)]
struct KeyboardSource {
    device: DeviceKey,
    digital: DigitalTracker,
}

#[derive(Debug)]
struct MouseSource {
    device: DeviceKey,
    source_key: String,
    digital: DigitalTracker,
}

#[derive(Debug)]
struct GamepadSource {
    device: DeviceKey,
    connection: MotionConnectionId,
    availability: LiveSourceAvailability,
    last_sample_at: Option<Instant>,
    digital: DigitalTracker,
    analog: Option<LiveGamepadAnalog>,
    /// 断段后下一次成功采样只建基线、不回放边沿
    baseline_reset: bool,
}

#[derive(Debug, Default)]
struct DigitalTracker {
    /// code → 边沿序号 / held / 最近有效按下时刻
    codes: HashMap<u16, CodeState>,
    /// 本来源最近一次 Pause pulse（去重用）
    pause_last_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
struct CodeState {
    held: bool,
    press_seq: u64,
    last_press_at: Option<Instant>,
}

impl CodeState {
    fn fresh() -> Self {
        Self {
            held: false,
            press_seq: 0,
            last_press_at: None,
        }
    }
}

impl DigitalTracker {
    fn clear_held_and_pulses(&mut self) {
        for state in self.codes.values_mut() {
            state.held = false;
            state.last_press_at = None;
        }
        self.pause_last_at = None;
    }

    fn entry_mut(&mut self, code: u16) -> &mut CodeState {
        self.codes.entry(code).or_insert_with(CodeState::fresh)
    }

    /// 有效按下边沿：press_seq +1，记录时刻，可选置 held。
    fn note_press(&mut self, code: u16, now: Instant, hold: bool) {
        let state = self.entry_mut(code);
        state.press_seq = state.press_seq.saturating_add(1);
        state.last_press_at = Some(now);
        if hold {
            state.held = true;
        }
    }

    fn release(&mut self, code: u16) {
        if let Some(state) = self.codes.get_mut(&code) {
            state.held = false;
        }
    }

    fn is_held(&self, code: u16) -> bool {
        self.codes.get(&code).map(|s| s.held).unwrap_or(false)
    }

    /// 基线重建：按当前硬件 held 集合置位，不增加 press_seq、不写 last_press_at。
    fn set_held_baseline(&mut self, held: &[u16]) {
        for state in self.codes.values_mut() {
            state.held = false;
        }
        for &code in held {
            let state = self.entry_mut(code);
            state.held = true;
        }
    }

    fn to_live(&self, now: Instant) -> LiveDigitalState {
        let mut held_codes: Vec<u16> = self
            .codes
            .iter()
            .filter(|(_, s)| s.held)
            .map(|(&c, _)| c)
            .collect();
        held_codes.sort_unstable();
        held_codes.dedup();

        let mut presses: Vec<LivePress> = Vec::new();
        for (&code, state) in &self.codes {
            let Some(at) = state.last_press_at else {
                continue;
            };
            if state.press_seq == 0 {
                continue;
            }
            presses.push(LivePress {
                code,
                press_seq: state.press_seq.to_string(),
                age_ms: age_ms(at, now),
            });
        }
        presses.sort_by_key(|p| p.code);

        LiveDigitalState {
            held_codes,
            presses,
        }
    }
}

impl LiveInputRuntime {
    /// 初始 capture=unknown、无来源、epoch/序号为 0。
    pub fn new(session_id: String) -> Self {
        Self {
            inner: Mutex::new(Inner {
                session_id,
                capture_state: LiveCaptureState::Unknown,
                capture_epoch: 0,
                reset_epoch: 0,
                sample_seq: 0,
                raw_last_touch: None,
                raw_stale_cleared: false,
                keyboards: HashMap::new(),
                mice: HashMap::new(),
                gamepads: HashMap::new(),
            }),
        }
    }

    /// 仅 `Active` 时返回当前 capture 代际；必须在原生取样/解包前捕获。
    pub fn capture_token(&self) -> Option<LiveCaptureToken> {
        let g = self.lock();
        if g.capture_state == LiveCaptureState::Active {
            Some(LiveCaptureToken {
                epoch: g.capture_epoch,
            })
        } else {
            None
        }
    }

    /// 每次显式会话复位均增 capture epoch 并清态，即使 `state` 相同；不改 Flags.paused。
    pub fn set_capture_state(&self, state: LiveCaptureState) {
        let mut g = self.lock();
        g.capture_epoch = g.capture_epoch.saturating_add(1);
        g.reset_epoch = g.reset_epoch.saturating_add(1);
        g.capture_state = state;
        g.clear_all_sources();
        g.raw_last_touch = None;
        g.raw_stale_cleared = false;
    }

    /// RAW 窗口 25ms 定时器心跳；旧 epoch 无效。
    pub fn touch_raw_input(&self, token: LiveCaptureToken, now: Instant) {
        let mut g = self.lock();
        if !g.token_ok(token) {
            return;
        }
        g.ensure_raw_not_stale(now);
        g.raw_last_touch = Some(now);
        g.raw_stale_cleared = false;
    }

    /// 键盘边沿：来源去重、held 及 pressSeq；自动重复不增序号；Pause 为 pulse-only。
    pub fn keyboard_edge(
        &self,
        token: LiveCaptureToken,
        source: InputSourceId,
        device: &DeviceKey,
        code: u16,
        down: bool,
        now: Instant,
    ) {
        let mut g = self.lock();
        if !g.token_ok(token) {
            return;
        }
        g.ensure_raw_not_stale(now);
        g.raw_last_touch = Some(now);
        g.raw_stale_cleared = false;

        let entry = g
            .keyboards
            .entry(source)
            .or_insert_with(|| KeyboardSource {
                device: device.clone(),
                digital: DigitalTracker::default(),
            });
        entry.device = device.clone();

        if code == PAUSE_SCANCODE {
            if down {
                if let Some(prev) = entry.digital.pause_last_at {
                    if now.duration_since(prev) <= PAUSE_DEDUP {
                        return;
                    }
                }
                entry.digital.note_press(code, now, false);
                entry.digital.pause_last_at = Some(now);
            }
            // up：Pause 从不 held，无需释放
            return;
        }

        if down {
            if entry.digital.is_held(code) {
                // 自动重复：不增加 press_seq
                return;
            }
            entry.digital.note_press(code, now, true);
        } else {
            entry.digital.release(code);
        }
    }

    /// 鼠标包：buttons 1..5 的 down/up 与滚轮 6..9 pulse。
    pub fn mouse_packet(
        &self,
        token: LiveCaptureToken,
        source: InputSourceId,
        device: &DeviceKey,
        source_key: &str,
        packet: LiveMousePacket,
        now: Instant,
    ) {
        let mut g = self.lock();
        if !g.token_ok(token) {
            return;
        }
        g.ensure_raw_not_stale(now);
        g.raw_last_touch = Some(now);
        g.raw_stale_cleared = false;

        let entry = g.mice.entry(source).or_insert_with(|| MouseSource {
            device: device.clone(),
            source_key: source_key.to_string(),
            digital: DigitalTracker::default(),
        });
        entry.device = device.clone();
        entry.source_key = source_key.to_string();

        for (code, down) in packet.edges {
            if !(1..=5).contains(&code) {
                continue;
            }
            if down {
                if entry.digital.is_held(code) {
                    continue;
                }
                entry.digital.note_press(code, now, true);
            } else {
                entry.digital.release(code);
            }
        }
        for code in packet.wheel_codes {
            if !(6..=9).contains(&code) {
                continue;
            }
            // 滚轮只 pulse，永不 held
            entry.digital.note_press(code, now, false);
        }
    }

    /// 同次 XInput 全部实时值、数字边沿、来源心跳。
    pub fn gamepad_sample(&self, token: LiveCaptureToken, sample: LiveGamepadSample, now: Instant) {
        let mut g = self.lock();
        if !g.token_ok(token) {
            return;
        }
        // 其它槽位过期检查（本槽将写入，先处理全局过期）
        g.ensure_gamepads_not_stale(now);

        let held_now = digital_from_xinput(
            sample.buttons,
            sample.left_trigger,
            sample.right_trigger,
        );
        let analog = LiveGamepadAnalog {
            left_stick: LivePoint {
                x: sample.left.x,
                y: sample.left.y,
            },
            right_stick: LivePoint {
                x: sample.right.x,
                y: sample.right.y,
            },
            left_trigger: sample.left_trigger,
            right_trigger: sample.right_trigger,
        };

        let slot = sample.slot;
        let entry = g.gamepads.entry(slot).or_insert_with(|| GamepadSource {
            device: sample.device.clone(),
            connection: sample.connection,
            // 首连按正常边沿处理（可产生首次 pulse）；仅断段后的 baseline_reset 抑制回放
            availability: LiveSourceAvailability::Ready,
            last_sample_at: None,
            digital: DigitalTracker::default(),
            analog: None,
            baseline_reset: false,
        });

        // 本槽若已过期且尚为 Ready，先断段（ensure 已处理其它槽；本槽在 entry 取出前亦已处理）
        if entry.availability == LiveSourceAvailability::Ready {
            if let Some(last) = entry.last_sample_at {
                if now.duration_since(last)
                    > Duration::from_millis(LIVE_SOURCE_STALE_MS as u64)
                {
                    entry.mark_unavailable_segment();
                }
            }
        }

        // 仅真实断段后的恢复抑制边沿回放；品牌新来源走 apply_digital_edges
        let rebuild = entry.baseline_reset;

        entry.device = sample.device.clone();
        entry.connection = sample.connection;
        entry.last_sample_at = Some(now);
        entry.analog = Some(analog);
        entry.availability = LiveSourceAvailability::Ready;

        if rebuild {
            entry.digital.set_held_baseline(&held_now);
            entry.baseline_reset = false;
        } else {
            apply_digital_edges(&mut entry.digital, &held_now, now);
        }
    }

    /// 断连删除；瞬时失败保留连接身份但清态 / analog=None。
    pub fn gamepad_unavailable(&self, slot: u8, disconnected: bool) {
        let mut g = self.lock();
        if disconnected {
            g.gamepads.remove(&slot);
            return;
        }
        if let Some(pad) = g.gamepads.get_mut(&slot) {
            // 持续失败：已 unavailable 的断段不再反复清其它基线/本槽 pressSeq 语义
            if pad.availability == LiveSourceAvailability::Unavailable && pad.baseline_reset {
                return;
            }
            pad.mark_unavailable_segment();
        }
    }

    /// 移除对应 RAW 来源，不影响同型号其它来源。
    pub fn remove_raw_source(&self, source: InputSourceId) {
        let mut g = self.lock();
        g.keyboards.remove(&source);
        g.mice.remove(&source);
    }

    /// 采集循环退出/重建时清 RAW 来源。
    pub fn clear_raw_sources(&self) {
        let mut g = self.lock();
        g.keyboards.clear();
        g.mice.clear();
        g.raw_last_touch = None;
        g.raw_stale_cleared = false;
    }

    /// 手柄线程退出时清其来源。
    pub fn clear_gamepads(&self) {
        let mut g = self.lock();
        g.gamepads.clear();
    }

    /// 单锁复制完整快照，计算 ages，递增 sampleSeq。
    pub fn snapshot(&self, now: Instant, statistics_paused: bool) -> LiveSnapshot {
        let mut g = self.lock();
        g.ensure_raw_not_stale(now);
        g.ensure_gamepads_not_stale(now);

        g.sample_seq = g.sample_seq.saturating_add(1);

        let raw_input_age_ms = g.raw_last_touch.map(|t| age_ms(t, now));

        let mut sources: Vec<LiveSource> = Vec::new();

        let mut kb_ids: Vec<InputSourceId> = g.keyboards.keys().copied().collect();
        kb_ids.sort_by_key(|s| s.0);
        for id in kb_ids {
            let src = &g.keyboards[&id];
            sources.push(LiveSource::Keyboard {
                source_id: id.0.to_string(),
                device: src.device.clone(),
                digital: src.digital.to_live(now),
            });
        }

        let mut mouse_ids: Vec<InputSourceId> = g.mice.keys().copied().collect();
        mouse_ids.sort_by_key(|s| s.0);
        for id in mouse_ids {
            let src = &g.mice[&id];
            sources.push(LiveSource::Mouse {
                source_id: id.0.to_string(),
                device: src.device.clone(),
                source_key: src.source_key.clone(),
                digital: src.digital.to_live(now),
            });
        }

        let mut slots: Vec<u8> = g.gamepads.keys().copied().collect();
        slots.sort_unstable();
        for slot in slots {
            let src = &g.gamepads[&slot];
            let age = src
                .last_sample_at
                .map(|t| age_ms(t, now))
                .unwrap_or(0);
            sources.push(LiveSource::Gamepad {
                source_id: src.connection.0.to_string(),
                device: src.device.clone(),
                slot,
                connection_id: src.connection.0.to_string(),
                availability: src.availability,
                age_ms: age,
                digital: src.digital.to_live(now),
                analog: src.analog,
                unsupported_codes: vec![GUIDE_CODE],
            });
        }

        LiveSnapshot {
            session_id: g.session_id.clone(),
            sample_seq: g.sample_seq.to_string(),
            reset_epoch: g.reset_epoch.to_string(),
            statistics_paused,
            capture_state: g.capture_state,
            raw_input_age_ms,
            sources,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Inner {
    fn token_ok(&self, token: LiveCaptureToken) -> bool {
        token.epoch == self.capture_epoch
            && self.capture_state == LiveCaptureState::Active
    }

    fn clear_all_sources(&mut self) {
        self.keyboards.clear();
        self.mice.clear();
        self.gamepads.clear();
    }

    /// RAW 心跳过期：破坏性清 held/可播放 pulse，推进 reset_epoch（每断段一次）。
    fn ensure_raw_not_stale(&mut self, now: Instant) {
        let Some(last) = self.raw_last_touch else {
            return;
        };
        if now.duration_since(last) <= Duration::from_millis(LIVE_SOURCE_STALE_MS as u64) {
            return;
        }
        if self.raw_stale_cleared {
            return;
        }
        for kb in self.keyboards.values_mut() {
            kb.digital.clear_held_and_pulses();
        }
        for ms in self.mice.values_mut() {
            ms.digital.clear_held_and_pulses();
        }
        self.reset_epoch = self.reset_epoch.saturating_add(1);
        self.raw_stale_cleared = true;
    }

    fn ensure_gamepads_not_stale(&mut self, now: Instant) {
        for pad in self.gamepads.values_mut() {
            if pad.availability != LiveSourceAvailability::Ready {
                continue;
            }
            let Some(last) = pad.last_sample_at else {
                continue;
            };
            if now.duration_since(last) > Duration::from_millis(LIVE_SOURCE_STALE_MS as u64) {
                pad.mark_unavailable_segment();
            }
        }
    }
}

impl GamepadSource {
    fn mark_unavailable_segment(&mut self) {
        self.digital.clear_held_and_pulses();
        self.analog = None;
        self.availability = LiveSourceAvailability::Unavailable;
        self.baseline_reset = true;
    }
}

fn age_ms(from: Instant, now: Instant) -> u32 {
    now.duration_since(from)
        .as_millis()
        .min(u128::from(u32::MAX)) as u32
}

/// 从 wButtons + 扳机行程推导当前数字 held 集合（不含 Guide）。
fn digital_from_xinput(buttons: u16, left_trigger: f64, right_trigger: f64) -> Vec<u16> {
    let mut held = Vec::with_capacity(16);
    let pairs: [(u16, u16); 14] = [
        (XINPUT_A, GamepadButton::South as u16),
        (XINPUT_B, GamepadButton::East as u16),
        (XINPUT_Y, GamepadButton::North as u16),
        (XINPUT_X, GamepadButton::West as u16),
        (XINPUT_LEFT_SHOULDER, GamepadButton::LeftTrigger as u16),
        (XINPUT_RIGHT_SHOULDER, GamepadButton::RightTrigger as u16),
        (XINPUT_BACK, GamepadButton::Select as u16),
        (XINPUT_START, GamepadButton::Start as u16),
        (XINPUT_LEFT_THUMB, GamepadButton::LeftThumb as u16),
        (XINPUT_RIGHT_THUMB, GamepadButton::RightThumb as u16),
        (XINPUT_DPAD_UP, GamepadButton::DPadUp as u16),
        (XINPUT_DPAD_DOWN, GamepadButton::DPadDown as u16),
        (XINPUT_DPAD_LEFT, GamepadButton::DPadLeft as u16),
        (XINPUT_DPAD_RIGHT, GamepadButton::DPadRight as u16),
    ];
    for (bit, code) in pairs {
        if buttons & bit != 0 {
            held.push(code);
        }
    }
    if left_trigger >= TRIGGER_DIGITAL_GATE {
        held.push(GamepadButton::LeftTrigger2 as u16);
    }
    if right_trigger >= TRIGGER_DIGITAL_GATE {
        held.push(GamepadButton::RightTrigger2 as u16);
    }
    held.sort_unstable();
    held.dedup();
    held
}

fn apply_digital_edges(digital: &mut DigitalTracker, held_now: &[u16], now: Instant) {
    let prev: Vec<u16> = digital
        .codes
        .iter()
        .filter(|(_, s)| s.held)
        .map(|(&c, _)| c)
        .collect();

    for &code in held_now {
        if !digital.is_held(code) {
            digital.note_press(code, now, true);
        }
    }
    for code in prev {
        if !held_now.contains(&code) {
            digital.release(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::codes::DeviceKind;

    fn kb_device(name: &str) -> DeviceKey {
        DeviceKey {
            kind: DeviceKind::Keyboard,
            vid: 0x04D9,
            pid: 0x0169,
            name: name.to_string(),
        }
    }

    fn mouse_device() -> DeviceKey {
        DeviceKey {
            kind: DeviceKind::Mouse,
            vid: 0x046D,
            pid: 0xC077,
            name: "USB Optical Mouse".to_string(),
        }
    }

    fn pad_device() -> DeviceKey {
        DeviceKey {
            kind: DeviceKind::Gamepad,
            vid: 0x045E,
            pid: 0x028E,
            name: "Xbox 360 Controller".to_string(),
        }
    }

    fn activate(rt: &LiveInputRuntime) -> LiveCaptureToken {
        rt.set_capture_state(LiveCaptureState::Active);
        rt.capture_token().expect("active token")
    }

    fn kb_digital<'a>(snap: &'a LiveSnapshot, source_id: &str) -> &'a LiveDigitalState {
        for s in &snap.sources {
            if let LiveSource::Keyboard {
                source_id: id,
                digital,
                ..
            } = s
            {
                if id == source_id {
                    return digital;
                }
            }
        }
        panic!("keyboard {source_id} missing");
    }

    fn pad_source(snap: &LiveSnapshot, slot: u8) -> &LiveSource {
        snap.sources
            .iter()
            .find(|s| matches!(s, LiveSource::Gamepad { slot: sl, .. } if *sl == slot))
            .expect("gamepad missing")
    }

    #[test]
    fn new_starts_unknown_without_sources() {
        let rt = LiveInputRuntime::new("1:100".into());
        assert!(rt.capture_token().is_none());
        let t0 = Instant::now();
        let snap = rt.snapshot(t0, false);
        assert_eq!(snap.capture_state, LiveCaptureState::Unknown);
        assert_eq!(snap.sample_seq, "1");
        assert_eq!(snap.reset_epoch, "0");
        assert!(snap.sources.is_empty());
        assert_eq!(snap.raw_input_age_ms, None);
    }

    #[test]
    fn old_token_rejected_after_reset() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = kb_device("K");
        rt.keyboard_edge(tok, InputSourceId(1), &dev, 0x1E, true, t0);

        rt.set_capture_state(LiveCaptureState::Active);
        let t1 = t0 + Duration::from_millis(1);
        // 旧 token 不得回填
        rt.keyboard_edge(tok, InputSourceId(1), &dev, 0x1E, true, t1);
        let snap = rt.snapshot(t1, false);
        assert!(snap.sources.is_empty());
        assert_eq!(snap.reset_epoch, "2"); // 两次 set_capture_state
    }

    #[test]
    fn same_source_autorepeat_does_not_bump_press_seq() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = kb_device("K");
        rt.keyboard_edge(tok, InputSourceId(1), &dev, 0x1E, true, t0);
        rt.keyboard_edge(
            tok,
            InputSourceId(1),
            &dev,
            0x1E,
            true,
            t0 + Duration::from_millis(30),
        );
        rt.keyboard_edge(
            tok,
            InputSourceId(1),
            &dev,
            0x1E,
            true,
            t0 + Duration::from_millis(60),
        );
        let snap = rt.snapshot(t0 + Duration::from_millis(70), false);
        let d = kb_digital(&snap, "1");
        assert_eq!(d.held_codes, vec![0x1E]);
        assert_eq!(d.presses.len(), 1);
        assert_eq!(d.presses[0].press_seq, "1");
    }

    #[test]
    fn two_same_model_sources_release_keeps_union_held() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        // 同型号四字段一致
        let dev = kb_device("HID Keyboard Device");
        rt.keyboard_edge(tok, InputSourceId(1), &dev, 0x1D, true, t0); // Ctrl
        rt.keyboard_edge(tok, InputSourceId(2), &dev, 0x1D, true, t0);
        rt.keyboard_edge(
            tok,
            InputSourceId(1),
            &dev,
            0x1D,
            false,
            t0 + Duration::from_millis(10),
        );
        let snap = rt.snapshot(t0 + Duration::from_millis(20), false);
        let d1 = kb_digital(&snap, "1");
        let d2 = kb_digital(&snap, "2");
        assert!(d1.held_codes.is_empty());
        assert_eq!(d2.held_codes, vec![0x1D]);
        // GUI 并集：至少一侧仍 held
        assert!(d1.held_codes.contains(&0x1D) || d2.held_codes.contains(&0x1D));
    }

    #[test]
    fn short_press_between_snapshots_keeps_press_seq() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = kb_device("K");
        // 两帧之间完成 down/up
        rt.keyboard_edge(tok, InputSourceId(1), &dev, 0x1E, true, t0);
        rt.keyboard_edge(
            tok,
            InputSourceId(1),
            &dev,
            0x1E,
            false,
            t0 + Duration::from_millis(5),
        );
        let snap = rt.snapshot(t0 + Duration::from_millis(40), false);
        let d = kb_digital(&snap, "1");
        assert!(d.held_codes.is_empty());
        assert_eq!(d.presses.len(), 1);
        assert_eq!(d.presses[0].code, 0x1E);
        assert_eq!(d.presses[0].press_seq, "1");
        assert!(d.presses[0].age_ms >= 35);
    }

    #[test]
    fn remove_raw_source_drops_only_that_source() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = kb_device("K");
        rt.keyboard_edge(tok, InputSourceId(1), &dev, 0x1E, true, t0);
        rt.keyboard_edge(tok, InputSourceId(2), &dev, 0x1F, true, t0);
        rt.remove_raw_source(InputSourceId(1));
        let snap = rt.snapshot(t0 + Duration::from_millis(1), false);
        assert_eq!(snap.sources.len(), 1);
        assert!(matches!(
            &snap.sources[0],
            LiveSource::Keyboard { source_id, .. } if source_id == "2"
        ));
    }

    #[test]
    fn pause_is_pulse_only_with_companion_dedup() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = kb_device("K");
        // 伴随 make：30ms 内两次 down → 只计一次
        rt.keyboard_edge(tok, InputSourceId(1), &dev, PAUSE_SCANCODE, true, t0);
        rt.keyboard_edge(
            tok,
            InputSourceId(1),
            &dev,
            PAUSE_SCANCODE,
            true,
            t0 + Duration::from_millis(10),
        );
        // 无 break 也不应 held
        let snap = rt.snapshot(t0 + Duration::from_millis(20), false);
        let d = kb_digital(&snap, "1");
        assert!(d.held_codes.is_empty());
        assert_eq!(d.presses.len(), 1);
        assert_eq!(d.presses[0].code, PAUSE_SCANCODE);
        assert_eq!(d.presses[0].press_seq, "1");

        // 超过去重窗口后再按 → 序号 +1，仍不 held
        rt.keyboard_edge(
            tok,
            InputSourceId(1),
            &dev,
            PAUSE_SCANCODE,
            true,
            t0 + Duration::from_millis(50),
        );
        let snap2 = rt.snapshot(t0 + Duration::from_millis(55), false);
        let d2 = kb_digital(&snap2, "1");
        assert!(d2.held_codes.is_empty());
        assert_eq!(d2.presses[0].press_seq, "2");
    }

    #[test]
    fn stale_press_then_touch_without_snapshot_does_not_relight() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = kb_device("K");
        rt.keyboard_edge(tok, InputSourceId(1), &dev, 0x1E, true, t0);
        // 无中间 snapshot；501ms 后仅 touch
        let t1 = t0 + Duration::from_millis(501);
        rt.touch_raw_input(tok, t1);
        let snap = rt.snapshot(t1 + Duration::from_millis(1), false);
        let d = kb_digital(&snap, "1");
        assert!(
            d.held_codes.is_empty(),
            "stale clear must drop held before touch accepts heartbeat"
        );
        // pressSeq 单调保留，但可播放 pulse 已清
        assert!(d.presses.is_empty());
        // 破坏性断段推进 reset_epoch（初次 set_capture=1，再 stale=+1 → "2"）
        assert_eq!(snap.reset_epoch, "2");
    }

    #[test]
    fn gamepad_segment_recovery_does_not_replay_pulses() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = pad_device();

        // 首次采样：A 按下 → 有 pulse
        rt.gamepad_sample(
            tok,
            LiveGamepadSample {
                slot: 0,
                connection: MotionConnectionId(7),
                device: dev.clone(),
                buttons: XINPUT_A,
                left: StickPoint { x: 0.0, y: 0.0 },
                right: StickPoint { x: 0.0, y: 0.0 },
                left_trigger: 0.0,
                right_trigger: 0.0,
            },
            t0,
        );
        let snap1 = rt.snapshot(t0 + Duration::from_millis(1), false);
        match pad_source(&snap1, 0) {
            LiveSource::Gamepad {
                digital,
                availability,
                unsupported_codes,
                ..
            } => {
                assert_eq!(*availability, LiveSourceAvailability::Ready);
                assert_eq!(digital.held_codes, vec![1]);
                assert_eq!(digital.presses[0].press_seq, "1");
                assert_eq!(unsupported_codes, &vec![GUIDE_CODE]);
            }
            _ => panic!("expected gamepad"),
        }

        // 过期断段（>500ms）
        let t_stale = t0 + Duration::from_millis(501);
        let snap_stale = rt.snapshot(t_stale, false);
        match pad_source(&snap_stale, 0) {
            LiveSource::Gamepad {
                digital,
                availability,
                analog,
                ..
            } => {
                assert_eq!(*availability, LiveSourceAvailability::Unavailable);
                assert!(digital.held_codes.is_empty());
                assert!(digital.presses.is_empty());
                assert!(analog.is_none());
            }
            _ => panic!("expected gamepad"),
        }

        // 恢复时 A 仍按下：只建基线，不回放旧 pulse（press_seq 仍为 1，无新 last_press）
        let t2 = t_stale + Duration::from_millis(10);
        rt.gamepad_sample(
            tok,
            LiveGamepadSample {
                slot: 0,
                connection: MotionConnectionId(7),
                device: dev.clone(),
                buttons: XINPUT_A,
                left: StickPoint { x: 0.1, y: 0.2 },
                right: StickPoint { x: 0.0, y: 0.0 },
                left_trigger: 0.0,
                right_trigger: 0.5,
            },
            t2,
        );
        let snap2 = rt.snapshot(t2 + Duration::from_millis(1), false);
        match pad_source(&snap2, 0) {
            LiveSource::Gamepad {
                digital,
                availability,
                analog,
                ..
            } => {
                assert_eq!(*availability, LiveSourceAvailability::Ready);
                assert_eq!(digital.held_codes, vec![1, 8]); // A + RT(0.5>=0.33)
                // 无新 pulse（基线重建不 note_press）
                assert!(
                    digital.presses.is_empty(),
                    "recovery must not replay press edges; got {:?}",
                    digital.presses
                );
                let a = analog.expect("analog");
                assert!((a.right_trigger - 0.5).abs() < 1e-9);
            }
            _ => panic!("expected gamepad"),
        }

        // 真正新边沿才增序号
        rt.gamepad_sample(
            tok,
            LiveGamepadSample {
                slot: 0,
                connection: MotionConnectionId(7),
                device: dev,
                buttons: XINPUT_A | XINPUT_B,
                left: StickPoint { x: 0.0, y: 0.0 },
                right: StickPoint { x: 0.0, y: 0.0 },
                left_trigger: 0.0,
                right_trigger: 0.0,
            },
            t2 + Duration::from_millis(20),
        );
        let snap3 = rt.snapshot(t2 + Duration::from_millis(25), false);
        match pad_source(&snap3, 0) {
            LiveSource::Gamepad { digital, .. } => {
                assert_eq!(digital.held_codes, vec![1, 2]);
                let b = digital.presses.iter().find(|p| p.code == 2).unwrap();
                assert_eq!(b.press_seq, "1");
            }
            _ => panic!("expected gamepad"),
        }
    }

    #[test]
    fn gamepad_unavailable_disconnect_removes_transient_keeps_identity() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = pad_device();
        rt.gamepad_sample(
            tok,
            LiveGamepadSample {
                slot: 1,
                connection: MotionConnectionId(3),
                device: dev,
                buttons: XINPUT_A,
                left: StickPoint { x: 0.0, y: 0.0 },
                right: StickPoint { x: 0.0, y: 0.0 },
                left_trigger: 0.4,
                right_trigger: 0.0,
            },
            t0,
        );
        rt.gamepad_unavailable(1, false);
        let snap = rt.snapshot(t0 + Duration::from_millis(1), false);
        match pad_source(&snap, 1) {
            LiveSource::Gamepad {
                availability,
                analog,
                digital,
                connection_id,
                ..
            } => {
                assert_eq!(*availability, LiveSourceAvailability::Unavailable);
                assert!(analog.is_none());
                assert!(digital.held_codes.is_empty());
                assert_eq!(connection_id, "3");
            }
            _ => panic!("expected gamepad"),
        }
        // 持续失败不再反复破坏
        rt.gamepad_unavailable(1, false);
        rt.gamepad_unavailable(1, true);
        let snap2 = rt.snapshot(t0 + Duration::from_millis(2), false);
        assert!(snap2
            .sources
            .iter()
            .all(|s| !matches!(s, LiveSource::Gamepad { slot: 1, .. })));
    }

    #[test]
    fn mouse_buttons_held_wheels_pulse_only() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = mouse_device();
        rt.mouse_packet(
            tok,
            InputSourceId(9),
            &dev,
            r"\\?\hid#vid_046d&pid_c077#1",
            LiveMousePacket {
                edges: vec![(1, true), (1, false), (2, true)],
                wheel_codes: vec![6, 7],
            },
            t0,
        );
        let snap = rt.snapshot(t0 + Duration::from_millis(1), false);
        match &snap.sources[0] {
            LiveSource::Mouse {
                source_key,
                digital,
                ..
            } => {
                assert!(source_key.contains("vid_046d"));
                assert_eq!(digital.held_codes, vec![2]);
                // 左键短按留序号；滚轮 pulse
                assert!(digital.presses.iter().any(|p| p.code == 1 && p.press_seq == "1"));
                assert!(digital.presses.iter().any(|p| p.code == 6));
                assert!(digital.presses.iter().any(|p| p.code == 7));
                assert!(!digital.held_codes.contains(&6));
            }
            _ => panic!("expected mouse"),
        }
    }

    #[test]
    fn clear_raw_and_gamepads_drop_sources() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let kb = kb_device("K");
        let pad = pad_device();
        rt.keyboard_edge(tok, InputSourceId(1), &kb, 0x1E, true, t0);
        rt.gamepad_sample(
            tok,
            LiveGamepadSample {
                slot: 0,
                connection: MotionConnectionId(1),
                device: pad,
                buttons: XINPUT_A,
                left: StickPoint { x: 0.0, y: 0.0 },
                right: StickPoint { x: 0.0, y: 0.0 },
                left_trigger: 0.0,
                right_trigger: 0.0,
            },
            t0,
        );
        rt.clear_raw_sources();
        let snap = rt.snapshot(t0 + Duration::from_millis(1), false);
        assert!(snap.sources.iter().all(|s| !matches!(s, LiveSource::Keyboard { .. })));
        assert!(snap.sources.iter().any(|s| matches!(s, LiveSource::Gamepad { .. })));
        rt.clear_gamepads();
        let snap2 = rt.snapshot(t0 + Duration::from_millis(2), false);
        assert!(snap2.sources.is_empty());
    }

    #[test]
    fn statistics_paused_flag_is_passthrough() {
        let rt = LiveInputRuntime::new("s".into());
        let _ = activate(&rt);
        let t0 = Instant::now();
        assert!(rt.snapshot(t0, true).statistics_paused);
        assert!(!rt.snapshot(t0 + Duration::from_millis(1), false).statistics_paused);
    }

    #[test]
    fn capture_token_none_when_suspended() {
        let rt = LiveInputRuntime::new("s".into());
        rt.set_capture_state(LiveCaptureState::Suspended);
        assert!(rt.capture_token().is_none());
        rt.set_capture_state(LiveCaptureState::Active);
        assert!(rt.capture_token().is_some());
    }

    #[test]
    fn trigger_digital_gate_uses_033_analog_keeps_raw() {
        let rt = LiveInputRuntime::new("s".into());
        let tok = activate(&rt);
        let t0 = Instant::now();
        let dev = pad_device();
        rt.gamepad_sample(
            tok,
            LiveGamepadSample {
                slot: 0,
                connection: MotionConnectionId(1),
                device: dev,
                buttons: 0,
                left: StickPoint { x: 0.0, y: 1.0 },
                right: StickPoint { x: 0.0, y: 0.0 },
                left_trigger: 0.32,
                right_trigger: 0.33,
            },
            t0,
        );
        let snap = rt.snapshot(t0, false);
        match pad_source(&snap, 0) {
            LiveSource::Gamepad { digital, analog, .. } => {
                assert!(!digital.held_codes.contains(&(GamepadButton::LeftTrigger2 as u16)));
                assert!(digital.held_codes.contains(&(GamepadButton::RightTrigger2 as u16)));
                let a = analog.unwrap();
                assert!((a.left_trigger - 0.32).abs() < 1e-9);
                assert!((a.right_trigger - 0.33).abs() < 1e-9);
                assert!((a.left_stick.y - 1.0).abs() < 1e-9);
            }
            _ => panic!("expected gamepad"),
        }
    }
}
