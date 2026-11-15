use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use std::{path::Path, sync::LazyLock};
use wasmtime::*;
use wasmtime_wasi::sockets::SocketAddrUse;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
pub mod sandbox;
// 目录模块的根文件是 `dir/dir.rs`，其子模块用显式 `#[path]` 指向同目录文件。
// The `dir` module root is `dir/dir.rs`; its submodules point at sibling files via `#[path]`.
#[path = "dir/dir.rs"]
pub mod dir;
pub mod config;
mod worker_log;
use sandbox::*;

/// 集成测试专用钩子（仅 `debug_assertions`；release 编译掉）。
///
/// 取代原先的会话级环境变量开关：`std::env::set_var` 在 edition 2024 为 `unsafe`，
/// 且同一测试二进制内并行测试对 env 的读写存在数据竞争。这里改用进程内全局开关
/// （原子 / `Mutex`），串行、并行都无 UB。
#[cfg(debug_assertions)]
pub mod test_hooks {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    static FORCE_THREAD_FALLBACK: AtomicBool = AtomicBool::new(false);
    static WORKER_EXE: Mutex<Option<PathBuf>> = Mutex::new(None);

    /// 强制 `run*` 走线程回退（跳过进程 spawn），用于测试线程回退路径。
    pub fn force_thread_fallback(enabled: bool) {
        FORCE_THREAD_FALLBACK.store(enabled, Ordering::SeqCst);
    }

    /// 覆盖 worker 可执行文件路径（集成测试指向真正的 worker 二进制以验证进程链路）。
    pub fn worker_exe(path: Option<PathBuf>) {
        *WORKER_EXE.lock().unwrap() = path;
    }

    pub(crate) fn thread_fallback_forced() -> bool {
        FORCE_THREAD_FALLBACK.load(Ordering::SeqCst)
    }

    pub(crate) fn worker_exe_override() -> Option<PathBuf> {
        WORKER_EXE.lock().unwrap().clone()
    }
}

//全局初始化引擎，考虑配置编译缓存，但是需要小心缓存中毒，需要考虑路径保护，喵！
static ENGINE: LazyLock<Engine> =
    LazyLock::new(|| Engine::new(&get_engine_config()).expect("failed to init wasmtime engine"));
//懒加载，在init初始化后才应该被读取
static IS_WORKER: OnceLock<Arc<AtomicBool>> = OnceLock::new();
/// 宿主解析的编译缓存目录；必须在 `ENGINE` 首次初始化前设置（全局引擎只初始化一次）。
/// Host-resolved compile-cache dir; must be set before the first `ENGINE` init (the engine
/// is global and initialized once).
static COMPILE_CACHE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 记录本次运行的编译缓存目录（首次设置生效，与全局引擎一致）。
/// Record the compile-cache dir for this run (first set wins, matching the global engine).
fn set_compile_cache_dir(dir: Option<PathBuf>) {
    let _ = COMPILE_CACHE_DIR.set(dir);
}

fn get_engine_config() -> wasmtime::Config {
    let mut config = Config::new();
    // 未 init 时按非 worker 处理（不再 panic）；公开入口会用 require_init 提前报错。
    let is_worker = IS_WORKER
        .get()
        .map(|flag| flag.load(Ordering::SeqCst))
        .unwrap_or(false);
    if is_worker {
        //在worker模式中，则已进入进程，无需线程回退的异步协作模式，因为异步协作有性能损耗。
        // 但组件模型（p2）是硬要求，显式开启避免依赖默认值。
        config.wasm_component_model(true);
    } else {
        config.wasm_component_model_async(true);
        //config.consume_fuel(true);
        config.wasm_component_model(true);
        // epoch 打断已启用（线程回落关闭用）。必须配 load_component_for_thread 在实例化之前
        // 设置 deadline+callback，否则默认 deadline=0 + 回调未注册会直接 Trap::Interrupt。
        config.epoch_interruption(true);
    }
    attach_compile_cache(&mut config);
    config
}

/// 启用 wasmtime 编译缓存：仅当宿主解析出可用目录时；失败则保持禁用。
///
/// 缓存是纯优化，不影响正确性/安全；因此这里 best-effort：目录不存在/`Cache::new`
/// 失败都只是不启用，不阻断启动。目录由 [`SandboxConfig::enable_compile_cache`] 在宿主侧
/// 解析（每用户 cache 目录；portable 为 `<exe_dir>/cache`）并随配置传到 worker。
/// Enable the wasmtime compile cache only when the host resolved a usable directory;
/// failures leave it disabled. The cache is a pure optimization, so this is best-effort and
/// never blocks startup. The directory is resolved host-side by
/// `SandboxConfig::enable_compile_cache` (per-user cache dir; portable `<exe_dir>/cache`)
/// and carried to the worker in the config.
fn attach_compile_cache(config: &mut wasmtime::Config) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(dir) = COMPILE_CACHE_DIR.get().and_then(|d| d.as_ref()) {
        // 目录由宿主侧 `enable_compile_cache` 创建，且 L1 白名单已允许其读写。
        // The dir is created host-side by `enable_compile_cache`; L1 already allows it.
        if dir.is_dir() {
            let mut cache_config = wasmtime::CacheConfig::new();
            // `with_directory` 要求绝对路径；宿主解析出的路径已是绝对路径。
            cache_config.with_directory(dir.as_path());
            if let Ok(cache) = wasmtime::Cache::new(cache_config) {
                config.cache(Some(cache));
            }
        }
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let _ = config;
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
    // 配置冲突（如 deny_network 与 sockets 越权）必须在落盘前拒绝：
    // 否则 `apply_sandbox` 里的 assert! 会 panic，把配置错误变成不可诊断的崩溃。
    let violations = wasi_config.validate();
    if !violations.is_empty() {
        return Err(WorkerError::new(
            WorkerExit::Decode,
            anyhow::anyhow!("沙箱配置冲突：{}", violations.join("；")),
        ));
    }
    // 在应用沙箱与首次初始化 ENGINE 之前，把宿主解析的缓存目录告知引擎。
    // Tell the engine the host-resolved cache dir before sandboxing / first ENGINE init.
    set_compile_cache_dir(wasi_config.compile_cache_dir.clone());
    //先应用全局进程沙箱配置，不支持的配置项会打进 stderr 供审计
    apply_sandbox();
    //connect channel
    run_wasm(source, &input, &wasi_config)
}

