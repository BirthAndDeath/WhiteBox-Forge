use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use whitebox_core::*;
use whitebox_core::{SandboxHandle, ThreadCommand};
#[test]
fn try_use() -> Result<(), Box<dyn std::error::Error>> {
    //标定全局状态（IS_WORKER 等），否则引擎初始化会 panic
    init().expect("init must succeed");
    // 同时验证两件事：WASI fd_write 已通过 Store 接入（写 stdout），以及自定义 host_func 仍可用
    let wat = r#"
        (module
            (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (import "host" "host_func" (func $host_hello (param i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "hello from wasi\n")
            (data (i32.const 32) "\00\00\00\00\0f\00\00\00")
            (func (export "_start")
                i32.const 1
                i32.const 32
                i32.const 1
                i32.const 100
                call $fd_write
                drop
                i32.const 3
                call $host_hello)
        )
    "#;
    println!("If you see this, --nocapture is enabled!");
    // load_* 现在返回 Module，需经 load_module(module, shutdown_flag) 实例化
    let module = load_wasm_bytes(wat.into())?;
    let sandbox = load_module(module, Arc::new(AtomicBool::new(false)))?;

    // run_module 现在是 async：需要取消通道 + 一个 current-thread runtime 驱动
    let (_tx, rx) = tokio::sync::watch::channel(ThreadCommand::Empty);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(SandboxHandle::run_module_in_thread(sandbox, rx))?;
    Ok(())
}
