use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::{path::Path, sync::LazyLock};
use wasmtime::*;
pub mod sandbox;

use sandbox::*;

//全局初始化引擎，考虑配置编译缓存，但是需要小心缓存中毒，需要考虑路径保护，喵！
static ENGINE: LazyLock<Engine> =
    LazyLock::new(|| Engine::new(&get_engine_config()).expect("failed to init wasmtime engine"));
//懒加载，在init初始化后才应该被读取
static CONFIG: OnceLock<Config> = OnceLock::new();
static IS_WORKER: OnceLock<Arc<AtomicBool>> = OnceLock::new();
use rayon::ThreadPoolBuilder;

static THREADPOOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    ThreadPoolBuilder::new()
        .num_threads(0 /*表示自动处理 */)
        .build()
        .unwrap()
});

fn get_engine_config() -> wasmtime::Config {
    let mut config = Config::new();
    if IS_WORKER
        .get()
        .expect("you must call init function first!")
        .as_ref()
        .load(Ordering::SeqCst)
    {
        //在worker模式中，则已进入进程，无需线程回退的异步协作模式，因为异步协作有性能损耗
        return config;
    } else {
        config.wasm_component_model_async(true);
        config.consume_fuel(true);
        config.wasm_component_model(true);
        config
    }
}
///！用于确定全局状态，因此必须在一切core函数运行前运行！！喵！MIAO!
pub fn init() -> anyhow::Result<()> {
    if std::env::var_os("__WASM_WORKER").is_some() {
        IS_WORKER.get_or_init(|| Arc::new(AtomicBool::new(true)));
        run_worker()?;
        std::process::exit(0);
    }
    IS_WORKER.get_or_init(|| Arc::new(AtomicBool::new(false)));
    anyhow::Ok(())
}

fn run_worker() -> anyhow::Result<()> {
    let mut stdin = std::io::stdin().lock();
    //协议：三帧 [wasm 路径][输入字节][沙箱配置字节]
    let wasm_path = read_string(&mut stdin)?;
    let input = read_bytes(&mut stdin)?;
    let config_bytes = read_bytes(&mut stdin)?;
    if !config_bytes.is_empty() {
        //父进程传入的沙箱参数 → 全局配置
        if let Ok(config) = bincode::deserialize::<SandboxConfig>(&config_bytes) {
            let _ = set_sandbox_config(config);
        }
    }
    //先应用全局进程沙箱配置，不支持的配置项会打进 stderr 供审计
    apply_sandbox();
    //connect channel
    run_wasm(&wasm_path, &input);
    anyhow::Ok(())
}

/// 应用全局沙箱配置并输出不支持项的警告
fn apply_sandbox() {
    for item in setup_sandbox() {
        if item.status == ApplyStatus::Error {
            eprintln!(
                "[whitebox] sandbox capability '{}' is not supported on this platform",
                item.capability.as_str()
            );
        }
    }
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

/// 往流里写一个 u32 长度前缀 + 字节数组（与 read_bytes 对称）
fn write_frame(w: &mut impl std::io::Write, bytes: &[u8]) -> std::io::Result<()> {
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(bytes)
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
    // 宿主开启了 consume_fuel；若不设置燃料额度，任何调用都会因“燃料耗尽”立刻陷入
    store.set_fuel(u64::MAX)?;

    let instance = linker.instantiate(&mut store, &module)?;
    let sandbox = WasmSandbox::new(instance, store);
    Ok(sandbox)
}
use std::process::{Child, Command, Stdio};
impl SandboxHandle {
    /// 以独立 worker 进程运行指定 wasm，并把沙箱参数作为第三帧传给 worker。
    /// 进程创建失败时回落到线程模式（相机应对）。
    pub fn run(path: PathBuf, config: &SandboxConfig) -> anyhow::Result<Runner> {
        // set self exe path
        #[cfg(target_os = "linux")]
        let exe = std::path::PathBuf::from("/proc/self/exe"); //更安全

        #[cfg(not(target_os = "linux"))]
        let exe = std::env::current_exe()?;

        let mut command = Command::new(exe);
        //必须先清环境再设置标志：env_clear() 会抹掉其后设置的全部变量，反之则保留
        command
            .env_clear() // 清掉其他所有 env
            .env("__WASM_WORKER", "1");
        //.env("PATH", "/usr/bin:/bin") // 补回最小 PATH
        // Windows 上 DLL 搜索依赖 SystemRoot：清空后至少恢复最小 PATH，否则 worker 可能起不来
        #[cfg(target_os = "windows")]
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            let sys = system_root.to_string_lossy();
            command.env("PATH", format!("{sys}\\System32;{sys}"));
        }
        let spawn_command_result = command.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn();
        //匿名通道相对安全，如果这个都保证不了安全的情况我觉得好像也没什么其它办法可以保护了（）
        //我反正思考了半天，发现好像能对本应用进行攻击的应用基本上也啥都能干了，防护好像没啥用（（（
        //只能说尽可能保证安全吧

        match spawn_command_result {
            Ok(mut child) => {
                // 写入三帧：wasm 路径 / 输入(空) / 沙箱配置，随后关闭 stdin 让 worker 读到 EOF
                let write_result = (|| -> anyhow::Result<()> {
                    let Some(mut stdin) = child.stdin.take() else {
                        anyhow::bail!("worker stdin unavailable");
                    };
                    write_frame(&mut stdin, path.to_string_lossy().as_bytes())?;
                    write_frame(&mut stdin, &[])?;
                    let config_bytes = bincode::serialize(config)?;
                    write_frame(&mut stdin, &config_bytes)?;
                    drop(stdin);
                    Ok(())
                })();
                if let Err(e) = write_result {
                    let _ = child.kill();
                    return Err(e);
                }
                anyhow::Ok(Runner::Process(child))
            }
            Err(_) => {
                //创建进程失败，回落到创建线程
                //等会我写取消令牌
                let config = config.clone();
                let thread = std::thread::spawn(move || -> Result<()> {
                    //线程模式没有任何进程隔离，仅尽力应用沙箱限制
                    let _ = set_sandbox_config(config);
                    apply_sandbox();
                    let sandbox = load_from_wasm_file(&path)?;
                    Self::run_module(sandbox)?;
                    Ok(())
                });
                anyhow::Ok(Runner::Thread(thread))
            }
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
