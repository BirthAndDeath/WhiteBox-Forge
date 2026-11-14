# WhiteBox-Forge 项目记忆 / Project Memory

> 约定与协作规则见 AGENTS.md；待办见 TODO.md。这里记录**项目专属知识**：架构、决策理由、踩坑。

## 架构速览 / Architecture

- **两层沙箱，一张配置**：`SandboxConfig` 同时管 L1（进程/OS：`fs_rules`/网络/进程/资源/加固，走 `setup_sandbox()`）与 L2（WASI：`wasi_stdin/out/err`、`wasi_env/args`、`wasi_preopens`、`wasi_sockets/clocks/random`，走 `build_store` 构建 `WasiCtx`）。
- **组件 / WASI p2（已整体切换，无 p1 路径）**：guest 是 WASI p2 组件（`wasi:cli/command` 世界，导出 `wasi:cli/run`）；宿主用 `wasmtime::component::{Component, Linker}` + `wasmtime_wasi::p2::add_to_linker_{sync,async}` + `Command::{instantiate,instantiate_async}` 运行。`wasmtime-wasi` 只开 `p2` 特性（去掉 p1/wiggle）。
- **双执行路径**：
  - `_for_worker`：`current_exe()` 重启的子进程，**纯同步**引擎，`p2::add_to_linker_sync` + `sync::Command::instantiate` + `run_module_for_worker`；
  - `_for_thread`：宿主线程回退，**异步**（`wasm_component_model_async` + `epoch_interruption`），`add_to_linker_async` + `Command::instantiate_async`，必须在调用方自己的 tokio runtime 内 await（禁止嵌套 runtime）。
- **共享构件**：`build_linker(async_wasi)`（p2 WASI；无 host_func——组件 host 导入需 WIT 接口，未接）、`build_store(flag, &cfg, input)`（显式传本次配置建 `WasiCtx` + `ResourceTable` + Store；记录 Capture 管道句柄）、`load_wasm_bytes/load_from_wasm_file`（→ `component::Component`）。
- **HostState（`WasiView`）**：`wasi: WasiCtx` + `table: ResourceTable` + `shutdown_requested_only_for_threadfallback: Arc<AtomicBool>` + `captured_stdout/stderr`。
- **句柄收敛**：对外只有 `SandboxHandle { runner: Option<Runner> }`（`Some` = 未消费）：
  - `wait()` / `wait_output()`（无界）、`wait_timeout` / `wait_output_timeout`（有界）；
  - `shutdown()`（有界：Thread 置标志 + 循环轻推 epoch；Process kill+wait）；
  - `Drop`：未 `wait*`/`shutdown` 就丢弃时尽力终止（Process kill+wait；Thread 置标志 + 单次 epoch）；`runner` 被取走后（正常结束）Drop 不再动作；
  - Thread 侧用 `finished: Arc<AtomicBool>` 供有界等待轮询；Process 侧用 `spawn_stdout_drain` 排空并捕获 stdout（**上限 8 MiB**，超限继续排空但丢弃）。
- **协议（三帧，全 postcard 序列化，无需版本号——父子同二进制）**：
  1. `WasmSource::{Path(PathBuf), Bytes(Vec<u8>)}`——路径 or 字节；
  2. 输入字节 `Vec<u8>`（已接线：`build_store` 用 `MemoryInputPipe` 喂 WASI stdin；空则按模式）；
  3. `SandboxConfig`（解码失败 → `WorkerExit::Decode` fail-closed）。

## 关键决策与理由 / Decisions & Rationale

