//! clrecoder-diagnostics —— 共享有界诊断日志（usability-runtime-v3 §4.1，S1）。
//!
//! 每进程一个角色 sink（[`Role::Collector`] → `collector.log`、[`Role::Gui`] → `gui.log`），
//! 供提权采集进程与 GUI 各自打开；两角色文件互不干扰，只操作本角色文件与 `.1` 备份。
//!
//! - **记录形状**（JSONL，逐字段对齐 §4.1）：
//!   `{"time":RFC3339,"pid":u32,"role":"collector"|"gui","level":"info"|"warn"|"error","code":string,"message":string}`；
//!   不增加任意 RawEvent payload，不记录输入键码/按钮/组合、前台 exe 流水、逐条计数。
//! - **有界**：当前文件 256 KiB（[`MAX_CURRENT_BYTES`]），仅一份 `.1` 备份，两角色合计约
//!   1 MiB（[`MAX_TOTAL_BYTES`]）；单条记录（含换行）≤ [`MAX_LINE_BYTES`] 2048 字节，
//!   message 截断保持合法 JSON 与合法 UTF-8（不粗切 JSON 尾部）。
//! - **限频**：按稳定事件 code 分桶——前 63 个首次出现的 code 各占一个专属桶，其后新 code
//!   **永久**走共享 overflow 桶（共 64 项）；每桶按"最近真正写入时间"60 秒最多一条，
//!   message 可变化；进程内不淘汰/不迁移桶。抑制不是错误，不影响 [`DiagnosticLog::state`]。
//! - **失败安全**：open/record 失败只更新内存 state / 回退 stderr，不 panic、不递归记录
//!   自身失败、不影响业务结果；锁内轮转并写，轮转失败跳过该次并标不可用——绝不无界 append。
//! - **facade 适配**（[`install_project_log_adapter`]）：仅本项目 target（`clrecoder` 前缀）
//!   的 Warn/Error 进角色文件，code 取该条记录的固定 target；第三方 target 与 Info/Debug
//!   不落盘；必要 Info 由调用方用 `record` 显式代码（`service.started`/`service.stopped`/
//!   `task.policy_updated`/`collector.health_changed`/`collector.health_recovered`）。
//!
//! 边界（§2.1）：不做统计/GUI/任务/网络逻辑，不做 core IO；依赖仅 chrono/serde/serde_json/log。
//!
//! 同步写语义：[`DiagnosticLog::record`] 在锁内同步 `write_all`，返回即已落盘——
//! 调用进程随后 `std::process::exit` 不会丢日志，也无后台 logger 线程。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{Local, SecondsFormat};
use serde::Serialize;

/// 单角色当前文件上限（字节）：256 KiB；超出前轮转为 `.1`。
pub const MAX_CURRENT_BYTES: u64 = 256 * 1024;

/// 全部角色文件合计上限（约）：256 KiB ×（当前 + 一份备份）× 2 角色。
pub const MAX_TOTAL_BYTES: u64 = 1_048_576;

/// 单条 JSONL 记录上限（含结尾换行）。
pub const MAX_LINE_BYTES: usize = 2048;

/// code 字段上限（字节；超长按 UTF-8 字符边界截断）。
pub const MAX_CODE_BYTES: usize = 64;

/// 备份文件后缀（仅一份 `.1`，轮转时覆盖旧备份）。
const BACKUP_SUFFIX: &str = ".1";

/// 专属限频桶数量；其后首次出现的 code 永久进共享 overflow 桶（共 64 项）。
const DEDICATED_BUCKETS: usize = 63;

/// 每桶限频间隔：按最近真正写入时间，60 秒最多一条。
const RATE_LIMIT: Duration = Duration::from_secs(60);

/// 本项目 target 前缀：facade 仅持久化该前缀的 Warn/Error。
const PROJECT_TARGET_PREFIX: &str = "clrecoder";

/// 日志角色（每进程一个角色 sink，§4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 提权采集进程（`collector.log`）
    Collector,
    /// GUI 进程（`gui.log`）
    Gui,
}

