use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::{fs::read, num::NonZero, path::Path, sync::LazyLock};
use wasmtime::{component::JoinHandle, error::Ok, *};
use wasmtime_wasi::p2::bindings::io;
mod platform;

use platform::*;
static CONFIG: LazyLock<wasmtime::Config> = LazyLock::new(|| {
    let mut config = Config::new();

    config.wasm_component_model_async(true);

    config.wasm_component_model(true);
    config
});

//全局初始化引擎，考虑配置编译缓存，但是需要小心缓存中毒，需要考虑路径保护
static ENGINE: LazyLock<Engine> =
    LazyLock::new(|| Engine::new(&CONFIG.to_owned()).expect("failed to init wasmtime engine"));
use rayon::ThreadPoolBuilder;

static THREADPOOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    ThreadPoolBuilder::new()
        .num_threads(0 /*表示自动处理 */)
        .build()
        .unwrap()
});

pub fn init() -> anyhow::Result<()> {
    if std::env::var_os("__WASM_WORKER").is_some() {
        run_worker()?;
        std::process::exit(0);
    }
    anyhow::Ok(())
}

fn run_worker() -> anyhow::Result<()> {
    let mut stdin = std::io::stdin().lock();
    let wasm_path = read_string(&mut stdin)?;
    let input = read_bytes(&mut stdin)?;
    //setup_sandbox
    //connect channel
    run_wasm(&wasm_path, &input);
    anyhow::Ok(())
}
//阻塞运行
fn run_wasm(path: &String, input: &Vec<u8>) {}
use std::io::Read;

/// 从流里读一个 u32 长度前缀 + UTF-8 字符串
fn read_string(r: &mut impl Read) -> std::io::Result<String> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// 从流里读一个 u32 长度前缀 + 字节数组
fn read_bytes(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}
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

pub enum Runner {
    Thread(std::thread::JoinHandle<Result<()>>),
    Process(Child),
}
pub struct SandboxHandle {
    runner: Runner,
}

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
//pub fn load_from_cwasm_file() {}
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
use std::process::{Child, Command, Stdio};
impl SandboxHandle {
    pub fn run(path: PathBuf) -> anyhow::Result<Runner> {
        // set self exe path
        #[cfg(target_os = "linux")]
        let exe = std::path::PathBuf::from("/proc/self/exe"); //更安全

        #[cfg(not(target_os = "linux"))]
        let exe = std::env::current_exe()?;

        let spawn_command_result = Command::new(exe)
            .env("__WASM_WORKER", "1")
            .env_clear() // 清掉其他所有 env
            //.env("PATH", "/usr/bin:/bin") // 补回最小 PATH
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn();
        if let Err(e) = spawn_command_result {
            let cancel = Arc::new(AtomicBool::new(false));
            //创建进程失败，回落到创建线程
            //等会我写取消令牌
            let thread = std::thread::spawn(move || -> Result<()> {
                let sandbox = load_from_wasm_file(&path)?;
                Self::run_module(sandbox)?;
                Ok(())
            });
            anyhow::Ok(Runner::Thread(thread))
        } else {
            anyhow::Ok(Runner::Process(spawn_command_result.unwrap()))
            //创建进程成功
        }
    }

    pub fn run_module<T: Send>(mut sandbox: WasmSandbox<T>) -> Result<(), wasmtime::error::Error> {
        let start = sandbox
            .instance
            .get_typed_func::<(), ()>(&mut sandbox.store, "_start")?;

        start.call(&mut sandbox.store, ())?;
        /*无参数启动 */
        Ok(())
    }
}