- **不引入平行的 `WasmCapability` 模型**：L1 粗上界 + L2 细粒度本应分离，但最终统一进 `SandboxConfig`（`wasi_*` 段），单源、无漂移；manifest 式快捷写法未来可做成 `SandboxConfig::from_manifest` 糖，不建第二套枚举。
- **Deny-by-Default**：`SandboxConfig::default()` 全关闭（sealed fs/禁网/禁执行/禁提权/防调试）；`open()` 全开放用于测试与"不套沙箱"；授权靠 builder（`allow_fs_read`、`allow_network_ports`、`wasi_*`）。
- **L1 失败不中止 / L2 失败必须终止**：进程沙箱尽力而为（能力不支持→`ApplyStatus::Error` 报告后继续）；WASI 配置失败（preopen 打不开等）→ `build_store` 返回错误 → worker 按 `Instantiate` 码退出 / 线程回退返回 Err。
- **线程回退无法强制 kill**：Rust 线程无安全终止（`pthread_cancel`/`TerminateThread` 会撕裂 unwinding/锁/wasmtime 状态 = UB）。只靠 epoch 协作打断（wasm 代码卡死）+ 超时→abandon（宿主调用/`poll_oneoff` 长睡卡死，内存有界泄漏）。「硬杀」只在进程模式（`child.kill()`）。
  - 有界化：`wait_timeout`/`shutdown` 用 `finished` 标志轮询 + 到点**循环轻推 `increment_epoch`**（不依赖单次命中），超 `DEFAULT_SHUTDOWN_GRACE`（2s）仍不结束则 abandon 并返回错误。**超时是否返回不再依赖 epoch，但“真正终止线程内 guest”仍只能靠 epoch**（纯计算）；阻塞宿主调用两者都无解。
  - **不施加 L1**：回退线程不调用 `apply_sandbox_with`——`chroot`/Landlock/rlimit/Job 是进程级且不可逆，施加到宿主会封死/误杀宿主；回退仅保留 L2（WASI），并打印明确警告。
- **epoch vs fuel**：fuel（`Config::consume_fuel` + `store.set_fuel`，当前未启用）是确定性指令预算、自包含（无需外部轻推），但仍只在 wasm 指令边界扣——和 epoch 同类，打断不了阻塞宿主调用。
- **三层有界停止（提议，见 TODO）**：`Store::call_hook`（**在 `#[cfg(feature="call-hook")]` 下**，我们未开该 feature；边界回调可读 HostState 并 trap）+ fuel（确定性预算）+ 协作/可取消宿主 API。关键边界：call-hook 只在"宿主调用/进出 wasm"边界生效，**打不断纯 wasm 长循环与已在进行的单个阻塞宿主调用**；后两者仍靠 epoch/fuel 与 async 取消/进程 kill。
- **浏览器类比**：进程隔离靠 OS kill（`child.kill` ≈ 杀渲染进程）；进程内强终止 V8 靠 `TerminateExecution`（VM 级、可复位），wasmtime 等价物是 epoch 检查点 + call-hook 边界检查（后者未实现）。
- **`increment_epoch()` 是引擎级广播**：会触发同进程所有 store 的检查；各 store 回调读自己的 `Arc<AtomicBool>` flag 区分目标，非目标 `Continue(1)`。
- **可审查性 = 确定性优先（提议，见 TODO）**：精确结论（权限/依赖/证据）只来自确定性分析——wasm 组件 imports/WIT（`wasmcap` 思路）、审计日志、哈希链/签名；**AI/LLM 仅作"建议/解释"、绝不作裁决或证据**（不可复现、不可审计）。若用 AI，优先**本地专用小模型**（离线、可分级），不接通用云 LLM（隐私/不可复现）。
- **CI 供应链门禁**：`cargo deny`（`advisories/bans/sources`）**阻断**，`cargo audit` 仅**信息报告**（`continue-on-error`）。`deny.toml` 里 `unmaintained="none"`（无安全升级的传递依赖交给 audit 报告）、`licenses` 暂不进（Slint 多许可证/GPL 待评审）。为消除 zstd 的三个 advisory，`wasmtime` 用 `default-features=false` + "默认减 `cache`"（我们尚未启用编译缓存）；同批把 wasmtime→49.0.5、tokio→1.54.1、pulldown-cmark→0.13.6、clipboard-win→5.4.3（yanked）。

## 踩坑记录 / Corrections（血泪经验）

