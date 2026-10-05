//! 直接 XInput 完整四轴采样（motion-dpi §4.3 手柄帧）——与 gilrs 按钮事件隔离的运动帧源。
//!
//! 职责边界（§2 模块表 gamepad/xinput_motion 行）：原 gilrs 按钮逻辑保留（见
//! [`crate::gamepad`]），本模块**只**做摇杆运动帧：每 ≥[`XINPUT_SAMPLE_INTERVAL`] 直接
//! 调用 `XInputGetState` 采样 slot0..=3，一次 `XINPUT_STATE` 快照已含四轴、同一帧读取
//! ——不逐 `AxisChanged` 算行程，不读取、不改变 gilrs 已 deadzone 过滤的轴缓存与 filter。
//!
//! 关键语义（§4.3 锁定）：
//! - 归一化：正 i16 轴除 32767、负值除 32768（[`axis_to_f64`]）；XInput 轴 Y 正向上，
//!   无需翻转；径向规范化与活动迟滞由 tracker（[`clrecoder_engine::stick_motion`]）处理；
//! - 无变化也采样：保持帧持续产生停留热度（§4.4"只有保持帧产生的热度也必须落库"）；
//! - 设备身份：固定复用本 backend 的 [`crate::gamepad::device_key`]("Xbox Controller")
//!   ——与既有型号行一致（gilrs xinput 后端 name 恒定、vid/pid 恒 0）；
//! - 连接代际：按原生 slot 独立分配（不假定 GamepadId 等于 slot）。首次成功采样分配；
//!   `ERROR_DEVICE_NOT_CONNECTED` 断连清槽、重连重新分配新值；其他读取失败跳帧并
//!   reset（经 `GamepadMotionDisconnected` 通知 aggregator 复位 tracker，槽位连接
//!   保留——瞬时故障不换代际）；轮询上下文重建（sampler 随线程重建）自然换代际；
//! - 断连绝不合成立即回中的运动帧（不伪造中立路程）。
//!
//! 采样侧不做暂停判定：帧携带捕获时刻的 [`MotionControlSnapshot`]（调用方经
//! [`MotionRuntime`] 成对取得 stamp/control）；喂入或 reset/跳过的决策在 aggregator
//! （§4.3.1），采集侧只保证快照与帧一一对应。
//!
//! 可测性：[`XInputMotionSampler::sample`] 的槽位读取经 [`SlotRead`] 抽象，单测注入
//! 读取序列驱动全部状态迁移（fixture 禁真实 HID——生产路径 [`poll_slot`] 不被测试触达）。

use std::time::Duration;

use clrecoder_core::event::{AggEvent, DeviceKey};
use clrecoder_core::motion::{
    GamepadMotionFrame, MotionConnectionId, MotionControlSnapshot, MotionStamp, StickPoint,
};
use windows::Win32::Foundation::{ERROR_DEVICE_NOT_CONNECTED, ERROR_SUCCESS};
use windows::Win32::UI::Input::XboxController::{XInputGetState, XINPUT_GAMEPAD, XINPUT_STATE};

use crate::gamepad::device_key;
use crate::motion_runtime::MotionRuntime;

/// XInput 采样周期下限（§4.3：每 ≥20ms 直接调用 XInputGetState；调用方按此节流）。
pub const XINPUT_SAMPLE_INTERVAL: Duration = Duration::from_millis(20);

/// XInput 原生槽位数（slot0..=3，XUSER_MAX_COUNT）。
const SLOT_COUNT: usize = 4;

/// 单个槽位的运动连接状态（§4.3 设计声明：slot0..3 连接代际）。
///
/// `None` = 当前无活动连接（从未成功采样或已断连清槽）；`Some` 携带该连接的代际与
/// 设备身份（固定 "Xbox Controller" 型号行）。
#[derive(Debug)]
struct SlotMotion {
    connection: MotionConnectionId,
    device: DeviceKey,
}

/// 直接 XInput 摇杆采样器：逐 slot 轮询完整四轴并产出运动事件。
///
/// 一个实例服务一条采样循环；线程重建（panic 重启）即新实例——连接代际随之换代际
/// （§4.3"轮询上下文重建也换代际"）。
pub struct XInputMotionSampler {
    slots: [Option<SlotMotion>; SLOT_COUNT],
}

