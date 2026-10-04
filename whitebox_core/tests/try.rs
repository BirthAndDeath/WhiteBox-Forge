use whitebox_core::*;
#[test]
fn try_use() -> Result<(), wasmtime::error::Error> {
    //标定全局状态（IS_WORKER 等），否则引擎初始化会 panic
    init().expect("init must succeed");
    let wat = r#"
        (module
            (import "host" "host_func" (func $host_hello (param i32)))

            (func (export "_start")
                i32.const 3
                call $host_hello)
        )
    "#;
    println!("If you see this, --nocapture is enabled!");
    let sandbox = load_wasm_bytes(wat.into())?;
    SandboxHandle::run_module(sandbox)?;
    Ok(())
}
