//! # 进程沙箱：统一能力模型（Mental Model）
//!
//! 1. [`SandboxConfig`] 的每个字段对应一个 [`SandboxCapability`]（顺序无关）。
//! 2. 每个平台实现 [`setup`](platform_setup)：
//!    只 **覆盖** 自己支持的能力，其余能力保持 [`ReportSet::from_config`] 给出的
//!    “不支持”默认态（请求了 → `Error`，未请求 → `Skipped`）。
//! 3. [`ReportSet`] 按键登记，杜绝“位置数组错位 → 能力映射错乱”的维护风险；
//!    新增能力不会影响既有平台代码的顺序。
//! 4. 互相矛盾的能力组合由 [`SandboxConfig::validate`] 在开发期用 `assert!` 报出。
//!
//! ## 如何新增一项沙箱能力（维护清单）
//!
//! 1. 在 [`SandboxCapability`] 增加一个变体，并在 [`SandboxCapability::ALL`] 尾部追加。
//! 2. 在 [`SandboxCapability::as_str`] 增加人类可读名。
//! 3. 在 [`SandboxConfig`] 增加对应字段。
//! 4. 在 [`SandboxConfig::is_requested`] 登记“如何判断该项被请求”。
//! 5. 如与既有选项互斥，在 [`SandboxConfig::validate`] 增加断言。
//! 6. 各平台在 `setup` 里用 [`ReportSet::override_status`] 覆盖其能实现的能力；
//!    无法实现的能力自动保持“不支持 → Error”，无需编写。
//! 7. 在 `whitebox_core/tests/sandbox.rs` 增加覆盖测试。
//!
//! 所有宏 / cfg 均按平台互斥编译（一个目标系统只会启用一份 `setup`），
//! `libc` 常量与 `setrlimit` 等在调用点直接配对，杜绝跨平台类型冲突。

#[cfg(unix)]
mod unix_common;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// 统一定义的沙箱能力键。
/// 顺序本身无约束；各平台通过 [`ReportSet`] 按键登记，新增能力不影响任何既有代码顺序。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SandboxCapability {
    // ---- 文件系统（capability 式，可叠加） ----
    /// 路径级读写白名单（未列出路径默认拒绝）
    FileAccess,
    /// 全局只读文件系统（拒绝一切写入）
    ReadonlyFileSystem,
    /// 完全封闭文件系统（拒绝一切文件访问）
    FilesystemSealed,
    /// 文件系统根重定向（chroot 类）
    FilesystemRoot,
    /// 允许写系统临时目录
    TempWrite,
    // ---- 网络（四种形态互斥，见 `SandboxConfig::validate`） ----
    /// 完全禁止网络
    Network,
    /// 端口段级放行 / 封禁
    NetworkPorts,
    /// 仅回环 / 本机网络
    LoopbackOnly,
    /// 仅允许 DNS
    DnsOnly,
    // ---- 进程 ----
    /// 禁止执行新程序 / 派生进程
    NoExec,
    /// 子进程数上限
    MaxChildren,
    /// 禁止获得新特权
    NoNewPrivileges,
    /// 禁止调试 / ptrace / core dump
    AntiDebug,
    // ---- 资源 ----
    /// 最大内存（字节）
    MaxMemory,
    /// 打开文件数上限
    MaxOpenFiles,
    /// 最大 CPU 时间（毫秒）
    MaxCpuTime,
    /// 单文件写入大小上限（字节）
    MaxFileSize,
    /// 核心转储大小上限（字节，0 即关闭）
    MaxCoreSize,
    // ---- 内核 / 系统加固 ----
    /// seccomp 严格模式（拒绝一组危险 syscall）
    StrictSyscalls,
    /// Windows: 禁用 Win32k 系统调用
    NoWin32k,
    /// Windows: 仅放行微软签名二进制
    MsSignedOnly,
    /// Windows: 严格句柄校验
    StrictHandles,
}

impl SandboxCapability {
    /// 所有能力。平台实现按能力键登记，因此顺序仅影响报告输出，不影响正确性。
    pub const ALL: [SandboxCapability; 22] = [
        SandboxCapability::FileAccess,
        SandboxCapability::ReadonlyFileSystem,
        SandboxCapability::FilesystemSealed,
        SandboxCapability::FilesystemRoot,
        SandboxCapability::TempWrite,
        SandboxCapability::Network,
        SandboxCapability::NetworkPorts,
        SandboxCapability::LoopbackOnly,
        SandboxCapability::DnsOnly,
        SandboxCapability::NoExec,
        SandboxCapability::MaxChildren,
        SandboxCapability::NoNewPrivileges,
        SandboxCapability::AntiDebug,
        SandboxCapability::MaxMemory,
        SandboxCapability::MaxOpenFiles,
        SandboxCapability::MaxCpuTime,
        SandboxCapability::MaxFileSize,
        SandboxCapability::MaxCoreSize,
        SandboxCapability::StrictSyscalls,
        SandboxCapability::NoWin32k,
        SandboxCapability::MsSignedOnly,
        SandboxCapability::StrictHandles,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SandboxCapability::FileAccess => "file_access",
            SandboxCapability::ReadonlyFileSystem => "readonly_fs",
            SandboxCapability::FilesystemSealed => "filesystem_sealed",
            SandboxCapability::FilesystemRoot => "filesystem_root",
            SandboxCapability::TempWrite => "temp_allow_write",
            SandboxCapability::Network => "deny_network",
            SandboxCapability::NetworkPorts => "network_ports",
            SandboxCapability::LoopbackOnly => "loopback_only",
            SandboxCapability::DnsOnly => "dns_only",
            SandboxCapability::NoExec => "deny_exec",
            SandboxCapability::MaxChildren => "max_children",
            SandboxCapability::NoNewPrivileges => "no_new_privileges",
            SandboxCapability::AntiDebug => "anti_debug",
            SandboxCapability::MaxMemory => "max_memory",
            SandboxCapability::MaxOpenFiles => "max_open_files",
            SandboxCapability::MaxCpuTime => "max_cpu_time",
            SandboxCapability::MaxFileSize => "max_file_size",
            SandboxCapability::MaxCoreSize => "max_core_size",
            SandboxCapability::StrictSyscalls => "strict_syscalls",
            SandboxCapability::NoWin32k => "no_win32k",
            SandboxCapability::MsSignedOnly => "ms_signed_only",
            SandboxCapability::StrictHandles => "strict_handles",
        }
    }
}

