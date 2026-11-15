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
//!
//! 失败原因统一经 [`unix_common::SyscallOutcome`] 区分：`EACCES`/`EPERM` → `Denied`，
//! 其余 → `Error`。

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::SandboxCapability::{
    AntiDebug, FileAccess, FilesystemRoot, FilesystemSealed, MaxCoreSize, MaxCpuTime, MaxFileSize,
    MaxOpenFiles, Network, NoExec, NoNewPrivileges, ReadonlyFileSystem, StrictSyscalls, TempWrite,
};
use super::unix_common::{self, SyscallOutcome};
use super::{CapabilityReport, ReportSet, SandboxConfig};

pub(super) fn setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    // fs_root：先做不可逆的 chroot（必须在 Landlock / seccomp 之前，否则该路径/syscall 已被封死）
    let fs_root_applied = match &config.fs_root {
        Some(root) => chroot_root(root),
        None => SyscallOutcome::Failed,
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
        SyscallOutcome::Failed
    };

    // 副作用调用必须按配置项门控：Rust 会先求值实参，若把
    // deny_network()/anti_debug() 等放进 status(...) 实参，即使配置为 false 也会执行不可逆操作
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
        FileAccess,
        unix_common::status(!config.fs_rules.is_empty(), fs_restricted),
    )
    .override_status(
        ReadonlyFileSystem,
        unix_common::status(config.readonly_fs, fs_restricted),
    )
    .override_status(
        FilesystemSealed,
        unix_common::status(config.deny_file_access, fs_restricted),
    )
    .override_status(
        FilesystemRoot,
        unix_common::status(config.fs_root.is_some(), fs_root_applied),
    )
    .override_status(
        TempWrite,
        unix_common::status(config.temp_allow_write, fs_restricted),
    )
    .override_status(
        Network,
        unix_common::status(config.deny_network, network_applied),
    )
    .override_status(
        NoExec,
        unix_common::status(config.deny_exec, fs_restricted),
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

/// chroot 到一个新根，并把当前工作目录切换进去。
/// 不可逆；成功后本进程看到的所有绝对路径都以 root 为根。
fn chroot_root(root: &Path) -> SyscallOutcome {
    let Ok(c_root) = CString::new(root.as_os_str().as_bytes()) else {
        return SyscallOutcome::Failed;
    };
    let chroot_oc = SyscallOutcome::from_ret(unsafe { libc::chroot(c_root.as_ptr().cast()) } as i64);
    if !chroot_oc.is_applied() {
        return chroot_oc;
    }
    SyscallOutcome::from_ret(unsafe { libc::chdir(c"/".as_ptr().cast()) } as i64)
}
