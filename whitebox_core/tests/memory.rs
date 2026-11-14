//! L2 内存上限 e2e：`SandboxConfig::max_memory_bytes` 经 `Store::limiter`（`StoreLimits`）
//! 限制 guest 线性内存增长。
//!
//! 直接驱动 store 层（`load_component_for_thread` + `run_module_for_thread`），**不施加 L1**——
//! 避免 Windows Job 的 `JOB_OBJECT_LIMIT_JOB_MEMORY` 作用于测试进程自身；本测试只验证 L2。
//!
//! guest：1 页初始内存，`run` 里 `memory.grow(1)`；被拒返回 -1 → `!= -1` 为 0 → ok，
//! 故「run 成功」等价于「增长被限流拒绝」。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use whitebox_core::sandbox::SandboxConfig;
use whitebox_core::{
    SandboxHandle, init, load_component_for_thread, load_wasm_bytes,
};

const MEMORY_GROW_COMMAND: &str = r#"
    (component
        (core module $m
            (memory 1)
            (func (export "run") (result i32)
                i32.const 1
                memory.grow
                i32.const -1
                i32.ne))

        (core instance $i (instantiate $m))

        (type $run_ty (func (result (result))))
        (func $run (type $run_ty) (canon lift (core func $i "run")))

        (instance $run_iface (export "run" (func $run)))
        (export "wasi:cli/run@0.2.0" (instance $run_iface))
    )
"#;

/// 在 store 层跑一次 guest，返回结果（错误转字符串，避免测试直接依赖 wasmtime 类型）。
fn run_grow(cfg: &SandboxConfig) -> Result<Vec<u8>, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let component = load_wasm_bytes(MEMORY_GROW_COMMAND.as_bytes().to_vec())
            .map_err(|e| e.to_string())?;
        let sandbox =
            load_component_for_thread(component, Arc::new(AtomicBool::new(false)), cfg, &[])
                .await
                .map_err(|e| e.to_string())?;
        SandboxHandle::run_module_for_thread(sandbox)
            .await
            .map_err(|e| e.to_string())
    })
}

#[test]
fn memory_limit_denies_growth() -> Result<(), Box<dyn std::error::Error>> {
    init().expect("init must succeed");
    // 上限 = 初始 1 页；任何增长都被拒 → guest 返回 ok
    let cfg = SandboxConfig::open().max_memory(65536);
    run_grow(&cfg).expect("growth beyond the limit must be denied (guest returns ok)");
    Ok(())
}

#[test]
fn without_memory_limit_growth_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    init().expect("init must succeed");
    // 无上限：memory.grow 成功（返回页码 != -1）→ guest 返回 err（证明对照成立）
    let cfg = SandboxConfig::open();
    let err = run_grow(&cfg).expect_err("without a limit, growth must succeed and run must fail");
    assert!(err.contains("wasi:cli/run"), "unexpected error: {err}");
    Ok(())
}
