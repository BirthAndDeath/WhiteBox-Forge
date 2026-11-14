mod common;

use common::RANDOM_COMMAND_COMPONENT;
use whitebox_core::SandboxHandle;
use whitebox_core::sandbox::SandboxConfig;
use whitebox_core::*;

#[test]
fn try_use() -> Result<(), Box<dyn std::error::Error>> {
    //标定全局状态（IS_WORKER 等），否则引擎初始化会 panic
    init().expect("init must succeed");
    println!("If you see this, --nocapture is enabled!");

    // 集成测试进程的 current_exe() 是测试二进制，spawn 成 worker 会递归跑测试，
    // 因此强制 `run_data` 的线程回退路径（进程 worker 链路见 tests/worker.rs）。
    whitebox_core::test_hooks::force_thread_fallback(true);

    let runner = SandboxHandle::run_data(
        RANDOM_COMMAND_COMPONENT.as_bytes().to_vec(),
        &SandboxConfig::open(),
    )?;
    runner.wait()?;
    Ok(())
}
