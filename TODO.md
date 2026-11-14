# WhiteBox-Forge TODO（未完成项）

> 已完成条目已归档到 [`achieved.md`](./achieved.md)；本文件只保留 `[ ]` 未完成项。
> 基线：`wasmtime`/`wasmtime-wasi` **49.0.2**（组件/p2-only）。验证：`cargo check --workspace --all-targets` 零告警、`cargo test -p whitebox_core` 全绿。

## 优先级排序（按「易 → 难 + 进度」，供排期；详细条目见下）

- **P-A 快赢**（低难度、无前置，可立即做）：启用 `call-hook` feature；`DEFAULT_SHUTDOWN_GRACE` 可配；README/公开 API doc 示例；有界停止「边界写清」；`ResourceLimiter` 扩展（table/instances）；`log`/`tracing`；CI 加固（fmt/deny/MSRV）；abandon 线程计数上限。
- **P-B 中等·高价值**（少量前置 / API 改动）：`validate()` 返 `Result`；`max_memory_bytes` 拆 L1/L2；CLI 暴露新面；能力/依赖静态提取 + 依赖/溯源；fuel 接入；`build_store` 注册 call_hook；线程路径 async 可取消；有界停止对照测试；property/fuzz；策略即代码+分层锁定；生命周期状态机；编译缓存目录保护 / GC；资源使用画像；防篡改哈希链；差异对比；错误类型统一。
- **P-C 有前置**（需 guest 工具链 / 平台 / "包"抽象）：`input` 帧 e2e；sockets guest e2e；p1 构建期转换（运行期 p1 sync 路径已否决）；**组件 host 导入**（→ 凭据代理的前置）；真机 linux/macOS 行为断言（需 CI）；磁盘 I/O / 网络带宽配额；每包原子化 / `.wasm` 关联 / 原生快捷方式。
- **P-D 大工程**（跨模块 / 平台重）：快照+回滚；结构化审计日志；凭据代理；拖拽安装+确认；恶意/漏洞模式识别；可选专用 AI 辅助层；microVM；TPM/keyring 签名；内存快照（不推荐）；「一次运行=可验证事务」总装。

## 遗留（原规划/P3 未清）

- [ ] **sockets 的 guest 级 e2e**：sockets 允许/拒绝端口。（Deterministic clocks 已 `tests/wasi_clocks.rs`、Seeded random 已 `tests/wasi_random.rs`；sockets 仅策略单测 `tests::socket_policy_allow_deny_loopback_none` + config 级测试）
  - 遗留原因：组件 `network`/`tcp-socket` 等 resource 句柄只能由 guest 自建，宿主无法注入（需自定义 WIT host 导入 + `linker.root().func_wrap` 返回 resource），手写成本高。
- [ ] **真机覆盖 Linux/macOS 沙箱（Landlock/seatbelt/seccomp）行为断言**
  - 矩阵已在真实 linux/macos runner 上编译并运行现有测试（可抓 unix 编译错误）。
  - 遗留：`tests/sandbox.rs` 仍是平台无关逻辑 + 被 guard 的 no-op，**未断言 Landlock/seatbelt 实际生效**（`ApplyStatus::{Applied,Denied}`）；需在 linux/macos 上补针对性测试。

## 可改进项（评审产出，未排期）

> 目标：把"能配"变"管用/管安全"。

### 正确性 / 安全（优先）

- [ ] **`max_memory_bytes` 一字段两用**：L1（Windows `JOB_OBJECT_LIMIT_JOB_MEMORY`，`sandbox/windows.rs`）与 L2（`StoreLimits`，`lib.rs::build_store`）。小值会让 Job 杀掉 wasmtime 自身。
  - 改：拆 `max_memory_bytes`(L2 wasm 堆) 与 `max_process_commit_bytes`(L1)，或 L1 = L2 + 余量。
- [ ] **`validate()` 仍以 `assert!` 收尾**（`sandbox/mod.rs::setup_sandbox_with`）：配置错误是数据不应 panic。
  - 现状：入口已预校验（`run_source` / worker `run_worker` 在应用沙箱前 `validate` 并返回可诊断错误），但 `setup_sandbox_with` 本身仍 `assert!`。
  - 改：`validate() -> Result<(), ConfigError>`，`build_store` 前置校验返回 Err（fail-closed 且可控）。

