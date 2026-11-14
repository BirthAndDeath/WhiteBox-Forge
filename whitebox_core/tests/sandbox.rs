use std::path::PathBuf;

use whitebox_core::sandbox::{
    ApplyStatus, NetworkPorts, SandboxCapability, SandboxConfig, WasiClocks, WasiRandom,
    WasiSockets, setup_sandbox_with,
};

// 默认配置 = 全关闭（deny-by-default）：文件系统封闭、禁网、禁执行、禁提权、防调试。
#[test]
fn default_is_locked_down() {
    let cfg = SandboxConfig::default();
    assert!(cfg.deny_file_access);
    assert!(cfg.deny_network);
    assert!(cfg.deny_exec);
    assert!(cfg.no_new_privileges);
    assert!(cfg.validate().is_empty());
}

// open() = 全开放（真正的 no-op）：setup 不产生任何限制副作用。
#[test]
fn open_config_is_a_noop() {
    let reports = setup_sandbox_with(&SandboxConfig::open());
    assert_eq!(reports.len(), SandboxCapability::ALL.len());
    assert!(reports.iter().all(|r| r.status == ApplyStatus::Skipped));
}

#[test]
fn report_order_matches_capability_order() {
    let reports = setup_sandbox_with(&SandboxConfig::open());
    for (report, capability) in reports.iter().zip(SandboxCapability::ALL) {
        assert_eq!(report.capability, capability);
    }
}

// 显式授权可以叠加在全关闭基线上，不构成冲突
#[test]
fn grants_compose_over_locked_default() {
    let cfg = SandboxConfig::default()
        .allow_fs_read(PathBuf::from("/etc/hosts"))
        .allow_network_ports(vec![(443, 443)]);
    assert!(cfg.validate().is_empty());
}

#[test]
fn validate_detects_sealed_with_fs_root() {
    // 默认 sealed=true；再设 fs_root 即冲突
    let cfg = SandboxConfig::default().fs_root("/srv/sandbox");
    assert!(!cfg.validate().is_empty());
}

#[test]
fn validate_detects_fs_root_with_temp_write() {
    let cfg = SandboxConfig::default()
        .fs_root("/srv/sandbox")
        .temp_allow_write(true);
    assert!(!cfg.validate().is_empty());
}

#[test]
fn validate_detects_anti_debug_and_core_conflict() {
    // 默认 anti_debug=true；设非 0 core 上限即冲突
    assert!(
        !SandboxConfig::default()
            .max_core_size(1024 * 1024)
            .validate()
            .is_empty()
    );
    // 0 视为关闭 core dump，与 anti_debug 一致，合法
    assert!(
        SandboxConfig::default()
            .max_core_size(0)
            .validate()
            .is_empty()
    );
}

#[test]
fn validate_detects_deny_exec_and_children_conflict() {
    // 默认 deny_exec=true；再设 max_children 即冲突
    let cfg = SandboxConfig::default().max_children(2);
    assert!(!cfg.validate().is_empty());
}

#[test]
fn conflicting_config_panics_in_setup() {
    // deny_exec(true) + max_children 仍互斥 → assert! 报错
    let conflicting = || {
        let cfg = SandboxConfig::open().deny_exec(true).max_children(2);
        let _ = setup_sandbox_with(&cfg);
    };
    assert!(std::panic::catch_unwind(conflicting).is_err());
}

#[test]
fn builder_constructs_legit_config() {
    let cfg = SandboxConfig::default()
        .allow_fs_read(PathBuf::from("/etc/hosts"))
        .allow_fs_read_write(PathBuf::from("/data/out"))
        .allow_network_ports(vec![(443, 443), (8000, 9000)])
        .max_memory(256 * 1024 * 1024)
        .max_open_files(128)
        .max_cpu_ms(30_000);
    assert!(cfg.validate().is_empty());
    assert_eq!(cfg.fs_rules.len(), 2);
    assert!(matches!(cfg.network_ports, NetworkPorts::Allow(_)));
    assert_eq!(cfg.max_memory_bytes, Some(256 * 1024 * 1024));
}

#[test]
fn config_roundtrips_over_wire_format() {
    // worker 协议用 postcard 序列化沙箱配置，验证可往返
    let cfg = SandboxConfig::new()
        .allow_fs_read(PathBuf::from("/etc/hosts"))
        .deny_network_ports(vec![(1080, 1080)])
        .strict_syscalls(true);
    let bytes = postcard::to_stdvec(&cfg).expect("serialize");
    let decoded: SandboxConfig = postcard::from_bytes(&bytes).expect("deserialize");
    assert_eq!(decoded, cfg);
    assert!(decoded.validate().is_empty());
}

