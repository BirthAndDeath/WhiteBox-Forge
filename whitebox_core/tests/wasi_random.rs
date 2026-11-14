//! guest 级 e2e：`WasiRandom::Seeded` 必须让 guest 看到确定性、可复现的随机序列。
//!
//! guest 导入 `wasi:random/insecure` 连续取 3 个 u64：`Deterministic` 以 16 字节为周期循环，
//! 每次 u64 消耗 8 字节 → 第 1、3 次相同、第 1、2 次不同。据此判定接线与确定性。

use std::time::Duration;

use whitebox_core::sandbox::{SandboxConfig, WasiRandom};
use whitebox_core::{SandboxHandle, init};

const SEEDED_INSECURE_RANDOM: &str = r#"
    (component
        (import "wasi:random/insecure@0.2.0" (instance $r
            (export "get-insecure-random-bytes" (func (param "len" u64) (result (list u8))))
            (export "get-insecure-random-u64" (func (result u64)))))

        (alias export $r "get-insecure-random-u64" (func $u64))
        (core func $u64c (canon lower (func $u64)))

        (core module $m
            (import "r" "u64" (func $u64 (result i64)))
            (func (export "run") (result i32)
                (local $a i64) (local $b i64) (local $c i64)
                call $u64 local.set $a
                call $u64 local.set $b
                call $u64 local.set $c
                ;; ok(0) 当且仅当 a==c（周期回到起点）且 a!=b（确实在推进）
                local.get $a local.get $c i64.eq
                local.get $a local.get $b i64.ne
                i32.and
                i32.eqz))

        (core instance $i
            (instantiate $m
                (with "r" (instance (export "u64" (func $u64c))))))

        (type $run_ty (func (result (result))))
        (func $run (type $run_ty) (canon lift (core func $i "run")))

        (instance $run_iface (export "run" (func $run)))
        (export "wasi:cli/run@0.2.0" (instance $run_iface))
    )
"#;

#[test]
fn seeded_random_is_deterministic_and_served() -> Result<(), Box<dyn std::error::Error>> {
    init().expect("init must succeed");

    whitebox_core::test_hooks::force_thread_fallback(true);

    // 用 open() 避免把 L1 限制施加到测试进程自身
    let cfg = SandboxConfig::open().wasi_random(WasiRandom::Seeded(0x0123_4567_89AB_CDEF));
    let runner = SandboxHandle::run_data(SEEDED_INSECURE_RANDOM.as_bytes().to_vec(), &cfg)?;
    runner
        .wait_timeout(Duration::from_secs(10))
        .expect("seeded insecure random must satisfy the deterministic-cycle check");
    Ok(())
}