/// 应用沙箱配置（L1，尽力而为：失败只报告不中止）并输出不支持/权限不足项的警告。
fn apply_sandbox_with(cfg: &SandboxConfig) {
    for item in setup_sandbox_with(cfg) {
        match item.status {
            ApplyStatus::Error => eprintln!(
                "[whitebox] sandbox capability '{}' is not supported on this platform",
                item.capability.as_str()
            ),
            ApplyStatus::Denied => eprintln!(
                "[whitebox] sandbox capability '{}' could not be applied: permission denied",
                item.capability.as_str()
            ),
            _ => {}
        }
    }
}

/// 应用全局沙箱配置（worker 分支）
fn apply_sandbox() {
    apply_sandbox_with(&sandbox_config());
}
//阻塞运行
fn run_wasm(source: WasmSource, input: &[u8], cfg: &SandboxConfig) -> Result<(), WorkerError> {
    let component = match source {
        // 路径来源需要父进程在 spawn 前显式授权读取（默认全关闭文件系统）
        WasmSource::Path(path) => {
            load_from_wasm_file(&path).map_err(|e| WorkerError::new(WorkerExit::Load, e))?
        }
        // 字节来源不经文件系统，默认封闭文件系统下也能运行
        WasmSource::Bytes(bytes) => {
            load_wasm_bytes(bytes).map_err(|e| WorkerError::new(WorkerExit::Load, e))?
        }
    };
    let sandbox =
        load_component_for_worker(component, Arc::new(AtomicBool::new(false)), cfg, input)
            .map_err(|e| WorkerError::new(WorkerExit::Instantiate, e))?; //后面的flag在此处是无用的
    let captured = SandboxHandle::run_module_for_worker(sandbox)
        .map_err(|e| WorkerError::new(WorkerExit::Run, e))?;
    // Capture 模式的 stdout 由这里落回 worker 的进程 stdout，父进程排空后经 wait_output 取回
    if !captured.is_empty() {
        use std::io::Write as _;
        let _ = std::io::stdout().write_all(&captured);
    }
    Ok(())
}
use std::io::Read;

/// 单帧上限：长度前缀是 guest 不可控但协议输入端仍应设防的 u32（最高 4 GiB）。
/// 不设上限时，构造一个超大长度即可让 worker 直接按该值分配 → OOM。
const MAX_PROTOCOL_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// 从流里读一个 u32 长度前缀 + 字节数组
fn read_bytes(r: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_PROTOCOL_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds limit {MAX_PROTOCOL_FRAME_BYTES}"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// 往流里写一个 u32 长度前缀 + 字节数组（与 read_bytes 对称）
fn write_frame(w: &mut impl std::io::Write, bytes: &[u8]) -> std::io::Result<()> {
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(bytes)
}

/// worker stdout 捕获上限：超出部分继续读取但丢弃。
/// 不设上限时，恶意 guest 持续输出即可让父进程把整段输出缓进内存 → OOM。
/// 8 MiB 足以容纳常规输出；与 `WasiStdio::Capture` 的 1 MiB 管道同为有界策略。
const MAX_WORKER_STDOUT_BYTES: usize = 8 * 1024 * 1024;

/// 后台排空 worker stdout 并返回捕获内容：持续读取直到 EOF（避免管道写满阻塞），
/// 但只保留前 `MAX_WORKER_STDOUT_BYTES` 字节，超限部分读取后丢弃。
fn spawn_stdout_drain(reader: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut captured = Vec::new();
        let mut chunk = [0u8; 8 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let room = MAX_WORKER_STDOUT_BYTES.saturating_sub(captured.len());
                    if room > 0 {
                        captured.extend_from_slice(&chunk[..n.min(room)]);
                    }
                    // 超限字节仍须继续读走：否则管道写满后 worker 会阻塞。
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        captured
    })
}

/// 端口是否落在任一授权段内（含端点）。
fn port_in_ranges(ranges: &[PortRange], port: u16) -> bool {
    ranges.iter().any(|(lo, hi)| port >= *lo && port <= *hi)
}

/// 端口策略（按远程端口/回环判定），与 `SocketAddrUse` 无关，亦便于单测。
fn socket_addr_allowed(policy: &WasiSockets, addr: std::net::SocketAddr) -> bool {
    match policy {
        WasiSockets::None => false,
        WasiSockets::Allow(ports) => port_in_ranges(ports, addr.port()),
        WasiSockets::Deny(ports) => !port_in_ranges(ports, addr.port()),
        WasiSockets::LoopbackOnly => addr.ip().is_loopback(),
    }
}

/// 结合 `SocketAddrUse` 的完整判定（`socket_addr_check` 的宿主侧策略）：
/// - 出站/收包（connect/send/receive）按端口/回环策略；
/// - 绑定只放行 OS 分配临时端口的隐式绑定（connect/send 的前置步骤，地址为通配 + 端口 0），
///   拒绝显式固定端口绑定；
/// - 监听 / 接受入站一律拒绝：本策略只授出站，避免 guest 在宿主上开监听。
fn socket_addr_use_allowed(
    policy: &WasiSockets,
    addr: std::net::SocketAddr,
    use_: SocketAddrUse,
) -> bool {
    match use_ {
        SocketAddrUse::TcpConnect | SocketAddrUse::UdpSend | SocketAddrUse::UdpReceive => {
            socket_addr_allowed(policy, addr)
        }
        SocketAddrUse::TcpBind | SocketAddrUse::UdpBind => {
            !matches!(policy, WasiSockets::None) && addr.port() == 0
        }
        SocketAddrUse::TcpListen | SocketAddrUse::TcpAccept => false,
    }
}