/// 单个配置项的应用结果。
///
/// - `Applied`：请求了，并且已成功应用。
/// - `Skipped`：配置中没有请求该项。
/// - `Error`：请求了，但当前平台 / 内核 / 权限不支持，无法应用（即“不支持 → error”）。
/// - `Denied`：请求了，平台本身支持，但当前权限不足（如 Windows 管理员进程无法施加
///   `JOB_OBJECT_SECURITY_NO_ADMIN`）；与“不支持”区分，便于上层给出可行动的提示。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStatus {
    Applied,
    Skipped,
    Error,
    Denied,
}

/// 与配置项一一对应的能力检查报告
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityReport {
    pub capability: SandboxCapability,
    pub status: ApplyStatus,
}

/// 端口段（含端点）。
pub type PortRange = (u16, u16);

/// 端口级网络策略。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NetworkPorts {
    /// 不限制端口
    #[default]
    Default,
    /// 仅允许这些端口（段）建立对外 TCP 连接
    Allow(Vec<PortRange>),
    /// 禁止这些端口（段），其余允许
    Deny(Vec<PortRange>),
}

impl NetworkPorts {
    /// 是否请求了端口级策略
    pub fn is_requested(&self) -> bool {
        !matches!(self, NetworkPorts::Default)
    }
}

/// 端口区间 `[lo, hi]` 是否被 `ranges` 的并集完整覆盖。
/// Whether `[lo, hi]` is fully covered by the union of `ranges`.
fn range_covered(ranges: &[PortRange], lo: u16, hi: u16) -> bool {
    let mut cursor = lo;
    loop {
        let next_end = ranges
            .iter()
            .filter(|(a, b)| *a <= cursor && cursor <= *b)
            .map(|(_, b)| *b)
            .max();
        match next_end {
            None => return false,
            // `end == u16::MAX` 时 `end >= hi` 恒成立（hi <= MAX），上一分支已覆盖，无需特判。
            Some(end) if end >= hi => return true,
            Some(end) => cursor = end + 1,
        }
    }
}

/// `ranges` 是否覆盖整个端口空间 `0..=65535`。
/// Whether `ranges` covers the whole port space.
fn ranges_cover_all(ranges: &[PortRange]) -> bool {
    range_covered(ranges, 0, u16::MAX)
}

/// WASI sockets（L2，guest 可见的 `wasi:sockets`）配置。默认 `None`（完全禁用）。
///
/// 与 L1 的 [`SandboxConfig::network_ports`] 求交：L1 是进程级粗粒度封禁，L2 是
/// guest 侧细粒度授权；两者同时出现时必须一致（见 [`SandboxConfig::validate`]）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WasiSockets {
    /// 完全禁用 sockets（默认）
    #[default]
    None,
    /// 仅允许连接这些远程端口（段）
    Allow(Vec<PortRange>),
    /// 允许除这些远程端口（段）外的全部
    Deny(Vec<PortRange>),
    /// 仅允许回环地址（任意端口）
    LoopbackOnly,
}

/// WASI clocks（L2）。默认 `Host`（宿主时钟）；`Deterministic` 用于可复现运行。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WasiClocks {
    /// 使用宿主墙钟 / 单调钟（默认）
    #[default]
    Host,
    /// 确定性时钟：wall 恒为 0；monotonic 初值取 seed 低位，**每次读取 +1ms**
    /// （避免 guest 等待时间永不动而空转）。同 seed → 同序列，可复现。
    ///
    /// 未实现期许 / original aspiration: 最初设计期望 monotonic **恒为 0（完全冻结）**；
    /// 当前为保证时间推进改为每次 +1ms。若将来需要严格冻结，需另行解决 guest 超时空转。
    Deterministic(u128),
}

/// WASI random（L2）。默认 `Os`（安全流 `wasi:random/random` 用 OS 真随机）。
///
/// `Seeded` 只播种**非安全流**（`wasi:random/insecure` + `insecure-seed`），使其确定性、可复现；
/// 安全流仍用 OS 播种的 CSPRNG，**不因种子降级为可预测**（符合 WASI 语义）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WasiRandom {
    /// 宿主熵源（默认）：安全流 OS 真随机，非安全流 OS 播种 PRNG
    #[default]
    Os,
    /// 播种非安全流（`wasi:random/insecure`）使其可复现；安全流保持真随机
    Seeded(u128),
}

/// 一条路径访问规则（capability 式）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRule {
    /// 必须是绝对路径；目录规则递归生效
    pub path: PathBuf,
    /// 允许读
    pub read: bool,
    /// 允许写（readonly_fs 开启时全局禁止写，规则内的 write 也会被压制）
    pub write: bool,
}

impl FsRule {
    /// 构造一条路径规则
    pub fn new(path: PathBuf, read: bool, write: bool) -> Self {
        Self { path, read, write }
    }
}

/// 文件系统路径白名单。为空表示不限制（保持默认行为）；
/// 非空表示“只允许这些路径按规则访问，未列出的路径默认拒绝”。
pub type FsAccess = Vec<FsRule>;

/// WASI 标准流模式（L2，guest 可见的 stdio）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WasiStdio {
    /// 继承宿主进程的对应流
    #[default]
    Inherit,
    /// 捕获到内存（run 后经结果通道取回；线程路径落回宿主 stdout，进程路径经 `wait_output` 取回）
    Capture,
    /// 丢弃（null）
    Null,
}

/// WASI 预打开目录（细粒度：guest 路径 + 读/写权限）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WasiPreopen {
    /// 宿主端目录（必须可被进程沙箱读取/写入，否则配置失败 → 报错终止）
    pub host_path: PathBuf,
    /// guest 侧可见路径
    pub guest_path: String,
    /// 允许读
    pub read: bool,
    /// 允许写
    pub write: bool,
}

