//! clrecoder-store —— SQLite schema/迁移、writer/reader 全部 SQL 的唯一归属（PLAN §2.2/§4.5）。
//!
//! - [`schema`]：`migrate(conn)` 幂等执行 schema v1 完整 DDL（§4.5 逐字对齐），`schema_migrations` 表记录版本；
//! - [`writer`]：collector 唯一写入口 `Writer::{open, get_or_create_device, flush(FlushBatch),
//!   rebuild_wp_tables(WpImportBatch)}`（单事务批量 upsert，失败保留聚合桶重试、绝不 panic）；
//! - [`reader`]：GUI 全部查询 SQL 的唯一归属（§4.5 各 query_* 函数与行类型）。
//!
//! 约束：不含业务统计逻辑；GUI 对自有统计表只读（仅导入写 wp_* 表）。
//! 内部行类型不做 camelCase——GUI DTO 的 `#[serde(rename_all="camelCase")]` 映射在 src-tauri 侧完成（§4.7）。

pub mod reader;
pub mod schema;
pub mod writer;

use clrecoder_core::codes::DeviceKind;

/// store 统一错误类型（错误类型细化属 PLAN §9.2 executor 自主项）。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// 底层 SQLite 错误（约束冲突/锁超时/库损坏等）。
    #[error("SQLite 错误: {0}")]
    Db(#[from] rusqlite::Error),
    /// 建目录/文件系统错误（`Writer::open` 建父目录时）。
    #[error("文件系统错误: {0}")]
    Io(#[from] std::io::Error),
    /// u64 计数超出 SQLite INTEGER（i64）值域——现实中不可能出现，防御性报错。
    #[error("计数超出 SQLite 整数范围: {0}")]
    CountOverflow(u64),
    /// 无法启用 WAL 模式（两进程并发读写架构依赖 WAL，PLAN §9.1-1；只读连接/异常文件系统会走到这里）。
    #[error("无法启用 WAL 模式（journal_mode={0}）")]
    WalUnavailable(String),
    /// `devices.kind` 列出现契约外的值（库损坏或被外部改写）。
    #[error("未知设备种类: {0}")]
    UnknownDeviceKind(String),
}

/// store 统一 Result 别名。
pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// `DeviceKind` → `devices.kind` 列文本（DDL CHECK 约束值 `'keyboard'|'mouse'|'gamepad'`，
/// 与 §4.1 serde 小写序列化逐字一致，由单测保证两侧不漂移）。
pub(crate) fn kind_to_text(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Keyboard => "keyboard",
        DeviceKind::Mouse => "mouse",
        DeviceKind::Gamepad => "gamepad",
    }
}

/// `devices.kind` 列文本 → `DeviceKind`；契约外值报错（库损坏防御）。
pub(crate) fn kind_from_text(s: &str) -> Result<DeviceKind> {
    match s {
        "keyboard" => Ok(DeviceKind::Keyboard),
        "mouse" => Ok(DeviceKind::Mouse),
        "gamepad" => Ok(DeviceKind::Gamepad),
        other => Err(StoreError::UnknownDeviceKind(other.to_string())),
    }
}

/// u64 计数 → SQLite INTEGER。超出 i64 值域时报 [`StoreError::CountOverflow`]。
pub(crate) fn count_to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StoreError::CountOverflow(v))
}

/// SQLite INTEGER → u64 计数。负数（损坏行）按 0 处理——绝不 crash、绝不污染统计（PLAN §1 原则 3）。
pub(crate) fn i64_to_count(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// SQLite INTEGER → u16（`vid`/`pid`/`code`）。越界（损坏行）按 0（未知）处理。
pub(crate) fn i64_to_u16(v: i64) -> u16 {
    u16::try_from(v).unwrap_or(0)
}

/// SQLite INTEGER → u8（`combo_daily.mods` 位掩码）。越界（损坏行）按 0 处理。
pub(crate) fn i64_to_u8(v: i64) -> u8 {
    u8::try_from(v).unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod testutil {
    //! 单测用临时库工具（PLAN §8-S4：单测用临时库）。
    //! 依赖白名单不含 tempfile——用 `%TEMP%` 下"进程号+标签"唯一路径，`Drop` 时尽力清理。

    use std::ops::Deref;
    use std::path::{Path, PathBuf};

    /// 清理库文件与 WAL 旁路文件（`-wal`/`-shm`）。
    fn remove_db_files(p: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = p.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(&name);
        }
    }

    /// 每个测试独占的临时库路径（进程号 + 标签区分，先清理可能残留的旧文件）。
    pub(crate) fn temp_db_path(tag: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("clrecoder-store-{}-{tag}.db", std::process::id()));
        remove_db_files(&p);
        p
    }

    /// 持有临时库路径，`Drop` 时删除库文件与 WAL 旁路文件（断言失败也不留垃圾）。
    pub(crate) struct TempDb(pub PathBuf);

    impl TempDb {
        pub(crate) fn new(tag: &str) -> Self {
            Self(temp_db_path(tag))
        }
    }

    impl Deref for TempDb {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    impl AsRef<Path> for TempDb {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            remove_db_files(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clrecoder_core::codes::DeviceKind;

    /// `kind_to_text` 必须与 §4.1 serde 小写序列化逐字一致（DDL CHECK 值与 serde 契约不漂移）。
    #[test]
    fn kind_text_matches_serde_lowercase() {
        for k in [DeviceKind::Keyboard, DeviceKind::Mouse, DeviceKind::Gamepad] {
            let text = kind_to_text(k);
            let quoted = format!("\"{text}\"");
            assert_eq!(serde_json::to_string(&k).unwrap(), quoted);
            assert_eq!(kind_from_text(text).unwrap(), k);
        }
        // 契约外值拒绝（大小写敏感，与 DDL CHECK 同语义）
        assert!(kind_from_text("joystick").is_err());
        assert!(kind_from_text("").is_err());
        assert!(kind_from_text("Keyboard").is_err());
    }

    #[test]
    fn count_conversions_defensive() {
        assert_eq!(count_to_i64(0).unwrap(), 0);
        assert_eq!(count_to_i64(123).unwrap(), 123);
        assert!(matches!(count_to_i64(u64::MAX), Err(StoreError::CountOverflow(_))));
        assert_eq!(i64_to_count(42), 42);
        assert_eq!(i64_to_count(-1), 0); // 损坏负数按 0，绝不 crash
        assert_eq!(i64_to_count(i64::MAX), i64::MAX as u64);
        assert_eq!(i64_to_u16(0x04D9), 0x04D9);
        assert_eq!(i64_to_u16(70_000), 0); // 越界按未知
        assert_eq!(i64_to_u16(-5), 0);
        assert_eq!(i64_to_u8(0b1010), 0b1010);
        assert_eq!(i64_to_u8(300), 0);
        assert_eq!(i64_to_u8(-1), 0);
    }
}