/// 记录级别（serde 线上形状固定小写，§4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// 打开配置：日志目录 + 角色。
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// 日志目录（如 `%LOCALAPPDATA%\ClRecoder\logs`）
    pub directory: PathBuf,
    /// 本进程角色
    pub role: Role,
}

/// 内存状态快照（不读磁盘；抑制/限频不影响 `available`）。
#[derive(Debug, Clone)]
pub struct LogState {
    /// sink 是否可写（打开失败由 `open` 直接报错；此处 false = 轮转/写入失败后停用）
    pub available: bool,
    /// 最近一次失败原因
    pub last_error: Option<String>,
}

/// JSONL 记录（私有；字段顺序即线上字段顺序，§4.1）。
#[derive(Serialize)]
struct LogRecord<'a> {
    time: &'a str,
    pid: u32,
    role: Role,
    level: Level,
    code: &'a str,
    message: &'a str,
}

/// 全部可变状态（文件句柄、字节数、限频桶、可用性）都在锁内；轮转与写同锁（§4.1）。
#[derive(Debug)]
struct Inner {
    /// 当前文件句柄（append 模式）；None = 已被轮转关闭（等待重建/停用）
    file: Option<File>,
    /// 当前文件字节量（打开/轮转后校准，写入累计——每角色单写者模型）
    size: u64,
    /// 专属限频桶（≤63 项，按 code 首次出现顺序）：(code, 最近真正写入时间；None=尚未写过)
    buckets: Vec<(String, Option<Instant>)>,
    /// 共享 overflow 桶（第 64 项）的最近真正写入时间
    overflow: Option<Instant>,
    /// 可写状态：轮转/写入失败置 false，之后快速拒绝——不得对坏 sink 无界重试
    available: bool,
    /// 最近一次失败原因（供 [`DiagnosticLog::state`]）
    last_error: Option<String>,
}

/// 共享有界诊断日志（§4.1）。
///
/// 失败语义：open 失败返回 `Err`（调用方做 stderr-only 降级，不影响业务）；
/// record 失败只更新内存 state 并回退 stderr 一次，返回 `false`，不 panic。
#[derive(Debug)]
pub struct DiagnosticLog {
    inner: Mutex<Inner>,
    /// 固定角色（决定文件名与记录 `role` 字段）
    role: Role,
    /// 打开时的进程号（记录 `pid` 字段）
    pid: u32,
    /// 本角色当前文件路径（`<directory>/<role>.log`）
    file_path: PathBuf,
}