impl WasiPreopen {
    pub fn new(
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        read: bool,
        write: bool,
    ) -> Self {
        Self {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
            read,
            write,
        }
    }
}

/// 统一的进程沙箱限制配置（尽可能细粒度、广兼容）。
///
/// 冲突组合由 [`SandboxConfig::validate`] 拦截，开发期通过 [`setup_sandbox_with`]
/// 里的 `assert!`（expect 语义）明确告知开发者。
///
/// 构建示例：
/// ```no_run
/// use std::path::PathBuf;
/// use whitebox_core::sandbox::{FsRule, NetworkPorts, SandboxConfig};
///
/// let config = SandboxConfig::new()
///     .allow_fs_read(PathBuf::from("/etc/hosts"))
///     .allow_fs_read_write(PathBuf::from("/data/output"))
///     .allow_network_ports(vec![(443, 443), (8000, 9000)])
///     .max_memory(256 * 1024 * 1024)
///     .deny_exec(true)
///     .anti_debug(true);
/// ```
///
/// ## L1（进程/OS 沙箱）与 L2（WASI）的失败语义不同
/// - L1（`setup_sandbox`）：尽力而为，配置失败只报告不中止（已无法再收紧）；
/// - L2（WASI，`wasi_*` 字段）：**必须成功配置**，失败即报错终止（fail-closed）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxConfig {
    // ---- 文件系统 ----
    /// 路径级文件访问白名单（设置 `fs_root` 时，这些路径在 chroot 之后解析，即相对新根）。
    pub fs_rules: FsAccess,
    /// 全局只读
    pub readonly_fs: bool,
    /// 完全封闭文件系统
    pub deny_file_access: bool,
    /// 文件系统根重定向（要求特权/平台支持，否则 Error）。设置后 `fs_rules` 路径按新根解析。
    pub fs_root: Option<PathBuf>,
    /// 允许写应用临时目录（worker 继承父进程 temp 路径；Linux 优先每用户 runtime）。
    pub temp_allow_write: bool,
    /// 编译缓存目录（宿主侧解析，随配置传入 worker）。
    ///
    /// 由 [`SandboxConfig::enable_compile_cache`] 设置；为 `None` 表示不启用。
    /// 启用时该目录会在 L1 密封/白名单模式下被加入读写白名单。
    /// Compile-cache directory (resolved host-side and passed to the worker). Set by
    /// [`SandboxConfig::enable_compile_cache`]; `None` means disabled. When enabled it is
    /// added to the L1 read/write allowlist under sealed/allowlist modes.
    #[serde(default)]
    pub compile_cache_dir: Option<PathBuf>,
    // ---- 网络 ----
    /// 完全禁止网络
    pub deny_network: bool,
    /// 端口级策略
    pub network_ports: NetworkPorts,
    /// 仅回环 / 本机
    pub localhost_only: bool,
    /// 仅允许 DNS（53 端口）
    pub dns_only: bool,
    // ---- 进程 ----
    /// 禁止派生 / 执行新程序
    pub deny_exec: bool,
    /// 子进程数上限
    pub max_children: Option<u32>,
    /// 禁止获得新特权
    pub no_new_privileges: bool,
    /// 禁止调试 / core dump
    pub anti_debug: bool,
    // ---- 资源 ----
    /// 最大内存（字节）
    pub max_memory_bytes: Option<u64>,
    /// 打开文件数上限
    pub max_open_files: Option<u64>,
    /// 最大 CPU 时间（毫秒）
    pub max_cpu_ms: Option<u64>,
    /// 单文件写入大小上限（字节）
    pub max_file_size_bytes: Option<u64>,
    /// 核心转储上限（字节）
    pub max_core_bytes: Option<u64>,
    // ---- 内核加固 ----
    /// seccomp 严格模式
    pub strict_syscalls: bool,
    /// Windows: 禁用 Win32k 系统调用
    pub block_win32k: bool,
    /// Windows: 仅放行微软签名二进制
    pub block_non_microsoft_binaries: bool,
    /// Windows: 严格句柄校验
    pub strict_handle_checks: bool,
    // ---- WASI（L2，配置失败必须报错终止；默认最小 = 继承 stdio、无 env/args/preopen）----
    /// WASI 标准输入
    pub wasi_stdin: WasiStdio,
    /// WASI 标准输出
    pub wasi_stdout: WasiStdio,
    /// WASI 标准错误
    pub wasi_stderr: WasiStdio,
    /// WASI 可见的环境变量（显式白名单，不继承宿主）
    pub wasi_env: Vec<(String, String)>,
    /// WASI 命令行参数
    pub wasi_args: Vec<String>,
    /// WASI 预打开目录（细粒度 guest 路径 + 读写权限）
    pub wasi_preopens: Vec<WasiPreopen>,
    /// WASI sockets（L2；默认 `None` 完全禁用）
    pub wasi_sockets: WasiSockets,
    /// 是否允许 `wasi:sockets/ip-name-lookup`（DNS 解析）；默认 false
    pub wasi_ip_name_lookup: bool,
    /// WASI clocks（L2；默认 `Host`）
    pub wasi_clocks: WasiClocks,
    /// WASI random（L2；默认 `Os`）
    pub wasi_random: WasiRandom,
    /// `wasi:random` 单次最大字节数；None = 引擎默认（64 MiB）
    pub wasi_max_random_size: Option<u64>,
    /// guest 初始工作目录（`wasi:cli` initial-cwd）；None = 引擎默认（"/"）
    pub wasi_initial_cwd: Option<String>,
    /// 是否允许 WASI 阻塞调用直接阻塞当前线程（false = 走 spawn_blocking）
    pub wasi_allow_blocking_current_thread: bool,
}