impl Default for XInputMotionSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// 单槽读取结果（注入点抽象；生产实现 [`poll_slot`] 即一次 `XInputGetState`）。
#[derive(Clone)]
pub(crate) enum SlotRead {
    /// 读取成功：**同一次** `XINPUT_STATE` 快照的四轴原始值（左右摇杆同帧）。
    Connected {
        /// 左摇杆 X 原始值
        lx: i16,
        /// 左摇杆 Y 原始值（正向上）
        ly: i16,
        /// 右摇杆 X 原始值
        rx: i16,
        /// 右摇杆 Y 原始值（正向上）
        ry: i16,
    },
    /// `ERROR_DEVICE_NOT_CONNECTED`：槽位无手柄（断连清锚点，重连换代际）。
    NotConnected,
    /// 其他读取失败：跳帧并 reset（瞬时故障，连接代际保留）。
    Failed,
}

impl XInputMotionSampler {
    /// 创建空采样器：全部槽位无连接。
    #[must_use]
    pub fn new() -> Self {
        Self { slots: [None, None, None, None] }
    }

    /// 采样一轮（§4.3 签名）：逐 slot 直接调用 `XInputGetState`，产出运动事件
    /// （`AggEvent::GamepadMotion` / `AggEvent::GamepadMotionDisconnected`）。
    ///
    /// `stamp` / `control` 由调用方在同一采样时刻经 [`MotionRuntime`] 成对取得；
    /// 连接代际分配经 `motion`（进程内单调）。无变化的槽位也产出帧（保持帧计停留）。
    pub fn sample(
        &mut self,
        stamp: MotionStamp,
        control: MotionControlSnapshot,
        motion: &MotionRuntime,
    ) -> Vec<AggEvent> {
        self.sample_with(stamp, control, motion, poll_slot)
    }

    /// [`Self::sample`] 的注入版（单测；生产固定走 [`poll_slot`]）。
    /// `poll` 对每个槽位恰好调用一次——帧内左右摇杆必来自同一次快照。
    fn sample_with(
        &mut self,
        stamp: MotionStamp,
        control: MotionControlSnapshot,
        motion: &MotionRuntime,
        mut poll: impl FnMut(u32) -> SlotRead,
    ) -> Vec<AggEvent> {
        let mut events = Vec::new();
        for slot in 0..u32::try_from(SLOT_COUNT).expect("槽位数固定为 4") {
            let idx = slot as usize;
            match poll(slot) {
                SlotRead::Connected { lx, ly, rx, ry } => {
                    // 首次成功（或断连重连/上下文重建后）分配新连接代际；既有连接复用。
                    if self.slots[idx].is_none() {
                        let connection = motion.allocate_connection();
                        self.slots[idx] =
                            Some(SlotMotion { connection, device: xinput_device_key() });
                    }
                    let slot_state = self.slots[idx].as_ref().expect("上方已确保槽位存在");
                    events.push(AggEvent::GamepadMotion(frame_from_axes(
                        slot_state.device.clone(),
                        slot_state.connection,
                        stamp,
                        control,
                        lx,
                        ly,
                        rx,
                        ry,
                    )));
                }
                SlotRead::NotConnected => {
                    // 断连清锚点：清槽并通知 aggregator 复位该连接 tracker；绝不合成
                    // 立即回中的运动帧。重连（下次成功）时重新分配新代际。
                    if let Some(s) = self.slots[idx].take() {
                        events.push(AggEvent::GamepadMotionDisconnected {
                            connection: s.connection,
                        });
                    }
                }
                SlotRead::Failed => {
                    // 其他读取失败：跳帧并 reset（aggregator 复位 tracker，下帧重锚）。
                    // 瞬时故障不换代际——槽位连接保留，下次成功同连接继续。
                    if let Some(s) = &self.slots[idx] {
                        events.push(AggEvent::GamepadMotionDisconnected {
                            connection: s.connection,
                        });
                    }
                }
            }
        }
        events
    }
}

