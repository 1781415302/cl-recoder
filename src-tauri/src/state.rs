//! state —— GUI 全局状态（PLAN §3：`AppState{ ro_conn: Mutex<Connection>, settings: RwLock<Settings> }`）。
//!
//! - `ro_conn`：统计库只读连接（§2.4）；stats.db 不存在（采集器从未运行）时用内存兜底连接，
//!   查询得到空结果即 §5.1 的"引导态"；`with_ro` 在兜底模式下每次尝试重新连接真实库，
//!   collector 建库后 GUI 无需重启即可看到数据。
//! - `settings`：settings.json 的内存镜像（§6），结构体字段即文件 JSON 字段
//!   （`gui_autostart`/`wp_db_path`/`first_run_done`，snake_case）。
//!
//! 两字段包 `Arc` 仅为了把状态廉价克隆进 `spawn_blocking`（§4.7 全部 async command），
//! 字段名与 PLAN §3 逐字一致。

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use rusqlite::Connection;

use crate::db;
use clrecoder_store as store;

/// settings.json 的持久化形状（PLAN §6 逐字：`{ "gui_autostart": bool, "wp_db_path": string|null, "first_run_done": bool }`）。
///
/// 文件字段名 = Rust 字段名（serde 默认，不加 camelCase——camelCase 只约束 GUI 对前端的 DTO，§4.7 注）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    /// GUI 开机自启（HKCU Run，tauri-plugin-autostart 落实，§4.7）
    pub gui_autostart: bool,
    /// WhatPulse 库路径覆盖（`%LOCALAPPDATA%\WhatPulse\whatpulse.db` 的默认探测值；null=用默认）
    pub wp_db_path: Option<String>,
    /// 首启引导是否已完成
    pub first_run_done: bool,
}

impl Settings {
    /// 从指定路径读取；文件缺失/损坏 → 默认值重建（PLAN §6："损坏/缺失→默认值重建"）。
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                crate::gui_log!("WARN: settings.json 损坏（{e}），使用默认值重建");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// 写入指定路径（§6 的唯一持久化时机：set_settings 落盘）。
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

/// GUI 全局状态（PLAN §3 逐字字段名；Arc 包装用于跨 `spawn_blocking` 共享）。
#[derive(Clone)]
pub struct AppState {
    /// 统计库只读连接（stats.db 不在场时为内存兜底连接，见 [`Self::with_ro`]）
    pub ro_conn: Arc<Mutex<Connection>>,
    /// settings.json 内存镜像（§6）
    pub settings: Arc<RwLock<Settings>>,
}

impl AppState {
    /// 构造：打开 ro 连接（失败→内存兜底）+ 加载设置（损坏/缺失→默认值）。
    ///
    /// 本函数**不失败**：§5.1 要求"DB 不存在/短暂 BUSY → 命令返回空数据 + 引导态"，
    /// 而不是 GUI 启动失败。
    #[must_use]
    pub fn new() -> Self {
        let stats = db::stats_db_path();
        let conn = db::open_ro(&stats).unwrap_or_else(|e| {
            crate::gui_log!("WARN: stats.db 不可用（{e}），使用内存兜底连接（引导态空数据）");
            Connection::open_in_memory().expect("内存连接不可能打开失败")
        });
        let settings = Settings::load_from(&db::settings_path());
        Self { ro_conn: Arc::new(Mutex::new(conn)), settings: Arc::new(RwLock::new(settings)) }
    }

    /// 取 ro 连接。毒化互斥量恢复为可用——GUI 查询线程 panic 不应让后续查询永久失败。
    fn lock_ro(&self) -> MutexGuard<'_, Connection> {
        self.ro_conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 当前连接是否为内存兜底（stats.db 尚不可用的引导态）。
    /// `sqlite3_db_filename` 对 `:memory:` 连接可能返回空串而非 ":memory:"，两者都视为兜底。
    fn is_fallback_conn(conn: &Connection) -> bool {
        match conn.path() {
            None => true,
            Some(p) => p.is_empty() || p == ":memory:",
        }
    }

