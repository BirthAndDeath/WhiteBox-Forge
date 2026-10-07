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