// WASI 状态机（L2）：默认为最小面，open() 才放开 sockets，clocks/random 默认宿主。
#[test]
fn wasi_state_machine_defaults() {
    let cfg = SandboxConfig::default();
    assert_eq!(cfg.wasi_sockets, WasiSockets::None);
    assert!(!cfg.wasi_ip_name_lookup);
    assert_eq!(cfg.wasi_clocks, WasiClocks::Host);
    assert_eq!(cfg.wasi_random, WasiRandom::Os);
    assert_eq!(cfg.wasi_max_random_size, None);
    assert_eq!(cfg.wasi_initial_cwd, None);
    assert!(!cfg.wasi_allow_blocking_current_thread);

    let open = SandboxConfig::open();
    assert!(matches!(open.wasi_sockets, WasiSockets::Allow(_)));
    assert!(open.wasi_ip_name_lookup);
    assert!(open.wasi_allow_blocking_current_thread);
    assert!(open.validate().is_empty());
}

// DNS（ip-name-lookup）与禁网 / 禁用 sockets 互斥。
#[test]
fn wasi_ip_name_lookup_conflicts() {
    let vs_none = SandboxConfig::default()
        .wasi_sockets(WasiSockets::None)
        .wasi_ip_name_lookup(true);
    assert!(!vs_none.validate().is_empty());

    let denied = SandboxConfig::open()
        .deny_network(true)
        .wasi_ip_name_lookup(true);
    assert!(!denied.validate().is_empty());
}

// L2 sockets 与 L1 网络求交：deny_network 下授予 guest sockets 必须报错（fail-closed）。
#[test]
fn wasi_sockets_conflict_with_deny_network() {
    let cfg = SandboxConfig::default().wasi_sockets(WasiSockets::LoopbackOnly);
    assert!(!cfg.validate().is_empty());
}

// 基线拒绝 + 显式端口授权（network_ports=Allow）时，L2 sockets 可在子集内共存。
#[test]
fn wasi_sockets_allowed_over_baseline_deny_with_grant() {
    let cfg = SandboxConfig::default() // deny_network = true
        .allow_network_ports(vec![(443, 443)])
        .wasi_sockets(WasiSockets::Allow(vec![(443, 443)]));
    assert!(cfg.validate().is_empty(), "violations: {:?}", cfg.validate());
}

// L2 Allow 端口与 L1 拒绝项重叠 → 报错（L1∩L2 求交）。
#[test]
fn wasi_sockets_allow_conflicts_with_l1_deny_overlap() {
    let cfg = SandboxConfig::open()
        .deny_network_ports(vec![(25, 25)])
        .wasi_sockets(WasiSockets::Allow(vec![(25, 25)]));
    assert!(!cfg.validate().is_empty());
}

// L2 sockets 端口必须落在 L1 network_ports 授权内。
#[test]
fn wasi_sockets_ports_must_subset_l1() {
    let ok = SandboxConfig::open()
        .deny_network(false)
        .allow_network_ports(vec![(443, 443), (8000, 9000)])
        .wasi_sockets(WasiSockets::Allow(vec![(8000, 8100)]));
    assert!(ok.validate().is_empty());

    let bad = ok.clone().wasi_sockets(WasiSockets::Allow(vec![(7000, 7000)]));
    assert!(!bad.validate().is_empty());
}

// 新增 L2 字段参与 wire 往返（worker 帧 3 必须无损）。
#[test]
fn wasi_state_machine_roundtrips_over_wire_format() {
    let cfg = SandboxConfig::open()
        .wasi_clocks(WasiClocks::Deterministic(0xDEAD_BEEF))
        .wasi_random(WasiRandom::Seeded(42))
        .wasi_sockets(WasiSockets::Deny(vec![(25, 25)]))
        .wasi_max_random_size(4096)
        .wasi_initial_cwd("/work")
        .wasi_allow_blocking_current_thread(true);
    let bytes = postcard::to_stdvec(&cfg).expect("serialize");
    let decoded: SandboxConfig = postcard::from_bytes(&bytes).expect("deserialize");
    assert_eq!(decoded, cfg);
}