1. **PowerShell 写盘灾难（最严重）**：`Set-Content -Encoding utf8` 把 UTF-8 无 BOM 文件按系统 ANSI(GBK) 误读再以 UTF-8 写出 → 全文件中文注释双重编码乱码；反向解码遇 `?`/U+FFFD **不可逆**。恢复 = `git checkout-index -f -- <file>` 从暂存区取回 + 重做。铁律见 AGENTS.md「文件编辑与编码」，**勤提交/暂存**。
2. **epoch `Trap::Interrupt` 根因**：`instantiate_async` 阶段 store 默认 deadline=0（"恒已过期"）+ 回调未注册 → 首个检查点 Interrupt。修复 = **deadline+callback 必须在实例化之前设置**（`load_component_for_thread`）。曾误判为"epoch 机制本身有问题"，实为时序问题。
3. **async 导入即 async-required**：`add_to_linker_async` 注册的导入会把 store 标记为 async-required，同步 `instantiate` 报「use *_async」，必须 `instantiate_async`。
4. **同步 WASI × async 调用 = panic**：`add_to_linker_sync` 的 WASI 内部 `block_on`，在 async 上下文里嵌套 panic（wasmtime-wasi runtime.rs）；async 路径必须配 async linker。
5. **wasmtime 47 无 `interrupt_handle`/`Config::interruptable`**：曾建议改用它们，搜索确认不存在；线程关闭只能走 epoch。
6. **运行实例不进 `SandboxHandle`**：运行期 store 归属子进程/执行闭包，父进程句柄拿不到活实例（旧的 `WasmSandbox`/`call_func` 已随 p2 迁移删除）。组件路径运行入口是 `run_module_for_worker/for_thread`（p2 `Command`），进程内交互式调用需另设计（组件导出需按名 alias 取 Func）。
7. **集成测试递归风险**：测试进程的 `current_exe()` 是测试二进制，`run`/`run_data` spawn 它会递归跑测试；测试改用 `whitebox_core::test_hooks::{force_thread_fallback, worker_exe}`（**仅 `#[cfg(debug_assertions)]`** 的进程内开关，取代 `unsafe std::env::set_var`，消除并行测试对 env 的数据竞争）强制线程回退 / 指向真 worker 二进制。
8. **非 UTF-8 路径**：原 `to_string_lossy` + `read_string` 失真；改 `WasmSource` postcard 序列化后无损。
9. **cfg 平台代码本地编不到**：`sandbox/unix_common.rs`（linux/android）、`linux.rs`、`macos.rs` 在 Windows 上被 `#[cfg]` 排除，改后 Windows `cargo check` 报不出来——曾漏包 `unsafe`、多余 `mut` 等，靠 CI（ubuntu）才暴露。改动这些文件时多自查（`libc::` 函数调用必须 `unsafe`），由 CI 验证。

## WASI 配置细节 / WASI Options（wasmtime-wasi 49.0.2，仅启用 p2）

> 版本：`wasmtime`/`wasmtime-wasi` = **49.0.2**（2026-10-02，MSRV Rust 1.96；本地 rustc 1.98 OK）。
> 从 48.0.5 升级无需改代码（API 兼容），已 `cargo check --workspace --all-targets` 零告警 + `cargo test -p whitebox_core` 全绿。
> 选 49 的两个直接相关原因：(1) **fuel 语义修正**（`call_ref`/异常返回、动态 record lifting 的燃料不再漏计）；(2) **#14265 `unconstrained`**：wasip2 sync shim 的 `noop_waker`/`poll_noop` 退出 Tokio 每任务协作预算，修复 sync 路径 sockets 就绪误报（issue #14174：`check-write` 因预算耗尽对所有 socket 报 not-ready）——正是我们 worker 的 sync 路径。