/// 由**同一次** `XINPUT_STATE` 快照的四轴原始值构造摇杆帧（纯函数）。
///
/// 四轴一次读取、按名映射——X/Y 值的提取顺序不影响结果（§4.3"同一帧读取"）。
/// （位置参数为刻意保留：验收用例以"上报反序"逐位反传同组值断言帧一致。）
#[allow(clippy::too_many_arguments)]
fn frame_from_axes(
    device: DeviceKey,
    connection: MotionConnectionId,
    stamp: MotionStamp,
    control: MotionControlSnapshot,
    lx: i16,
    ly: i16,
    rx: i16,
    ry: i16,
) -> GamepadMotionFrame {
    GamepadMotionFrame {
        device,
        connection,
        stamp,
        left: StickPoint { x: axis_to_f64(lx), y: axis_to_f64(ly) },
        right: StickPoint { x: axis_to_f64(rx), y: axis_to_f64(ry) },
        control,
    }
}

/// XInput 轴原始 i16 → [-1, 1]（§4.3：正值除 32767、负值除 32768；XInput 轴 Y 正向上）。
fn axis_to_f64(v: i16) -> f64 {
    if v >= 0 {
        f64::from(v) / 32_767.0
    } else {
        f64::from(v) / 32_768.0
    }
}

/// 本 backend 的手柄 DeviceKey：固定 `"Xbox Controller"`——与 gilrs 按钮事件的既有
/// 型号行一致（运动结果归既有 deviceId）。
fn xinput_device_key() -> DeviceKey {
    device_key("Xbox Controller")
}

