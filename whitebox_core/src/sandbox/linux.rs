//! Linux 平台进程沙箱实现。
//!
//! 只覆盖本平台能实现的能力，其余能力由 [`super::ReportSet::from_config`] 自动标为
//! “不支持 → Error”。应用机制：
//! - fs_root：`chroot()` + `chdir("/")`（需要 CAP_SYS_CHROOT，否则 Error）
//! - file_access / readonly_fs / filesystem_sealed / temp_allow_write / deny_exec：Landlock
//! - deny_network：`unshare(CLONE_NEWNET)`（需 CAP_SYS_ADMIN，否则 Error）
//! - no_new_privileges / anti_debug：`prctl`
//! - max_open_files / max_cpu / max_file_size / max_core：`setrlimit`
//! - strict_syscalls：seccomp BPF（仅 x86_64 安装过滤器，其它架构 Error 保持广兼容）
//!
//! network_ports / loopback_only / dns_only / max_children 需要 nftables / cgroup，
//! 非特权用户态无法实现；max_memory 若实现为 RLIMIT_AS 会与 wasmtime 的大块虚拟地址
//! 保留冲突 —— 均不覆盖，自动为 Error。

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::SandboxCapability::{
    AntiDebug, FileAccess, FilesystemRoot, FilesystemSealed, MaxCoreSize, MaxCpuTime, MaxFileSize,
    MaxOpenFiles, Network, NoExec, NoNewPrivileges, ReadonlyFileSystem, StrictSyscalls, TempWrite,
};
use super::unix_common;
use super::{outcome, CapabilityReport, ReportSet, SandboxConfig};

pub(super) fn setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    // fs_root：先做不可逆的 chroot（必须在 Landlock / seccomp 之前，否则该路径/syscall 已被封死）
    let fs_root_applied = match &config.fs_root {
        Some(root) => chroot_root(root),
        None => false,
    };

    // Landlock 一套规则同时覆盖 5 项文件系统能力
    let fs_restricted = if config.readonly_fs
        || config.deny_exec
        || config.deny_file_access
        || config.temp_allow_write
        || !config.fs_rules.is_empty()
    {
        unix_common::landlock(
            config.readonly_fs,
            config.deny_exec,
            config.deny_file_access,
            &config.fs_rules,
            config.temp_allow_write,
        )
    } else {
        false
    };

    // 副作用调用必须按配置项门控：Rust 会先求值实参，若把
    // deny_network()/anti_debug() 等放进 outcome(...) 实参，即使配置为 false 也会执行不可逆操作
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
    set.override_status(FileAccess, outcome(!config.fs_rules.is_empty(), fs_restricted))
        .override_status(ReadonlyFileSystem, outcome(config.readonly_fs, fs_restricted))
        .override_status(FilesystemSealed, outcome(config.deny_file_access, fs_restricted))
        .override_status(FilesystemRoot, outcome(config.fs_root.is_some(), fs_root_applied))
        .override_status(TempWrite, outcome(config.temp_allow_write, fs_restricted))
        .override_status(Network, outcome(config.deny_network, network_applied))
        .override_status(NoExec, outcome(config.deny_exec, fs_restricted))
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

/// chroot 到一个新根，并把当前工作目录切换进去。
/// 不可逆；成功后本进程看到的所有绝对路径都以 root 为根。
fn chroot_root(root: &Path) -> bool {
    let Ok(c_root) = CString::new(root.as_os_str().as_bytes()) else {
        return false;
    };
    let chroot_ok = unsafe { libc::chroot(c_root.as_ptr().cast()) == 0 };
    chroot_ok && unsafe { libc::chdir(c"/".as_ptr().cast()) == 0 }
}