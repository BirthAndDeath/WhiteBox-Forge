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
//!    2. 在 [`SandboxCapability::as_str`] 增加人类可读名。
//!    3. 在 [`SandboxConfig`] 增加对应字段。
//!    4. 在 [`SandboxConfig::is_requested`] 登记“如何判断该项被请求”。
//!    5. 如与既有选项互斥，在 [`SandboxConfig::validate`] 增加断言。
//!    6. 各平台在 `setup` 里用 [`ReportSet::override_status`] 覆盖其能实现的能力；
//!       无法实现的能力自动保持“不支持 → Error”，无需编写。
//!    7. 在 `whitebox_core/tests/sandbox.rs` 增加覆盖测试。
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

use std::path::PathBuf;
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
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStatus {
    Applied,
    Skipped,
    Error,
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SandboxConfig {
    // ---- 文件系统 ----
    /// 路径级文件访问白名单
    pub fs_rules: FsAccess,
    /// 全局只读
    pub readonly_fs: bool,
    /// 完全封闭文件系统
    pub deny_file_access: bool,
    /// 文件系统根重定向（要求特权/平台支持，否则 Error）
    pub fs_root: Option<PathBuf>,
    /// 允许写系统临时目录
    pub temp_allow_write: bool,
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
    /// 网络四形态互斥；封闭文件系统（deny_file_access）与路径白名单 / 根重定向 / 临时写互斥；
    /// anti_debug 与“允许 core dump”互斥；deny_exec 与 max_children 互斥。
    pub fn validate(&self) -> Vec<String> {
        let mut violations = Vec::new();

        let network_modes = i32::from(self.deny_network)
            + i32::from(self.network_ports.is_requested())
            + i32::from(self.localhost_only)
            + i32::from(self.dns_only);
        if network_modes > 1 {
            violations.push(
                "网络模式互斥：deny_network / network_ports / localhost_only / dns_only \
                 只能同时启用一个；需要只放行少数项目时请改用 network_ports::Allow 白名单"
                    .into(),
            );
        }
        if matches!(&self.network_ports, NetworkPorts::Allow(ranges) if ranges.is_empty()) {
            violations.push(
                "network_ports::Allow 白名单为空：没有任何端口可用，等价于 deny_network，\
                 请直接使用 deny_network"
                    .into(),
            );
        }
        if self.deny_file_access {
            if !self.fs_rules.is_empty() {
                violations.push(
                    "deny_file_access 与 fs_rules 互斥：封闭文件系统会拒绝一切文件访问，路径白名单无意义"
                        .into(),
                );
            }
            if self.fs_root.is_some() {
                violations.push("deny_file_access 与 fs_root 互斥".into());
            }
            if self.temp_allow_write {
                violations.push("deny_file_access 与 temp_allow_write 互斥".into());
            }
        }
        if self.fs_root.is_some() && self.temp_allow_write {
            violations.push(
                "fs_root 与 temp_allow_write 互斥：chroot/根重定向后系统临时目录已重映射到根内部"
                    .into(),
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
            violations.push(
                "deny_exec 与 max_children 互斥：deny_exec 已隐含 0 个子进程上限".into(),
            );
        }

        violations
    }

    /// 空配置（等价于 [`SandboxConfig::default`]）。配合各 `with_*` 链式构造。
    pub fn new() -> Self {
        Self::default()
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