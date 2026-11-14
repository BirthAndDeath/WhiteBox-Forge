//! guest 级 e2e：`WasiClocks::Deterministic` 必须让 guest 看到固定步进的确定性时钟。
//!
//! guest 导入 `wasi:clocks/monotonic-clock`：读两次 `now` 与一次 `resolution`，
//! 判定 `resolution == 1ms` 且第二次比第一次恰好前进 `resolution`（确定性、可复现）。

use std::time::Duration;

use whitebox_core::sandbox::{SandboxConfig, WasiClocks};
use whitebox_core::{SandboxHandle, init};

const DETERMINISTIC_MONOTONIC_CLOCK: &str = r#"
    (component
        (import "wasi:clocks/monotonic-clock@0.2.0" (instance $c
            (type $instant u64)
            (export "instant" (type (eq $instant)))
            (type $duration u64)
            (export "duration" (type (eq $duration)))
            (export "now" (func (result u64)))
            (export "resolution" (func (result u64)))))

        (alias export $c "now" (func $now))
        (alias export $c "resolution" (func $res))
        (core func $nowc (canon lower (func $now)))
        (core func $resc (canon lower (func $res)))

        (core module $m
            (import "c" "now" (func $now (result i64)))
            (import "c" "res" (func $res (result i64)))
            (func (export "run") (result i32)
                (local $r i64) (local $n1 i64) (local $n2 i64)
                call $res local.set $r
                call $now local.set $n1
                call $now local.set $n2
                ;; ok(0) 当且仅当 resolution == 1_000_000 且 n2 - n1 == resolution
                local.get $r i64.const 1000000 i64.eq
                local.get $n2 local.get $n1 i64.sub local.get $r i64.eq
                i32.and
                i32.eqz))

        (core instance $i
            (instantiate $m
                (with "c" (instance
                    (export "now" (func $nowc))
                    (export "res" (func $resc))))))

        (type $run_ty (func (result (result))))
        (func $run (type $run_ty) (canon lift (core func $i "run")))

        (instance $run_iface (export "run" (func $run)))
        (export "wasi:cli/run@0.2.0" (instance $run_iface))
    )
"#;

#[test]
fn deterministic_monotonic_clock_reproducible() -> Result<(), Box<dyn std::error::Error>> {
    init().expect("init must succeed");

    whitebox_core::test_hooks::force_thread_fallback(true);

    // 用 open() 避免把 L1 限制施加到测试进程自身
    let cfg = SandboxConfig::open().wasi_clocks(WasiClocks::Deterministic(12345));
    let runner = SandboxHandle::run_data(DETERMINISTIC_MONOTONIC_CLOCK.as_bytes().to_vec(), &cfg)?;
    runner
        .wait_timeout(Duration::from_secs(10))
        .expect("deterministic monotonic clock must satisfy the fixed-step check");
    Ok(())
}