/// 确定性墙钟：恒为 0（同 seed / 同观测，可复现）。
struct DeterministicWallClock;
impl wasmtime_wasi::HostWallClock for DeterministicWallClock {
    fn resolution(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

/// 确定性单调钟：初值取 seed 低位，每次读取前进 1ms（避免 guest 等待时间永不动而空转）。
struct DeterministicMonotonicClock {
    now_ns: AtomicU64,
}
impl DeterministicMonotonicClock {
    fn new(seed: u128) -> Self {
        Self {
            now_ns: AtomicU64::new(seed as u64),
        }
    }
}
impl wasmtime_wasi::HostMonotonicClock for DeterministicMonotonicClock {
    fn resolution(&self) -> u64 {
        1_000_000
    }
    fn now(&self) -> u64 {
        self.now_ns.fetch_add(1_000_000, Ordering::Relaxed)
    }
}
// 签名/内容校验（WasmMetadata 占位）暂无消费方，已随死代码清理移除；接入签名校验时再加。
/// Store 的宿主状态：WASI p2 上下文 + 资源表。
/// p2 的 host 实现通过 `WasiView` 取用这里的 `WasiCtx`/`ResourceTable`。
pub struct HostState {
    wasi: WasiCtx,
    table: component::ResourceTable,
    shutdown_requested_only_for_threadfallback: Arc<AtomicBool>, //只对线程回落有用，不用就不管！哈！不许笑我
    /// `WasiStdio::Capture` 时的 stdout 捕获句柄（run 后取回内容；否则 None）。
    captured_stdout: Option<wasmtime_wasi::p2::pipe::MemoryOutputPipe>,
    /// `WasiStdio::Capture` 时的 stderr 捕获句柄。
    captured_stderr: Option<wasmtime_wasi::p2::pipe::MemoryOutputPipe>,
    /// L2 资源上限（线性内存）：由 `SandboxConfig::max_memory_bytes` 派生，经 `Store::limiter` 生效。
    limits: StoreLimits,
}
impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
/// worker（进程，同步）已实例化、待运行的组件沙箱。
pub struct WorkerSandbox {
    store: Store<HostState>,
    command: wasmtime_wasi::p2::bindings::sync::Command,
}
/// thread（宿主线程回落，异步）已实例化、待运行的组件沙箱。
pub struct ThreadSandbox {
    store: Store<HostState>,
    command: wasmtime_wasi::p2::bindings::Command,
}
//I want to write a Handle strure ,but i'am too tired ^_^.wait. .  .for me

pub enum Runner {
    Thread(
        std::thread::JoinHandle<Result<()>>,
        (Arc<AtomicBool>, Arc<AtomicBool>), /* (shutdown_requested, finished)：finished 供有界等待轮询；
                                             * 线程无法安全强杀，只能尽力打断 */
    ),
    /// 子进程 + stdout 排空线程 + worker stderr 日志线程。
    /// Child + stdout drain + worker stderr log thread.
    Process(
        Child,
        Option<std::thread::JoinHandle<Vec<u8>>>,
        Option<std::thread::JoinHandle<()>>,
    ),
}
/// 运行句柄：由 [`SandboxHandle::run`] / [`SandboxHandle::run_data`] 取得，
/// 提供 `wait`（等待结果）与 `shutdown`（主动终止）两个收敛入口。
pub struct SandboxHandle {
    /// `Some` 表示尚未被 `wait*` / `shutdown` 消费；被取走后 Drop 不再做清理。
    /// 用 Option 是因为实现 Drop 后不能再从 `self` 里按值移出字段。
    runner: Option<Runner>,
}

impl Drop for SandboxHandle {
    /// 句柄被丢弃却未 `wait*` / `shutdown`：尽力终止，避免 guest 脱离管控继续运行。
    fn drop(&mut self) {
        match self.runner.take() {
            Some(Runner::Process(mut child, _drain, _log)) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            Some(Runner::Thread(_join, (shutdown_requested, _finished))) => {
                // 只置标志 + 单次轻推 epoch：回调按 store 自己的 flag 判定，不会误杀并发运行。
                shutdown_requested.store(true, Ordering::SeqCst);
                ENGINE.increment_epoch();
            }
            None => {}
        }
    }
}

/// 公开入口前置检查：必须先调用 [`init`]（否则引擎/工作模式未知）。返回错误而非 panic。
fn require_init() -> anyhow::Result<()> {
    if IS_WORKER.get().is_none() {
        anyhow::bail!("whitebox_core::init() 必须在任何 core 函数之前调用");
    }
    Ok(())
}

pub fn load_from_wasm_file(path: &Path) -> Result<component::Component, wasmtime::Error> {
    require_init().map_err(|e| wasmtime::Error::msg(e.to_string()))?;
    // Box<dyn std::error::Error + 'static>

    let component = component::Component::from_file(&ENGINE, path)?;

    Ok(component)
}
pub fn load_wasm_bytes(wat: Vec<u8>) -> Result<component::Component, wasmtime::Error> {
    require_init().map_err(|e| wasmtime::Error::msg(e.to_string()))?;
    let component = component::Component::new(&ENGINE, wat)?;

    Ok(component)
}
//pub fn load_from_cwasm_file() {}

/// 构建 Linker：注册 WASI p2（worker 用同步、thread 用异步）。
fn build_linker(async_wasi: bool) -> Result<component::Linker<HostState>, wasmtime::Error> {
    let mut linker = component::Linker::new(&ENGINE);
    if async_wasi {
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    } else {
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    }
    Ok(linker)
}

/// 构建 Store（worker / thread 共用）：按调用方**显式传入**的 `SandboxConfig` WASI 段配置
/// guest 视野（不读进程级全局，避免多实例配置互相污染）。
/// `input` 为喂给 guest WASI stdin 的字节（空则按模式：Inherit 继承宿主 / Capture 空 / Null）。
/// WASI 是 L2：**配置失败必须报错终止**（如 preopen 目录打不开），由调用方冒泡成错误。
fn build_store(
    shutdown_flag: Arc<AtomicBool>,
    cfg: &SandboxConfig,
    input: &[u8],
) -> Result<Store<HostState>, wasmtime::Error> {
    let mut builder = wasmtime_wasi::WasiCtxBuilder::new();

    use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
    match cfg.wasi_stdin {
        // 有显式输入时优先喂给 guest（不论 Inherit/Capture）
        WasiStdio::Inherit if input.is_empty() => {
            builder.inherit_stdin();
        }
        WasiStdio::Inherit | WasiStdio::Capture => {
            builder.stdin(MemoryInputPipe::new(input.to_vec()));
        }
        WasiStdio::Null => {
            builder.stdin(std::io::empty());
        }
    }
    let mut captured_stdout = None;
    match cfg.wasi_stdout {
        WasiStdio::Inherit => {
            builder.inherit_stdout();
        }
        WasiStdio::Capture => {
            // 保留 pipe 句柄，run 之后可取回内容（否则随 ctx 丢弃、行为≈Null）
            let pipe = MemoryOutputPipe::new(1 << 20);
            builder.stdout(pipe.clone());
            captured_stdout = Some(pipe);
        }
        WasiStdio::Null => {
            builder.stdout(std::io::empty());
        }
    }
    let mut captured_stderr = None;
    match cfg.wasi_stderr {
        WasiStdio::Inherit => {
            builder.inherit_stderr();
        }
        WasiStdio::Capture => {
            let pipe = MemoryOutputPipe::new(1 << 20);
            builder.stderr(pipe.clone());
            captured_stderr = Some(pipe);
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

    // WASI sockets（L2）：默认 None 完全禁用；其余策略统一经 socket_addr_allowed 过滤远程地址
    match &cfg.wasi_sockets {
        WasiSockets::None => {
            builder
                .allow_tcp(false)
                .allow_udp(false)
                .allow_ip_name_lookup(false);
        }
        _ => {
            builder
                .allow_tcp(true)
                .allow_udp(true)
                .allow_ip_name_lookup(cfg.wasi_ip_name_lookup);
            let policy = cfg.wasi_sockets.clone();
            builder.socket_addr_check(move |addr, use_| {
                let ok = socket_addr_use_allowed(&policy, addr, use_);
                Box::pin(async move { ok })
            });
        }
    }

    // 其余可配置的 WASI 限制面
    if let Some(cwd) = &cfg.wasi_initial_cwd {
        builder.initial_cwd(cwd);
    }
    if let Some(max) = cfg.wasi_max_random_size {
        builder.max_random_size(max);
    }
    builder.allow_blocking_current_thread(cfg.wasi_allow_blocking_current_thread);

    // WASI clocks（L2）：Deterministic 用固定时钟，便于可复现运行
    match cfg.wasi_clocks {
        WasiClocks::Host => {}
        WasiClocks::Deterministic(seed) => {
            builder.wall_clock(DeterministicWallClock);
            builder.monotonic_clock(DeterministicMonotonicClock::new(seed));
        }
    }

    // WASI random（L2）：Seeded 只播种**非安全流**（wasi:random/insecure + insecure-seed），
    // 安全流 wasi:random/random 保持 wasmtime 默认的 OS 真随机（thread_rng），不被种子降级为可预测。
    match cfg.wasi_random {
        WasiRandom::Os => {}
        WasiRandom::Seeded(seed) => {
            builder.insecure_random_seed(seed);
            builder.insecure_random(wasmtime_wasi::Deterministic::new(seed.to_le_bytes().to_vec()));
        }
    }

    // L2 内存上限：把 max_memory_bytes 接到 StoreLimits（每块线性内存可增长到的字节上限）。
    // 比 OS 的 RLIMIT_AS 可靠——wasmtime 的大块虚拟地址预留不会被误杀。
    let limits = match cfg.max_memory_bytes {
        Some(bytes) => wasmtime::StoreLimitsBuilder::new()
            .memory_size(usize::try_from(bytes).unwrap_or(usize::MAX))
            .build(),
        None => StoreLimits::default(),
    };

    let wasi_ctx = builder.build();
    let mut store = Store::new(
        &ENGINE,
        HostState {
            wasi: wasi_ctx,
            table: component::ResourceTable::new(),
            shutdown_requested_only_for_threadfallback: shutdown_flag,
            captured_stdout,
            captured_stderr,
            limits,
        },
    );
    // 挂上 ResourceLimiter：guest 线性内存增长超限时 memory.grow 返回 -1
    store.limiter(|state| &mut state.limits);
    Ok(store)
}

/// worker（进程）用加载：同步 linker + `sync::Command` 实例化。
/// worker 引擎是纯同步配置，无需 tokio，也不会踩 epoch 问题。
pub fn load_component_for_worker(
    component: component::Component,
    shutdown_flag: Arc<AtomicBool>,
    cfg: &SandboxConfig,
    input: &[u8],
) -> Result<WorkerSandbox, wasmtime::Error> {
    let linker = build_linker(false)?;
    let mut store = build_store(shutdown_flag, cfg, input)?;
    let command =
        wasmtime_wasi::p2::bindings::sync::Command::instantiate(&mut store, &component, &linker)?;
    Ok(WorkerSandbox { store, command })
}

/// thread（宿主/线程回落）用加载：异步 linker + `Command::instantiate_async`。
/// 必须在调用方自己的 tokio runtime 内 await（禁止再起嵌套 runtime）。
pub async fn load_component_for_thread(
    component: component::Component,
    shutdown_flag: Arc<AtomicBool>,
    cfg: &SandboxConfig,
    input: &[u8],
) -> Result<ThreadSandbox, wasmtime::Error> {
    let linker = build_linker(true)?;
    let mut store = build_store(shutdown_flag, cfg, input)?;
    // epoch 打断：必须在实例化之前设置，否则默认 deadline=0（恒已过期）会 Trap::Interrupt
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(SandboxHandle::epoch_callback);
    // add_to_linker_async 注册的导入会把 store 标记为 async-required，只能走 instantiate_async
    let command =
        wasmtime_wasi::p2::bindings::Command::instantiate_async(&mut store, &component, &linker)
            .await?;
    Ok(ThreadSandbox { store, command })
}
use std::process::{Child, Command, Stdio};

use crate::Runner::{Process, Thread};
use wasmtime::{StoreContextMut, UpdateDeadline};

/// 有界等待/关闭时的轮询间隔、超时后轻推 epoch 的宽限：由用户配置 `run.json` 提供。
/// Poll interval & shutdown grace for bounded waits: from user config `run.json`.
fn poll_interval() -> Duration {
    Duration::from_millis(crate::config::run().poll_ms)
}
fn shutdown_grace() -> Duration {
    Duration::from_millis(crate::config::run().shutdown_grace_ms)
}

impl SandboxHandle {
    /// 等待运行结束并返回结果（**无界**，可能永久阻塞）。
    /// 需要上界请用 [`SandboxHandle::wait_timeout`]；需要 stdout 请用 [`SandboxHandle::wait_output`]。
    pub fn wait(self) -> anyhow::Result<()> {
        self.wait_inner(None).map(|_| ())
    }

    /// 等待运行结束并返回捕获的 worker stdout（无界）。
    /// `Thread` 路径恒为空（线程内 guest 直接写宿主 stdout，无独立管道）。
    pub fn wait_output(self) -> anyhow::Result<Vec<u8>> {
        self.wait_inner(None)
    }

    /// 有界等待：最多等 `timeout`，超时则请求关闭并尽力打断后放弃。
    ///
    /// - `Process`：超时即 `kill()` 并回收，是真正的强杀。
    /// - `Thread`：超时后置关闭标志并**循环轻推 epoch**（不依赖单次 epoch 命中），
    ///   若因此结束则返回真实结果；若仍卡在不可打断的宿主调用，则放弃该线程
    ///   （Rust 线程无法安全强杀，会造成有界泄漏），返回超时错误。
    pub fn wait_timeout(self, timeout: Duration) -> anyhow::Result<()> {
        self.wait_inner(Some(timeout)).map(|_| ())
    }

    /// 同 [`SandboxHandle::wait_timeout`]，但返回捕获的 worker stdout。
    pub fn wait_output_timeout(self, timeout: Duration) -> anyhow::Result<Vec<u8>> {
        self.wait_inner(Some(timeout))
    }

    /// 等待实现（`timeout = None` 无界）。返回捕获的 worker stdout（线程路径为空）。
    fn wait_inner(mut self, timeout: Option<Duration>) -> anyhow::Result<Vec<u8>> {
        let start = Instant::now();
        let runner = self
            .runner
            .take()
            .ok_or_else(|| anyhow::anyhow!("SandboxHandle 已被消费"))?;
        match runner {
            Runner::Thread(joinhandle, (shutdown_requested, finished)) => {
                if let Some(timeout) = timeout {
                    while !finished.load(Ordering::SeqCst) && start.elapsed() < timeout {
                        std::thread::sleep(poll_interval());
                    }
                    if !finished.load(Ordering::SeqCst) {
                        shutdown_requested.store(true, Ordering::SeqCst);
                        Self::nudge_until_finished(&finished, shutdown_grace());
                        if !finished.load(Ordering::SeqCst) {
                            return Err(anyhow::anyhow!(
                                "thread run exceeded {timeout:?}; abandoned (not force-killable)"
                            ));
                        }
                    }
                }
                Self::join_thread(joinhandle)?;
                Ok(Vec::new())
            }
            Runner::Process(mut child, drain, log) => {
                if let Some(timeout) = timeout {
                    while child.try_wait()?.is_none() && start.elapsed() < timeout {
                        std::thread::sleep(poll_interval());
                    }
                    if child.try_wait()?.is_none() {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = drain.and_then(|h| h.join().ok());
                        // 与 shutdown()/finish_process 一致：日志线程也回收（EOF 后应已结束）。
                        if let Some(log) = log {
                            let _ = log.join();
                        }
                        return Err(anyhow::anyhow!("worker process exceeded {timeout:?}; killed"));
                    }
                }
                Self::finish_process(child, drain, log)
            }
        }
    }

    /// 请求关闭并等待结束（有界）。
    ///
    /// - `Process`：`kill()` + `wait()` 强杀，并回收 stdout 排空线程。
    /// - `Thread`：置关闭标志后**循环轻推 epoch** 直到线程结束或超过宽限期；
    ///   仍不结束则返回错误（线程已放弃，无法安全强杀）。
    pub fn shutdown(mut self) -> anyhow::Result<()> {
        let runner = self
            .runner
            .take()
            .ok_or_else(|| anyhow::anyhow!("SandboxHandle 已被消费"))?;
        match runner {
            Thread(joinhandle, (shutdown_requested, finished)) => {
                shutdown_requested.store(true, Ordering::SeqCst); //写入关闭标记
                // 循环轻推：单次 increment_epoch 不保证命中，循环检查 finished 直到结束或宽限期
                Self::nudge_until_finished(&finished, shutdown_grace());
                if finished.load(Ordering::SeqCst) {
                    // 关闭成功；线程内 guest 因关闭而返回的错误属预期，只上报 panic
                    let _ = joinhandle
                        .join()
                        .map_err(|_| anyhow::anyhow!("worker thread panicked"))?;
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "shutdown grace period elapsed; thread abandoned (not force-killable)"
                    ))
                }
            }
            Process(mut child, drain, log) => {
                let _ = child.kill(); // 进程可能已退出，kill 报错可忽略
                let wait = child.wait(); // 回收（拒绝僵尸）
                let _ = drain.and_then(|h| h.join().ok());
                if let Some(log) = log {
                    let _ = log.join(); // stderr 已 EOF，日志线程应已结束
                }
                wait?;
                Ok(())
            }
        }
    }

    /// 回收子进程：等待退出 → join stdout 排空线程取回捕获 → 按退出码解码。返回捕获的 stdout。
    fn finish_process(
        mut child: Child,
        drain: Option<std::thread::JoinHandle<Vec<u8>>>,
        log: Option<std::thread::JoinHandle<()>>,
    ) -> anyhow::Result<Vec<u8>> {
        let status = child.wait()?;
        let output = drain.and_then(|h| h.join().ok()).unwrap_or_default();
        if let Some(log) = log {
            let _ = log.join(); // stderr 已 EOF，日志线程应已结束
        }
        Self::decode_process_status(status)?;
        Ok(output)
    }

    /// join 线程并把 panic / 内部错误转成 `anyhow`。
    fn join_thread(joinhandle: std::thread::JoinHandle<Result<()>>) -> anyhow::Result<()> {
        joinhandle
            .join()
            .map_err(|_| anyhow::anyhow!("worker thread panicked"))?
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// 循环轻推 epoch，直到 `finished` 置位或超过 `grace`。多次轻推避免单次 epoch 的时序问题。
    fn nudge_until_finished(finished: &AtomicBool, grace: Duration) {
        let start = Instant::now();
        while !finished.load(Ordering::SeqCst) {
            ENGINE.increment_epoch();
            if start.elapsed() >= grace {
                break;
            }
            std::thread::sleep(poll_interval());
        }
    }

    /// 按 [`WorkerExit`] 解码子进程退出状态（非 0 视为失败）。
    fn decode_process_status(status: std::process::ExitStatus) -> anyhow::Result<()> {
        if status.success() {
            return Ok(());
        }
        match status.code() {
            Some(code) => match WorkerExit::decode(code) {
                Ok(worker_exit) => anyhow::bail!("worker failed (exit {code}: {:?})", worker_exit),
                Err(unknown) => anyhow::bail!("worker failed with unknown exit code {unknown}"),
            },
            None => anyhow::bail!("worker terminated by signal"),
        }
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
        Self::run_with_input(path, Vec::new(), config)
    }

    /// 同 [`SandboxHandle::run`]，并额外把 `input` 作为 guest WASI stdin 输入喂入。
    pub fn run_with_input(
        path: PathBuf,
        input: Vec<u8>,
        config: &SandboxConfig,
    ) -> anyhow::Result<SandboxHandle> {
        Self::run_source(&WasmSource::Path(path), &input, config)
    }

    /// 以独立 worker 进程运行内存中的 wasm 字节（不落盘、走字节帧传输，
    /// 默认封闭文件系统下也无需授权路径）。其余行为与 [`SandboxHandle::run`] 一致。
    pub fn run_data(data: Vec<u8>, config: &SandboxConfig) -> anyhow::Result<SandboxHandle> {
        Self::run_data_with_input(data, Vec::new(), config)
    }

    /// 同 [`SandboxHandle::run_data`]，并额外把 `input` 作为 guest WASI stdin 输入喂入。
    pub fn run_data_with_input(
        data: Vec<u8>,
        input: Vec<u8>,
        config: &SandboxConfig,
    ) -> anyhow::Result<SandboxHandle> {
        Self::run_source(&WasmSource::Bytes(data), &input, config)
    }

    fn run_source(
        source: &WasmSource,
        input: &[u8],
        config: &SandboxConfig,
    ) -> anyhow::Result<SandboxHandle> {
        require_init()?;
        // 配置冲突提前拒绝：worker 侧 setup_sandbox_with 用 assert! 表达开发者不变量，
        // 直接把冲突配置送进去会变成 panic / 未知退出码，应在入口返回可诊断错误。
        let violations = config.validate();
        if !violations.is_empty() {
            anyhow::bail!("沙箱配置冲突：{}", violations.join("；"));
        }
        // 在 spawn/线程回退与首次 ENGINE 初始化之前设置宿主解析的缓存目录。
        // Set the host-resolved cache dir before spawning/thread fallback and ENGINE init.
        set_compile_cache_dir(config.compile_cache_dir.clone());
        // set self exe path
        #[cfg(target_os = "linux")]
        let exe = std::path::PathBuf::from("/proc/self/exe"); //更安全

        #[cfg(not(target_os = "linux"))]
        let exe = std::env::current_exe()?;

        // 调试/集成测试钩子：`test_hooks::worker_exe(..)` 覆盖 worker 可执行文件（默认自身）。
        // 仅 debug 构建生效（release 编译掉，生产始终用自身）。集成测试进程并非 worker 入口，
        // 借此指向真正的 worker 二进制来端到端验证进程链路。
        #[cfg(debug_assertions)]
        let exe = test_hooks::worker_exe_override().unwrap_or(exe);

        let mut command = Command::new(exe);
        // worker stderr 继承宿主，作为 worker 进程日志；guest stderr 已由配置隔离
        // （CLI 为 Null），因此该日志只含宿主/引擎诊断。
        // The worker's stderr is inherited as the worker log; guest stderr is isolated by
        // config (Null in the CLI), so this log only carries host/engine diagnostics.
        //必须先清环境再设置标志：env_clear() 会抹掉其后设置的全部变量，反之则保留
        command
            .env_clear() // 清掉其他所有 env
            .env("__WASM_WORKER", "1");
        // 保留父进程的临时目录：worker 的 env_clear 会让 std::env::temp_dir() 回退到
        // 共享 /tmp（Unix）或 C:\Windows（Windows），使 temp_allow_write 指向错误位置。
        // Preserve the host temp dir: after env_clear, temp_dir() would fall back to shared
        // /tmp (Unix) or C:\Windows (Windows), mis-targeting temp_allow_write.
        // 优先应用自己的 temp（Linux 每用户 runtime / portable <exe_dir>/tmp）；仅在
        // temp_allow_write 请求时创建它（避免无谓副作用），否则要求已存在；最后回退 OS temp。
        // 这样 temp_allow_write 的 L1 规则不会因目录不存在或落在共享 /tmp 而失效。
        // Prefer the app temp; create it only when temp_allow_write is requested; otherwise
        // require it to exist; finally fall back to the OS temp. This keeps the L1 rule from
        // failing on a missing dir or silently targeting shared /tmp.
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        let host_temp = match crate::dir::tmp_dir() {
            Ok(p) if p.is_dir() => p,
            Ok(p) if config.temp_allow_write && crate::dir::ensure_dir(&p).is_ok() => p,
            _ => std::env::temp_dir(),
        };
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let host_temp = std::env::temp_dir();
        #[cfg(unix)]
        command.env("TMPDIR", host_temp.as_os_str());
        #[cfg(windows)]
        {
            command.env("TEMP", host_temp.as_os_str());
            command.env("TMP", host_temp.as_os_str());
        }
        //.env("PATH", "/usr/bin:/bin") // 补回最小 PATH
        // Windows 上 DLL 搜索依赖 SystemRoot：清空后至少恢复最小 PATH，否则 worker 可能起不来
        #[cfg(target_os = "windows")]
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            let sys = system_root.to_string_lossy();
            command.env("PATH", format!("{sys}\\System32;{sys}"));
        }
        // 调试/集成测试钩子：`test_hooks::force_thread_fallback(true)` 强制走线程回退。
        // 仅 debug 构建生效（release 编译掉，避免生产被静默降级）。
        let spawn_command_result = {
            #[cfg(debug_assertions)]
            {
                if test_hooks::thread_fallback_forced() {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "test hook: force_thread_fallback(true)",
                    ))
                } else {
                    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
                }
            }
            #[cfg(not(debug_assertions))]
            {
                command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
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
                    // 帧 2 = guest WASI stdin 输入（空则按模式）
                    let input_bytes = postcard::to_stdvec(input)?;
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
                // 结果通道：排空 worker stdout，避免 guest 大量写满管道阻塞；捕获供 wait_output 取回。
                let drain = child.stdout.take().map(spawn_stdout_drain);
                // worker stderr：异步转义并写入日志文件（有上限），不直接接到宿主 stderr。
                let log = child.stderr.take().map(worker_log::spawn_worker_log_drain);
                anyhow::Ok(SandboxHandle {
                    runner: Some(Runner::Process(child, drain, log)),
                })
            }
            Err(_) => {
                //创建进程失败，回落到创建线程
                eprintln!(
                    "[whitebox] 警告：进程创建失败，回退到线程模式——无进程隔离；\
                     为避免把不可逆的 L1 限制（chroot/landlock/rlimit/Job）施加到宿主进程，\
                     线程回退不应用 L1，仅保留 L2（WASI）限制"
                );

                let mut config = config.clone();
                // 线程回退没有进程隔离：不把 guest 输出直接接到宿主 stdio。
                // - stdout: Inherit → Capture（有界，仍由上层输出）
                // - stderr: 一律 Null（防止污染宿主日志/终端）
                // Thread fallback has no process isolation: never wire guest output directly
                // to host stdio. stdout Inherit→Capture (bounded); stderr always Null.
                if config.wasi_stdout == WasiStdio::Inherit {
                    config.wasi_stdout = WasiStdio::Capture;
                }
                config.wasi_stderr = WasiStdio::Null;
                let source = source.clone();
                let input = input.to_vec();
                let shutdown_flag_for_thread = Arc::new(AtomicBool::new(false));
                let shutdown_flag_clone = shutdown_flag_for_thread.clone();
                let finished = Arc::new(AtomicBool::new(false));
                let finished_for_thread = finished.clone();
                let thread = std::thread::spawn(move || -> Result<()> {
                    // 无论成功/失败/panic 都标记完成，供有界等待（wait_timeout/shutdown）解除阻塞。
                    // 用 Drop guard 而非末尾 store：闭包内部 panic 时也能置位。
                    // Mark finished on every exit path (success/err/panic): use a Drop guard,
                    // not a trailing store, so an inner panic still unblocks bounded waits.
                    struct MarkDone(Arc<AtomicBool>);
                    impl Drop for MarkDone {
                        fn drop(&mut self) {
                            self.0.store(true, Ordering::SeqCst);
                        }
                    }
                    let _mark = MarkDone(finished_for_thread.clone());
                    // 线程回退不应用 L1：chroot/landlock/rlimit/Job 等是进程级且不可逆，
                    // 施加到宿主会封死/误杀宿主；L1 只在有进程隔离的 worker 内生效。
                    // 加载与运行在同一个 current-thread runtime 内闭环，避免嵌套 runtime panic
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let result: Result<()> = rt.block_on(async move {
                        let component = match source {
                            WasmSource::Path(path) => load_from_wasm_file(&path)?,
                            WasmSource::Bytes(bytes) => load_wasm_bytes(bytes)?,
                        };
                        let sandbox = load_component_for_thread(
                            component,
                            shutdown_flag_clone,
                            &config,
                            &input,
                        )
                        .await?;
                        let _out = Self::run_module_for_thread(sandbox).await?;
                        // 线程路径不输出 guest stdout（策略待定，避免终端注入）。
                        // Do not emit guest stdout from the thread path (policy TBD;
                        // avoid terminal injection).
                        Ok(())
                    });
                    result?; //传播错误
                    Ok(())
                });
                anyhow::Ok(SandboxHandle {
                    runner: Some(Runner::Thread(thread, (shutdown_flag_for_thread, finished))),
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
            .load(Ordering::SeqCst)
        //
        {
            Err(wasmtime::Error::msg("Shutdown requested"))
        } else {
            Ok(UpdateDeadline::Continue(1))
        }
    }
    /// 取回 `WasiStdio::Capture` 捕获的 stderr（转发到宿主 stderr）与 stdout（返回，供上层输出/结果通道）。
    fn drain_captured(store: &Store<HostState>) -> Vec<u8> {
        let data = store.data();
        if let Some(pipe) = data.captured_stderr.as_ref() {
            let bytes = pipe.contents();
            if !bytes.is_empty() {
                use std::io::Write as _;
                let _ = std::io::stderr().write_all(&bytes);
            }
        }
        data.captured_stdout
            .as_ref()
            .map(|pipe| pipe.contents().to_vec())
            .unwrap_or_default()
    }

    /// 运行 worker 组件，返回 `WasiStdio::Capture` 捕获的 stdout（其余情况为空）。
    pub fn run_module_for_worker(mut sandbox: WorkerSandbox) -> Result<Vec<u8>, wasmtime::error::Error> {
        let run_result = sandbox.command.wasi_cli_run().call_run(&mut sandbox.store);
        let captured = Self::drain_captured(&sandbox.store);
        run_result?.map_err(|()| wasmtime::Error::msg("guest wasi:cli/run returned an error"))?;
        Ok(captured)
    }
    /// 运行 thread（回退）组件，返回 `WasiStdio::Capture` 捕获的 stdout。
    pub async fn run_module_for_thread(
        mut sandbox: ThreadSandbox,
    ) -> Result<Vec<u8>, wasmtime::error::Error> {
        let run_result = sandbox
            .command
            .wasi_cli_run()
            .call_run(&mut sandbox.store)
            .await;
        let captured = Self::drain_captured(&sandbox.store);
        run_result?.map_err(|()| wasmtime::Error::msg("guest wasi:cli/run returned an error"))?;
        Ok(captured)
    }
}

#[cfg(test)]
mod tests {
    use super::{WasiSockets, WorkerExit};

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

    #[test]
    fn stdout_drain_captures_all_bytes() {
        // 守护结果通道：排空线程必须完整读取直到 EOF（大 payload 也不能截断）
        let payload = vec![0xA5u8; 200_000];
        let handle = super::spawn_stdout_drain(std::io::Cursor::new(payload.clone()));
        assert_eq!(handle.join().expect("drain thread must not panic"), payload);
    }

    #[test]
    fn socket_policy_allow_deny_loopback_none() {
        // sockets 端口策略判定（socket_addr_check 的宿主侧逻辑）
        let allow = WasiSockets::Allow(vec![(8000, 9000)]);
        assert!(super::socket_addr_allowed(
            &allow,
            "1.2.3.4:8500".parse().unwrap()
        ));
        assert!(!super::socket_addr_allowed(
            &allow,
            "1.2.3.4:80".parse().unwrap()
        ));

        let deny = WasiSockets::Deny(vec![(25, 25)]);
        assert!(!super::socket_addr_allowed(
            &deny,
            "1.2.3.4:25".parse().unwrap()
        ));
        assert!(super::socket_addr_allowed(
            &deny,
            "1.2.3.4:443".parse().unwrap()
        ));

        assert!(super::socket_addr_allowed(
            &WasiSockets::LoopbackOnly,
            "127.0.0.1:9".parse().unwrap()
        ));
        assert!(!super::socket_addr_allowed(
            &WasiSockets::LoopbackOnly,
            "8.8.8.8:9".parse().unwrap()
        ));

        assert!(!super::socket_addr_allowed(
            &WasiSockets::None,
            "127.0.0.1:9".parse().unwrap()
        ));
    }

    #[test]
    fn stdout_drain_truncates_at_cap() {
        // 守护捕获上限：超限仍要排空到 EOF，但只保留前 MAX_WORKER_STDOUT_BYTES 字节
        let payload = vec![7u8; super::MAX_WORKER_STDOUT_BYTES + 1024];
        let handle = super::spawn_stdout_drain(std::io::Cursor::new(payload));
        let out = handle.join().expect("drain thread must not panic");
        assert_eq!(out.len(), super::MAX_WORKER_STDOUT_BYTES);
        assert!(out.iter().all(|b| *b == 7));
    }

    #[test]
    fn socket_use_policy_only_allows_outbound() {
        use wasmtime_wasi::sockets::SocketAddrUse;

        let allow = WasiSockets::Allow(vec![(443, 443)]);
        let remote: std::net::SocketAddr = "1.2.3.4:443".parse().unwrap();
        // 出站允许
        assert!(super::socket_addr_use_allowed(
            &allow,
            remote,
            SocketAddrUse::TcpConnect
        ));
        // 显式固定端口绑定被拒，隐式临时绑定（端口 0）放行
        assert!(!super::socket_addr_use_allowed(
            &allow,
            remote,
            SocketAddrUse::TcpBind
        ));
        assert!(super::socket_addr_use_allowed(
            &allow,
            "0.0.0.0:0".parse().unwrap(),
            SocketAddrUse::TcpBind
        ));
        // 监听 / 接受入站一律拒绝
        assert!(!super::socket_addr_use_allowed(
            &allow,
            remote,
            SocketAddrUse::TcpListen
        ));
        assert!(!super::socket_addr_use_allowed(
            &allow,
            remote,
            SocketAddrUse::TcpAccept
        ));
    }
}
