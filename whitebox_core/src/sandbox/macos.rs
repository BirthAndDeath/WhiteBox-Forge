//! macOS 平台进程沙箱实现。
//!
//! 应用机制（`sandbox_init` 自定义 seatbelt profile，无需 root）：
//! - 文件能力组（file_access / readonly_fs / sealed / temp_allow_write / no_exec）：文件 profile
//! - 网络能力组（deny_network / network_ports / loopback_only / dns_only）：网络 profile
//! - anti_debug：`ptrace(PT_DENY_ATTACH)`
//! - 资源类 max_open_files / max_cpu / max_file_size / max_core：`setrlimit`
//!
//! 其余能力（fs_root / max_children / no_new_privileges / max_memory / strict_syscalls /
//! Windows 缓解策略）由 [`super::ReportSet::from_config`] 自动标为“不支持 → Error”。
//! 注意：`sandbox_init` 已被 Apple 标记为 deprecated，但至今仍可用且无需 root。
//!
//! 失败原因统一经 [`unix_common::SyscallOutcome`] 区分：`EACCES`/`EPERM` → `Denied`，
//! 其余（含 seatbelt 语法/环境类失败）→ `Error`。

use std::ffi::{CString, c_char};

use super::SandboxCapability::{
    AntiDebug, DnsOnly, FileAccess, FilesystemSealed, LoopbackOnly, MaxCoreSize, MaxCpuTime,
    MaxFileSize, MaxOpenFiles, Network, NetworkPorts, NoExec, ReadonlyFileSystem, TempWrite,
};
use super::unix_common::{self, SyscallOutcome};
use super::{CapabilityReport, NetworkPorts as NetPorts, ReportSet, SandboxConfig};

/// libc 0.2.189 未导出 sandbox_init / sandbox_free_error，这里手动声明。
/// 符号来自 libSystem，任何 macOS 程序都链接得到。
unsafe extern "C" {
    fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> i32;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

pub(super) fn setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    let network_applied = if config.deny_network
        || config.network_ports.is_requested()
        || config.localhost_only
        || config.dns_only
    {
        apply_network_profile(config)
    } else {
        SyscallOutcome::Failed
    };
    let fs_applied = if config.readonly_fs
        || config.deny_exec
        || config.deny_file_access
        || config.temp_allow_write
        || !config.fs_rules.is_empty()
    {
        apply_fs_profile(config)
    } else {
        SyscallOutcome::Failed
    };
    // PT_DENY_ATTACH 不可逆，必须按配置项门控，不能当作实参无条件调用
    let anti_debug_applied = if config.anti_debug {
        ptrace_deny_attach()
    } else {
        SyscallOutcome::Failed
    };

    let mut set = ReportSet::from_config(config);
    set.override_status(
        FileAccess,
        unix_common::status(!config.fs_rules.is_empty(), fs_applied),
    )
    .override_status(
        ReadonlyFileSystem,
        unix_common::status(config.readonly_fs, fs_applied),
    )
    .override_status(
        FilesystemSealed,
        unix_common::status(config.deny_file_access, fs_applied),
    )
    .override_status(
        TempWrite,
        unix_common::status(config.temp_allow_write, fs_applied),
    )
    .override_status(
        Network,
        unix_common::status(config.deny_network, network_applied),
    )
    .override_status(
        NetworkPorts,
        unix_common::status(config.network_ports.is_requested(), network_applied),
    )
    .override_status(
        LoopbackOnly,
        unix_common::status(config.localhost_only, network_applied),
    )
    .override_status(
        DnsOnly,
        unix_common::status(config.dns_only, network_applied),
    )
    .override_status(NoExec, unix_common::status(config.deny_exec, fs_applied))
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
    );
    set.finish()
}

/// 拒绝被调试：成功后本进程不可再被 attach，并断开已存在的调试器
fn ptrace_deny_attach() -> SyscallOutcome {
    SyscallOutcome::from_ret(unsafe {
        libc::ptrace(
            libc::PT_DENY_ATTACH,
            0,
            std::ptr::null_mut::<libc::c_char>(),
            0,
        )
    } as i64)
}

