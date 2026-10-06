//! Android 平台进程沙箱实现。
//!
//! Android 应用本就被 Zygote / app 沙箱（SELinux + seccomp）限制。
//! 只覆盖可叠加的：prctl（no_new_privileges / anti_debug）、setrlimit 资源四件套、
//! seccomp 严格模式（尝试安装，通常会因 Zygote seccomp 禁止而返回 Error）。
//! 其余能力（Landlock / chroot / 网络形态 / 内存 / 子进程数等）应用层无权操作，
//! 由 [`super::ReportSet::from_config`] 自动标为“不支持 → Error”。

use super::SandboxCapability::{
    AntiDebug, MaxCoreSize, MaxCpuTime, MaxFileSize, MaxOpenFiles, Network, NoNewPrivileges,
    StrictSyscalls,
};
use super::unix_common;
use super::{outcome, CapabilityReport, ReportSet, SandboxConfig};

pub(super) fn setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    // 副作用调用按配置项门控（Rust 会先求值实参，不能把副作用调用塞进 outcome(...) 实参）
    let network_applied = if config.deny_network {
        unix_common::deny_network()
    } else {
        false
    };
    let no_new_priv_applied = if config.no_new_privileges {
        unix_common::set_no_new_privileges()
    } else {
        false
    };
    let anti_debug_applied = if config.anti_debug {
        unix_common::anti_debug()
    } else {
        false
    };
    #[cfg(target_arch = "x86_64")]
    let strict_applied = if config.strict_syscalls {
        unix_common::strict_syscalls()
    } else {
        false
    };
    #[cfg(not(target_arch = "x86_64"))]
    let strict_applied = false;

    let mut set = ReportSet::from_config(config);
    set.override_status(Network, outcome(config.deny_network, network_applied))
        .override_status(
            NoNewPrivileges,
            outcome(config.no_new_privileges, no_new_priv_applied),
        )
        .override_status(AntiDebug, outcome(config.anti_debug, anti_debug_applied))
        .override_status(
            MaxOpenFiles,
            outcome(
                config.max_open_files.is_some(),
                config.max_open_files.is_some_and(unix_common::limit_open_files),
            ),
        )
        .override_status(
            MaxCpuTime,
            outcome(
                config.max_cpu_ms.is_some(),
                config.max_cpu_ms.is_some_and(unix_common::limit_cpu_ms),
            ),
        )
        .override_status(
            MaxFileSize,
            outcome(
                config.max_file_size_bytes.is_some(),
                config.max_file_size_bytes.is_some_and(unix_common::limit_file_size),
            ),
        )
        .override_status(
            MaxCoreSize,
            outcome(
                config.max_core_bytes.is_some(),
                config.max_core_bytes.is_some_and(unix_common::limit_core_bytes),
            ),
        )
        .override_status(StrictSyscalls, outcome(config.strict_syscalls, strict_applied));
    set.finish()
}