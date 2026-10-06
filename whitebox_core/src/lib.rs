use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::{path::Path, sync::LazyLock};
use tokio::sync::watch;
use wasmtime::*;
pub mod sandbox;
use sandbox::*;

//全局初始化引擎，考虑配置编译缓存，但是需要小心缓存中毒，需要考虑路径保护，喵！
static ENGINE: LazyLock<Engine> =
    LazyLock::new(|| Engine::new(&get_engine_config()).expect("failed to init wasmtime engine"));
//懒加载，在init初始化后才应该被读取
static IS_WORKER: OnceLock<Arc<AtomicBool>> = OnceLock::new();

fn get_engine_config() -> wasmtime::Config {
    let mut config = Config::new();
    if IS_WORKER
        .get()
        .expect("you must call init function first!")
        .as_ref()
        .load(Ordering::SeqCst)
    {
        //在worker模式中，则已进入进程，无需线程回退的异步协作模式，因为异步协作有性能损耗
        config
    } else {
        config.wasm_component_model_async(true);
        //config.consume_fuel(true);
        config.wasm_component_model(true);
        // epoch 打断已启用（线程回落关闭用）。必须配 load_module 在 instantiate_async 之前
        // 设置 deadline+callback，否则默认 deadline=0 + 回调未注册会直接 Trap::Interrupt。
        config.epoch_interruption(true);
        config
    }
}
//！用于确定全局状态，因此必须在一切core函数运行前运行！！喵！MIAO!

/// worker 进程失败退出的阶段码。父进程通过 `wait()/try_wait()` 的退出码区分失败阶段。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerExit {
    Ok = 0,
    /// 未知/未归类错误
    Other = 1,
    /// 帧协议 / IO 读取失败（管道损坏、长度非法）
    Io = 2,
    /// 帧 payload 反序列化失败（路径/输入/配置损坏）
    Decode = 3,
    /// wasm 模块读取失败（文件不可读、损坏）
    Load = 4,
    /// 实例化失败（WASI / linker / store）
    Instantiate = 5,
    /// 运行 `_start` 时 trap
    Run = 6,
}

impl WorkerExit {
    pub fn code(self) -> i32 {
        self as i32
    }

    /// [`WorkerExit::code`] 的逆映射：父进程拿到子进程退出码后还原失败阶段。
    /// 未知数字返回 `Err(该数字)`。
    pub fn decode(code: i32) -> std::result::Result<Self, i32> {
        match code {
            0 => Ok(WorkerExit::Ok),
            1 => Ok(WorkerExit::Other),
            2 => Ok(WorkerExit::Io),
            3 => Ok(WorkerExit::Decode),
            4 => Ok(WorkerExit::Load),
            5 => Ok(WorkerExit::Instantiate),
            6 => Ok(WorkerExit::Run),
            other => Err(other),
        }
    }
}

/// 带退出码的错误：`run_worker` 各阶段把底层错误按阶段包装进去。
#[derive(Debug)]
pub struct WorkerError {
    pub code: WorkerExit,
    source: anyhow::Error,
}