impl Default for SandboxConfig {
    /// 默认值：**全关闭 / Deny-by-Default** —— 每个能力都默认禁止，
    /// 必须通过各 `with_*` 方法显式授权才能放行。
    ///
    /// ```
    /// use whitebox_core::sandbox::SandboxConfig;
    /// let cfg = SandboxConfig::default();
    /// assert!(cfg.deny_network);
    /// assert!(cfg.deny_file_access);
    /// assert!(cfg.deny_exec);
    /// assert!(cfg.no_new_privileges);
    /// ```
    fn default() -> Self {
        Self {
            // 文件系统：默认封闭（显式 allow_fs_read / allow_fs_read_write 按路径放行）
            fs_rules: FsAccess::new(),
            readonly_fs: false,
            deny_file_access: true,
            fs_root: None,
            temp_allow_write: false,
            compile_cache_dir: None,
            // 网络：默认完全关闭（显式 allow_network_ports / loopback_only / dns_only 放行）
            deny_network: true,
            network_ports: NetworkPorts::Default,
            localhost_only: false,
            dns_only: false,
            // 进程：默认禁执行 / 禁提权 / 防调试
            deny_exec: true,
            max_children: None,
            no_new_privileges: true,
            anti_debug: true,
            // 资源上限：默认不额外限制（可经 builder 显式设定）
            max_memory_bytes: None,
            max_open_files: None,
            max_cpu_ms: None,
            max_file_size_bytes: None,
            max_core_bytes: None,
            // 内核加固属叠加选项，默认关闭（seccomp / Win32k / 签名 / 句柄）
            strict_syscalls: false,
            block_win32k: false,
            block_non_microsoft_binaries: false,
            strict_handle_checks: false,
            // WASI：默认最小 —— 继承 stdio，无 env/args/preopen
            wasi_stdin: WasiStdio::Inherit,
            wasi_stdout: WasiStdio::Inherit,
            wasi_stderr: WasiStdio::Inherit,
            wasi_env: Vec::new(),
            wasi_args: Vec::new(),
            wasi_preopens: Vec::new(),
            wasi_sockets: WasiSockets::None,
            wasi_ip_name_lookup: false,
            wasi_clocks: WasiClocks::Host,
            wasi_random: WasiRandom::Os,
            wasi_max_random_size: None,
            wasi_initial_cwd: None,
            wasi_allow_blocking_current_thread: false,
        }
    }
}

impl SandboxConfig {
    /// 该能力是否被请求（新增能力时在此登记）。
    pub fn is_requested(&self, capability: SandboxCapability) -> bool {
        match capability {
            SandboxCapability::FileAccess => !self.fs_rules.is_empty(),
            SandboxCapability::ReadonlyFileSystem => self.readonly_fs,
            SandboxCapability::FilesystemSealed => self.deny_file_access,
            SandboxCapability::FilesystemRoot => self.fs_root.is_some(),
            SandboxCapability::TempWrite => self.temp_allow_write,
            SandboxCapability::Network => self.deny_network,
            SandboxCapability::NetworkPorts => self.network_ports.is_requested(),
            SandboxCapability::LoopbackOnly => self.localhost_only,
            SandboxCapability::DnsOnly => self.dns_only,
            SandboxCapability::NoExec => self.deny_exec,
            SandboxCapability::MaxChildren => self.max_children.is_some(),
            SandboxCapability::NoNewPrivileges => self.no_new_privileges,
            SandboxCapability::AntiDebug => self.anti_debug,
            SandboxCapability::MaxMemory => self.max_memory_bytes.is_some(),
            SandboxCapability::MaxOpenFiles => self.max_open_files.is_some(),
            SandboxCapability::MaxCpuTime => self.max_cpu_ms.is_some(),
            SandboxCapability::MaxFileSize => self.max_file_size_bytes.is_some(),
            SandboxCapability::MaxCoreSize => self.max_core_bytes.is_some(),
            SandboxCapability::StrictSyscalls => self.strict_syscalls,
            SandboxCapability::NoWin32k => self.block_win32k,
            SandboxCapability::MsSignedOnly => self.block_non_microsoft_binaries,
            SandboxCapability::StrictHandles => self.strict_handle_checks,
        }
    }

