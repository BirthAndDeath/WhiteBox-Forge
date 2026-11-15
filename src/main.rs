use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, ValueEnum};
use whitebox_core::config;
use whitebox_core::sandbox::{SandboxConfig, WasiStdio};
use whitebox_core::{SandboxHandle, init};

/// 运行一个 WASI p2 组件（沙箱内）。默认 Deny-by-Default。
#[derive(Parser)]
#[command(name = "whitebox-forge", version, about = "Run a WASI p2 component inside a sandbox")]
struct Cli {
    /// 要运行的 WASI p2 组件（.wasm 文件）
    #[arg(value_name = "WASM")]
    wasm: PathBuf,

    /// guest 的 stdout 模式（inherit/capture/null）；stderr 恒丢弃
    #[arg(long, value_enum, default_value_t = StdioMode::Inherit)]
    stdio: StdioMode,

    /// 执行上界（毫秒）：进程超时强杀，线程回退超时放弃（放弃会返回错误）
    #[arg(long, value_name = "MS")]
    timeout_ms: Option<u64>,

    /// 完全关闭沙箱限制（open 配置）——仅用于可信 guest
    #[arg(long)]
    no_sandbox: bool,

    /// 追加一条只读预打开目录，格式 HOST:GUEST（可重复；与 --no-sandbox 互斥）
    #[arg(
        long = "preopen-ro",
        value_name = "HOST:GUEST",
        value_parser = PreopenSpec::parse,
        conflicts_with = "no_sandbox"
    )]
    preopen_ro: Vec<PreopenSpec>,
}

#[derive(Clone, Copy, ValueEnum)]
enum StdioMode {
    Inherit,
    Capture,
    Null,
}

impl From<StdioMode> for WasiStdio {
    fn from(mode: StdioMode) -> Self {
        match mode {
            StdioMode::Inherit => WasiStdio::Inherit,
            StdioMode::Capture => WasiStdio::Capture,
            StdioMode::Null => WasiStdio::Null,
        }
    }
}

/// 一条 `--preopen-ro HOST:GUEST` 规格 / One `--preopen-ro HOST:GUEST` spec.
#[derive(Clone, Debug)]
struct PreopenSpec {
    host: PathBuf,
    guest: String,
}

impl PreopenSpec {
    /// 解析并校验一条规格（交给 clap 作为 `value_parser`，错误在参数解析期报出）。
    ///
    /// 用 `rsplit_once(':')` 取最后一个 `:`，兼容 Windows 盘符（如 `C:\dir:/guest`）；
    /// `GUEST` 不能为空，`HOST` 必须是绝对路径。
    ///
    /// Parse and validate one spec (used as clap's `value_parser`, so errors surface at
    /// argument parsing). `rsplit_once(':')` keeps Windows drive letters intact; `GUEST`
    /// must be non-empty and `HOST` must be absolute.
    fn parse(spec: &str) -> Result<Self, String> {
        let (host, guest) = spec
            .rsplit_once(':')
            .ok_or_else(|| format!("需要 HOST:GUEST 形式，得到 {spec:?}"))?;
        if guest.is_empty() {
            return Err(format!("GUEST 路径为空：{spec:?}"));
        }
        let host = PathBuf::from(host);
        if !host.is_absolute() {
            return Err(format!("HOST 必须是绝对路径：{host:?}"));
        }
        Ok(Self {
            host,
            guest: guest.to_owned(),
        })
    }
}

fn build_config(cli: &Cli) -> anyhow::Result<(SandboxConfig, config::AppConfig)> {
    // 用户配置（JSON 默认层）：必须在任何 run / 首次 ENGINE 初始化前加载。
    // User config (JSON defaults layer): load before any run / first ENGINE init.
    let app = config::AppConfig::load()?;
    let mut cfg = if cli.no_sandbox {
        SandboxConfig::open()
    } else {
        SandboxConfig::default()
    };
    // 资源上限只在沙箱模式生效；--no-sandbox 明确要求不套限制。
    if !cli.no_sandbox {
        app.limits.apply_to(&mut cfg);
    }
    // 统一走 builder：stdio 与只读预打开都通过配置方法收敛。
    // Uniform builder style: stdio and read-only preopens go through config methods.
    let stdio: WasiStdio = cli.stdio.into();
    // guest stderr 不接宿主：恶意输出不得污染 worker 日志/宿主终端。
    // Guest stderr is never wired to the host: malicious output must not pollute the
    // worker log or the host terminal.
    cfg = cfg.wasi_stdout(stdio).wasi_stderr(WasiStdio::Null);
    for spec in &cli.preopen_ro {
        cfg = cfg.wasi_preopen_ro(spec.host.as_path(), spec.guest.as_str());
    }
    // 简易开关：宿主侧解析/创建缓存目录，并在 L1 白名单模式下放行它；目录可被
    // caching.json 覆盖（仍会 protect_dir 收紧）。
    if app.caching.enabled {
        cfg = cfg.enable_compile_cache_at(app.caching.directory.as_deref());
    }
    let violations = cfg.validate();
    if !violations.is_empty() {
        anyhow::bail!("沙箱配置冲突：{}", violations.join("；"));
    }
    Ok((cfg, app))
}

fn main() -> anyhow::Result<()> {
    // worker 模式：init() 内读取协议帧运行 guest 并 process::exit，不会返回；
    // 非 worker 模式：完成全局状态标定后返回，继续解析 CLI。
    init()?;

    let cli = Cli::parse();
    let (cfg, app) = build_config(&cli)?;

    // 先读字节再走 run_data：默认封闭文件系统下 worker 读不到模块路径，字节直传无需授权路径。
    let bytes = std::fs::read(&cli.wasm)
        .with_context(|| format!("读取 wasm 模块失败：{}", cli.wasm.display()))?;

    let handle = SandboxHandle::run_data(bytes, &cfg)?;
    // 执行上界：CLI 显式参数 > run.json 默认。
    // Bounded wait timeout: explicit CLI arg wins over run.json default.
    let timeout_ms = cli.timeout_ms.or(app.run.timeout_ms);
    let _output = match timeout_ms {
        Some(ms) => handle.wait_output_timeout(Duration::from_millis(ms))?,
        None => handle.wait_output()?,
    };
    // TODO: stdout 输出策略待定；当前不写宿主，避免终端注入。
    // TODO: stdout output policy is TBD; do not write it to the host for now (avoid
    // terminal injection).
    anyhow::Ok(())
}
