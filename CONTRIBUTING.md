# Welcome! We welcome all contributions: bug reports, feature requests, documentation, and code

欢迎！我们欢迎各种贡献：错误报告、功能请求、文档和代码。

Code of Conduct: We follow the Contributor Covenant 2.0. Be respectful and inclusive.

行为准则：我们遵守贡献者公约 2.0。请保持尊重和包容。

Quick Start:

1. Fork the repo.
2. Create a branch: git checkout -b feat/your-feature
3. Run checks: cargo check && cargo test --all-features (clippy/fmt are advisory, not gates)
4. Commit with Conventional Commits: feat:, fix:, docs:, refactor:, test:, chore:
5. Push and open a Pull Request to main.

快速开始：

1. 复刻仓库。
2. 创建分支：git checkout -b feat/你的功能
3. 运行检查：cargo check && cargo test --all-features（clippy/fmt 仅建议，不设门禁）
4. 使用约定式提交：feat:、fix:、docs:、refactor:、test:、chore:
5. 推送并打开一个指向 main 分支的拉取请求。

Code Rules:

- cargo check must pass (compile errors). clippy warnings and fmt are advisory, not gated.
- No unsafe code unless justified with comments.
- No unwrap() / panic!() in library code – use Result.
- Public APIs must have doc comments (///) with examples.

代码规则：

- 必须通过 cargo check（编译错误）。clippy 警告与 fmt 只是建议，不作门禁。
- 除非有充分的注释说明，否则不允许 unsafe 代码。
- 库代码中禁止使用 unwrap() 或 panic!()，请使用 Result。
- 公共 API 必须有文档注释（///）并附带示例。

License: Apache 2.0 (see LICENSE file in the repository).

许可证：Apache 2.0（详见仓库中的 LICENSE 文件）

## 构建环境 / Build environment

### Windows：MSVC 14.52 下 zstd-sys 等 C 依赖编译失败（D8050）

- 现象：`cargo check/build` 编译 `zstd-sys`（wasmtime `cache` 特性引入）时，`cl.exe -Brepro` 报
  `命令行 error D8050：无法执行 c1.dll：未能将命令行放入调试记录中`，且**每个文件**都失败。
  裸 `cl.exe -Brepro` 手动重放同一命令成功，`-j1` 无效——是 VS 18 / MSVC 14.52 工具链与
  cc-rs 的 `/Brepro` 调用形态的交互缺陷，与仓库代码无关。
- 解决：安装 Visual Studio 的 **C++ Clang tools for Windows** 组件（`VC\Tools\Llvm`），
  每次构建前把 C/C++ 编译器指到 `clang-cl.exe`（Rust 链接仍是默认 MSVC）：

  ```powershell
  # 自动定位 VS 的 LLVM 组件，无需硬编码版本路径
  $llvm = Get-ChildItem "C:\Program Files\Microsoft Visual Studio" -Recurse -Filter clang-cl.exe -ErrorAction SilentlyContinue |
          Where-Object { $_.FullName -match '\\VC\\Tools\\Llvm\\[^\\]+\\bin\\' } |
          Select-Object -First 1 -ExpandProperty FullName
  $env:CC_x86_64_pc_windows_msvc = $llvm
  $env:CXX_x86_64_pc_windows_msvc = $llvm
  cargo check --workspace --all-targets
  ```

  该环境变量只影响 cc-rs 构建的 C/C++ 依赖（zstd-sys 等），Rust 代码无影响；
  CI（ubuntu/macos）不需要。

- 相关本地怪癖：本工作站的测试二进制进程对 `%TEMP%` 写入被环境（AV/宿主沙箱）拒绝，
  单元测试用 `CARGO_TARGET_TMPDIR` 代替 `std::env::temp_dir()`；详见 `memory.md` 踩坑记录 10/11。
