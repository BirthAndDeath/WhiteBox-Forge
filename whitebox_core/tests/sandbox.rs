use std::path::PathBuf;

use whitebox_core::platform::{
    setup_sandbox_with, ApplyStatus, NetworkPorts, SandboxCapability, SandboxConfig,
};

/// setup 只应在真正的 worker 进程里跑一次（单向沙箱），这里为测试安全起见
/// 只验证“空配置＝全 Skipped 的无副作用调用”。
#[test]
fn default_config_is_a_noop() {
    let reports = setup_sandbox_with(&SandboxConfig::default());
    assert_eq!(reports.len(), SandboxCapability::ALL.len());
    assert!(reports.iter().all(|r| r.status == ApplyStatus::Skipped));
}

#[test]
fn report_order_matches_capability_order() {
    let reports = setup_sandbox_with(&SandboxConfig::default());
    for (report, capability) in reports.iter().zip(SandboxCapability::ALL) {
        assert_eq!(report.capability, capability);
    }
}

#[test]
fn validate_detects_network_mode_exclusivity() {
    let mut cfg = SandboxConfig::default();
    cfg.deny_network = true;
    cfg.localhost_only = true;
    assert!(!cfg.validate().is_empty());

    cfg.localhost_only = false;
    cfg.network_ports = NetworkPorts::Allow(vec![(443, 443)]);
    assert!(!cfg.validate().is_empty());
}

#[test]
fn validate_detects_sealed_fs_conflicts() {
    let mut cfg = SandboxConfig::default();
    cfg.deny_file_access = true;
    cfg.fs_rules = vec![whitebox_core::platform::FsRule {
        path: PathBuf::from("/tmp"),
        read: true,
        write: false,
    }];
    assert!(!cfg.validate().is_empty());

    let mut cfg2 = SandboxConfig::default();
    cfg2.deny_file_access = true;
    cfg2.temp_allow_write = true;
    assert!(!cfg2.validate().is_empty());
}

#[test]
fn validate_detects_anti_debug_and_core_conflict() {
    let mut cfg = SandboxConfig::default();
    cfg.anti_debug = true;
    cfg.max_core_bytes = Some(1024 * 1024);
    assert!(!cfg.validate().is_empty());

    // 0 视为关闭 core dump，与 anti_debug 一致，合法
    cfg.max_core_bytes = Some(0);
    assert!(cfg.validate().is_empty());
}

#[test]
fn validate_detects_deny_exec_and_children_conflict() {
    let mut cfg = SandboxConfig::default();
    cfg.deny_exec = true;
    cfg.max_children = Some(2);
    assert!(!cfg.validate().is_empty());
}

#[test]
fn conflicting_config_panics_in_setup() {
    let conflicting = || {
        let mut cfg = SandboxConfig::default();
        cfg.deny_network = true;
        cfg.dns_only = true;
        let _ = setup_sandbox_with(&cfg);
    };
    assert!(std::panic::catch_unwind(conflicting).is_err());
}

#[test]
fn builder_constructs_legit_config() {
    let cfg = SandboxConfig::new()
        .allow_fs_read(PathBuf::from("/etc/hosts"))
        .allow_fs_read_write(PathBuf::from("/data/out"))
        .allow_network_ports(vec![(443, 443), (8000, 9000)])
        .max_memory(256 * 1024 * 1024)
        .max_open_files(128)
        .max_cpu_ms(30_000)
        .deny_exec(true)
        .anti_debug(true);
    assert!(cfg.validate().is_empty());
    assert_eq!(cfg.fs_rules.len(), 2);
    assert!(matches!(cfg.network_ports, NetworkPorts::Allow(_)));
    assert_eq!(cfg.max_memory_bytes, Some(256 * 1024 * 1024));
}

#[test]
fn config_roundtrips_over_wire_format() {
    // worker 协议用 bincode 序列化沙箱配置，验证可往返
    let cfg = SandboxConfig::new()
        .allow_fs_read(PathBuf::from("/etc/hosts"))
        .deny_network_ports(vec![(1080, 1080)])
        .sealed_fs(false)
        .strict_syscalls(true);
    let bytes = bincode::serialize(&cfg).expect("serialize");
    let decoded: SandboxConfig = bincode::deserialize(&bytes).expect("deserialize");
    assert_eq!(decoded, cfg);
    assert!(decoded.validate().is_empty());
}