/// 生产读取：一次 `XInputGetState`。成功映射四轴；`ERROR_DEVICE_NOT_CONNECTED` 映射
/// 断连；其他错误码映射瞬时失败（跳帧并 reset）。
fn poll_slot(slot: u32) -> SlotRead {
    let mut state = XINPUT_STATE::default();
    // SAFETY: state 为本函数栈上的有效指针；XInputGetState 同步调用、仅写入该结构。
    let code = unsafe { XInputGetState(slot, &mut state) };
    if code == ERROR_SUCCESS.0 {
        // SAFETY: state 已被 XInputGetState 成功写入，Gamepad 成员完整有效。
        let gamepad: &XINPUT_GAMEPAD = &state.Gamepad;
        SlotRead::Connected {
            lx: gamepad.sThumbLX,
            ly: gamepad.sThumbLY,
            rx: gamepad.sThumbRX,
            ry: gamepad.sThumbRY,
        }
    } else if code == ERROR_DEVICE_NOT_CONNECTED.0 {
        SlotRead::NotConnected
    } else {
        SlotRead::Failed
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use clrecoder_core::codes::DeviceKind;

    use crate::ipc_server::Flags;
    use crate::motion_runtime::MotionRuntime;

    use super::*;

    /// 测试基准时刻：2026-06-01T00:00:00Z（引擎测试同款，只作相对时间）。
    const BASE_UNIX_US: i64 = 1_780_272_000_000_000;

    fn stamp(mono_us: u64) -> MotionStamp {
        MotionStamp { mono_us, unix_us: BASE_UNIX_US + mono_us as i64 }
    }

    fn offline_runtime() -> Arc<MotionRuntime> {
        MotionRuntime::offline(Arc::new(Flags::default()))
    }

    fn connected(lx: i16, ly: i16, rx: i16, ry: i16) -> SlotRead {
        SlotRead::Connected { lx, ly, rx, ry }
    }

    /// 注入式采样：`reads[slot]` 为每次采样该槽位的读取序列（按次弹出，耗尽后重复末项）。
    fn sample_with_sequence(
        sampler: &mut XInputMotionSampler,
        motion: &MotionRuntime,
        reads: &mut [Vec<SlotRead>; SLOT_COUNT],
    ) -> Vec<AggEvent> {
        sampler.sample_with(stamp(0), MotionControlSnapshot::default(), motion, |slot| {
            let seq = &mut reads[slot as usize];
            if seq.len() > 1 {
                seq.remove(0)
            } else {
                seq.first().cloned().unwrap_or(SlotRead::NotConnected)
            }
        })
    }

    // ------------------------------------------------------------------
    // 连接代际 / 单快照 / 设备身份
    // ------------------------------------------------------------------

    /// 首次成功分配连接代际；一轮采样对每个槽位恰好一次读取（同帧单快照）；设备身份
    /// 固定复用本 backend 的 "Xbox Controller" 型号行。
    #[test]
    fn motion_dpi_xinput_single_snapshot_per_frame_and_backend_device_key() {
        let motion = offline_runtime();
        let mut sampler = XInputMotionSampler::new();
        let mut polls = 0usize;
        let events = sampler.sample_with(stamp(0), MotionControlSnapshot::default(), &motion, |_| {
            polls += 1;
            connected(16_384, -16_384, 32_767, 0)
        });
        assert_eq!(polls, SLOT_COUNT, "一轮采样必须恰好读取 slot0..=3 各一次");
        assert_eq!(events.len(), 4, "四个已连接槽位各产一帧");
        for ev in &events {
            let AggEvent::GamepadMotion(frame) = ev else {
                panic!("已连接槽位应产 GamepadMotion 帧: {ev:?}");
            };
            assert_eq!(
                frame.device,
                DeviceKey {
                    kind: DeviceKind::Gamepad,
                    vid: 0,
                    pid: 0,
                    name: "Xbox Controller".to_string(),
                },
                "设备身份必须与 gilrs 按钮事件的既有型号行一致"
            );
            assert!(frame.connection.0 >= 1, "连接代际非零");
            assert_eq!(frame.control, MotionControlSnapshot::default());
            assert_eq!(frame.stamp, stamp(0));
        }
        // 各槽位连接代际相互独立（进程内单调分配，不共享）
        let conns: Vec<u64> = events
            .iter()
            .map(|ev| match ev {
                AggEvent::GamepadMotion(f) => f.connection.0,
                _ => unreachable!(),
            })
            .collect();
        assert!(conns.windows(2).all(|w| w[0] < w[1]), "代际必须互不相同且单调: {conns:?}");
    }

    /// 同一次快照构造帧：X/Y 提取反序后按名归位，帧逐字段一致（§4.3"同一帧读取"）；
    /// 归一化：正值 /32767、负值 /32768、Y 正向上。
    #[test]
    fn motion_dpi_xinput_frame_axis_order_invariant_and_normalization() {
        let dev = xinput_device_key();
        let conn = MotionConnectionId(3);
        let st = stamp(1_000);
        let control = MotionControlSnapshot { epoch: 1, paused: false };
        let direct =
            frame_from_axes(dev.clone(), conn, st, control, 16_384, -16_384, 32_767, -32_768);
        // "上报反序"：同一快照按 ry→rx→ly→lx 顺序提取，再按名归位——结果必须一致
        let (ry, rx, ly, lx) = (-32_768i16, 32_767i16, -16_384i16, 16_384i16);
        let reversed = frame_from_axes(dev, conn, st, control, lx, ly, rx, ry);
        assert_eq!(direct, reversed);
        // 归一化数值：左 (16384/32767, -16384/32768)、右 (1.0, -1.0)
        assert!((direct.left.x - 16_384.0 / 32_767.0).abs() < 1e-12);
        assert!(
            (direct.left.y + 16_384.0 / 32_768.0).abs() < 1e-12,
            "负值除 32768，值本身即正向上"
        );
        assert!((direct.right.x - 1.0).abs() < 1e-12, "+32767 → 1.0");
        assert!((direct.right.y + 1.0).abs() < 1e-12, "-32768 → -1.0");
        // 边界：+1 → 1/32767、-1 → -1/32768（正负分母不对称）
        assert!((axis_to_f64(1) - 1.0 / 32_767.0).abs() < 1e-15);
        assert!((axis_to_f64(-1) + 1.0 / 32_768.0).abs() < 1e-15);
        assert_eq!(axis_to_f64(0), 0.0);
    }

    /// `ERROR_DEVICE_NOT_CONNECTED` 断连清锚点：仅发 `GamepadMotionDisconnected`
    /// （绝不合成立即回中的运动帧）；重连后重新分配新连接代际。
    #[test]
    fn motion_dpi_xinput_disconnect_clears_slot_and_reconnect_gets_new_generation() {
        let motion = offline_runtime();
        let mut sampler = XInputMotionSampler::new();
        let mut reads: [Vec<SlotRead>; SLOT_COUNT] = [
            vec![connected(32_767, 0, 0, 0), SlotRead::NotConnected, connected(0, 32_767, 0, 0)],
            [SlotRead::NotConnected].into(),
            [SlotRead::NotConnected].into(),
            [SlotRead::NotConnected].into(),
        ];
        // 第 1 轮：slot0 连接，分配代际 A
        let first = sample_with_sequence(&mut sampler, &motion, &mut reads);
        let AggEvent::GamepadMotion(f1) = &first[0] else { panic!("{first:?}") };
        let conn_a = f1.connection;
        // 第 2 轮：断连——只有 Disconnected 事件，无任何合成运动帧
        let second = sample_with_sequence(&mut sampler, &motion, &mut reads);
        assert_eq!(
            second
                .iter()
                .filter(|ev| matches!(ev, AggEvent::GamepadMotion(_)))
                .count(),
            0,
            "断连不得合成回中立路程帧: {second:?}"
        );
        assert!(matches!(
            second.first(),
            Some(AggEvent::GamepadMotionDisconnected { connection }) if *connection == conn_a
        ));
        // 第 3 轮：重连——新连接代际（断连重连生成新值）
        let third = sample_with_sequence(&mut sampler, &motion, &mut reads);
        let AggEvent::GamepadMotion(f2) = &third[0] else { panic!("{third:?}") };
        assert_ne!(f2.connection, conn_a, "重连必须换代际");
    }

    /// 其他读取失败：跳帧并 reset（发 Disconnected 复位 tracker），瞬时故障不换代际
    /// ——下次成功同连接重锚。
    #[test]
    fn motion_dpi_xinput_failed_read_skips_frame_and_keeps_generation() {
        let motion = offline_runtime();
        let mut sampler = XInputMotionSampler::new();
        let mut reads: [Vec<SlotRead>; SLOT_COUNT] = [
            vec![connected(0, 0, 32_767, 0), SlotRead::Failed, connected(0, 0, 32_767, 0)],
            [SlotRead::NotConnected].into(),
            [SlotRead::NotConnected].into(),
            [SlotRead::NotConnected].into(),
        ];
        let first = sample_with_sequence(&mut sampler, &motion, &mut reads);
        let AggEvent::GamepadMotion(f1) = &first[0] else { panic!("{first:?}") };
        let conn = f1.connection;
        // 失败轮：跳帧（无 GamepadMotion），仅 Disconnected 复位信号
        let second = sample_with_sequence(&mut sampler, &motion, &mut reads);
        assert_eq!(
            second
                .iter()
                .filter(|ev| matches!(ev, AggEvent::GamepadMotion(_)))
                .count(),
            0,
            "读取失败必须跳帧: {second:?}"
        );
        assert!(matches!(
            second.first(),
            Some(AggEvent::GamepadMotionDisconnected { connection }) if *connection == conn
        ));
        // 恢复轮：同连接（瞬时故障不换代际）
        let third = sample_with_sequence(&mut sampler, &motion, &mut reads);
        let AggEvent::GamepadMotion(f2) = &third[0] else { panic!("{third:?}") };
        assert_eq!(f2.connection, conn, "瞬时失败不得换代际");
    }

    /// 无变化也采样（§4.3）：相同快照连续采样各产一帧——保持帧由此持续产生停留热度。
    #[test]
    fn motion_dpi_xinput_static_stick_still_samples_every_round() {
        let motion = offline_runtime();
        let mut sampler = XInputMotionSampler::new();
        let mut reads: [Vec<SlotRead>; SLOT_COUNT] =
            [[connected(8_192, 0, 0, 0)].into(), [SlotRead::NotConnected].into(), [SlotRead::NotConnected].into(), [SlotRead::NotConnected].into()];
        let first = sample_with_sequence(&mut sampler, &motion, &mut reads);
        let second = sample_with_sequence(&mut sampler, &motion, &mut reads);
        let third = sample_with_sequence(&mut sampler, &motion, &mut reads);
        for round in [first, second, third] {
            assert_eq!(round.len(), 1, "无变化也必须产出帧: {round:?}");
            assert!(matches!(round[0], AggEvent::GamepadMotion(_)));
        }
    }
}