    /// 检查互斥 / 矛盾组合，返回冲突说明列表（空 = 合法）。
    ///
    /// 默认全关闭（deny-by-default）：`deny_file_access`/`deny_network` 是“基线拒绝”，
    /// `fs_rules` / `network_ports` 等是**显式授权**，二者可以共存（基线下放行特定项）。
    /// 仍互斥的组合：`deny_file_access` 与 `fs_root`；`fs_root` 与 `temp_allow_write`；
    /// `anti_debug` 与“允许 core dump”；`deny_exec` 与 `max_children`。
    pub fn validate(&self) -> Vec<String> {
        let mut violations = Vec::new();

        if self.deny_file_access && self.fs_root.is_some() {
            violations.push("deny_file_access 与 fs_root 互斥".into());
        }
        if self.deny_file_access && !self.wasi_preopens.is_empty() {
            violations.push(
                "deny_file_access 与 wasi_preopens 冲突：封闭文件系统下任何目录都无法预打开".into(),
            );
        }
        if self.fs_root.is_some() && self.temp_allow_write {
            violations.push(
                "fs_root 与 temp_allow_write 互斥：chroot/根重定向后系统临时目录已重映射到根内部"
                    .into(),
            );
        }
        if self.compile_cache_dir.is_some() && self.fs_root.is_some() {
            violations.push(
                "compile_cache_dir 与 fs_root 冲突：chroot 后宿主缓存路径不可达".into(),
            );
        }
        if self.anti_debug && matches!(self.max_core_bytes, Some(n) if n != 0) {
            violations.push(
                "anti_debug 与 max_core_bytes 互斥：anti_debug 强制禁止 core dump，\
                 max_core_bytes 只能为 None 或 0"
                    .into(),
            );
        }
        if self.deny_exec && self.max_children.is_some() {
            violations
                .push("deny_exec 与 max_children 互斥：deny_exec 已隐含 0 个子进程上限".into());
        }
        // L2 sockets 与 L1 网络求交：grant 给 guest 的网络能力不得超出 L1 已放行的范围。
        // L1 视为“已放行网络”的条件：显式端口授权（Allow 是基线 deny 下的 grant），
        // 或在基线下不设 deny_network。deny_network=true 且 network_ports 未 Allow → 未放行。
        let l1_network_open =
            !self.deny_network || matches!(self.network_ports, NetworkPorts::Allow(_));

        match &self.wasi_sockets {
            WasiSockets::None => {}
            _ if !l1_network_open => violations.push(
                "wasi_sockets 需要 L1 网络授权：deny_network=true 且 network_ports 未显式 Allow".into(),
            ),
            WasiSockets::Allow(ports) => match &self.network_ports {
                NetworkPorts::Allow(l1_ports) => {
                    for (lo, hi) in ports {
                        if !l1_ports.iter().any(|(a, b)| lo >= a && hi <= b) {
                            violations.push(format!(
                                "wasi_sockets 端口 {lo}-{hi} 超出 L1 network_ports 授权"
                            ));
                        }
                    }
                }
                NetworkPorts::Deny(l1_denied) => {
                    for (lo, hi) in ports {
                        if l1_denied.iter().any(|(a, b)| lo <= b && a <= hi) {
                            violations.push(format!(
                                "wasi_sockets 端口 {lo}-{hi} 与 L1 network_ports 拒绝项重叠"
                            ));
                        }
                    }
                }
                NetworkPorts::Default => {}
            },
            // L2 `Deny(d2)` 放行“除 d2 外全部”；它不可能窄于 L1 `Allow(部分端口)`。
            // L2 `Deny(d2)` allows everything outside `d2`; it cannot be narrower than an
            // L1 `Allow(partial)` set.
            WasiSockets::Deny(denied) => match &self.network_ports {
                NetworkPorts::Default => {}
                NetworkPorts::Allow(l1_ports) => {
                    if !ranges_cover_all(l1_ports) {
                        violations.push(
                            "wasi_sockets::Deny 过宽：L1 network_ports::Allow 只授权了部分端口"
                                .into(),
                        );
                    }
                }
                NetworkPorts::Deny(l1_denied) => {
                    for (lo, hi) in l1_denied {
                        if !range_covered(denied, *lo, *hi) {
                            violations.push(format!(
                                "wasi_sockets::Deny 未覆盖 L1 拒绝端口 {lo}-{hi}：L2 比 L1 更宽"
                            ));
                        }
                    }
                }
            },
            // L2 `LoopbackOnly` 回环任意端口；只有 L1 全放行（或不拒绝任何端口）时才不越权。
            // L2 `LoopbackOnly` allows every loopback port; only safe when L1 allows all.
            WasiSockets::LoopbackOnly => match &self.network_ports {
                NetworkPorts::Default => {}
                NetworkPorts::Allow(l1_ports) => {
                    if !ranges_cover_all(l1_ports) {
                        violations.push(
                            "wasi_sockets::LoopbackOnly 过宽：L1 network_ports::Allow 未放行全部端口"
                                .into(),
                        );
                    }
                }
                NetworkPorts::Deny(l1_denied) => {
                    if !l1_denied.is_empty() {
                        violations.push(
                            "wasi_sockets::LoopbackOnly 与 L1 network_ports::Deny 冲突：回环任一端口可能落在拒绝项"
                                .into(),
                        );
                    }
                }
            },
        }
        if self.wasi_ip_name_lookup && matches!(self.wasi_sockets, WasiSockets::None) {
            violations.push(
                "wasi_ip_name_lookup 与 wasi_sockets::None 冲突：禁用 sockets 时无法解析 DNS".into(),
            );
        }
        if self.wasi_ip_name_lookup && !l1_network_open {
            violations.push("wasi_ip_name_lookup 需要 L1 网络授权（deny_network/network_ports）".into());
        }

        violations
    }

    /// 完全开放的空配置：所有能力默认**不限制**（等价于旧的全默认值）。
    /// 与 [`SandboxConfig::default`]（全关闭）正好相反，用于明确表达“不套沙箱”。
    pub fn open() -> Self {
        Self {
            fs_rules: FsAccess::new(),
            readonly_fs: false,
            deny_file_access: false,
            fs_root: None,
            temp_allow_write: false,
            compile_cache_dir: None,
            deny_network: false,
            network_ports: NetworkPorts::Default,
            localhost_only: false,
            dns_only: false,
            deny_exec: false,
            max_children: None,
            no_new_privileges: false,
            anti_debug: false,
            max_memory_bytes: None,
            max_open_files: None,
            max_cpu_ms: None,
            max_file_size_bytes: None,
            max_core_bytes: None,
            strict_syscalls: false,
            block_win32k: false,
            block_non_microsoft_binaries: false,
            strict_handle_checks: false,
            wasi_stdin: WasiStdio::Inherit,
            wasi_stdout: WasiStdio::Inherit,
            wasi_stderr: WasiStdio::Inherit,
            wasi_env: Vec::new(),
            wasi_args: Vec::new(),
            wasi_preopens: Vec::new(),
            wasi_sockets: WasiSockets::Allow(vec![(0, 65535)]),
            wasi_ip_name_lookup: true,
            wasi_clocks: WasiClocks::Host,
            wasi_random: WasiRandom::Os,
            wasi_max_random_size: None,
            wasi_initial_cwd: None,
            wasi_allow_blocking_current_thread: true,
        }
    }

    /// 全关闭的默认配置（同 [`SandboxConfig::default`]，即“不授权就全不可用”）。
    pub fn new() -> Self {
        Self::default()
    }

    // ---- WASI 配置（L2：配置失败必须报错终止）----

    /// WASI 标准输入模式
    pub fn wasi_stdin(mut self, mode: WasiStdio) -> Self {
        self.wasi_stdin = mode;
        self
    }

    /// WASI 标准输出模式
    pub fn wasi_stdout(mut self, mode: WasiStdio) -> Self {
        self.wasi_stdout = mode;
        self
    }

    /// WASI 标准错误模式
    pub fn wasi_stderr(mut self, mode: WasiStdio) -> Self {
        self.wasi_stderr = mode;
        self
    }

