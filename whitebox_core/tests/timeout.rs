mod common;

use common::SPIN_COMMAND_COMPONENT;
use std::time::Duration;
use whitebox_core::SandboxHandle;
use whitebox_core::sandbox::SandboxConfig;
use whitebox_core::*;

#[test]
fn wait_timeout_does_not_hang_on_spinning_guest() -> Result<(), Box<dyn std::error::Error>> {
    //标定全局状态（IS_WORKER 等），否则引擎初始化会 panic
    init().expect("init must succeed");

    // 走线程回退：死循环 guest 只能靠 epoch 协作打断，验证 wait_timeout 有界返回、不无限阻塞。
    whitebox_core::test_hooks::force_thread_fallback(true);

    let runner = SandboxHandle::run_data(
        SPIN_COMMAND_COMPONENT.as_bytes().to_vec(),
        &SandboxConfig::open(),
    )?;
    // 必须返回（不挂死）；无论返回“打断后的真实错误”还是“放弃错误”，都不能是 Ok
    let err = runner
        .wait_timeout(Duration::from_millis(500))
        .expect_err("spinning guest must not report success");
    println!("wait_timeout returned as expected: {err}");
    Ok(())
}

// 线程关闭端到端：shutdown() 置标志 + 循环轻推 epoch，打断死循环 guest 并 join 返回。
#[test]
fn shutdown_interrupts_spinning_guest() -> Result<(), Box<dyn std::error::Error>> {
    init().expect("init must succeed");

    whitebox_core::test_hooks::force_thread_fallback(true);

    let runner = SandboxHandle::run_data(
        SPIN_COMMAND_COMPONENT.as_bytes().to_vec(),
        &SandboxConfig::open(),
    )?;
    // 留出时间让 guest 真正进入死循环，再请求关闭
    std::thread::sleep(Duration::from_millis(150));
    runner
        .shutdown()
        .expect("epoch interrupt should stop the spinning guest");
    Ok(())
}