### 能力完整性

- [ ] **`ResourceLimiter` 只限内存**：`StoreLimitsBuilder` 还有 `table_elements`/`instances`/`tables` 未纳入 `SandboxConfig`。
  - 改：加 `max_table_elements`/`max_instances`（防表增长/实例爆炸 DoS）。
- [ ] **组件 host 导入未接**（原 core `host_func` 已删）：guest 不能回调宿主，宿主能力核心缺失。
  - 改：定 WIT 接口 + `bindgen!`/`linker.root().func_wrap`，补最小 host 函数 + e2e。
- [ ] **`input` 帧无 e2e**：`run_data_with_input`/`run_with_input` 已接，但没有 guest 真读 stdin 的链路测试。
  - 改：一个读 `wasi:cli` stdin 的组件 + 断言收到输入。
- [ ] **p1 兼容：仅构建期转换（运行期 p1 sync 路径已否决）**
  - 结论：维持组件/p2-only。**不**在运行期加 p1 core-module 的同步路径——`wasi_snapshot_preview1` 是**同步阻塞**宿主 API、无 await 点，不可 `select!` 取消，会给线程回退引入"更不可中断"的路径，并与「有界停止」的 p2 语义不一致。
  - 推荐：p1 由**构建期**过官方 preview1→preview2 adapter 转组件（`wasm-tools component new --adapt wasi_snapshot_preview1=<adapter>`），进入沙箱的一律是 p2 组件。
  - 现状：p1 core module 直接 `failed to parse WebAssembly module`（**明确失败，不静默**）。影响面 = 覆盖率（存量 p1），非正确性。

### 有界停止：call-hook + fuel + 协作宿主 API（提议）
>
> 目标：停止不再单点依赖 epoch。三层叠加——宿主调用/进出 wasm 边界检查（读 HostState）、确定性 CPU 预算（fuel）、把阻塞型宿主 API 改成协作/可取消。

- [ ] **启用 wasmtime `call-hook` feature**（现 `whitebox_core/Cargo.toml` 仅 `features=["cache"]`；`Store::call_hook` 在 `#[cfg(feature="call-hook")]` 下）。
- [ ] **`build_store` 注册 `Store::call_hook`**：在 `CallingHost`/`CallingWasm` 边界读 `HostState`（shutdown flag + 墙钟 deadline），命中即 `Err` → trap。**不依赖 epoch** 的协作式停止，覆盖"频繁调宿主但自身不循环"的 guest。
- [ ] **fuel 接入**：`Config::consume_fuel(true)` + `max_fuel: Option<u64>` + `store.set_fuel`（当前 `get_engine_config` 里是注释）；耗尽在指令边界 trap（确定性），可与 call-hook 状态检查结合决定"正常结束 vs 超限"。
- [ ] **替换阻塞型宿主 API**：任何会阻塞的宿主函数（sleep / poll / wait / 长 IO）改为**小步循环轮询** shutdown flag + deadline，命中即返回；禁止无界阻塞。策略文档化："宿主侧不得做不可取消的无界阻塞"。
- [ ] **异步可取消（线程路径）**：`add_to_linker_async` + 把 `Command::instantiate_async`/`call_run` 放进 `tokio::select!`（对关闭/超时定时器），使 WASI 阻塞调用可随 future drop 取消。
- [ ] **边界写清（注释/文档）**：call-hook 只在宿主调用/进出 wasm 边界检查，**打断不了纯 wasm 长循环**（仍须 epoch/fuel），也**打断不了已在进行的单个阻塞宿主调用**（须 async 取消或进程 kill）。
- [ ] **测试**：guest 反复调宿主 → call-hook 无 epoch 即停止；guest 纯长循环 → fuel 停止（与 epoch 路径对照）。

### 工程 / 测试

- [ ] **abandon 线程无上限**：`wait_timeout` 超时后线程泄漏（有界但可累积）。
  - 改：统计 abandoned 线程数，超阈值拒绝新线程运行。