    /// 一次性设置 stdin/stdout/stderr 为同一模式（常见简写）。
    ///
    /// ```
    /// use whitebox_core::sandbox::{SandboxConfig, WasiStdio};
    /// let cfg = SandboxConfig::new().wasi_stdio(WasiStdio::Null);
    /// assert_eq!(cfg.wasi_stdin, WasiStdio::Null);
    /// assert_eq!(cfg.wasi_stdout, WasiStdio::Null);
    /// assert_eq!(cfg.wasi_stderr, WasiStdio::Null);
    /// ```
    pub fn wasi_stdio(mut self, mode: WasiStdio) -> Self {
        self.wasi_stdin = mode;
        self.wasi_stdout = mode;
        self.wasi_stderr = mode;
        self
    }

    /// 追加一条 WASI 环境变量（可重复调用）
    pub fn wasi_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.wasi_env.push((key.into(), value.into()));
        self
    }

    /// 设置 WASI 命令行参数
    pub fn wasi_args(mut self, args: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.wasi_args = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        self
    }

    /// WASI sockets 策略（默认 `None`）。与 L1 `deny_network`/`network_ports` 求交，冲突见 `validate`。
    pub fn wasi_sockets(mut self, policy: WasiSockets) -> Self {
        self.wasi_sockets = policy;
        self
    }

    /// WASI clocks 策略（默认 `Host`）。`Deterministic` 让 guest 看到固定时钟，便于可复现运行。
    pub fn wasi_clocks(mut self, clocks: WasiClocks) -> Self {
        self.wasi_clocks = clocks;
        self
    }

    /// WASI random 策略（默认 `Os`）。`Seeded` 让 guest 看到确定性随机，便于可复现运行（**不安全**）。
    pub fn wasi_random(mut self, random: WasiRandom) -> Self {
        self.wasi_random = random;
        self
    }

    /// 是否允许 `wasi:sockets/ip-name-lookup`（DNS 解析，默认禁用）
    pub fn wasi_ip_name_lookup(mut self, enabled: bool) -> Self {
        self.wasi_ip_name_lookup = enabled;
        self
    }

    /// `wasi:random` 单次最大字节数（默认引擎值 64 MiB）
    pub fn wasi_max_random_size(mut self, bytes: u64) -> Self {
        self.wasi_max_random_size = Some(bytes);
        self
    }

    /// guest 初始工作目录（`wasi:cli` initial-cwd，默认引擎值 "/"）
    pub fn wasi_initial_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.wasi_initial_cwd = Some(cwd.into());
        self
    }

    /// 是否允许 WASI 阻塞调用直接阻塞当前线程（默认 false，走 spawn_blocking）
    pub fn wasi_allow_blocking_current_thread(mut self, enabled: bool) -> Self {
        self.wasi_allow_blocking_current_thread = enabled;
        self
    }

    /// 追加一条 WASI 预打开目录（可重复调用）。宿主目录不可用/无权限 → 配置阶段报错终止。
    ///
    /// 注意：`SandboxConfig::default()` 默认 `deny_file_access = true`（L1 完全封闭文件系统），
    /// 与预打开冲突——`validate()` 会拒绝。**只读场景请优先用
    /// [`SandboxConfig::wasi_preopen_ro`]**：它会同步 L1 只读 + 路径白名单，避免全局解封；
    /// 需要读写时再显式 `sealed_fs(false)` 并自行保证 L1 授权，或直接使用
    /// [`SandboxConfig::open`]。
    pub fn wasi_preopen(
        mut self,
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        read: bool,
        write: bool,
    ) -> Self {
        self.wasi_preopens
            .push(WasiPreopen::new(host_path, guest_path, read, write));
        self
    }

    /// 追加一条**只读**预打开，并同步收紧 L1 文件系统语义（可重复调用）。
    ///
    /// 内部一次完成：`deny_file_access=false`（解除全局封盘）+ `readonly_fs=true` +
    /// 对 `host` 的 L1 读白名单 + L2 只读 preopen。集中实现 L1∩L2 不变量，避免调用方
    /// 各自拼装导致“全局解封”之类的过度授权。Windows 的 L1 文件系统能力未实现，故
    /// 跳过 L1 两项以免产生无意义的 “unsupported” 报告；L2 preopen 仍严格只读。
    ///
    /// Append a **read-only** preopen and keep L1 in sync: clear full sealing, force the
    /// FS read-only, grant an L1 read rule for `host`, then register the L2 read-only
    /// preopen. Centralizes the L1∩L2 invariant so callers cannot over-unseal by accident.
    /// Windows has no L1 filesystem primitive, so the L1 bits are skipped there to avoid
    /// spurious "unsupported" reports; the L2 preopen is still strictly read-only.
    pub fn wasi_preopen_ro(
        mut self,
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
    ) -> Self {
        let host_path = host_path.into();
        self.deny_file_access = false;
        #[cfg(not(windows))]
        {
            self.readonly_fs = true;
            self.fs_rules.push(FsRule::new(host_path.clone(), true, false));
        }
        self.wasi_preopens
            .push(WasiPreopen::new(host_path, guest_path, true, false));
        self
    }

    /// 追加一条“只读某路径”规则（可重复调用）
    pub fn allow_fs_read(mut self, path: impl Into<PathBuf>) -> Self {
        self.fs_rules.push(FsRule::new(path.into(), true, false));
        self
    }

    /// 追加一条“读写某路径”规则（可重复调用）
    pub fn allow_fs_read_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.fs_rules.push(FsRule::new(path.into(), true, true));
        self
    }

    /// 启用 wasmtime 编译缓存（简易选项）：宿主侧解析并创建 `<cache_dir>/wasmtime`，
    /// 写入 [`SandboxConfig::compile_cache_dir`]，并在 L1 密封/白名单模式下把该目录加入
    /// 读写白名单。
    ///
    /// 缓存放在**每用户 cache 目录**（portable 为 `<exe_dir>/cache`），不使用共享 `/tmp`；
    /// Unix 上额外 `protect_dir` 收紧为属主专用（纵深防御）。
    /// 不满足条件时**静默不启用**（返回原配置）：`fs_root`（chroot 后宿主路径不可达）、
    /// `readonly_fs`（缓存无法写入）、`dir::cache_dir()` / `ensure_dir` / `protect_dir` 失败。
    /// 未启用时可从 `compile_cache_dir == None` 观察到。
    ///
    /// Enable the wasmtime compile cache (simple option): resolve and create
    /// `<cache_dir>/wasmtime` on the host, record it in `compile_cache_dir`, and add it to
    /// the L1 read/write allowlist under sealed/allowlist modes. The cache lives in the
    /// per-user cache dir (portable: `<exe_dir>/cache`), not shared `/tmp`; on Unix we also
    /// `protect_dir` it as defense-in-depth. Silently stays disabled when
    /// `fs_root`/`readonly_fs`/directory resolution/hardening fails.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn enable_compile_cache(self) -> Self {
        self.enable_compile_cache_at(None)
    }

    /// 同 [`SandboxConfig::enable_compile_cache`]，但允许指定缓存目录（绝对路径）。
    /// 目录仍会经 `ensure_dir`/`protect_dir` 收紧（缓存中毒防御不因覆盖而豁免）；
    /// `dir_override` 为 `None` 时用默认 `<cache_dir>/wasmtime`。
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn enable_compile_cache_at(mut self, dir_override: Option<&Path>) -> Self {
        // chroot 后宿主缓存路径不可达；全局只读则缓存无法写入。
        if self.fs_root.is_some() || self.readonly_fs {
            return self;
        }
        let dir = match dir_override {
            Some(dir) => dir.to_path_buf(),
            None => {
                let Ok(cache_root) = crate::dir::cache_dir() else {
                    return self;
                };
                cache_root.join("wasmtime")
            }
        };
        if crate::dir::ensure_dir(&dir).is_err() {
            return self;
        }
        // Unix：收紧为属主专用（纵深防御）；失败则不启用（fail-closed）。
        // On Unix, tighten to owner-only as defense-in-depth; failure disables the cache.
        #[cfg(unix)]
        if crate::dir::protect_dir(&dir).is_err() {
            return self;
        }
        // portable + Windows：缓存位于 `<exe_dir>/cache`，可能落在共享可写目录；同样收紧
        // 为属主专用（TreeSet 递归，故只在 portable 模式下承担该成本）。
        // portable + Windows: the cache lives under `<exe_dir>/cache` which may be shared;
        // tighten it too (TreeSet is recursive, so only portable mode pays this).
        #[cfg(windows)]
        if cfg!(feature = "portable") && crate::dir::protect_dir(&dir).is_err() {
            return self;
        }
        // 仅在 L1 已处于密封/白名单模式时追加规则；L1 全开时加规则反而会把沙箱收紧为
        // “只允许缓存目录”。Windows 的 L1 文件系统能力未实现，加规则只会产生无意义的
        // “unsupported” 报告，故跳过（缓存仍可被 worker 访问）。
        // Add the rule only under sealed/allowlist L1; on Windows L1 fs is unimplemented and
        // the rule would only produce a spurious "unsupported" report, so skip it there.
        #[cfg(not(windows))]
        if self.deny_file_access || !self.fs_rules.is_empty() {
            self.fs_rules.push(FsRule::new(dir.clone(), true, true));
        }
        self.compile_cache_dir = Some(dir);
        self
    }

    /// 移动端占位：目录布局未定义，暂不启用编译缓存。
    #[cfg(any(target_os = "android", target_os = "ios"))]
    pub fn enable_compile_cache(self) -> Self {
        self
    }

    /// 移动端占位：同 [`SandboxConfig::enable_compile_cache`]。
    #[cfg(any(target_os = "android", target_os = "ios"))]
    pub fn enable_compile_cache_at(self, _dir_override: Option<&Path>) -> Self {
        self
    }

    /// 全局只读文件系统
    pub fn readonly_fs(mut self, enabled: bool) -> Self {
        self.readonly_fs = enabled;
        self
    }

    /// 完全封闭文件系统（拒绝一切文件访问）
    pub fn sealed_fs(mut self, enabled: bool) -> Self {
        self.deny_file_access = enabled;
        self
    }

    /// 文件系统根重定向（chroot 类，要求特权 / 平台支持）
    pub fn fs_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.fs_root = Some(root.into());
        self
    }

    /// 允许写系统临时目录
    pub fn temp_allow_write(mut self, enabled: bool) -> Self {
        self.temp_allow_write = enabled;
        self
    }

    /// 完全禁止网络
    pub fn deny_network(mut self, enabled: bool) -> Self {
        self.deny_network = enabled;
        self
    }

    /// 仅允许这些端口段（对外 TCP）
    pub fn allow_network_ports(mut self, ranges: Vec<PortRange>) -> Self {
        self.network_ports = NetworkPorts::Allow(ranges);
        self
    }

    /// 禁止这些端口段，其余允许
    pub fn deny_network_ports(mut self, ranges: Vec<PortRange>) -> Self {
        self.network_ports = NetworkPorts::Deny(ranges);
        self
    }

    /// 仅回环 / 本机网络
    pub fn loopback_only(mut self, enabled: bool) -> Self {
        self.localhost_only = enabled;
        self
    }

    /// 仅允许 DNS
    pub fn dns_only(mut self, enabled: bool) -> Self {
        self.dns_only = enabled;
        self
    }

    /// 禁止执行新程序 / 派生进程
    pub fn deny_exec(mut self, enabled: bool) -> Self {
        self.deny_exec = enabled;
        self
    }

    /// 子进程数上限（deny_exec 时会被冲突检查拒绝）
    pub fn max_children(mut self, limit: u32) -> Self {
        self.max_children = Some(limit);
        self
    }

    /// 禁止获得新特权
    pub fn no_new_privileges(mut self, enabled: bool) -> Self {
        self.no_new_privileges = enabled;
        self
    }

    /// 禁止调试 / ptrace / core dump
    pub fn anti_debug(mut self, enabled: bool) -> Self {
        self.anti_debug = enabled;
        self
    }

    /// 最大内存（字节）
    pub fn max_memory(mut self, bytes: u64) -> Self {
        self.max_memory_bytes = Some(bytes);
        self
    }

    /// 打开文件数上限
    pub fn max_open_files(mut self, limit: u64) -> Self {
        self.max_open_files = Some(limit);
        self
    }

    /// 最大 CPU 时间（毫秒）
    pub fn max_cpu_ms(mut self, ms: u64) -> Self {
        self.max_cpu_ms = Some(ms);
        self
    }

    /// 单文件写入大小上限（字节）
    pub fn max_file_size(mut self, bytes: u64) -> Self {
        self.max_file_size_bytes = Some(bytes);
        self
    }

    /// 核心转储上限（字节）；0 即关闭
    pub fn max_core_size(mut self, bytes: u64) -> Self {
        self.max_core_bytes = Some(bytes);
        self
    }

    /// seccomp 严格模式
    pub fn strict_syscalls(mut self, enabled: bool) -> Self {
        self.strict_syscalls = enabled;
        self
    }

    /// Windows: 禁用 Win32k 系统调用
    pub fn block_win32k(mut self, enabled: bool) -> Self {
        self.block_win32k = enabled;
        self
    }

    /// Windows: 仅放行微软签名二进制
    pub fn ms_signed_only(mut self, enabled: bool) -> Self {
        self.block_non_microsoft_binaries = enabled;
        self
    }

    /// Windows: 严格句柄校验
    pub fn strict_handles(mut self, enabled: bool) -> Self {
        self.strict_handle_checks = enabled;
        self
    }
}

