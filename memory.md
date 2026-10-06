# WhiteBox-Forge 项目记忆 / Project Memory

> 约定与协作规则见 AGENTS.md；待办见 TODO.md。这里记录**项目专属知识**：架构、决策理由、踩坑。

## 架构速览 / Architecture

- **两层沙箱，一张配置**：`SandboxConfig` 同时管 L1（进程/OS：`fs_rules`/网络/进程/资源/加固，走 `setup_sandbox()`）与 L2（WASI：`wasi_stdin/out/err`、`wasi_env/args`、`wasi_preopens`，走 `build_store` 构建 `WasiCtx`）。
- **双执行路径**：
  - `_for_worker`：`current_exe()` 重启的子进程，**纯同步**引擎（worker 分支的 `get_engine_config` 返回空配置），同步 linker + `instantiate` + `run_module_for_worker`；
  - `_for_thread`：宿主线程回退，**异步**（`wasm_component_model_async` + `epoch_interruption`），异步 linker + `instantiate_async` + `run_module_for_thread`，必须在调用方自己的 tokio runtime 内 await（禁止嵌套 runtime）。
- **共享构件**：`build_linker(async_wasi)`（host_func + WASI sync/async）、`build_store(flag, &cfg)`（**显式传本次配置**建 WasiCtx + Store，不读全局）、`load_wasm_bytes/load_from_wasm_file`（→ Module，两模式同用）。
- **句柄收敛**：对外只有 `SandboxHandle { runner }`，方法 `wait()`（Thread join / Process wait+退出码解码）与 `shutdown()`（Thread：flag+`increment_epoch`；Process：kill+wait）。
- **协议（三帧，全 postcard 序列化，无需版本号——父子同二进制）**：
  1. `WasmSource::{Path(PathBuf), Bytes(Vec<u8>)}`——路径 or 字节；
  2. 输入字节 `Vec<u8>`（当前未接线到 WASI stdin）；
  3. `SandboxConfig`（解码失败 → `WorkerExit::Decode` fail-closed）。

## 关键决策与理由 / Decisions & Rationale

- **不引入平行的 `WasmCapability` 模型**：L1 粗上界 + L2 细粒度本应分离，但最终统一进 `SandboxConfig`（`wasi_*` 段），单源、无漂移；manifest 式快捷写法未来可做成 `SandboxConfig::from_manifest` 糖，不建第二套枚举。
- **Deny-by-Default**：`SandboxConfig::default()` 全关闭（sealed fs/禁网/禁执行/禁提权/防调试）；`open()` 全开放用于测试与"不套沙箱"；授权靠 builder（`allow_fs_read`、`allow_network_ports`、`wasi_*`）。
- **L1 失败不中止 / L2 失败必须终止**：进程沙箱尽力而为（能力不支持→`ApplyStatus::Error` 报告后继续）；WASI 配置失败（preopen 打不开等）→ `build_store` 返回错误 → worker 按 `Instantiate` 码退出 / 线程回退返回 Err。
- **线程回退无法强制 kill**：Rust 线程无安全终止（`pthread_cancel`/`TerminateThread` 会撕裂 unwinding/锁/wasmtime 状态 = UB）。只靠 epoch 协作打断（wasm 代码卡死）+ 超时→abandon（宿主调用/`poll_oneoff` 长睡卡死，内存有界泄漏）。「硬杀」只在进程模式（`child.kill()`）。
- **浏览器类比**：进程隔离靠 OS kill（`child.kill` ≈ 杀渲染进程）；进程内强终止 V8 靠 `TerminateExecution`（VM 级、可复位），wasmtime 等价物是 epoch 检查点 + call-hook 边界检查（后者未实现）。
- **`increment_epoch()` 是引擎级广播**：会触发同进程所有 store 的检查；各 store 回调读自己的 `Arc<AtomicBool>` flag 区分目标，非目标 `Continue(1)`。

## 踩坑记录 / Corrections（血泪经验）

1. **PowerShell 写盘灾难（最严重）**：`Set-Content -Encoding utf8` 把 UTF-8 无 BOM 文件按系统 ANSI(GBK) 误读再以 UTF-8 写出 → 全文件中文注释双重编码乱码；反向解码遇 `?`/U+FFFD **不可逆**。恢复 = `git checkout-index -f -- <file>` 从暂存区取回 + 重做。铁律见 AGENTS.md「文件编辑与编码」，**勤提交/暂存**。
2. **epoch `Trap::Interrupt` 根因**：`instantiate_async` 阶段 store 默认 deadline=0（"恒已过期"）+ 回调未注册 → 首个检查点 Interrupt。修复 = **deadline+callback 必须在实例化之前设置**（`load_module_for_thread`）。曾误判为"epoch 机制本身有问题"，实为时序问题。
3. **async 导入即 async-required**：`add_to_linker_async` 注册的导入会把 store 标记为 async-required，同步 `instantiate` 报「use *_async」，必须 `instantiate_async`。
4. **同步 WASI × async 调用 = panic**：`add_to_linker_sync` 的 WASI 内部 `block_on`，在 async 上下文里嵌套 panic（wasmtime-wasi runtime.rs）；async 路径必须配 async linker。
5. **wasmtime 47 无 `interrupt_handle`/`Config::interruptable`**：曾建议改用它们，搜索确认不存在；线程关闭只能走 epoch。
6. **`WasmSandbox` 不能存进 `SandboxHandle`**：运行期 store 归属子进程/执行闭包，父进程句柄拿不到活实例；死字段 `wasm_sandbox` 已删。要做进程内交互就用 `load_module_for_thread` + `call_func` 自己持有。
7. **集成测试递归风险**：测试进程的 `current_exe()` 是测试二进制，`run`/`run_data` spawn 它会递归跑测试；`try.rs` 靠 `WHITEBOX_FORCE_THREAD_FALLBACK`（仅 `#[cfg(debug_assertions)]`）强制线程回退。
8. **非 UTF-8 路径**：原 `to_string_lossy` + `read_string` 失真；改 `WasmSource` postcard 序列化后无损。

## WASI 配置细节 / WASI Options（wasmtime-wasi 47，p1/p2 均为默认特性）

- `WasiStdio::{Inherit, Capture, Null}` → builder：`inherit_*` / `MemoryInputPipe/MemoryOutputPipe`（p2 pipe，Capture 检索尚未接线）/ `std::io::empty()`。
- `wasi_preopens` → `preopened_dir(&host, &guest, DirPerms::READ[|MUTATE], FilePerms::READ[|WRITE])`，打开失败即报错（L2 fail-closed）。
- 权限面 `DirPerms`/`FilePerms` 位集在 `wasmtime_wasi` 根部；`StdinStream/StdoutStream` 实现：`std::io::Stdin/Stdout/Stderr/Empty`、`p2::pipe::Memory*`。
- sockets：WASI p1 无；p2 有但本项目未接。

## 已知缺口（详见 TODO.md）

- `src/main.rs` 未调 `whitebox_core::init()` → 进程 worker 端到端未通（协作项）。
- stdout 结果通道未实现（`Runner::Process` 不读 stdout，wasm 大量写会 pipe 阻塞；`WasiStdio::Capture` 检索也未接线）。
- `max_memory_bytes` 未接 `ResourceLimiter`（线程模式无 OS 内存兜底）。
- `input` 帧未接到 WASI stdin。
