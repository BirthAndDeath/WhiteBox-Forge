//! 用户可配置项 / User-configurable settings.
//!
//! 从 `dir::config_dir()` 读取 `run.json` / `limits.json` / `logging.json` / `caching.json`，
//! 按功能拆分、以 `serde_json` 加载到结构。**只承载用户会随部署调整的常量**；
//! 安全/开发者内部常量（协议帧上限、捕获上限、socket/landlock/seccomp 语义、退出码等）
//! 一律不进配置文件。
//!
//! 语义（fail-closed，且文件可选）：
//! - 文件缺失 / 目录不可确定 → 内置默认（配置是可选项，不算错误）；
//! - 字段缺失 → 该字段回落默认（`serde(default)`），无需整节齐全；
//! - 文件存在但非法 JSON / 未知字段 / 类型错误 / 值越界 → **拒绝启动**；
//! - 优先级：内置默认 < JSON < CLI 显式参数 < 库 API 显式赋值（JSON 只是默认层）。
//!
//! 运行时唯一真源仍是 `SandboxConfig` + 少量进程级常量；本模块在启动时把它们翻译过去。
//! 必须在第一次 `ENGINE` 初始化之前 `load()`（编译缓存目录要赶在引擎构建前生效）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::sandbox::SandboxConfig;

// ---- 运行 / 等待 / Run & wait ----

/// `run.json`：执行上界与有界等待/关闭的时序。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    /// 默认执行上界（毫秒）；`None` = 无界。仅作 CLI 的默认值，显式参数覆盖。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// 线程放弃宽限（毫秒）：`shutdown`/`wait_timeout` 超时后循环轻推 epoch 的最长窗口。
    #[serde(default = "RunConfig::default_shutdown_grace_ms")]
    pub shutdown_grace_ms: u64,
    /// 有界等待/关闭的轮询间隔（毫秒）。
    #[serde(default = "RunConfig::default_poll_ms")]
    pub poll_ms: u64,
}

impl RunConfig {
    pub const fn default_shutdown_grace_ms() -> u64 {
        2_000
    }
    pub const fn default_poll_ms() -> u64 {
        20
    }
    pub const fn new_default() -> Self {
        Self {
            timeout_ms: None,
            shutdown_grace_ms: Self::default_shutdown_grace_ms(),
            poll_ms: Self::default_poll_ms(),
        }
    }
}

impl Default for RunConfig {
    fn default() -> Self {
        Self::new_default()
    }
}

/// 已安装的运行配置（未 `load()` 时用内置默认）。供 wait/shutdown 内部读取。
pub fn run() -> &'static RunConfig {
    INSTALLED_RUN.get().unwrap_or(&DEFAULT_RUN)
}

// ---- 资源上限 / Resource limits ----

/// `limits.json`：对应 `SandboxConfig` 的 `max_*`，全部 `null` = 不设限。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    pub max_memory_bytes: Option<u64>,
    pub max_cpu_ms: Option<u64>,
    pub max_file_size_bytes: Option<u64>,
    pub max_open_files: Option<u64>,
    pub max_core_bytes: Option<u64>,
    pub max_children: Option<u32>,
}

impl LimitsConfig {
    /// 把上限写入沙箱配置。只在沙箱模式调用；`--no-sandbox` 时由调用方跳过。
    pub fn apply_to(&self, cfg: &mut SandboxConfig) {
        cfg.max_memory_bytes = self.max_memory_bytes;
        cfg.max_cpu_ms = self.max_cpu_ms;
        cfg.max_file_size_bytes = self.max_file_size_bytes;
        cfg.max_open_files = self.max_open_files;
        cfg.max_core_bytes = self.max_core_bytes;
        cfg.max_children = self.max_children;
    }
}

// ---- worker 日志 / Worker logging ----

/// `logging.json`：worker 日志轮换与保留。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// 单个日志文件上限（字节），达到后轮换到带时间戳的新文件。
    #[serde(default = "LoggingConfig::default_per_file_bytes")]
    pub per_file_bytes: u64,
    /// 轮换日志保留天数（按文件名中的 UTC 天数）。
    #[serde(default = "LoggingConfig::default_retention_days")]
    pub retention_days: u64,
}