/// 全局统一的沙箱配置（只能设置一次，因为进程沙箱一般是单向的）
static GLOBAL_CONFIG: OnceLock<SandboxConfig> = OnceLock::new();

/// 设置全局沙箱配置。只能成功设置一次。
pub fn set_sandbox_config(config: SandboxConfig) -> bool {
    GLOBAL_CONFIG.set(config).is_ok()
}

/// 读取全局沙箱配置；未设置时返回全默认（不限制）。
pub fn sandbox_config() -> SandboxConfig {
    GLOBAL_CONFIG.get().cloned().unwrap_or_default()
}

/// 应用全局沙箱配置到当前进程。
pub fn setup_sandbox() -> Vec<CapabilityReport> {
    setup_sandbox_with(&sandbox_config())
}

/// 应用指定配置到当前进程，返回与配置项一一对应的结果数组（顺序固定为
/// [`SandboxCapability::ALL`]）。配置存在互斥矛盾时直接 `assert!` 失败，
/// 把冲突原因明确告诉开发者（开发期 “expect” 语义），而不是静默部分生效。
pub fn setup_sandbox_with(config: &SandboxConfig) -> Vec<CapabilityReport> {
    let violations = config.validate();
    assert!(
        violations.is_empty(),
        "WhiteBox-Forge 沙箱配置存在互斥项，无法启用：\n  - {}",
        violations.join("\n  - ")
    );
    let reports = platform_setup(config);
    debug_assert_eq!(reports.len(), SandboxCapability::ALL.len());
    reports
}