impl WorkerError {
    fn new(code: WorkerExit, source: impl Into<anyhow::Error>) -> Self {
        Self {
            code,
            source: source.into(),
        }
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for WorkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

pub fn init() -> anyhow::Result<()> {
    if std::env::var_os("__WASM_WORKER").is_some() {
        IS_WORKER.get_or_init(|| Arc::new(AtomicBool::new(true)));
        // 注：曾考虑让父进程捕获 worker 的 stderr 以获得结构化错误文本，但那本质是
        // 异步字节流（需线程持续读取，与同步 wait() 语义冲突），不便同步处理，
        // 故先用“阶段退出码”方案；结构化 stderr 捕获留作后续（见 TODO 相关讨论）。
        match run_worker() {
            Ok(()) => std::process::exit(WorkerExit::Ok.code()),
            Err(e) => {
                eprintln!("[whitebox] worker error (exit {}): {:#}", e.code.code(), e);
                std::process::exit(e.code.code());
            }
        }
    }
    IS_WORKER.get_or_init(|| Arc::new(AtomicBool::new(false)));
    anyhow::Ok(())
}
/// worker 协议帧 1 的载荷：wasm 按文件路径传输，还是整段字节传输。
/// 字节路径无需落盘、也不依赖文件系统授权（默认封闭文件系统下也能跑）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum WasmSource {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

///基于环境变量识别，启动自己的进程来为自己打工（
fn run_worker() -> Result<(), WorkerError> {
    let mut stdin = std::io::stdin().lock();
    //协议：三帧（payload 均 postcard 序列化）[模块源 WasmSource][输入字节 Vec<u8>][沙箱配置]
    let source_bytes = read_bytes(&mut stdin).map_err(|e| WorkerError::new(WorkerExit::Io, e))?;
    let source: WasmSource =
        postcard::from_bytes(&source_bytes).map_err(|e| WorkerError::new(WorkerExit::Decode, e))?;
    let input_bytes = read_bytes(&mut stdin).map_err(|e| WorkerError::new(WorkerExit::Io, e))?;
    let input: Vec<u8> =
        postcard::from_bytes(&input_bytes).map_err(|e| WorkerError::new(WorkerExit::Decode, e))?;
    let config_bytes = read_bytes(&mut stdin).map_err(|e| WorkerError::new(WorkerExit::Io, e))?;
    // 帧 3 缺失时用默认（全关闭）；损坏时 fail-closed。
    // 本地保留一份显式传给 run_wasm/build_store，避免依赖进程级全局配置。
    let wasi_config = if config_bytes.is_empty() {
        sandbox_config()
    } else {
        match postcard::from_bytes::<SandboxConfig>(&config_bytes) {
            Ok(config) => {
                // 全局配置仅用于 apply_sandbox（L1 进程沙箱）
                let _ = set_sandbox_config(config.clone());
                config
            }
            Err(e) => {
                // 配置损坏：fail-closed，拒绝运行（不落到默认配置）
                return Err(WorkerError::new(WorkerExit::Decode, e));
            }
        }
    };
    //先应用全局进程沙箱配置，不支持的配置项会打进 stderr 供审计
    apply_sandbox();
    //connect channel
    run_wasm(source, &input, &wasi_config)
}

/// 应用沙箱配置（L1，尽力而为：失败只报告不中止）并输出不支持项的警告
fn apply_sandbox_with(cfg: &SandboxConfig) {
    for item in setup_sandbox_with(cfg) {
        if item.status == ApplyStatus::Error {
            eprintln!(
                "[whitebox] sandbox capability '{}' is not supported on this platform",
                item.capability.as_str()
            );
        }
    }
}

/// 应用全局沙箱配置（worker 分支）
fn apply_sandbox() {
    apply_sandbox_with(&sandbox_config());
}
//阻塞运行
fn run_wasm(source: WasmSource, _input: &Vec<u8>, cfg: &SandboxConfig) -> Result<(), WorkerError> {
    let module = match source {
        // 路径来源需要父进程在 spawn 前显式授权读取（默认全关闭文件系统）
        WasmSource::Path(path) => {
            load_from_wasm_file(&path).map_err(|e| WorkerError::new(WorkerExit::Load, e))?
        }
        // 字节来源不经文件系统，默认封闭文件系统下也能运行
        WasmSource::Bytes(bytes) => {
            Module::new(&ENGINE, bytes).map_err(|e| WorkerError::new(WorkerExit::Load, e))?
        }
    };
    let sandbox = load_module_for_worker(module, Arc::new(AtomicBool::new(false)), cfg)
        .map_err(|e| WorkerError::new(WorkerExit::Instantiate, e))?; //后面的flag在此处是无用的
    SandboxHandle::run_module_for_worker(sandbox)
        .map_err(|e| WorkerError::new(WorkerExit::Run, e))?;
    Ok(())
}
use std::io::Read;

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
// 签名/内容校验的占位结构，后续接入
#[allow(dead_code)] // 签名/内容校验的占位结构，后续接入
pub struct WasmMetadata {
    pubkey: Option<Vec<u8>>,
    signature: Option<Vec<u8>>,
    //content_sha256:
}
/// Store 的宿主状态：WASI 上下文 + 宿主自定义标记
pub struct HostState {
    wasi: wasmtime_wasi::p1::WasiP1Ctx,
    marker: u32,
    shutdown_requested_only_for_threadfallback: Arc<AtomicBool>, //只对线程回落有用，不用就不管！哈！不许笑我
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
//I want to write a Handle strure ,but i'am too tired ^_^.wait. .  .for me

///用watch去控制thread。便于扩展
#[derive(Clone, PartialEq, Copy)]
pub enum ThreadCommand {
    Shutdown,
    Empty, //最初的填充
           //Other(Option<Vec<v8>>) 我在想什么……
}
pub enum Runner {
    Thread(
        std::thread::JoinHandle<Result<()>>,
        (watch::Sender<ThreadCommand>, Arc<AtomicBool>), /*关闭信号，本来准备写一整个数组watch但是好累，紫砂……操了还是写叭*//*草了还是需要一个关闭信号区别每个模块是否被关闭*/
    ),
    Process(Child),
}
/// 运行句柄：由 [`SandboxHandle::run`] / [`SandboxHandle::run_data`] 取得，
/// 提供 `wait`（等待结果）与 `shutdown`（主动终止）两个收敛入口。
pub struct SandboxHandle {
    runner: Runner,
}

pub fn load_from_wasm_file(path: &Path) -> Result<Module, wasmtime::Error> {
    // Box<dyn std::error::Error + 'static>

    let module = Module::from_file(&ENGINE, path)?;

    Ok(module)
}
pub fn load_wasm_bytes(wat: Vec<u8>) -> Result<Module, wasmtime::Error> {
    let module = Module::new(&ENGINE, wat)?;

    Ok(module)
}
//pub fn load_from_cwasm_file() {}

/// 构建 Linker：注册 host_func + WASI（worker 用同步、thread 用异步）
fn build_linker(async_wasi: bool) -> Result<Linker<HostState>, wasmtime::Error> {
    let mut linker = Linker::new(&ENGINE);
    if async_wasi {
        wasmtime_wasi::p1::add_to_linker_async(&mut linker, |state: &mut HostState| {
            &mut state.wasi
        })?;
    } else {
        //store.set_fuel(u64::MAX)?;考虑在开发者模式中加入可配置用以跟踪资源消耗？fuel/time
        //不是worker模式是线程
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |state: &mut HostState| {
            &mut state.wasi
        })?;
    }
    linker.func_wrap(
        "host",
        "host_func",
        |caller: Caller<'_, HostState>, param: i32| {
            println!("Got {} from WebAssembly", param);
            println!("my host state is: {}", caller.data().marker);
        },
    )?;
    Ok(linker)
}

/// 构建 Store（worker / thread 共用）：按调用方**显式传入**的 `SandboxConfig` WASI 段配置
/// guest 视野（不读进程级全局，避免多实例配置互相污染）。
/// WASI 是 L2：**配置失败必须报错终止**（如 preopen 目录打不开），由调用方冒泡成错误。
fn build_store(
    shutdown_flag: Arc<AtomicBool>,
    cfg: &SandboxConfig,
) -> Result<Store<HostState>, wasmtime::Error> {
    let mut builder = wasmtime_wasi::WasiCtxBuilder::new();

    use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
    match cfg.wasi_stdin {
        WasiStdio::Inherit => {
            builder.inherit_stdin();
        }
        WasiStdio::Capture => {
            builder.stdin(MemoryInputPipe::new(Vec::<u8>::new()));
        }
        WasiStdio::Null => {
            builder.stdin(std::io::empty());
        }
    }
    match cfg.wasi_stdout {
        WasiStdio::Inherit => {
            builder.inherit_stdout();
        }
        WasiStdio::Capture => {
            builder.stdout(MemoryOutputPipe::new(1 << 20));
        }
        WasiStdio::Null => {
            builder.stdout(std::io::empty());
        }
    }
    match cfg.wasi_stderr {
        WasiStdio::Inherit => {
            builder.inherit_stderr();
        }
        WasiStdio::Capture => {
            builder.stderr(MemoryOutputPipe::new(1 << 20));
        }
        WasiStdio::Null => {
            builder.stderr(std::io::empty());
        }
    }

    for (key, value) in &cfg.wasi_env {
        builder.env(key, value);
    }
    if !cfg.wasi_args.is_empty() {
        builder.args(&cfg.wasi_args);
    }
    for preopen in &cfg.wasi_preopens {
        let perms = if preopen.write {
            wasmtime_wasi::FsPerms::ReadWrite
        } else {
            wasmtime_wasi::FsPerms::ReadOnly
        };
        // 配置失败（目录不可开/权限不足）→ 直接报错终止（L2 fail-closed）
        builder
            .preopened_dir(&preopen.host_path, &preopen.guest_path, perms)
            .map_err(|e| {
                wasmtime::Error::msg(format!(
                    "wasi preopen '{}' -> '{}' failed: {e}",
                    preopen.host_path.display(),
                    preopen.guest_path
                ))
            })?;
    }

    let wasi_ctx = builder.build_p1();
    Ok(Store::new(
        &ENGINE,
        HostState {
            wasi: wasi_ctx,
            marker: 4,
            shutdown_requested_only_for_threadfallback: shutdown_flag,
        },
    ))
}

/// worker（进程）用加载：同步 linker + 同步 instantiate。
/// worker 引擎是纯同步配置，无需 tokio，也不会踩 epoch 问题。
pub fn load_module_for_worker(
    module: Module,
    shutdown_flag: Arc<AtomicBool>,
    cfg: &SandboxConfig,
) -> Result<WasmSandbox<HostState>, wasmtime::Error> {
    let linker = build_linker(false)?;
    let mut store = build_store(shutdown_flag, cfg)?;
    let instance = linker.instantiate(&mut store, &module)?;
    Ok(WasmSandbox::new(instance, store))
}

/// thread（宿主/线程回落）用加载：异步 linker + instantiate_async。
/// 必须在调用方自己的 tokio runtime 内 await（禁止再起嵌套 runtime）。
pub async fn load_module_for_thread(
    module: Module,
    shutdown_flag: Arc<AtomicBool>,
    cfg: &SandboxConfig,
) -> Result<WasmSandbox<HostState>, wasmtime::Error> {
    let linker = build_linker(true)?;
    let mut store = build_store(shutdown_flag, cfg)?;
    // epoch 打断：必须在 instantiate_async 之前设置，否则默认 deadline=0（恒已过期）会 Trap::Interrupt
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(SandboxHandle::epoch_callback);
    // add_to_linker_async 注册的导入会把 store 标记为 async-required，只能走 instantiate_async
    let instance = linker.instantiate_async(&mut store, &module).await?;
    Ok(WasmSandbox::new(instance, store))
}
use std::process::{Child, Command, Stdio};

use crate::Runner::{Process, Thread};
use wasmtime::{StoreContextMut, UpdateDeadline};

impl SandboxHandle {
    /// 等待运行结束并返回结果。
    /// - `Thread`：join 线程，合并内部错误；
    /// - `Process`：`wait` 子进程，按 [`WorkerExit`] 解码退出码（非 0 视为失败）。
    pub fn wait(self) -> anyhow::Result<()> {
        match self.runner {
            Runner::Thread(joinhandle, _) => joinhandle
                .join()
                .map_err(|_| anyhow::anyhow!("worker thread panicked"))?
                .map_err(|e| anyhow::anyhow!("{e}")),
            Runner::Process(mut child) => {
                let status = child.wait()?;
                if status.success() {
                    return Ok(());
                }
                match status.code() {
                    Some(code) => match WorkerExit::decode(code) {
                        Ok(worker_exit) => {
                            anyhow::bail!("worker failed (exit {code}: {:?})", worker_exit)
                        }
                        Err(unknown) => {
                            anyhow::bail!("worker failed with unknown exit code {unknown}")
                        }
                    },
                    None => anyhow::bail!("worker terminated by signal"),
                }
            }
        }
    }