- [ ] **CI 尚未加固**：已加 `cargo-deny` **门禁**（`advisories/bans/sources`）+ `cargo-audit` **信息报告**（`continue-on-error`）；`test-matrix` 未实跑；clippy 仍 advisory。
  - 剩余：`cargo fmt --check`、MSRV、`-D warnings` 策略；`deny.toml` 的 `licenses` 段（Slint 多许可证/GPL）待评审后决定是否进门禁。
- [ ] **无 property/fuzz 测试**：`validate`（区间求交、互斥）适合 proptest。
- [ ] **错误类型不统一**：`wasmtime::Error` / `anyhow` / `WorkerError` 混用，边界转换散布。
  - 改：统一在 run/wait 边界收敛。

### 构建 / 发布（考虑，未排期）

- [ ] **构建可复现（评估后暂不做）**：`--locked` 只固定 crate 版本；`dtolnay/rust-toolchain@stable` 与 `*-latest` runner 仍会漂移，且二进制含绝对路径、链接器 UUID/时间戳，tar/zip 的 mtime/owner/顺序也不固定。
  - 现状：`release.yml` 已用 `Swatinem/rust-cache@v2`（构建缓存已具备）；发布构建可考虑 `cache-targets: false` 只缓存 registry/git。
  - 「可验证」低成本方案：固定 `rust-toolchain.toml`（如 `1.98.1`）+ runner 镜像，并随产物发布 `SHA256SUMS`。
  - 「字节级」成本较高：再加 `--remap-path-prefix`、`-C strip=symbols`、平台链接 flags（`/Brepro`、`-Wl,-no_uuid`、`-Wl,--build-id=none`）与 `SOURCE_DATE_EPOCH` + 可复现 tar/zip。
  - 结论：非存证需求下不追求字节级；先保留校验和思路即可。

### DX / 文档

- [ ] **CLI 未暴露新面**：`src/main.rs` 仅 `--stdio/--timeout-ms/--no-sandbox/--preopen-ro`；sockets/clocks/random/initial-cwd/max-random-size 无 flag。
  - 改：按 `SandboxConfig` 补选项或 `--config <json>`。
- [ ] **无 README / 公开 API 缺 doc 示例**：`run`/`run_data`/`wait_output` 需 `# Examples`；CLI 用法未文档化。
- [ ] **日志用 `eprintln!`**：换 `log`/`tracing` 便于宿主集成与分级。
- [ ] **`DEFAULT_SHUTDOWN_GRACE` 硬编码 2s**（`lib.rs`）：超时语义应可配。

> 建议顺序：fuel → `max_memory_bytes` 拆分 → `validate` 返 `Result`；其余按需求排。

## 候选特性（提案，未排期）

> 方向记录，多数属"应用分发 / 包管理"范畴，**前提是先有"包/应用"这一抽象**（当前只有"跑一个 wasm/组件"）。仅登记，不代表承诺。

### 隔离档位

- [ ] **硬件虚拟化隔离层（microVM / hypervisor，最高档位）**：在现有 L1（进程/OS）+ L2（WASI）之上，加可选的第三层内核级隔离——WHP/Hyper-V（Win）、KVM（Linux）、HVF（macOS），或 Firecracker 式 microVM。
  - 成本：平台特定实现 + VM 启动/镜像裁剪 + guest 内核；属"可选加强层"，不替换现有两层。可行性 OK，非当前优先级。

### 编译缓存

- [ ] **编译缓存目录保护（防缓存中毒）**：wasmtime `Config::cache` 目录做权限/完整性保护（低权/guest 不可写）+ 校验和/签名（README 已提"guard against cache poisoning"）。
- [ ] **删除无引用编译缓存**：编译缓存 GC——按引用计数/可达性清理孤儿条目（模块/组件已不再引用）。

### 供应链 / 包管理

- [ ] **TPM / OS keyring 签名验证全部包管理内容**：用 TPM（TSS）或系统 keyring（Windows DPAPI、macOS Keychain、Linux libsecret）做密钥托管与签名校验，覆盖"全部包内容"防篡改。
- [ ] **每包目录的原子化操作**：每个包的安装/更新以原子方式落盘（临时目录写入 + rename 交换），失败可回滚、杜绝半状态。

### 系统集成

- [ ] **注册为 `.wasm` 默认打开方式**：安装时注册系统文件关联（Windows 注册表 / macOS LaunchServices / Linux `.desktop` + MIME）。
- [ ] **为包添加原生软件快捷方式**：桌面/开始菜单创建快捷方式（`.lnk` / `.app` / `.desktop`）。

