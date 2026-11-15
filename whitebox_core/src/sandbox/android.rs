//! Android 平台进程沙箱实现。
//!
//! Android 应用本就被 Zygote / app 沙箱（SELinux + seccomp）限制。
//! 只覆盖可叠加的：prctl（no_new_privileges / anti_debug）、setrlimit 资源四件套、
//! seccomp 严格模式（尝试安装，通常会因 Zygote seccomp 禁止而返回 Error）。
//! 其余能力（Landlock / chroot / 网络形态 / 内存 / 子进程数等）应用层无权操作，
//! 由 [`super::ReportSet::from_config`] 自动标为“不支持 → Error”。
//!
//! 失败原因统一经 [`unix_common::SyscallOutcome`] 区分：`EACCES`/`EPERM` → `Denied`，
//! 其余 → `Error`。

use super::SandboxCapability::{
    AntiDebug, MaxCoreSize, MaxCpuTime, MaxFileSize, MaxOpenFiles, Network, NoNewPrivileges,
    StrictSyscalls,
};
use super::unix_common::{self, SyscallOutcome};
use super::{CapabilityReport, ReportSet, SandboxConfig};

pub(super) fn setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    // 副作用调用按配置项门控（Rust 会先求值实参，不能把副作用调用塞进 status(...) 实参）
    let network_applied = if config.deny_network {
        unix_common::deny_network()
    } else {
        SyscallOutcome::Failed
    };
    let no_new_priv_applied = if config.no_new_privileges {
        unix_common::set_no_new_privileges()
    } else {
        SyscallOutcome::Failed
    };
    let anti_debug_applied = if config.anti_debug {
        unix_common::anti_debug()
    } else {
        SyscallOutcome::Failed
    };
    #[cfg(target_arch = "x86_64")]
    let strict_applied = if config.strict_syscalls {
        unix_common::strict_syscalls()
    } else {
        SyscallOutcome::Failed
    };
    #[cfg(not(target_arch = "x86_64"))]
    let strict_applied = SyscallOutcome::Failed;

    let mut set = ReportSet::from_config(config);
    set.override_status(
        Network,
        unix_common::status(config.deny_network, network_applied),
    )
    .override_status(
        NoNewPrivileges,
        unix_common::status(config.no_new_privileges, no_new_priv_applied),
    )
    .override_status(
        AntiDebug,
        unix_common::status(config.anti_debug, anti_debug_applied),
    )
    .override_status(
        MaxOpenFiles,
        unix_common::status(
            config.max_open_files.is_some(),
            config
                .max_open_files
                .map(unix_common::limit_open_files)
                .unwrap_or(SyscallOutcome::Failed),
        ),
    )
    .override_status(
        MaxCpuTime,
        unix_common::status(
            config.max_cpu_ms.is_some(),
            config
                .max_cpu_ms
                .map(unix_common::limit_cpu_ms)
                .unwrap_or(SyscallOutcome::Failed),
        ),
    )
    .override_status(
        MaxFileSize,
        unix_common::status(
            config.max_file_size_bytes.is_some(),
            config
                .max_file_size_bytes
                .map(unix_common::limit_file_size)
                .unwrap_or(SyscallOutcome::Failed),
        ),
    )
    .override_status(
        MaxCoreSize,
        unix_common::status(
            config.max_core_bytes.is_some(),
            config
                .max_core_bytes
                .map(unix_common::limit_core_bytes)
                .unwrap_or(SyscallOutcome::Failed),
        ),
    )
    .override_status(
        StrictSyscalls,
        unix_common::status(config.strict_syscalls, strict_applied),
    );
    set.finish()
}
