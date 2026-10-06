# WhiteBox-Forge TODO（现存遗留问题）

## P0 链路/可用性

- [ ] **`src/main.rs` 未调用 `whitebox_core::init()`**
  - `src/main.rs:1-3`（`println!("Hello, world!")`）
  - 相关：`whitebox_core/src/lib.rs`（`init()` / `run_source` spawn 自身）
  - 后果：进程 worker 由 `current_exe()` 重启自身，但子进程进不了 `run_worker` → 进程隔离链路端到端不通。
- [ ] **stdout 结果通道缺失**
  - `run_source` 中 `stdout(Stdio::piped())` 但从不读取；`Runner::Process(child)` 只持有，无消费线程
  - worker 侧 `inherit_stdio`
  - 后果：wasm 大量 `fd_write` 写满管道 → worker 阻塞。

## P1 健壮性/安全

- [x] ~~**多实例配置互相污染**~~ → 已修复：`build_store`/`load_module_for_worker/_thread`/`run_wasm` 改为**显式接收 `&SandboxConfig`**，线程回退用 `apply_sandbox_with(&config)` 不再写进程级全局；worker 保留帧 3 → 全局（L1）。
- [ ] **`get_engine_config().expect("you must call init")`**
  - 未 `init()` 就 `load_*` 会 panic；建议返回 `Result` 或文档强制。
- [ ] **`increment_epoch()` 引擎级广播 + 关闭/超时语义补全**
  - `ENGINE.increment_epoch()` 触发同进程所有 store 的 epoch 检查（靠各 store 回调 flag 缓解误杀）
  - 进程(worker)路径：阻塞执行可被父进程 `child.kill()` 硬杀（`SandboxHandle::shutdown()` 已有）；缺口在**自动超时**——`wait()` 阻塞时无"等待 N 秒再 kill"的能力
  - 线程回退路径：无法强制 kill 线程，仅靠 epoch 协作打断（`shutdown_requested` flag → 回调 Err），若 wasm 卡在阻塞宿主调用可能一直 join 不返回
- [ ] **`WasiStdio::Capture` 输出检索未接线**：stdout/stderr 捕获进 `MemoryOutputPipe` 后随 ctx 丢弃（行为≈Null）；待 stdout 结果通道（P0）一并接回；`MemoryOutputPipe` 1MB 上限写满会截断/报错（可接受）。
- [ ] **默认 sealed + `wasi_preopen()` 反直觉**：`SandboxConfig::default()` 为 `deny_file_access=true`，调 `wasi_preopen` 会触发 validate 冲突——需 `sealed_fs(false)`/`open()`；在 builder 文档与 AGENTS 补充说明。

## P2 工程/清理

- [ ] **线程回退无进程隔离**
  - `run_source` 的线程回退分支：`apply_sandbox_with(&config)` 在宿主线程执行
  - rlimit/prctl/unshare 是进程级，多线程回退互相影响且限制的是宿主；建议运行时"无进程隔离"降级警告。
- [ ] **`input` 帧未接线到 WASI stdin**
  - `run_source` 写空输入帧、`run_wasm` 参数未消费、`WasiStdio::Capture` stdin 同理恒空
- [ ] **死代码/未用**
  - `WasmMetadata` 无消费方（已 `#[allow(dead_code)]`，待签名校验接入）
  - `ThreadCommand` / `_rx` 参数 —— watch 已注释，可整链清理
- [ ] **`shutdown()` 忽略返回值**
  - `joinhandle.join()` / `child.kill()` / `child.wait()` 的 Result 未处理
- [ ] **平台报错语义未区分**
  - `sandbox/windows.rs`（`JOB_OBJECT_SECURITY_NO_ADMIN` 对管理员进程是"权限不足"而非"不支持"）
  - `sandbox/unix_common.rs`（`deny_network` = `unshare(CLONE_NEWNET)` 会切断回环；`limit_cpu_ms` 受 RLIMIT_CPU 秒级粒度限制；`strict_syscalls` 仅 x86_64 安装过滤器）
  - 建议未来在 `CapabilityReport` 区分错误原因。

## 规划：WASI P2 接入 + 全可配置面状态机

> 方向已定：接入 wasmtime-wasi **p2（preview2，含 wasi:sockets/clocks/random）**，
> 把所有 WASI 可配置面做成显式状态机；L2 铁律不变——每面一经配置必须可兑现，否则 `build_store` 报错终止。

### 状态机（每面一个枚举，默认取最小）

| 面 | 状态 | 兑现机制 | p1 | p2 |
|---|---|---|---|---|
| stdio | `WasiStdio::{Inherit, Capture, Null}`（已有） | `inherit_*/Memory*Pipe/io::empty()` | ✅ | ✅ |
| env/args | 显式白名单 Vec（已有） | `env()/args()` | ✅ | ✅ |
| preopens | `WasiPreopen{host,guest,read,write}`（已有） | `preopened_dir(FsPerms::{ReadOnly,ReadWrite})` | ✅ | ✅ |
| **sockets**（新） | `WasiSockets::{None, Allow(Vec<PortRange>), Deny(..), LoopbackOnly}` | `socket_addr_check(SocketAddr, SocketAddrUse)` | ❌ 配置即报错 | ✅ |
| **clocks**（新） | `WasiClocks::{Host, Deterministic(seed)}` | `builder.clocks()` | ✅ | ✅ |
| **random**（新） | `WasiRandom::{Os, Seeded(u128)}` | `secure_random/insecure_random(_seed)` | ✅ | ✅ |

### 状态规则

- 每面默认最小值（`Inherit`/ 空 / `None` / `Host` / `Os`）；配置一经出现不可再"静默降级"。
- **可兑现集按 p1/p2 区分**：sockets 仅在 p2 兑现，p1 下配置即 `Err`（L2 fail-closed，绝不放宽为忽略）。
- **与 L1 求交**：`wasi_sockets ⊆ network_ports`、`wasi_preopens ⊆ fs_rules`（父进程侧 `validate` 校验；越界报错）。

### 接入步骤

- [ ] 新增 `WasiSockets`/`WasiClocks`/`WasiRandom` 枚举 + `SandboxConfig` 字段 + builder + serde + `Default/open()` + `validate`（L1∩L2 求交）。
- [ ] `build_store` 接入 p1 可兑现项（clocks/random）；socket_addr_check 仅在 p2 路径启用，p1 配置 sockets → `Err`。
- [ ] 确定 p2 路径：core 模块走 `wasmtime_wasi::p2::add_to_linker` 需 `WasiView` 宿主 + async store；核对 `wasi:io/ sockets` 导入形态与现有 p1/host_func 的衔接。
- [ ] 端到端测试：sockets 允许/拒绝端口、Deterministic clocks 可复现、Seeded random 可复现。
- [ ] 更新 `memory.md`（WASI 表）与 `AGENTS.md`（如需）。

## P3 测试缺口

- [ ] **真机覆盖 Linux/macOS 沙箱（Landlock/seatbelt/seccomp）**
  - `whitebox_core/tests/sandbox.rs` 目前纯逻辑；平台实现 `sandbox/linux.rs`、`sandbox/macos.rs`、`sandbox/unix_common.rs`
- [ ] **线程关闭端到端**：`SandboxHandle::shutdown()` → `increment_epoch()` → `epoch_callback` Err 打断 → join 返回。
- [ ] **进程 worker 链路端到端**（依赖 P0 前两项）
  - 需：main 接 `init()` + 授权 wasm 目录（或 `run_data` 字节路径）+ stdout 结果校验。
