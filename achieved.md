# WhiteBox-Forge 已完成条目（从 TODO.md 归档）

> 由 `TODO.md` 迁出的已完成项，均附证据。继续把新完成项归档到本文件，`TODO.md` 只保留未完成项。
> 归档时基线：`cargo check --workspace --all-targets` 零告警；`cargo clippy -p whitebox_core --all-targets` 零告警；`cargo test -p whitebox_core` 全绿。
> 版本：`wasmtime`/`wasmtime-wasi` **49.0.2**（组件/p2-only；p1 已移除）。

## P0 链路/可用性

- [x] **stdout 结果通道缺失** → 已实现：
  - `run_source` 用 `spawn_stdout_drain` 排空 worker stdout（持续读至 EOF，不再写满管道阻塞）；
  - `SandboxHandle::wait_output` / `wait_output_timeout` 取回捕获字节；
  - `WasiStdio::Capture` 的 `MemoryOutputPipe` 句柄存入 `HostState`，run 后经 `drain_captured` 落回进程 stdout。
  - 证据：`stdout_drain_captures_all_bytes`、`worker_process_runs_component`。

## P1 健壮性/安全

- [x] ~~**多实例配置互相污染**~~ → 已修复：`build_store`/`load_component_for_worker/_thread`/`run_wasm` 改为**显式接收 `&SandboxConfig`**，线程回退用 `apply_sandbox_with(&config)` 不再写进程级全局；worker 保留帧 3 → 全局（L1）。
- [x] **`get_engine_config().expect("you must call init")`** → 已改安全化：未 init 不再 panic（按非 worker 处理）；公开入口 `require_init()` 提前返回错误。
- [x] **`increment_epoch()` 引擎级广播 + 关闭/超时语义补全** → 已补：
  - 新增 `wait_timeout` / `wait_output_timeout`（进程超时 `kill`，线程超时置标志 + **循环轻推 epoch**，超宽限 abandon 并返回错误）；
  - `shutdown()` 改为有界（不再无限 join）；
  - 边界：epoch 只能打断纯计算 guest，卡在阻塞宿主调用者只能 abandon（进程模式由父进程 `kill()` 兜底）。
- [x] **`WasiStdio::Capture` 输出检索未接线** → 已接线（见 P0）。
- [x] **默认 sealed + `wasi_preopen()` 反直觉** → 已补文档：`wasi_preopen` 注明与 `deny_file_access` 的冲突与解法；新增 `wasi_stdio(mode)` 一次性设置三流。

## P2 工程/清理

- [x] **线程回退无进程隔离** → 回退到线程时输出 `eprintln!` 降级警告（说明无进程隔离、L1 限制作用于宿主）。
- [x] **`input` 帧未接线到 WASI stdin** → 新增 `run_with_input` / `run_data_with_input`；帧 2 传真实输入；`build_store` 用 `MemoryInputPipe` 喂 WASI stdin。
- [x] **死代码/未用** → 已清理：移除 `WasmMetadata`、`ThreadCommand` 及 watch/`_rx` 链。
- [x] **`shutdown()` 忽略返回值** → Thread 上报 panic、Process 上报 `wait()` 错误，`kill`/drain 为尽力（已注释原因）。
- [x] **平台报错语义未区分** → 新增 `ApplyStatus::Denied`（平台支持但权限不足）；Windows `JOB_OBJECT_SECURITY_NO_ADMIN` 失败且 `ERROR_ACCESS_DENIED` 时映射为 `Denied`，`apply_sandbox_with` 分别告警。
  - unix 已统一：`unix_common::SyscallOutcome`（`EACCES`/`EPERM` → `Denied`，其余 → `Failed`→`Error`），`linux.rs`/`macos.rs`/`android.rs` 改用 `unix_common::status`；unix 文件待 CI（ubuntu/macos）实跑验证。

## 规划：WASI P2 接入 + 全可配置面状态机

