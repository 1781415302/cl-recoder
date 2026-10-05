//! clrecoder-core —— 跨进程共享契约 crate（PLAN §2.2/§4）。
//!
//! 纯度要求（S2 验收点）：本 crate **禁止**依赖 windows/tauri/rusqlite 任何一边的专有 API，
//! 禁止包含任何 IO。跨 crate 协作只认 PLAN §4 契约，实现由 S2 填充。
//!
//! 模块规划：
//! - [`codes`]：三种外设的 code 空间 + 归一化规则（§4.1，全系统最重要契约）
//! - [`event`]：采集层 → aggregator 的事件语言（§4.2）
//! - [`ipc`]：GUI ↔ collector 控制协议（§4.4）
//! - [`motion`]：运动共享类型与日历助手（motion-dpi §4.1；AggEvent 变体接入属 S4/S5）
//! - [`qtkeys`]：WhatPulse Qt 键码 → 显示名映射（§4.8）
//! - [`day`]：日期工具（一律 `YYYY-MM-DD` 本地时区字符串）

pub mod codes;
pub mod day;
pub mod event;
pub mod ipc;
pub mod motion;
pub mod qtkeys;