impl LoggingConfig {
    pub const fn default_per_file_bytes() -> u64 {
        1 << 20
    }
    pub const fn default_retention_days() -> u64 {
        7
    }
    pub const fn new_default() -> Self {
        Self {
            per_file_bytes: Self::default_per_file_bytes(),
            retention_days: Self::default_retention_days(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self::new_default()
    }
}

/// 已安装的日志配置（未 `load()` 时用内置默认）。供 worker_log 内部读取。
pub fn logging() -> &'static LoggingConfig {
    INSTALLED_LOGGING.get().unwrap_or(&DEFAULT_LOGGING)
}

// ---- 编译缓存 / Compile cache ----

/// `caching.json`：wasmtime 编译缓存开关与目录覆盖。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CachingConfig {
    /// 是否启用编译缓存（默认开；运行时不可用仍会自动禁用，见 `SandboxConfig::enable_compile_cache_at`）。
    pub enabled: bool,
    /// 缓存目录覆盖（必须绝对路径）；`None` = `dir::cache_dir()/wasmtime`。
    /// 目录仍会经 `protect_dir` 收紧为属主专用（缓存中毒防御不因覆盖而豁免）。
    pub directory: Option<PathBuf>,
}

impl CachingConfig {
    pub const fn default_enabled() -> bool {
        true
    }
    pub const fn new_default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            directory: None,
        }
    }
}

impl Default for CachingConfig {
    fn default() -> Self {
        Self::new_default()
    }
}

// ---- 聚合 / Aggregate ----

/// 全部用户配置。`load()` 读取、校验并安装运行/日志节。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AppConfig {
    pub run: RunConfig,
    pub limits: LimitsConfig,
    pub logging: LoggingConfig,
    pub caching: CachingConfig,
}

impl AppConfig {
    /// 从 `dir::config_dir()` 读取四个 JSON 并校验，随后把运行/日志节安装为进程全局。
    ///
    /// - 目录不可确定 / 文件缺失 → 内置默认（配置可选）；
    /// - 文件存在但非法 → `Err`（fail-closed，拒绝启动）。
    /// - 必须在第一次 `ENGINE` 初始化前调用。
    pub fn load() -> anyhow::Result<Self> {
        let Ok(base) = crate::dir::config_dir() else {
            return Ok(Self::default());
        };
        Self::load_from(&base)
    }

    /// `load()` 的底层实现：目录可注入（单测用）。读取 → 校验 → 安装运行/日志节。
    /// Underlying `load()` with an injectable base dir (for tests).
    fn load_from(base: &Path) -> anyhow::Result<Self> {
        let app = Self {
            run: load_section(base, "run.json")?,
            limits: load_section(base, "limits.json")?,
            logging: load_section(base, "logging.json")?,
            caching: load_section(base, "caching.json")?,
        };
        let violations = app.validate();
        if !violations.is_empty() {
            anyhow::bail!("用户配置冲突：{}", violations.join("；"));
        }
        // 全局只安装一次（首次生效，与 COMPILE_CACHE_DIR 同语义）。
        let _ = INSTALLED_RUN.set(app.run.clone());
        let _ = INSTALLED_LOGGING.set(app.logging.clone());
        Ok(app)
    }

    /// 值域校验（fail-closed 的最后一环）；冲突格式与 `SandboxConfig::validate` 一致。
    pub fn validate(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.run.shutdown_grace_ms == 0 {
            v.push("run.shutdown_grace_ms 必须 > 0".into());
        }
        if self.run.poll_ms == 0 {
            v.push("run.poll_ms 必须 > 0".into());
        }
        if self.run.poll_ms > MAX_POLL_MS {
            v.push(format!("run.poll_ms 不能超过 {MAX_POLL_MS}ms"));
        }
        // 轮询先睡满一个 poll 再判宽限；poll > grace 会让有界等待静默超跑一个 poll。
        if self.run.poll_ms > self.run.shutdown_grace_ms {
            v.push("run.poll_ms 不能大于 run.shutdown_grace_ms（轮询会超跑宽限）".into());
        }
        if self.logging.per_file_bytes < MIN_PER_FILE_BYTES {
            v.push(format!(
                "logging.per_file_bytes 不能小于 {MIN_PER_FILE_BYTES}（过小会在每条日志上轮换）"
            ));
        }
        if self.logging.retention_days == 0 {
            v.push("logging.retention_days 必须 >= 1".into());
        }
        if self.logging.retention_days > MAX_RETENTION_DAYS {
            v.push(format!("logging.retention_days 不能超过 {MAX_RETENTION_DAYS} 天"));
        }
        if self.caching.directory.as_ref().is_some_and(|d| !d.is_absolute()) {
            v.push("caching.directory 必须是绝对路径".into());
        }
        v
    }
}