impl DiagnosticLog {
    /// 打开（或创建）本角色日志文件；目录不存在则创建（仅 `create_dir_all` 建目录，
    /// 不删除任何内容）。打开失败返回 `Err`。
    pub fn open(config: LogConfig) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(&config.directory)?;
        let file_path = config.directory.join(role_file_name(config.role));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            inner: Mutex::new(Inner {
                file: Some(file),
                size,
                buckets: Vec::new(),
                overflow: None,
                available: true,
                last_error: None,
            }),
            role: config.role,
            pid: std::process::id(),
            file_path,
        })
    }

    /// 记录一条诊断事件（同步写，返回即落盘）。返回是否真正写入：
    /// `false` = sink 不可用 / 被限频抑制 / 序列化失败，均不影响调用方业务结果。
    ///
    /// 失败（非抑制）时更新内存 state 并回退 stderr 一次；此后快速拒绝，
    /// 不 panic、不递归记录自身失败。
    pub fn record(&self, level: Level, code: &str, message: &str) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !inner.available {
            return false; // 不可用后快速拒绝：不再对坏 sink 反复 IO（无界 append 防线）
        }
        if !Self::rate_allows(&mut inner, code) {
            return false; // 限频抑制不是错误：available 不变、不回退 stderr
        }
        let Some(body) = Self::build_line(self.role, self.pid, level, code, message) else {
            return false; // 序列化失败（本记录形状不可能发生）：静默放弃本条
        };
        // 轮转判据（锁内）：现有 size + 本条 > 256 KiB → 先轮转再写
        if inner.size + body.len() as u64 > MAX_CURRENT_BYTES {
            if let Err(e) = Self::rotate(&mut inner, &self.file_path) {
                inner.available = false;
                inner.last_error = Some(format!("轮转 {} 失败: {e}", self.file_path.display()));
                // 跳过该次并回退 stderr 一次（此后快速拒绝）——绝不绕过上限继续 append
                Self::fallback_stderr(self.role, level, code, message);
                return false;
            }
        }
        let Some(file) = inner.file.as_mut() else {
            inner.available = false;
            inner.last_error = Some("日志文件句柄缺失".to_string());
            Self::fallback_stderr(self.role, level, code, message);
            return false;
        };
        match file.write_all(&body) {
            Ok(()) => {
                inner.size += body.len() as u64;
                Self::mark_written(&mut inner, code); // 仅成功写入推进桶（按最近真正写入时间）
                true
            }
            Err(e) => {
                inner.available = false;
                inner.last_error = Some(format!("写入 {} 失败: {e}", self.file_path.display()));
                Self::fallback_stderr(self.role, level, code, message);
                false
            }
        }
    }

    /// 内存状态快照（不读磁盘；限频抑制不影响 `available`）。
    pub fn state(&self) -> LogState {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        LogState {
            available: inner.available,
            last_error: inner.last_error.clone(),
        }
    }

    /// 限频判据（§4.1）：前 63 个首次出现的 code 各占一个专属桶，其后所有新 code
    /// **永久**走共享 overflow 桶——不淘汰/不迁移，防止已写 overflow 事件换桶绕过间隔。
    fn rate_allows(inner: &mut Inner, code: &str) -> bool {
        if let Some((_, last)) = inner.buckets.iter_mut().find(|(c, _)| c == code) {
            return last.is_none_or(|t| t.elapsed() >= RATE_LIMIT);
        }
        if inner.buckets.len() < DEDICATED_BUCKETS {
            inner.buckets.push((code.to_string(), None)); // 尚未写过 → 不限
            return true;
        }
        inner.overflow.is_none_or(|t| t.elapsed() >= RATE_LIMIT)
    }

    /// 成功写入后推进对应桶的"最近真正写入时间"；桶归属判据与 [`Self::rate_allows`] 一致。
    fn mark_written(inner: &mut Inner, code: &str) {
        let now = Instant::now();
        if let Some((_, last)) = inner.buckets.iter_mut().find(|(c, _)| c == code) {
            *last = Some(now);
        } else if inner.buckets.len() < DEDICATED_BUCKETS {
            inner.buckets.push((code.to_string(), Some(now)));
        } else {
            inner.overflow = Some(now);
        }
    }

    /// 构造一条 JSONL（含结尾换行；≤ [`MAX_LINE_BYTES`]）。纯计算，无 IO：
    /// code 截断到 64 字节（UTF-8 边界）；整行超限时对 message 做"边界安全的收缩截断"
    /// 后重新序列化，保证最终整条为合法 JSON + 合法 UTF-8（serde_json 转义保证
    /// CR/LF 在字符串内不成新记录）。
    fn build_line(
        role: Role,
        pid: u32,
        level: Level,
        code: &str,
        message: &str,
    ) -> Option<Vec<u8>> {
        let code = truncate_utf8(code, MAX_CODE_BYTES);
        let time = now_rfc3339();
        let make = |msg: &str| -> Option<Vec<u8>> {
            let rec = LogRecord {
                time: &time,
                pid,
                role,
                level,
                code,
                message: msg,
            };
            serde_json::to_vec(&rec).ok()
        };
        let mut body = make(message)?;
        if body.len() + 1 > MAX_LINE_BYTES {
            // 转义最坏可放大 6 倍（控制字符 → \u00XX）：按 2/3 几何收缩重试；
            // 空消息的固定开销（time/pid/role/level/code ≈ ≤500B）必然放得下，循环必终止。
            let mut cut = truncate_utf8(message, MAX_LINE_BYTES);
            body = make(cut)?;
            while body.len() + 1 > MAX_LINE_BYTES && !cut.is_empty() {
                cut = truncate_utf8(message, cut.len() * 2 / 3);
                body = make(cut)?;
            }
            if body.len() + 1 > MAX_LINE_BYTES {
                return None; // 理论不可达：空消息必放得下
            }
        }
        body.push(b'\n');
        Some(body)
    }

    /// 轮转（锁内调用）：关闭当前句柄 → 当前文件改名为 `.1`（覆盖旧备份）→ 重建空当前文件。
    /// 只操作本角色的这两个文件，不递归删除目录；改名失败（如 `.1` 被目录占用）原样上抛，
    /// 由调用方跳过本条并标不可用。
    fn rotate(inner: &mut Inner, path: &Path) -> std::io::Result<()> {
        inner.file = None; // 先关闭句柄再改名
        std::fs::rename(path, backup_path(path))?;
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        inner.size = 0;
        inner.file = Some(file);
        Ok(())
    }

    /// 失败回退 stderr（一次性；此后 available=false 快速路径静默拒绝，无刷屏）。
    fn fallback_stderr(role: Role, level: Level, code: &str, message: &str) {
        eprintln!(
            "[clrecoder-diagnostics:{role:?}] 写入失败，回退 stderr [{level:?}] {code}: {message}"
        );
    }
}