### 前端 / GUI
- [ ] **拖拽安装 + 确认提示**：把安装包文件拖入主窗口即触发安装流程，并弹出**确认对话框**——先展示摘要（来源、申请的权限/能力、资源、依赖、签名状态），用户确认后再走落盘安装。取消则不改动任何状态。
  - 前置：Slint GUI 尚未接线（`src/main.rs` 目前只有 CLI）；安装流程依赖"包/应用"抽象 + 原子化安装（见上）；摘要可直接复用「可审查性 · 静态审查」的能力/依赖提取结果。

## 可审查性：让每次运行＝可验证事务（提案，未排期）

> 目标：应用运行**可审查**——申请了什么权限、怎么用资源、用多久、有哪些依赖、有无恶意/漏洞。与「候选特性」互补。
> **设计原则：确定性优先**——精确结论（权限/依赖/证据）一律来自确定性分析（imports/WIT/审计/签名）；**AI 仅作建议**，绝不作裁决或证据（不可复现、不可审计）。

### 策略与生命周期
- [ ] **策略即代码 + 分层锁定**：`SandboxConfig` ↔ 声明式策略文件（TOML/JSON），纳入版本控制；高层可 `lock_*` 锁定，低层不允许放宽（复用现有 `validate` 做交叉校验）。
- [ ] **生命周期状态机**：create → run → pause → stop → destroy，显式状态 + 资源释放；当前只有 run / wait / shutdown。

### 状态与差异
- [ ] **快照 + 回滚**：每次 run 前后对受控目录做基线/结束快照，支持恢复到任意点；先目录级，内容寻址 CAS(SHA-256)+去重、分支/分叉随后。
- [ ] **差异对比**：快照间目录树 / 行级 diff → 人读报告；语义化 diff（函数/类级）需语言感知，可选、后做。
- [ ] （可选）**内存快照**：跨平台基本不可行（Windows 无 CRIU 等价），优先"WASI 状态快照"而非通用内存快照。

### 审计与证据
- [ ] **结构化审计日志**：在可信/不可信边界（父进程、host 调用边界）记录 fs / 网络 / 进程 / 权限事件。
- [ ] **防篡改**：哈希链（`entry_hash = SHA256(prev || canonical_json)`）→ Merkle 树 + 对根数字签名（含/一致性证明视需要）。
- [ ] **凭据代理**：向 guest 注入短期令牌、原始凭据不进沙箱（**前置：组件 host 导入**，见上）。

### 静态审查（wasm 层，确定性优先）
- [ ] **能力/依赖静态提取**：解析组件 imports / WIT（`wasmcap` 思路 + `wasmparser` + `wasm-tools component wit`）→ 精确给出"申请哪些权限/依赖哪些接口"，直接映射到能力模型（imports ⊆ granted）。
- [ ] **资源使用画像（类别级）**：从 imports/结构推断资源面（clocks→可长睡、sockets/random/fs 的使用）；**不承诺精确"用多久"**（停机问题），精确上界靠运行期 epoch/fuel/配额。
- [ ] **恶意/漏洞模式识别**：对 WAT/IR 做启发式 / 模式分类（挖矿循环、硬编码外联、混淆、已知坏模式）；仅作**建议**，不作裁决。
- [ ] **依赖 / 溯源**：custom sections（name/producers/source-map）+ 组件 WIT 列依赖；无元数据时标注"未知"。
- [ ] （可选）**专用 AI 辅助层**：本地小模型对 WAT/IR 做"意图解释 + 可疑模式"标注，**离线、可复现性尽力、非权威**；不接入通用云 LLM（隐私/不可复现），仅在明确同意下可选。

### 资源配额（扩展）
- [ ] **磁盘 I/O / 网络带宽配额**：OS 层（cgroup v2 io/网络、Windows Job 计数）；CPU=fuel（见「有界停止」）、内存=`StoreLimits`（已接）。

### 审查证据链总装
- [ ] **一次运行 = 一个可验证事务**：把「快照 + 审计链 + diff + 策略结果 + 静态画像」串成单条证据链，输出人类报告 + 可签名存证。
