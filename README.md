# WhiteBox-Forge

A transparent, capability-based sandbox for WebAssembly applications. It gives users fine-grained control over filesystem access and allows complete rollback of any changes — turning every app into a temporary, auditable environment.

一个透明、基于能力（capability）的 WebAssembly 应用沙箱。它为用户提供对文件系统访问的细粒度控制，并允许完全回滚任何更改——让每个应用都变成一个临时的、可审计的环境。

**Status: early development.** The core execution engine and process isolation scaffolding exist, but the GUI and most sandbox capabilities are not wired up yet.

**状态：早期开发阶段。** 核心执行引擎与进程隔离框架已就绪，但 GUI 和大部分沙箱能力尚未接入。

## Features / 功能特性

- **Transparent sandboxing** — every WebAssembly application runs in an isolated environment you can inspect, not a black box.
- **Capability-based filesystem access** — grant only the exact filesystem access an app needs.
- **Full rollback** — any change made inside the sandbox can be discarded, so every app behaves like a temporary environment.
- **Isolated execution** — each sandbox runs in its own worker process (`__WASM_WORKER`), with a thread fallback when process spawning fails.
- **WASI support** — built on [Wasmtime] with the WASI component model and async support enabled.
- **Cross-platform design** — platform backends for Windows, macOS, Linux, iOS, and Android.
- **Native GUI** — a [Slint]-based desktop interface (in progress).

- **透明沙箱** — 每个 WebAssembly 应用都在可检查的隔离环境中运行，而非黑盒。
- **基于能力（capability）的文件系统访问** — 只授予应用所需的精确文件系统访问权限。
- **完全回滚** — 沙箱内产生的任何更改都可丢弃，让每个应用都像临时环境一样运行。
- **隔离执行** — 每个沙箱在独立的 worker 进程（`__WASM_WORKER`）中运行，进程创建失败时回退为线程。
- **WASI 支持** — 基于 [Wasmtime]，启用 WASI 组件模型与异步支持。
- **跨平台设计** — 提供 Windows、macOS、Linux、iOS 与 Android 平台后端。
- **原生 GUI** — 基于 [Slint] 的桌面界面（开发中）。

## Architecture / 架构

WhiteBox-Forge is split into two crates:

WhiteBox-Forge 分为两个 crate：

| Crate / crate | Role / 角色 |
| --- | --- |
| `whitebox-forge` | Binary / GUI frontend (Slint, CLI via clap) / 二进制与 GUI 前端（Slint，clap 提供 CLI） |
| `whitebox_core` | Sandbox engine (Wasmtime, WASI, process isolation) / 沙箱引擎（Wasmtime、WASI、进程隔离） |

Key design decisions:

关键设计决策：

- A single global `wasmtime::Engine` is lazily initialized and shared across sandboxes. Compilation caching is planned, with care taken to guard against cache poisoning.
- 一个全局的 `wasmtime::Engine` 被惰性初始化并在沙箱间共享。计划加入编译缓存，同时注意防范缓存中毒。

- Each sandbox is spawned as a fresh worker process by re-executing the current binary with the `__WASM_WORKER=1` environment variable and a cleared environment. This keeps hostile modules out of the host process. If process creation fails, execution falls back to a thread.
- 每个沙箱通过重新执行当前二进制（设置 `__WASM_WORKER=1` 环境变量并清空其余环境）来启动一个全新的 worker 进程，让不受信任的模块无法影响宿主进程。若进程创建失败，则回退为线程执行。

- Workers communicate over stdin/stdout using length-prefixed frames (a `u32` length followed by the payload), isolating untrusted workloads behind a simple protocol.
- worker 之间通过 stdin/stdout 以长度前缀帧（`u32` 长度 + 载荷）通信，将不受信任的工作负载隔离在简单协议之后。

- Platform-specific sandbox primitives live in `whitebox_core/src/sandbox/` and degrade gracefully instead of relying on hard compile-time feature splits.
- 平台相关的沙箱原语位于 `whitebox_core/src/sandbox/`，通过逐级回退优雅降级，而非依赖硬编码的编译期平台分支。

## Getting Started / 快速开始

### Prerequisites / 环境要求

- Rust toolchain with edition 2024 support (Rust 1.85 or newer)
- 支持 2024 edition 的 Rust 工具链（Rust 1.85 或更新版本）

### Build / 构建

```sh
cargo build
```

### Run tests / 运行测试

```sh
cargo test --all-features
```

### Run / 运行

```sh
cargo run
```

## Project Layout / 项目结构

```
├── src/                # Binary entry point (GUI / CLI frontend) / 二进制入口（GUI / CLI 前端）
├── whitebox_core/
│   ├── src/
│   │   ├── lib.rs      # Engine, loader, and worker protocol / 引擎、加载器与 worker 协议
│   │   └── sandbox/   # OS-specific sandbox backends / 各平台沙箱后端
│   └── tests/          # Integration tests / 集成测试
├── assets/             # Icons and UI assets / 图标与 UI 资源
├── build.rs            # Windows icon embedding / Windows 图标嵌入
└── CONTRIBUTING.md     # Contribution guide / 贡献指南
```

## Roadmap / 路线图

- [ ] Filesystem capability negotiation and enforcement / 文件系统能力协商与强制
- [ ] Change tracking and full rollback for app-visible state / 对应用可见状态做变更跟踪与完全回滚
- [ ] WASI preview 2 plumbing into the worker protocol / 将 WASI preview 2 接入 worker 协议
- [ ] Sandbox auditing UI in Slint / Slint 中的沙箱审计界面
- [ ] Cross-platform sandbox primitives on all supported OSes / 在所有支持的系统上完成跨平台沙箱原语

## Contributing / 贡献

Contributions of all kinds are welcome — bug reports, feature requests, documentation, and code. See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide. In short: fork, branch, run `cargo check` and `cargo test` (clippy and fmt are advisory), then open a Pull Request with a Conventional Commit message.

欢迎各种形式的贡献——错误报告、功能请求、文档和代码。完整指南见 [CONTRIBUTING.md](CONTRIBUTING.md)。简而言之：复刻仓库、创建分支、运行 `cargo check` 与 `cargo test`（clippy、fmt 仅为建议），然后以约定式提交信息打开一个 Pull Request。

## Support Us / 支持我们

If WhiteBox-Forge is useful to you, consider:

如果 WhiteBox-Forge 对你有用，可以考虑：

- Starring the repository and telling others about it / 给仓库点个 Star 并推荐给他人
- Opening issues for bugs and feature ideas / 提交 issue 反馈 Bug 与功能想法
- Contributing code or documentation / 贡献代码或文档
- Sponsoring development (details coming soon) / 赞助开发（详情即将推出）

## License / 许可证

Licensed under the [Apache License 2.0](LICENSE).

基于 [Apache License 2.0](LICENSE) 许可。

## Based on / 基于

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-light.svg#gh-light-mode-only)](https://slint.dev)
[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-dark.svg#gh-dark-mode-only)](https://slint.dev)

Powered by [Wasmtime](https://wasmtime.dev/) and [Wasmtime-WASI](https://github.com/bytecodealliance/wasmtime-wasi).

由 [Wasmtime](https://wasmtime.dev/) 与 [Wasmtime-WASI](https://github.com/bytecodealliance/wasmtime-wasi) 驱动。

[Slint]: https://slint.dev
[Wasmtime]: https://wasmtime.dev
