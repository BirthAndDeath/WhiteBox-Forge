//use rayon::prelude::*;
use std::{fs::read, num::NonZero, path::Path, sync::LazyLock};
use wasmtime::{component::JoinHandle, error::Ok, *};
use wasmtime_wasi::p2::bindings::io;

//全局初始化引擎，考虑配置编译缓存，但是需要小心缓存中毒，需要考虑路径保护
static ENGINE: LazyLock<Engine> = LazyLock::new(Engine::default);
use rayon::ThreadPoolBuilder;

/*static THREADPOOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    ThreadPoolBuilder::new()
        .num_threads(0 /*表示自动处理 */)
        .build()
        .unwrap()
}); */
pub struct WasmSandbox<T: 'static> {
    instance: Instance,
    store: Store<T>,
}
impl<T> WasmSandbox<T> {
    pub fn call_func(&mut self, name: &str) -> Result<(), Error> {
        let func = self
            .instance
            .get_typed_func::<(), ()>(&mut self.store, name)?;
        func.call(&mut self.store, ())
    }
    pub fn new(instance: Instance, store: Store<T>) -> Self {
        Self { instance, store }
    }
}
pub struct SandboxHandle {}
pub fn load_from_wasm_file(path: &Path) -> Result<WasmSandbox<u32>, wasmtime::Error> {
    // Box<dyn std::error::Error + 'static>

    let module = Module::from_file(&ENGINE, path)?;
    let sandbox = load_module(module)?;
    Ok(sandbox)
}
pub fn load_wasm_bytes(wat: Vec<u8>) -> Result<WasmSandbox<u32>, wasmtime::Error> {
    let module = Module::new(&ENGINE, wat)?;
    let sandbox = load_module(module)?;
    Ok(sandbox)
}
pub fn load_from_cwasm_file() {}
pub fn load_module(module: Module) -> Result<WasmSandbox<u32>, wasmtime::Error> {
    let mut linker = Linker::new(&ENGINE);

    linker.func_wrap(
        "host",
        "host_func",
        |caller: Caller<'_, u32>, param: i32| {
            println!("Got {} from WebAssembly", param);
            println!("my host state is: {}", caller.data());
        },
    )?;
    let mut store: Store<u32> = Store::new(&ENGINE, 4);

    let instance = linker.instantiate(&mut store, &module)?;
    let sandbox = WasmSandbox::new(instance, store);
    Ok(sandbox)
}
impl SandboxHandle {
    pub fn run_module<T: Send>(
        mut sandbox: WasmSandbox<T>,
    ) -> Result<std::thread::JoinHandle<Result<(), wasmtime::error::Error>>, wasmtime::error::Error>
    {
        let start = sandbox
            .instance
            .get_typed_func::<(), ()>(&mut sandbox.store, "_start")?;

        let handle = std::thread::spawn(move || {
            start.call(&mut sandbox.store, ())?;
            /*无参数启动 */
            Ok(())
        });
        Ok(handle)
    }
}
