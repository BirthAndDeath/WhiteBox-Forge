//! 进程 worker 入口：由父进程以 `__WASM_WORKER=1` 重启自身时进入。
//! 仅供进程隔离路径与集成测试使用；正常宿主程序不应直接调用。
fn main() -> anyhow::Result<()> {
    // worker 模式：init() 读取 stdin 协议帧、运行 guest，随后 process::exit；
    // 非 worker 模式：正常返回。
    whitebox_core::init()?;
    anyhow::Ok(())
}