- `WasiStdio::{Inherit, Capture, Null}` → builder：`inherit_*` / `MemoryInputPipe`+`MemoryOutputPipe`（Capture 已接线：句柄存 `HostState`，run 后 `drain_captured` 取回并落回进程 stdout）/ `std::io::empty()`。
- `wasi_preopens` → `preopened_dir(&host, &guest, FsPerms::{ReadOnly|ReadWrite})`，打开失败即报错（L2 fail-closed）。
- `WasiSockets::{None, Allow(ports), Deny(ports), LoopbackOnly}` → `allow_tcp/udp` + `socket_addr_check(SocketAddr, SocketAddrUse)`。默认 `None`（全禁）；与 L1 `deny_network`/`network_ports` 求交（`validate`）。`socket_addr_check` 按 `SocketAddrUse` 区分：出站/收包按端口/回环策略；隐式临时绑定（端口 0）放行；显式固定端口绑定、`Listen`/`Accept` 一律拒绝（只授出站，不在宿主开监听）。
- `wasi_ip_name_lookup: bool` → `allow_ip_name_lookup`（DNS）；与 `WasiSockets::None` / `deny_network` 互斥（`validate`）。默认 false。
- `WasiClocks::{Host, Deterministic(seed)}` → `builder.wall_clock/monotonic_clock`（本地 `DeterministicWallClock`/`DeterministicMonotonicClock`）。默认 `Host`。
- `WasiRandom::{Os, Seeded(seed)}` → 默认 `Os`：安全流 `wasi:random/random` = wasmtime 默认 `thread_rng()`（OS 播种 CSPRNG，真随机），非安全流 = `SmallRng`(OS 播种)。`Seeded` **只播种非安全流**（`insecure_random_seed` + `insecure_random(Deterministic)`，使 `wasi:random/insecure` 可复现），**不覆盖** `secure_random` → 安全流仍真随机（WASI 语义）。
- `wasi_max_random_size: Option<u64>` → `max_random_size`（None=引擎默认 64MiB）。
- `wasi_initial_cwd: Option<String>` → `initial_cwd`（None=引擎默认 "/"）。
- `wasi_allow_blocking_current_thread: bool` → `allow_blocking_current_thread`（默认 false，走 spawn_blocking）。
- **L2 内存上限**：`max_memory_bytes` → `StoreLimits`（`StoreLimitsBuilder::memory_size`）+ `Store::limiter`，限每块线性内存可增长字节；比 `RLIMIT_AS` 可靠（不误杀 wasmtime 的虚拟预留）。
- 权限面 `FsPerms` 在 `wasmtime_wasi` 根部；`StdinStream/StdoutStream` 实现：`std::io::Stdin/Stdout/Stderr/Empty`、`p2::pipe::Memory*`。
- 未暴露（守 Deny-by-Default）：`inherit_env`/`inherit_args`/`inherit_network`（宿主继承类，非限制项）。
- 组件 host 导入（原 core `host_func`）未接：组件模型下需 WIT 接口 + `bindgen`/`linker.root().func_wrap`，属独立后续工作。

## 已知缺口（详见 TODO.md）

- sockets 的 **guest 级 e2e** 未做（需 `wasi:sockets` 资源句柄与地址结构，手写 WAT 成本高）；clocks/random 已有 guest 级 e2e（`tests/wasi_clocks.rs`、`tests/wasi_random.rs`），sockets 有 config 级 + 策略单测。
- CPU：只有 epoch（中断）已接；fuel（确定性预算）与 cgroup `cpu.max`（速率）未接。
- unix 平台已统一区分权限不足：`unix_common::SyscallOutcome`（`EACCES`/`EPERM`→`Denied`，其余→`Error`）；`linux.rs`/`macos.rs`/`android.rs` 经 `unix_common::status` 映射。Landlock 路径规则按 `rule.read` 决定读位（write-only 不再隐式可读）。**本地（Windows）编不到 unix 文件，待 CI 验证**。
- Linux/macOS 真机沙箱行为断言仍缺（CI `test-matrix` 已在真实 linux/macos 上编译+跑测试，可抓 unix 编译错误，但未断言 Landlock/seatbelt 实际 `Applied`/`Denied`）。
- **不支持 p1 core module**：组件/p2-only，`wasi_snapshot_preview1` core module 在 `Component::new` 阶段即 `failed to parse WebAssembly module`；无运行期自动适配。wasmtime 提供两条桥（`p1` 特性的宿主适配层 / 构建期 preview1→preview2 adapter），但都需显式接。影响面 = 覆盖率（存量 p1），见 TODO。