/// 角色对应文件名（§4.1：collector.log / gui.log）。
fn role_file_name(role: Role) -> &'static str {
    match role {
        Role::Collector => "collector.log",
        Role::Gui => "gui.log",
    }
}

/// 备份路径：`collector.log` → `collector.log.1`（仅一份）。
fn backup_path(p: &Path) -> PathBuf {
    let mut name = p.file_name().unwrap_or_default().to_os_string();
    name.push(BACKUP_SUFFIX);
    p.with_file_name(name)
}

/// 当前本地时刻 RFC 3339（秒级精度、本地时区偏移；与 core `now_local_rfc3339` 同形。
/// 本 crate 按白名单不依赖 core，直接用 chrono 产出）。
fn now_rfc3339() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// 按 UTF-8 字符边界截断到 ≤ max 字节（stable Rust 无 `floor_char_boundary`，自行实现）。
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// 受控 log facade 适配器（§4.1）：仅本项目 target（`clrecoder` 前缀）的 Warn/Error
/// 进角色文件，code 取该条记录的固定 target（稳定事件类别，不从完整文案生成）；
/// 第三方 target 与 Info/Debug 不落盘。`debug_stderr=true`（对应原 `CLRECODER_DEBUG`
/// 诊断习惯）时额外把 facade 记录回显 stderr——仅 stderr，不影响持久化规则。
struct ProjectLogAdapter {
    sink: Arc<DiagnosticLog>,
    debug_stderr: bool,
}

impl log::Log for ProjectLogAdapter {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.debug_stderr {
            eprintln!("[{} {}] {}", record.level(), record.target(), record.args());
        }
        let target = record.target();
        let native_binary_target = ["cl_recoder", "cl_recoder_collector"].iter().any(|root| {
            target == *root || target.strip_prefix(root).is_some_and(|tail| tail.starts_with("::"))
        });
        if !target.starts_with(PROJECT_TARGET_PREFIX) && !native_binary_target {
            return; // 第三方 target 不落盘
        }
        let level = match record.level() {
            log::Level::Error => Level::Error,
            log::Level::Warn => Level::Warn,
            _ => return, // Info/Debug 不落盘（必要 Info 由调用方显式 record）
        };
        // 失败（含限频抑制）只影响本条，绝不影响业务结果。
        let _ = self
            .sink
            .record(level, record.target(), &record.args().to_string());
    }

    fn flush(&self) {}
}