/// 请求时失败 → Error，未请求 → Skipped，成功 → Applied
pub(super) fn outcome(requested: bool, applied: bool) -> ApplyStatus {
    if !requested {
        ApplyStatus::Skipped
    } else if applied {
        ApplyStatus::Applied
    } else {
        ApplyStatus::Error
    }
}

/// 该平台不支持此能力时，统一映射为：已请求 → Error，未请求 → Skipped
pub(super) fn unsupported(requested: bool) -> ApplyStatus {
    outcome(requested, false)
}

/// 平台支持但权限不足时：已请求 → Denied，未请求 → Skipped
pub(super) fn denied(requested: bool) -> ApplyStatus {
    if requested {
        ApplyStatus::Denied
    } else {
        ApplyStatus::Skipped
    }
}

/// 按键登记的能力报告集合（扩展 / 维护的核心切面）。
///
/// - [`ReportSet::from_config`] 先把每个能力标成“该平台不支持”（请求了 → `Error`）。
/// - 平台只对其 **能实现** 的能力调用 [`ReportSet::override_status`]。
/// - [`ReportSet::finish`] 按 [`SandboxCapability::ALL`] 顺序输出，保证
///   结果长度恒定、能力与状态永不错位，新增能力也无需改动既有平台代码。
#[derive(Default)]
pub(super) struct ReportSet {
    entries: Vec<(SandboxCapability, ApplyStatus)>,
}

impl ReportSet {
    /// 用配置预填“默认不支持”态
    pub(super) fn from_config(config: &SandboxConfig) -> Self {
        let mut set = Self::default();
        for capability in SandboxCapability::ALL {
            set.entries
                .push((capability, unsupported(config.is_requested(capability))));
        }
        set
    }

    /// 覆盖某项能力的实现结果（只在平台真实支持时调用）
    pub(super) fn override_status(
        &mut self,
        capability: SandboxCapability,
        status: ApplyStatus,
    ) -> &mut Self {
        if let Some(entry) = self.entries.iter_mut().find(|(k, _)| *k == capability) {
            entry.1 = status;
        } else {
            // 防御：新能力未走 from_config 预填时直接追加（正常情况下不会发生）
            self.entries.push((capability, status));
        }
        self
    }

    /// 按 `ALL` 顺序输出报告
    pub(super) fn finish(self) -> Vec<CapabilityReport> {
        SandboxCapability::ALL
            .into_iter()
            .map(|capability| {
                let status = self
                    .entries
                    .iter()
                    .find(|(k, _)| *k == capability)
                    .map(|(_, s)| *s)
                    .unwrap_or(ApplyStatus::Skipped);
                CapabilityReport { capability, status }
            })
            .collect()
    }
}

#[cfg(target_os = "windows")]
fn platform_setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    windows::setup(config)
}

#[cfg(target_os = "linux")]
fn platform_setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    linux::setup(config)
}

#[cfg(target_os = "android")]
fn platform_setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    android::setup(config)
}

#[cfg(target_os = "macos")]
fn platform_setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    macos::setup(config)
}

#[cfg(target_os = "ios")]
fn platform_setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    ios::setup(config)
}

// 未知平台兜底：保持编译，全部按“不支持 → Error”处理，便于未来新增 OS 后端
#[cfg(not(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn platform_setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    ReportSet::from_config(config).finish()
}
