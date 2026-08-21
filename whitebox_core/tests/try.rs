use whitebox_core::*;
#[test]
fn try_use() -> Result<(), wasmtime::error::Error> {
    let wat = r#"
        (module
            (import "host" "host_func" (func $host_hello (param i32)))

            (func (export "_start")
                i32.const 3
                call $host_hello)
        )
    "#;
    println!("If you see this, --nocapture is enabled!");
    let sandbox = load_wat_bytes(wat.into())?;
    SandboxHandle::run_module(sandbox)?.join();
    Ok(())
}