    pub fn shutdown(self) -> anyhow::Result<()> {
        match self.runner {
            Thread(joinhandle, (_tx, shutdown_requested)) => {
                shutdown_requested.store(true, Ordering::Relaxed); //写入关闭标记
                ENGINE.to_owned().increment_epoch(); //直接调用计数，但不保证一定能立刻杀掉。直接搞会导致全误杀，我添加一个回调atomicbool检查来处理误杀。
                let _ = joinhandle.join(); //这个错误需要传播吗？我先想想，之后再说。喵喵喵喵喵喵喵喵喵！
            }
            Process(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        Ok(())
    }
    /// 以独立 worker 进程运行指定 wasm **文件**，并把沙箱参数作为第三帧传给 worker。
    /// 进程创建失败时回落到线程模式（相机应对）。
    ///
    /// 注意：`SandboxConfig::default()` 是 **全关闭**（`deny_file_access=true`）。
    /// 在 Linux/macOS 上，worker 应用沙箱后仍需读取该 wasm 文件路径，
    /// 因此 spawn 前必须显式授权，例如：
    /// `let cfg = SandboxConfig::new().allow_fs_read(parent_dir_of_wasm);`
    /// 说明：Linux Landlock 是“路径层级 + 祖先遍历”授权，只给 wasm 文件本身授权，
    /// 遍历其祖先目录仍会被拒，建议直接授权 wasm 所在目录的读权限；
    /// macOS seatbelt 用 `(allow file-read* (subpath "…"))` 直接对该路径生效，无需祖先授权。
    ///
    /// 若不想授文件路径权限，请改用 [`SandboxHandle::run_data`]（字节直传，不落盘）。
    pub fn run(path: PathBuf, config: &SandboxConfig) -> anyhow::Result<SandboxHandle> {
        Self::run_source(&WasmSource::Path(path), config)
    }

    /// 以独立 worker 进程运行内存中的 wasm 字节（不落盘、走字节帧传输，
    /// 默认封闭文件系统下也无需授权路径）。其余行为与 [`SandboxHandle::run`] 一致。
    pub fn run_data(data: Vec<u8>, config: &SandboxConfig) -> anyhow::Result<SandboxHandle> {
        Self::run_source(&WasmSource::Bytes(data), config)
    }

    fn run_source(source: &WasmSource, config: &SandboxConfig) -> anyhow::Result<SandboxHandle> {
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
        // 调试/集成测试钩子：WHITEBOX_FORCE_THREAD_FALLBACK=1 强制走线程回退。
        // 仅 debug 构建生效（release 编译掉，避免生产被会话级 env 静默降级）。如果有需求再开
        // 该变量只被父进程读取；worker 经 env_clear() 启动，不会继承，也不读它。
        let spawn_command_result = {
            #[cfg(debug_assertions)]
            {
                if std::env::var_os("WHITEBOX_FORCE_THREAD_FALLBACK").is_some() {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "WHITEBOX_FORCE_THREAD_FALLBACK set",
                    ))
                } else {
                    command.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()
                }
            }
            #[cfg(not(debug_assertions))]
            {
                command.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()
            }
        };
        //匿名通道相对安全，如果这个都保证不了安全的情况我觉得好像也没什么其它办法可以保护了（）
        //我反正思考了半天，发现好像能对本应用进行攻击的应用基本上也啥都能干了，防护好像没啥用（（（
        //只能说尽可能保证安全吧

        match spawn_command_result {
            Ok(mut child) => {
                // 写入三帧（payload 均 postcard 序列化，保证无损）：模块源(WasmSource) / 输入字节(Vec<u8>) / 沙箱配置
                let write_result = (|| -> anyhow::Result<()> {
                    let Some(mut stdin) = child.stdin.take() else {
                        anyhow::bail!("worker stdin unavailable");
                    };
                    let source_bytes = postcard::to_stdvec(source)?;
                    write_frame(&mut stdin, &source_bytes)?;
                    let input: Vec<u8> = Vec::new();
                    let input_bytes = postcard::to_stdvec(&input)?;
                    write_frame(&mut stdin, &input_bytes)?;
                    let config_bytes = postcard::to_stdvec(config)?;
                    write_frame(&mut stdin, &config_bytes)?;
                    drop(stdin);
                    Ok(())
                })();
                if let Err(e) = write_result {
                    let _ = child.kill();
                    return Err(e);
                }
                anyhow::Ok(SandboxHandle {
                    runner: Runner::Process(child),
                })
            }
            Err(_) => {
                //创建进程失败，回落到创建线程

                let config = config.clone();
                let source = source.clone();
                let shutdown_flag_for_thread = Arc::new(AtomicBool::new(false));
                let shutdown_flag_clone = shutdown_flag_for_thread.clone();
                let (tx, rx) = watch::channel(ThreadCommand::Empty); //信号
                let thread = std::thread::spawn(move || -> Result<()> {
                    //线程模式没有任何进程隔离，仅尽力应用沙箱限制（显式传本次配置，不写全局）
                    apply_sandbox_with(&config);
                    // 加载与运行在同一个 current-thread runtime 内闭环，避免嵌套 runtime panic
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let result = rt.block_on(async move {
                        let module = match source {
                            WasmSource::Path(path) => load_from_wasm_file(&path)?,
                            WasmSource::Bytes(bytes) => Module::new(&ENGINE, bytes)?,
                        };
                        let sandbox =
                            load_module_for_thread(module, shutdown_flag_clone, &config).await?;
                        Self::run_module_for_thread(sandbox, rx).await
                    });
                    result?; //传播错误

                    Ok(())
                });
                anyhow::Ok(SandboxHandle {
                    runner: Runner::Thread(thread, (tx, shutdown_flag_for_thread)),
                })
            }
        }
    }
    fn epoch_callback(
        ctx: StoreContextMut<'_, HostState>,
    ) -> std::result::Result<UpdateDeadline, wasmtime::Error> {
        let data = ctx.data(); //拿到储存到hoststate的数据
        if data
            .shutdown_requested_only_for_threadfallback
            .load(Ordering::Relaxed)
        //
        {
            Err(wasmtime::Error::msg("Shutdown requested"))
        } else {
            Ok(UpdateDeadline::Continue(1))
        }
    }
    pub fn run_module_for_worker(
        mut sandbox: WasmSandbox<HostState>,
    ) -> Result<(), wasmtime::error::Error> {
        let start = sandbox
            .instance
            .get_typed_func::<(), ()>(&mut sandbox.store, "_start")?;
        start.call(&mut sandbox.store, ())?;
        Ok(())
    }
    // watch 取消线路暂时整体注释：关闭统一走 shutdown() 的 epoch/increment + HostState flag
    pub async fn run_module_for_thread(
        mut sandbox: WasmSandbox<HostState>,
        _rx: watch::Receiver<ThreadCommand>,
    ) -> Result<(), wasmtime::error::Error> {
        let start = sandbox
            .instance
            .get_typed_func::<(), ()>(&mut sandbox.store, "_start")?;

        // deadline+callback 幂等刷新（load_module 已在实例化前置好；shutdown 依赖 epoch）
        sandbox.store.set_epoch_deadline(1);
        sandbox.store.epoch_deadline_callback(Self::epoch_callback);
        /*
        let watch_task = tokio::spawn(async move {
            loop {
                if let Err(e) = rx.changed().await {
                    //通道关闭了
                    break;
                };

                match *rx.borrow_and_update() {
                    ThreadCommand::Shutdown => {
                        //经过分析认为此处不可靠直接移到shutdown函数去
                        //ENGINE.to_owned().increment_epoch();
                    }
                    ThreadCommand::Empty => {}
                }
            }
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        });
        tokio::select! {
            result = start.call_async(&mut sandbox.store, ()) => {
                result?;
            }
            _ = watch_task => {
                // sender 关闭了，但通常不会走到这里
            }
        }
        */
        start.call_async(&mut sandbox.store, ()).await?;

        /*无参数启动 */
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerExit;

    #[test]
    fn worker_exit_code_roundtrip() {
        // code() 与 decode() 互逆，且未知数字报错
        for want in [
            WorkerExit::Ok,
            WorkerExit::Other,
            WorkerExit::Io,
            WorkerExit::Decode,
            WorkerExit::Load,
            WorkerExit::Instantiate,
            WorkerExit::Run,
        ] {
            let got = WorkerExit::decode(want.code()).expect("known code must decode");
            assert_eq!(got, want);
        }
        assert_eq!(WorkerExit::decode(99), Err(99));
    }
}