/// 安装受控 log facade 适配器（每进程至多一次）。重复安装返回 `Err(SetLoggerError)`，
/// 调用方必须记录/报告不可用，不得忽略后宣称持久日志已接线（§4.1）。
///
/// `debug_stderr=true`（对应原 `CLRECODER_DEBUG` 习惯）时 facade 记录额外回显 stderr
/// 并把 max_level 放开到 Debug；持久化规则（项目 Warn/Error）不受它影响——
/// 持久日志是否启用与 `CLRECODER_DEBUG` 无关（§4.1）。
pub fn install_project_log_adapter(
    log: Arc<DiagnosticLog>,
    debug_stderr: bool,
) -> Result<(), log::SetLoggerError> {
    log::set_boxed_logger(Box::new(ProjectLogAdapter {
        sink: log,
        debug_stderr,
    }))?;
    log::set_max_level(if debug_stderr {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Warn
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 唯一临时目录（pid + 标签 + 计数器 + 纳秒时间戳；fixture 约束：绝不触碰生产目录）。
    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "clrecoder-diag-{}-{tag}-{n}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 以 Collector 角色打开（测试常用配置）。
    fn open_at(dir: &Path) -> DiagnosticLog {
        DiagnosticLog::open(LogConfig {
            directory: dir.to_path_buf(),
            role: Role::Collector,
        })
        .expect("打开测试 sink")
    }

    /// 读取日志文件并逐行校验 JSONL 合同：整体合法 UTF-8、每行（含换行）≤2048 字节、
    /// 每行可解析为 JSON 且六字段齐全。
    fn read_lines(path: &Path) -> Vec<serde_json::Value> {
        let bytes = std::fs::read(path).expect("读日志文件");
        let text = String::from_utf8(bytes).expect("日志必须整体是合法 UTF-8");
        let mut out = Vec::new();
        for line in text.lines() {
            assert!(
                line.len() < MAX_LINE_BYTES,
                "行（含换行）超限: {}",
                line.len() + 1
            );
            let v: serde_json::Value = serde_json::from_str(line).expect("每行必须是合法 JSON");
            for key in ["time", "pid", "role", "level", "code", "message"] {
                assert!(v.get(key).is_some(), "记录缺字段 {key}: {line}");
            }
            out.push(v);
        }
        out
    }

    /// 白盒时间平移：把全部桶的"最近真正写入时间"拨回 secs 秒前（模拟 60s 窗口过期，
    /// 测试不必真睡 60 秒）。仅测试内使用，生产路径无此入口。
    fn age_all_buckets(log: &DiagnosticLog, secs: u64) {
        let mut inner = log.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let past = Instant::now() - Duration::from_secs(secs);
        for bucket in inner.buckets.iter_mut() {
            bucket.1 = Some(past);
        }
        inner.overflow = Some(past);
    }

    /// 写满一轮：63 个专属 code 各写一条（每桶 60s 内各 1 条）。返回成功条数。
    fn write_window(log: &DiagnosticLog, tag: &str, msg: &str) -> usize {
        let mut n = 0;
        for i in 0..63 {
            if !log.record(Level::Info, &format!("{tag}{i}"), msg) {
                break;
            }
            n += 1;
        }
        n
    }

    #[test]
    fn usability_v3_line_length_utf8_and_json_valid() {
        let dir = temp_dir("shape");
        let log = open_at(&dir);
        // 超长 message：4 字节 emoji / 引号 / 反斜杠 / CR-LF / 多字节字符混排，≈10KB
        let unit = "键αβγ😀\"引\"\\反\n换行";
        let long: String = unit.repeat(600);
        assert!(long.len() > MAX_LINE_BYTES);
        assert!(log.record(Level::Warn, "test.long", &long));
        assert!(log.record(Level::Error, "test.short", "短消息"));

        let lines = read_lines(&dir.join("collector.log"));
        assert_eq!(lines.len(), 2);
        let first = &lines[0];
        assert_eq!(first["role"], "collector");
        assert_eq!(first["level"], "warn");
        assert_eq!(first["pid"], std::process::id());
        assert_eq!(first["code"], "test.long");
        // 截断保留前缀（不粗切 JSON 尾部）；多字节字符不切碎（能整体解析即边界合法）
        let msg = first["message"].as_str().expect("message 必须是字符串");
        assert!(long.starts_with(msg), "截断必须保留 message 前缀");
        assert!(msg.len() < long.len(), "超长 message 必须被截断");
        // time 可被 RFC3339 解析
        chrono::DateTime::parse_from_rfc3339(first["time"].as_str().expect("time 必须是字符串"))
            .expect("time 必须是合法 RFC3339");
        assert_eq!(lines[1]["level"], "error");
        assert_eq!(lines[1]["message"], "短消息");
    }

    #[test]
    fn usability_v3_code_capped_at_64_bytes_on_char_boundary() {
        let dir = temp_dir("code64");
        let log = open_at(&dir);
        let code = "代".repeat(64); // 192 字节 > 64
        assert!(log.record(Level::Info, &code, "m"));
        let lines = read_lines(&dir.join("collector.log"));
        let got = lines[0]["code"].as_str().expect("code 必须是字符串");
        assert!(
            got.len() <= MAX_CODE_BYTES,
            "code 必须 ≤64 字节: {}",
            got.len()
        );
        assert!(code.starts_with(got), "code 截断必须保留前缀且落在字符边界");
        assert_eq!(got.len(), 63); // 21 个 3 字节字符
    }

    #[test]
    fn usability_v3_cr_lf_stays_single_record() {
        let dir = temp_dir("crlf");
        let log = open_at(&dir);
        assert!(log.record(Level::Warn, "crlf", "第一行\r\n第二行\r第三\n行"));
        let raw = std::fs::read(dir.join("collector.log")).expect("读原始字节");
        assert_eq!(
            raw.iter().filter(|&&b| b == b'\n').count(),
            1,
            "CR/LF 不得成为新记录"
        );
        let lines = read_lines(dir.join("collector.log").as_path());
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0]["message"], "第一行\r\n第二行\r第三\n行",
            "转义必须可还原"
        );
    }

    #[test]
    fn usability_v3_rotation_caps_size_and_keeps_single_backup() {
        let dir = temp_dir("rotate");
        let log = open_at(&dir);
        let msg = "x".repeat(1900); // 整行 ≈1990B < 2048，不被截断
                                    // 三轮写满（每轮 63 条 ≈125KB；轮转发生在第二轮后）：时间白盒平移绕过限频
        assert_eq!(write_window(&log, "r", &msg), 63);
        age_all_buckets(&log, 61);
        assert_eq!(write_window(&log, "r", &msg), 63);
        age_all_buckets(&log, 61);
        assert_eq!(write_window(&log, "r", &msg), 63);

        let current = dir.join("collector.log");
        let backup = dir.join("collector.log.1");
        assert!(backup.exists(), "超过 256KiB 必须产生 .1 备份");
        assert!(!dir.join("collector.log.1.1").exists(), "只允许一份备份");
        let cur_len = std::fs::metadata(&current).expect("当前文件").len();
        assert!(
            cur_len <= MAX_CURRENT_BYTES,
            "当前文件不得超 256KiB: {cur_len}"
        );
        assert!(std::fs::metadata(&backup).expect("备份文件").len() > 0);
        // 两份文件合计行数 = 189（无丢失）；且全部是合法 JSONL
        let total = read_lines(&current).len() + read_lines(&backup).len();
        assert_eq!(total, 189, "轮转不得丢记录");

        // 再写两轮（第二轮必然再次触发轮转）：旧备份被覆盖——目录里始终只有
        // 当前文件 + 一份 .1，保留行数远小于累计写入量（315 条）
        age_all_buckets(&log, 61);
        assert_eq!(write_window(&log, "r", &msg), 63);
        age_all_buckets(&log, 61);
        assert_eq!(write_window(&log, "r", &msg), 63);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "只允许当前文件 + 一份 .1"
        );
        assert!(std::fs::metadata(&current).unwrap().len() <= MAX_CURRENT_BYTES);
        assert!(!dir.join("collector.log.1.1").exists());
        let retained = read_lines(&current).len() + read_lines(&backup).len();
        assert!(
            retained < 315,
            "轮转必须覆盖丢弃旧内容，实际保留 {retained} 条"
        );
    }

    #[test]
    fn usability_v3_rotation_failure_marks_unavailable_and_stops_appending() {
        let dir = temp_dir("rotate-fail");
        let log = open_at(&dir);
        // 预置同名目录占据备份路径：轮转 rename 必失败
        std::fs::create_dir(dir.join("collector.log.1")).expect("建占位目录");
        let mut failed = false;
        for _window in 0..4 {
            for i in 0..63 {
                if !log.record(Level::Warn, &format!("rf{i}"), &"y".repeat(1900)) {
                    failed = true;
                    break;
                }
            }
            if failed {
                break;
            }
            age_all_buckets(&log, 61);
        }
        assert!(failed, "备份路径被目录占据时轮转必须失败并标不可用");

        let state = log.state();
        assert!(!state.available, "轮转失败必须标不可用");
        let err = state.last_error.expect("必须有 last_error");
        assert!(err.contains("轮转"), "错误应指明轮转失败: {err}");

        // 之后继续 record：快速拒绝、文件不再增长（无界 append 防线），且不 panic
        let current = dir.join("collector.log");
        let before = std::fs::metadata(&current).unwrap().len();
        assert!(
            before <= MAX_CURRENT_BYTES,
            "失败前也不得越过上限: {before}"
        );
        age_all_buckets(&log, 61);
        for _ in 0..10 {
            assert!(!log.record(Level::Error, "rf.after", "轮转失败后的记录必须被拒"));
        }
        let after = std::fs::metadata(&current).unwrap().len();
        assert_eq!(before, after, "不可用后文件不得继续增长");
    }

    #[test]
    fn usability_v3_roles_use_separate_files_in_same_directory() {
        let dir = temp_dir("roles");
        let collector = open_at(&dir);
        let gui = DiagnosticLog::open(LogConfig {
            directory: dir.clone(),
            role: Role::Gui,
        })
        .expect("打开 gui sink");
        assert!(dir.join("collector.log").exists());
        assert!(dir.join("gui.log").exists());

        // 同一 code 在两个实例各自写入（限频按实例隔离），role 字段各归各
        assert!(collector.record(Level::Info, "shared.code", "collector 侧"));
        assert!(gui.record(Level::Info, "shared.code", "gui 侧"));
        let cl = read_lines(&dir.join("collector.log"));
        let gl = read_lines(&dir.join("gui.log"));
        assert_eq!(cl.len(), 1);
        assert_eq!(gl.len(), 1);
        assert_eq!(cl[0]["role"], "collector");
        assert_eq!(gl[0]["role"], "gui");
        assert_eq!(cl[0]["code"], "shared.code");
        assert_eq!(gl[0]["code"], "shared.code");
        assert!(
            !std::fs::read_to_string(dir.join("gui.log"))
                .unwrap()
                .contains("collector 侧"),
            "角色文件不得串内容"
        );
        // 目录内只应有这两个文件（不递归删除/不产生额外文件）
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    }

    #[test]
    fn usability_v3_rate_limit_65_codes_interleaved_varying_messages() {
        let dir = temp_dir("rate");
        let log = DiagnosticLog::open(LogConfig {
            directory: dir.clone(),
            role: Role::Gui,
        })
        .expect("打开 gui sink");
        let codes: Vec<String> = (0..65).map(|i| format!("evt.{i}")).collect();

        // 65 类 code 交错首写，message 每次都变（限频 key 是稳定 code，不是文案）：
        // 63 个专属桶各放行 1 条 + overflow 桶放行 1 条 = 64 条，第 65 类被共享桶抑制
        let mut first_round = 0;
        for (i, code) in codes.iter().enumerate() {
            let msg = format!("首写-{i}-{}", "x".repeat(i % 7));
            if log.record(Level::Warn, code, &msg) {
                first_round += 1;
            }
        }
        assert_eq!(first_round, 64, "63 专属桶 + overflow 放行 1 条 = 64");

        {
            let inner = log.inner.lock().unwrap_or_else(PoisonError::into_inner);
            assert_eq!(inner.buckets.len(), 63, "专属桶必须恰好 63 个");
            assert!(inner.overflow.is_some(), "overflow 桶必须已被使用");
        }

        // 60s 窗口内重放：全部抑制（含与首写同 code 的交错变文案），available 不受影响
        for code in &codes {
            assert!(
                !log.record(Level::Warn, code, "60s 内重放必须被抑制"),
                "{code}"
            );
        }
        assert!(
            !log.record(Level::Warn, "evt.0", "同 code 换文案也抑制"),
            "key 必须是 code"
        );
        assert_eq!(read_lines(&dir.join("gui.log")).len(), 64);
        assert!(log.state().available, "限频抑制不得把 available 标为 false");

        // 窗口过期（白盒平移 61s）→ 63 个专属桶各放行 1 条 + overflow 放行 1 条 = 64 条
        age_all_buckets(&log, 61);
        let mut second_round = 0;
        for code in &codes {
            if log.record(Level::Warn, code, "窗口过期后再写") {
                second_round += 1;
            }
        }
        assert_eq!(second_round, 64);
        assert_eq!(read_lines(&dir.join("gui.log")).len(), 128);
    }

    #[test]
    fn usability_v3_facade_persists_project_warn_error_and_installs_once() {
        let dir = temp_dir("facade");
        let log = Arc::new(open_at(&dir));
        install_project_log_adapter(Arc::clone(&log), false).expect("首次安装必须成功");
        // 全局 logger 唯一：第二次安装必须显式失败（不得静默重复接线）
        assert!(
            install_project_log_adapter(Arc::clone(&log), false).is_err(),
            "重复安装必须返回 SetLoggerError"
        );

        log::warn!(target: "cl_recoder_collector::ipc_server", "生产警告: pipe 建实例失败");
        log::error!(target: "cl_recoder_collector", "生产错误: 打开统计库失败 {}", 7);
        // 第三方 target 与 Info/Debug 一律不落盘
        log::warn!(target: "gilrs::core", "第三方警告不落盘");
        log::info!(target: "cl_recoder_collector::raw_input", "Info 不落盘");
        log::debug!(target: "cl_recoder_collector", "Debug 不落盘");

        let lines = read_lines(&dir.join("collector.log"));
        assert_eq!(lines.len(), 2, "仅本项目 target 的 Warn/Error 落盘");
        assert_eq!(lines[0]["level"], "warn");
        assert_eq!(
            lines[0]["code"], "cl_recoder_collector::ipc_server",
            "code 必须取固定 target"
        );
        assert_eq!(lines[0]["message"], "生产警告: pipe 建实例失败");
        assert_eq!(lines[1]["level"], "error");
        assert_eq!(lines[1]["code"], "cl_recoder_collector");
        assert_eq!(lines[1]["message"], "生产错误: 打开统计库失败 7");
        assert_eq!(lines[1]["role"], "collector");
        assert_eq!(lines[1]["pid"], std::process::id());

        // 同 code（同 target）60s 内第二条被限频抑制；抑制不影响 available
        log::warn!(target: "cl_recoder_collector::ipc_server", "60s 内第二条应被抑制");
        assert_eq!(read_lines(&dir.join("collector.log")).len(), 2);
        assert!(log.state().available);
    }

    #[test]
    fn usability_v3_io_failure_isolated_and_business_unaffected() {
        // 1) 目录路径被文件占据 → open 返回 Err（无实例、无 panic）
        let blocker = temp_dir("open-fail");
        std::fs::write(&blocker, b"placeholder").expect("写占位文件");
        assert!(DiagnosticLog::open(LogConfig {
            directory: blocker.join("logs"),
            role: Role::Gui,
        })
        .is_err());

        // 2) 一个 sink 不可用不影响另一个 sink（业务照常；record 对坏 sink 反复调用安全）
        let dir_a = temp_dir("sink-a");
        let dir_b = temp_dir("sink-b");
        let a = DiagnosticLog::open(LogConfig {
            directory: dir_a.clone(),
            role: Role::Gui,
        })
        .expect("打开 a");
        let b = DiagnosticLog::open(LogConfig {
            directory: dir_b.clone(),
            role: Role::Collector,
        })
        .expect("打开 b");
        std::fs::create_dir(dir_a.join("gui.log.1")).expect("预置轮转失败条件");
        let mut failed = false;
        for _window in 0..4 {
            for i in 0..63 {
                if !a.record(Level::Warn, &format!("a{i}"), &"x".repeat(1900)) {
                    failed = true;
                    break;
                }
            }
            if failed {
                break;
            }
            age_all_buckets(&a, 61);
        }
        assert!(failed, "a 的轮转必须失败");
        assert!(!a.state().available);
        for _ in 0..10 {
            assert!(!a.record(Level::Error, "a.after", "x"));
        }
        // b 不受 a 影响：限频按实例隔离，业务记录照常落盘
        assert!(b.record(Level::Warn, "b0", "业务不受其他 sink 失败影响"));
        assert!(b.state().available);
        assert_eq!(read_lines(&dir_b.join("collector.log")).len(), 1);
    }
}