> 已整体切到组件/p2（`p2::add_to_linker_{sync,async}` + `WasiView` 宿主 + `Command`）。

### 状态机（每面一个枚举，默认取最小）

| 面 | 状态 | 兑现机制 |
|---|---|---|
| stdio | `WasiStdio::{Inherit, Capture, Null}` | `inherit_*/Memory*Pipe/io::empty()` |
| env/args | 显式白名单 Vec | `env()/args()` |
| preopens | `WasiPreopen{host,guest,read,write}` | `preopened_dir(FsPerms::{ReadOnly,ReadWrite})` |
| sockets | `WasiSockets::{None, Allow(Vec<PortRange>), Deny(..), LoopbackOnly}` | `socket_addr_check(SocketAddr, SocketAddrUse)` |
| ip-name-lookup | `wasi_ip_name_lookup: bool` | `allow_ip_name_lookup` |
| clocks | `WasiClocks::{Host, Deterministic(seed)}` | `wall_clock/monotonic_clock` |
| random | `WasiRandom::{Os, Seeded(u128)}` + `wasi_max_random_size` | `Seeded` 只播**非安全流**（`insecure_random(_seed)`）；安全流保持 OS 真随机；`max_random_size` |
| initial-cwd | `wasi_initial_cwd: Option<String>` | `initial_cwd` |
| blocking | `wasi_allow_blocking_current_thread: bool` | `allow_blocking_current_thread` |
| 线性内存 | `max_memory_bytes`（L2 侧） | `StoreLimits` + `Store::limiter` |

### 状态规则

- 每面默认最小值（`Inherit`/ 空 / `None` / `Host` / `Os`）；配置一经出现不可再"静默降级"。
- 可兑现集：当前仅 p2；未兑现或与 L1 冲突的配置由 `validate` 在启动前拒绝（L2 fail-closed）。
- 与 L1 求交：`wasi_sockets ⊆ network_ports`、`wasi_preopens ⊆ fs_rules`。

### 接入步骤

- [x] 状态机枚举/字段/builder/serde/`Default`・`open()`/`validate`（L1∩L2 求交）全部落地。
- [x] `WasiCtxBuilder` 的可配置项已全部映射进 `SandboxConfig`（`wasi_ip_name_lookup`/`wasi_max_random_size`/`wasi_initial_cwd`/`wasi_allow_blocking_current_thread`；`inherit_env`/`inherit_args`/`inherit_network` 等宿主继承项按 Deny-by-Default 不暴露）。
- [x] `build_store` 逐面接线；p1 路径整体移除（组件/p2-only）。
- [x] Deterministic clocks（`tests/wasi_clocks.rs`）、Seeded random（`tests/wasi_random.rs`）guest 级 e2e；sockets 端口策略单测（`tests::socket_policy_allow_deny_loopback_none`）；config 级测试见 `tests/sandbox.rs`。
- [x] 更新 `memory.md`（WASI 表）。

> 遗留：**sockets 的 guest 级 e2e** 未做（仍开在 `TODO.md`）——组件 `network`/`tcp-socket` 等 resource 句柄只能由 guest 自建，宿主无法注入。

## P3 测试缺口

- [x] **CI 覆盖各平台 + GUI crate** → `.github/workflows/ci.yml` 的 `test-matrix`：`ubuntu`/`macos`/`windows` 跑 `cargo test -p whitebox_core`，非 ubuntu 再跑 `cargo test -p whitebox-forge`（Slint GUI crate）。（仍待 CI 实跑确认。）
- [x] **线程关闭端到端** → `tests/timeout.rs::shutdown_interrupts_spinning_guest`：死循环 guest + `shutdown()` 经 epoch 打断并返回。
- [x] **进程 worker 链路端到端** → `tests/worker.rs::worker_process_runs_component`：真 worker 二进制 + `run_data` + `wait_output`（stdout 结果通道）。
