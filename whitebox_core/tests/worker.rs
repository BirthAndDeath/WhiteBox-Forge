mod common;

use common::RANDOM_COMMAND_COMPONENT;
use whitebox_core::SandboxHandle;
use whitebox_core::sandbox::SandboxConfig;
use whitebox_core::*;

#[test]
fn worker_process_runs_component() -> Result<(), Box<dyn std::error::Error>> {
    //标定全局状态（IS_WORKER 等），否则引擎初始化会 panic
    init().expect("init must succeed");

    // 进程 worker 由 current_exe() 重启自身；集成测试进程不是 worker 入口，
    // 用 debug-only 钩子指向真正的 worker 二进制，端到端验证进程隔离链路
    // （同步 linker + sync::Command 实例化 + 退出码回传）。不设置线程回退，走真实进程路径。
    whitebox_core::test_hooks::worker_exe(Some(std::path::PathBuf::from(env!(
        "CARGO_BIN_EXE_whitebox_worker"
    ))));

    let runner = SandboxHandle::run_data(
        RANDOM_COMMAND_COMPONENT.as_bytes().to_vec(),
        &SandboxConfig::open(),
    )?;
    // wait_output 走 stdout 结果通道（排空 + 捕获），既验证进程链路也验证通道不阻塞
    let _ = runner.wait_output()?;
    Ok(())
}
