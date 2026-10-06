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

use std::ffi::{CString, c_char};

use super::SandboxCapability::{
    AntiDebug, DnsOnly, FileAccess, FilesystemSealed, LoopbackOnly, MaxCoreSize, MaxCpuTime,
    MaxFileSize, MaxOpenFiles, Network, NetworkPorts, NoExec, ReadonlyFileSystem, TempWrite,
};
use super::unix_common;
use super::{CapabilityReport, NetworkPorts as NetPorts, ReportSet, SandboxConfig, outcome};

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
        false
    };
    let fs_applied = if config.readonly_fs
        || config.deny_exec
        || config.deny_file_access
        || config.temp_allow_write
        || !config.fs_rules.is_empty()
    {
        apply_fs_profile(config)
    } else {
        false
    };
    // PT_DENY_ATTACH 不可逆，必须按配置项门控，不能当作实参无条件调用
    let anti_debug_applied = if config.anti_debug {
        ptrace_deny_attach()
    } else {
        false
    };

    let mut set = ReportSet::from_config(config);
    set.override_status(FileAccess, outcome(!config.fs_rules.is_empty(), fs_applied))
        .override_status(ReadonlyFileSystem, outcome(config.readonly_fs, fs_applied))
        .override_status(
            FilesystemSealed,
            outcome(config.deny_file_access, fs_applied),
        )
        .override_status(TempWrite, outcome(config.temp_allow_write, fs_applied))
        .override_status(Network, outcome(config.deny_network, network_applied))
        .override_status(
            NetworkPorts,
            outcome(config.network_ports.is_requested(), network_applied),
        )
        .override_status(
            LoopbackOnly,
            outcome(config.localhost_only, network_applied),
        )
        .override_status(DnsOnly, outcome(config.dns_only, network_applied))
        .override_status(NoExec, outcome(config.deny_exec, fs_applied))
        .override_status(AntiDebug, outcome(config.anti_debug, anti_debug_applied))
        .override_status(
            MaxOpenFiles,
            outcome(
                config.max_open_files.is_some(),
                config
                    .max_open_files
                    .is_some_and(unix_common::limit_open_files),
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
                config
                    .max_file_size_bytes
                    .is_some_and(unix_common::limit_file_size),
            ),
        )
        .override_status(
            MaxCoreSize,
            outcome(
                config.max_core_bytes.is_some(),
                config
                    .max_core_bytes
                    .is_some_and(unix_common::limit_core_bytes),
            ),
        );
    set.finish()
}

/// 拒绝被调试：成功后本进程不可再被 attach，并断开已存在的调试器
fn ptrace_deny_attach() -> bool {
    unsafe {
        libc::ptrace(
            libc::PT_DENY_ATTACH,
            0,
            std::ptr::null_mut::<libc::c_char>(),
            0,
        ) == 0
    }
}

/// 网络 profile。seatbelt 中“更具体的规则优先；同样具体则后写覆盖先写”，
/// 因此先 `(deny network*)` 封死全局网络，再用带端口/地址谓词的 allow 定向放行。
/// 端口段语法：`(allow network-outbound (remote tcp (remote-port a) (remote-port b)))`。
fn apply_network_profile(config: &SandboxConfig) -> bool {
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
        return true;
    }
    apply_profile(rules)
}

/// 文件 profile：sealed / 路径授权都默认拒绝，再按授权规则逐条放行（deny-by-default + 显式 grant）。
fn apply_fs_profile(config: &SandboxConfig) -> bool {
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

    for rule in &config.fs_rules {
        let path = rule.path.to_str().map(String::from);
        let Some(path) = path else { continue };
        if rule.read {
            rules.push(format!("(allow file-read* (subpath \"{path}\"))"));
        }
        if !config.readonly_fs && rule.write {
            rules.push(format!("(allow file-write* (subpath \"{path}\"))"));
        }
    }

    // 临时目录写放行（sealed 基线上的显式 grant）
    if config.temp_allow_write {
        let temp = std::env::temp_dir();
        if let Some(p) = temp.to_str() {
            rules.push(format!("(allow file-write* (subpath \"{p}\"))"));
        }
    }

    if config.deny_exec {
        rules.push(String::from("(deny process-exec*)"));
    }

    apply_profile(rules)
}

/// 调用 sandbox_init 一次性应用规则。失败时释放错误缓冲并返回 false。
fn apply_profile(rules: Vec<String>) -> bool {
    let Ok(profile) = CString::new(rules.join("\n")) else {
        return false;
    };
    let mut errbuf: *mut c_char = std::ptr::null_mut();
    let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &mut errbuf) };
    if rc == 0 {
        true
    } else {
        if !errbuf.is_null() {
            unsafe { sandbox_free_error(errbuf) };
        }
        false
    }
}
