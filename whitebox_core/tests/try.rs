use whitebox_core::sandbox::SandboxConfig;
use whitebox_core::*;

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

    // 集成测试进程的 current_exe() 是测试二进制，spawn 成 worker 会递归跑测试，
    // 因此强制 `run_data` 的线程回退路径（进程 worker 链路由真机/后续端到端测试覆盖）。
    // SAFETY: 测试早期单线程阶段设置；无并发读该变量的场景。
    unsafe { std::env::set_var("WHITEBOX_FORCE_THREAD_FALLBACK", "1") };

    let runner = SandboxHandle::run_data(wat.as_bytes().to_vec(), &SandboxConfig::open())?;
    runner.wait()?;
    Ok(())
}
