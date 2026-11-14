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

- [x] ~~**多实例配置互相污染**~~ → 已修复：`build_store`/`load_component_for_worker/_thread`/`run_wasm` 改为**显式接收 `&SandboxConfig`** 不再写进程级全局；worker 保留帧 3 → 全局（L1）。线程回退**不再应用 L1**（见「安全加固」）。
- [x] **`get_engine_config().expect("you must call init")`** → 已改安全化：未 init 不再 panic（按非 worker 处理）；公开入口 `require_init()` 提前返回错误。
- [x] **`increment_epoch()` 引擎级广播 + 关闭/超时语义补全** → 已补：
  - 新增 `wait_timeout` / `wait_output_timeout`（进程超时 `kill`，线程超时置标志 + **循环轻推 epoch**，超宽限 abandon 并返回错误）；
  - `shutdown()` 改为有界（不再无限 join）；
  - 边界：epoch 只能打断纯计算 guest，卡在阻塞宿主调用者只能 abandon（进程模式由父进程 `kill()` 兜底）。
- [x] **`WasiStdio::Capture` 输出检索未接线** → 已接线（见 P0）。
- [x] **默认 sealed + `wasi_preopen()` 反直觉** → 已补文档：`wasi_preopen` 注明与 `deny_file_access` 的冲突与解法；新增 `wasi_stdio(mode)` 一次性设置三流。

## P2 工程/清理

- [x] **线程回退无进程隔离** → 回退到线程时输出 `eprintln!` 降级警告（说明无进程隔离；且**不施加 L1**，见「安全加固」）。
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
- 与 L1 求交：`wasi_sockets ⊆ network_ports` 由 `validate` 强制；preopen 的 L1 只读/白名单一致性由 `wasi_preopen_ro` 统一保持，**未**在 `validate` 中做 `wasi_preopens ⊆ fs_rules` 的通用求交（`build_store` 打开宿主目录失败即 L2 fail-closed）。

### 接入步骤

- [x] 状态机枚举/字段/builder/serde/`Default`・`open()`/`validate`（sockets 与 `network_ports` 求交；preopen 由 `wasi_preopen_ro` 保持一致）全部落地。
- [x] `WasiCtxBuilder` 的可配置项已全部映射进 `SandboxConfig`（`wasi_ip_name_lookup`/`wasi_max_random_size`/`wasi_initial_cwd`/`wasi_allow_blocking_current_thread`；`inherit_env`/`inherit_args`/`inherit_network` 等宿主继承项按 Deny-by-Default 不暴露）。
- [x] `build_store` 逐面接线；p1 路径整体移除（组件/p2-only）。
- [x] Deterministic clocks（`tests/wasi_clocks.rs`）、Seeded random（`tests/wasi_random.rs`）guest 级 e2e；sockets 端口策略单测（`tests::socket_policy_allow_deny_loopback_none`）；config 级测试见 `tests/sandbox.rs`。
- [x] 更新 `memory.md`（WASI 表）。

> 遗留：**sockets 的 guest 级 e2e** 未做（仍开在 `TODO.md`）——组件 `network`/`tcp-socket` 等 resource 句柄只能由 guest 自建，宿主无法注入。

## P3 测试缺口

- [x] **CI 覆盖各平台 + GUI crate** → `.github/workflows/ci.yml` 的 `test-matrix`：`ubuntu`/`macos`/`windows` 跑 `cargo test -p whitebox_core`，非 ubuntu 再跑 `cargo test -p whitebox-forge`（Slint GUI crate）。（仍待 CI 实跑确认。）
- [x] **线程关闭端到端** → `tests/timeout.rs::shutdown_interrupts_spinning_guest`：死循环 guest + `shutdown()` 经 epoch 打断并返回。
- [x] **进程 worker 链路端到端** → `tests/worker.rs::worker_process_runs_component`：真 worker 二进制 + `run_data` + `wait_output`（stdout 结果通道）。

## 安全加固（评审修复）

> 证据：`cargo check --workspace --all-targets` 零告警、`cargo test -p whitebox_core` 全绿。unix 代码本机（Windows）不编译，待 CI 验证。

- [x] **线程回退不再对宿主施加不可逆 L1** → 回退线程删除 `apply_sandbox_with`；`chroot`/Landlock/rlimit/Job 等进程级原语不再作用于宿主，回退仅保留 L2（WASI），并打印明确警告。
  - 说明：若将来需要线程回退的 L1，应改成显式 opt-in 且只施加可逆子集（当前不提供）。
- [x] **worker stdout 捕获有上限** → `spawn_stdout_drain` 最多保留 8 MiB，超限继续排空但丢弃（防 guest 海量输出 OOM 宿主）。证据：`stdout_drain_truncates_at_cap`。
- [x] **`--preopen-ro` 最小授权** → 不再全局 `deny_file_access=false`；改为 `readonly_fs=true` + 仅对每个 host 路径 `allow_fs_read`（L1 只放行白名单）。解析用 `rsplit_once(':')`（兼容 Windows 盘符），并校验 GUEST 非空、HOST 为绝对路径。
- [x] **sockets 尊重 `SocketAddrUse`** → 出站（connect/send/receive）按端口策略；绑定仅放行端口 0 的隐式绑定；`Listen`/`Accept` 一律拒绝（禁止 guest 在宿主开监听）。证据：`socket_use_policy_only_allows_outbound`。
- [x] **Landlock write-only 不再顺带授予读** → 路径规则按 `rule.read` 决定读位，write-only 不再拿到 `READ_FILE/READ_DIR`（`unix_common.rs`，待 CI 验证）。
- [x] **`SandboxHandle` 丢弃即清理** → `runner: Option<Runner>` + `Drop`：未 `wait*`/`shutdown` 就丢弃时，Process `kill`+`wait`，Thread 置标志 + 单次轻推 epoch（回调按各自 flag 判定，不误杀并发）。
- [x] **协议帧上限 + 入口预校验** → `read_bytes` 限 64 MiB（防 u32 长度致 4 GiB 分配）；`run_source` / worker `run_worker` 应用前 `validate`，冲突返回可诊断错误而非 panic。
- [x] **Windows Job 多余句柄关闭** → `JOB_HANDLE` 已存在时 `CloseHandle` 本次新建的句柄，避免泄漏。