// ---- 实现 / plumbing ----

/// `poll_ms` 实用上界：轮询间隔超出后轮询失去「及时」意义，只应配置为小值。
const MAX_POLL_MS: u64 = 10_000;
/// worker 日志轮换文件保留天数上界（防无限保留）。
const MAX_RETENTION_DAYS: u64 = 365;
/// worker 日志单文件下限：过小会在每条日志上触发轮换，造成文件/目录膨胀。
const MIN_PER_FILE_BYTES: u64 = 4096;

const DEFAULT_RUN: RunConfig = RunConfig::new_default();
const DEFAULT_LOGGING: LoggingConfig = LoggingConfig::new_default();

static INSTALLED_RUN: OnceLock<RunConfig> = OnceLock::new();
static INSTALLED_LOGGING: OnceLock<LoggingConfig> = OnceLock::new();

/// 读取单个分节文件；缺失 → 默认，其余失败 → `Err`。路径便于单测注入。
fn load_section<T>(base: &Path, name: &str) -> anyhow::Result<T>
where
    T: Default + serde::de::DeserializeOwned,
{
    let path = base.join(name);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(anyhow::anyhow!("读取 {name} 失败: {e}")),
    };
    serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("解析 {name} 失败（fail-closed，拒绝启动）: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用临时目录：优先 `CARGO_TARGET_TMPDIR`（cargo 提供的 target 下 tmp），
    /// 否则回落测试二进制所在目录（都在工作区 target/ 内、可写）。
    /// 不用 `std::env::temp_dir()`：本工作站上测试二进制进程对 `%TEMP%` 的写入被环境
    /// （AV/宿主沙箱）白名单拒绝，而仓库 target/ 始终可写。
    fn section_dir(tag: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(Path::to_path_buf))
                    .unwrap_or_else(std::env::temp_dir)
            });
        base.join(format!("whitebox-config-test-{}-{}", tag, std::process::id()))
    }

    fn write_section(tag: &str, name: &str, body: &str) {
        let dir = section_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn missing_file_uses_defaults() {
        let run: RunConfig = load_section(&section_dir("missing"), "run.json").unwrap();
        assert_eq!(run, RunConfig::default());
        let logging: LoggingConfig = load_section(&section_dir("missing"), "logging.json").unwrap();
        assert_eq!(logging, LoggingConfig::default());
    }

    #[test]
    fn partial_fields_fall_back() {
        write_section("partial", "run.json", r#"{"poll_ms": 5}"#);
        let run: RunConfig = load_section(&section_dir("partial"), "run.json").unwrap();
        assert_eq!(run.poll_ms, 5);
        assert_eq!(run.timeout_ms, None);
        assert_eq!(run.shutdown_grace_ms, RunConfig::default_shutdown_grace_ms());
    }

    #[test]
    fn invalid_json_is_rejected() {
        write_section("invalid", "run.json", "not json");
        assert!(load_section::<RunConfig>(&section_dir("invalid"), "run.json").is_err());
    }

    #[test]
    fn empty_file_is_rejected() {
        write_section("empty", "logging.json", "");
        assert!(load_section::<LoggingConfig>(&section_dir("empty"), "logging.json").is_err());
    }

    #[test]
    fn unknown_field_is_rejected() {
        write_section("unknown", "run.json", r#"{"poll_ms": 5, "poll_interval": 5}"#);
        assert!(load_section::<RunConfig>(&section_dir("unknown"), "run.json").is_err());
    }

    #[test]
    fn section_partial_and_unknown() {
        // logging/caching/limits 的「部分字段缺省 + 未知字段拒绝」也与 run 一致。
        write_section("sec-partial", "logging.json", r#"{"per_file_bytes": 8192}"#);
        let logging: LoggingConfig =
            load_section(&section_dir("sec-partial"), "logging.json").unwrap();
        assert_eq!(logging.per_file_bytes, 8192);
        assert_eq!(logging.retention_days, LoggingConfig::default_retention_days());

        write_section("sec-partial", "caching.json", r#"{"enabled": false}"#);
        let caching: CachingConfig =
            load_section(&section_dir("sec-partial"), "caching.json").unwrap();
        assert!(!caching.enabled);
        assert_eq!(caching.directory, None);

        write_section("sec-partial", "limits.json", r#"{"max_memory_bytes": 1073741824}"#);
        let limits: LimitsConfig =
            load_section(&section_dir("sec-partial"), "limits.json").unwrap();
        assert_eq!(limits.max_memory_bytes, Some(1073741824));
        assert_eq!(limits.max_children, None);

        write_section("sec-unknown", "logging.json", r#"{"max_retry": 3}"#);
        assert!(load_section::<LoggingConfig>(&section_dir("sec-unknown"), "logging.json").is_err());
    }

    #[test]
    fn load_from_end_to_end() {
        // fail-closed 主链路：任一节非法 → 整体拒绝；缺文件 → 默认。
        let bad = section_dir("e2e-bad");
        write_section("e2e-bad", "logging.json", r#"{"per_file_bytes": 1}"#);
        assert!(AppConfig::load_from(&bad).is_err());

        let good = section_dir("e2e-good");
        write_section("e2e-good", "run.json", r#"{"timeout_ms": 5000, "poll_ms": 30}"#);
        write_section("e2e-good", "logging.json", r#"{"retention_days": 3}"#);
        let app = AppConfig::load_from(&good).unwrap();
        assert_eq!(app.run.timeout_ms, Some(5000));
        assert_eq!(app.run.poll_ms, 30);
        assert_eq!(app.logging.retention_days, 3);
        assert_eq!(app.limits, LimitsConfig::default());

        let empty = section_dir("e2e-empty");
        assert_eq!(AppConfig::load_from(&empty).unwrap(), AppConfig::default());
    }

    #[test]
    fn validate_catches_bad_values() {
        let bad_log = AppConfig {
            logging: LoggingConfig { per_file_bytes: 1, ..Default::default() },
            ..Default::default()
        };
        assert!(bad_log.validate().iter().any(|m| m.contains("per_file_bytes")));

        let bad_retention = AppConfig {
            logging: LoggingConfig { retention_days: 400, ..Default::default() },
            ..Default::default()
        };
        assert!(bad_retention.validate().iter().any(|m| m.contains("365")));

        // poll > grace：有界等待会超跑一个 poll，必须拒绝。
        let poll_over_grace = AppConfig {
            run: RunConfig { shutdown_grace_ms: 10, poll_ms: 100, ..Default::default() },
            ..Default::default()
        };
        assert!(poll_over_grace
            .validate()
            .iter()
            .any(|m| m.contains("不能大于 run.shutdown_grace_ms")));

        let bad_cache = AppConfig {
            caching: CachingConfig { directory: Some(PathBuf::from("relative/dir")), ..Default::default() },
            ..Default::default()
        };
        assert_eq!(
            bad_cache.validate(),
            vec!["caching.directory 必须是绝对路径".to_string()]
        );

        // 默认配置必须自洽（模板值与代码默认同值、全部落在界内）。
        assert!(AppConfig::default().validate().is_empty());
    }

    #[test]
    fn limits_apply_to_sandbox_config() {
        let mut cfg = SandboxConfig::default();
        LimitsConfig {
            max_memory_bytes: Some(1 << 30),
            max_children: Some(3),
            ..Default::default()
        }
        .apply_to(&mut cfg);
        assert_eq!(cfg.max_memory_bytes, Some(1 << 30));
        assert_eq!(cfg.max_children, Some(3));
        assert_eq!(cfg.max_cpu_ms, None);
    }
}