    /// 在只读连接上执行 `f`；兜底模式下先尝试重连真实库（collector 建库后自动恢复）。
    ///
    /// 错误交调用方按 §5.3 映射为空数据（见 `commands::swallow`）。
    pub fn with_ro<T>(
        &self,
        f: impl FnOnce(&Connection) -> store::Result<T>,
    ) -> store::Result<T> {
        let mut guard = self.lock_ro();
        if Self::is_fallback_conn(&guard) {
            // 仍不可用：继续用兜底连接跑 f（空表 → 空结果/错误，均由上层映射为空数据）
            if let Ok(real) = db::open_ro(&db::stats_db_path()) {
                crate::gui_log!("INFO: stats.db 已出现，从内存兜底连接切换到真实只读连接");
                *guard = real;
            }
        }
        f(&guard)
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    //! GUI 单测临时文件工具（与 store::testutil 同策略：进程号 + 标签唯一，Drop 清理）。
    //! 依赖白名单无 tempfile。

    use std::ops::Deref;
    use std::path::{Path, PathBuf};

    /// 唯一临时路径（不预先创建）。
    pub(crate) fn temp_path(tag: &str, ext: &str) -> PathBuf {
        std::env::temp_dir().join(format!("clrecoder-gui-{}-{tag}.{ext}", std::process::id()))
    }

    /// 持有临时库路径，`Drop` 时删除库文件与 WAL 旁路文件（断言失败也不留垃圾）。
    pub(crate) struct TempFile(pub PathBuf);

    impl TempFile {
        pub(crate) fn new(tag: &str, ext: &str) -> Self {
            let p = temp_path(tag, ext);
            remove_all(&p);
            Self(p)
        }
    }

    impl Deref for TempFile {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    impl AsRef<Path> for TempFile {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    /// 清理库文件与 `-wal`/`-shm` 旁路文件。
    pub(crate) fn remove_all(p: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = p.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(&name);
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            remove_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::testutil::TempFile;

    #[test]
    fn settings_roundtrip_matches_plan_json_shape() {
        let f = TempFile::new("settings-rt", "json");
        let s = Settings {
            gui_autostart: true,
            wp_db_path: Some(r"C:\Users\x\WhatPulse\whatpulse.db".into()),
            first_run_done: true,
        };
        s.save_to(&f).unwrap();
        // §6 文件形状：snake_case 字段名
        let text = std::fs::read_to_string(&f).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v.get("gui_autostart").is_some(), "settings.json 必须用 snake_case（§6）: {text}");
        assert!(v.get("wp_db_path").is_some());
        assert!(v.get("first_run_done").is_some());
        // 往返
        assert_eq!(Settings::load_from(&f), s);
    }

    #[test]
    fn settings_missing_or_corrupt_falls_back_to_default() {
        let f = TempFile::new("settings-bad", "json");
        assert_eq!(Settings::load_from(&f), Settings::default(), "缺失 → 默认值");
        std::fs::write(&f, "{ 不是 json").unwrap();
        assert_eq!(Settings::load_from(&f), Settings::default(), "损坏 → 默认值（§6）");
    }

    #[test]
    fn app_state_fallback_conn_returns_empty_semantics() {
        // stats.db 不在场：AppState 构造不失败，with_ro 在内存兜底连接上跑出
        // "no such table" 类错误——上层据此映射空数据（§5.1 引导态）。
        let st = AppState::new();
        assert!(AppState::is_fallback_conn(&st.lock_ro()), "测试环境无 stats.db，应为兜底连接");
        let r: store::Result<usize> =
            st.with_ro(|c| Ok(c.query_row("SELECT COUNT(*) FROM devices", [], |r| r.get::<_, i64>(0))? as usize));
        assert!(r.is_err(), "兜底连接未迁移 schema，查询应失败并由上层映射为空");
    }

    #[test]
    fn app_state_with_ro_on_real_db() {
        // 真实库在场：with_ro 直连文件并跑通 reader（覆盖"reader 查询"验收点的一部分）
        let f = TempFile::new("state-real", "db");
        let w = store::writer::Writer::open(f.as_ref()).unwrap();
        let dev = w
            .get_or_create_device(&clrecoder_core::event::DeviceKey {
                kind: clrecoder_core::codes::DeviceKind::Keyboard,
                vid: 0x04D9,
                pid: 0x0169,
                name: "测试键盘".into(),
            })
            .unwrap();
        w.flush(&store::writer::FlushBatch {
            input: vec![(dev, "2026-09-28".into(), 0x1E, 3)],
            ..Default::default()
        })
        .unwrap();
        drop(w);

        // 直接以该库为 ro 连接构造 AppState（绕过全局路径，便于测试）
        let conn = db::open_ro(f.as_ref()).unwrap();
        let st = AppState {
            ro_conn: Arc::new(Mutex::new(conn)),
            settings: Arc::new(RwLock::new(Settings::default())),
        };
        let rows = st
            .with_ro(|c| store::reader::daily_totals(c, "2026-09-01", "2026-09-30", None))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].day.as_str(), rows[0].total), ("2026-09-28", 3));
    }
}