/// 网络 profile。seatbelt 中“更具体的规则优先；同样具体则后写覆盖先写”，
/// 因此先 `(deny network*)` 封死全局网络，再用带端口/地址谓词的 allow 定向放行。
/// 端口段语法：`(allow network-outbound (remote tcp (remote-port a) (remote-port b)))`。
fn apply_network_profile(config: &SandboxConfig) -> SyscallOutcome {
    let mut rules = vec![String::from("(version 1)"), String::from("(allow default)")];

    match &config.network_ports {
        NetPorts::Allow(ranges) => {
            rules.push(String::from("(deny network-outbound)"));
            for (start, end) in ranges {
                rules.push(format!(
                    "(allow network-outbound (remote tcp (remote-port {start}) (remote-port {end})))"
                ));
            }
        }
        NetPorts::Deny(ranges) => {
            for (start, end) in ranges {
                rules.push(format!(
                    "(deny network-outbound (remote tcp (remote-port {start}) (remote-port {end})))"
                ));
            }
        }
        NetPorts::Default => {}
    }

    if config.deny_network {
        rules.push(String::from("(deny network*)"));
    }
    if config.loopback_only {
        rules.push(String::from("(deny network*)"));
        // 回环出入：仅放行 127.0.0.1 / ::1
        rules.push(String::from(
            "(allow network-outbound (remote-ip 127.0.0.1))",
        ));
        rules.push(String::from(
            "(allow network-outbound (local-ip 127.0.0.1))",
        ));
        rules.push(String::from("(allow network-inbound (local-ip 127.0.0.1))"));
    }
    if config.dns_only {
        rules.push(String::from("(deny network*)"));
        rules.push(String::from(
            "(allow network-outbound (remote udp (remote-port 53)))",
        ));
        rules.push(String::from(
            "(allow network-outbound (remote tcp (remote-port 53)))",
        ));
    }
    if rules.len() == 2 {
        // 未请求任何网络限制
        return SyscallOutcome::Applied;
    }
    apply_profile(rules)
}

/// 转义 Seatbelt 字符串：反斜杠、双引号；控制字符（含换行）返回 None（fail-closed）。
/// Escape a Seatbelt string (backslash/quote); reject control chars (incl. newline).
fn seatbelt_string(path: &std::path::Path) -> Option<String> {
    let s = path.to_str()?;
    if s.chars().any(char::is_control) {
        return None;
    }
    Some(s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 文件 profile：sealed / 路径授权都默认拒绝，再按授权规则逐条放行（deny-by-default + 显式 grant）。
fn apply_fs_profile(config: &SandboxConfig) -> SyscallOutcome {
    let mut rules = vec![String::from("(version 1)"), String::from("(allow default)")];

    let has_path_rules = !config.fs_rules.is_empty();
    // 封闭基线：sealed 或存在路径授权时，默认拒绝全部文件访问
    if config.deny_file_access || has_path_rules {
        rules.push(String::from("(deny file-read* file-write*)"));
    }
    // 全局只读：拒绝一切写入
    if config.readonly_fs {
        rules.push(String::from("(deny file-write*)"));
    }

    let mut partial = false;
    for rule in &config.fs_rules {
        let Some(path) = seatbelt_string(&rule.path) else {
            // 非 UTF-8 / 含控制字符：该授权无法安全编码，记为部分失败而非静默跳过。
            // Non-UTF-8 / control chars: cannot encode safely; mark partial.
            partial = true;
            continue;
        };
        if rule.read {
            rules.push(format!("(allow file-read* (subpath \"{path}\"))"));
        }
        if !config.readonly_fs && rule.write {
            rules.push(format!("(allow file-write* (subpath \"{path}\"))"));
        }
    }

    // 临时目录写放行（sealed 基线上的显式 grant）
    if config.temp_allow_write {
        match seatbelt_string(&std::env::temp_dir()) {
            Some(p) => rules.push(format!("(allow file-write* (subpath \"{p}\"))")),
            None => partial = true,
        }
    }

    if config.deny_exec {
        rules.push(String::from("(deny process-exec*)"));
    }

    let outcome = apply_profile(rules);
    if partial && outcome.is_applied() {
        SyscallOutcome::Failed
    } else {
        outcome
    }
}

/// 调用 sandbox_init 一次性应用规则。失败时分类 errno 并释放错误缓冲。
fn apply_profile(rules: Vec<String>) -> SyscallOutcome {
    let Ok(profile) = CString::new(rules.join("\n")) else {
        return SyscallOutcome::Failed;
    };
    let mut errbuf: *mut c_char = std::ptr::null_mut();
    let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &mut errbuf) };
    // 先分类（sandbox_init 可能设置 errno），再释放缓冲（free 可能覆盖 errno）
    let outcome = SyscallOutcome::from_ret(rc as i64);
    if rc != 0 && !errbuf.is_null() {
        unsafe { sandbox_free_error(errbuf) };
    }
    outcome
}
