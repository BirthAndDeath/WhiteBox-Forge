# 用户配置 / User configuration

运行时从 `dir::config_dir()` 读取以下四个 JSON 文件（便携 build 为 `<exe_dir>/config`，
否则为系统用户配置目录）。**本目录只是模板**：进程启动时不读仓库内文件，请把需要的
文件复制到上述用户目录后再修改。

At runtime the process reads these four JSON files from `dir::config_dir()`
(`<exe_dir>/config` in the portable build, otherwise the system per-user config dir).
**This directory only ships templates**: the process never reads repo files at startup;
copy the files you need into that user directory before editing.

| 文件 | 用途 |
|---|---|
| `run.json`    | 运行/等待行为：默认执行上界、线程放弃宽限、有界等待轮询间隔 |
| `limits.json` | 沙箱资源上限（对应 `SandboxConfig` 的 `max_*`，`null` = 不设限） |
| `logging.json`| worker 日志：单文件上限（字节）、按 UTC 天数保留期 |
| `caching.json`| wasmtime 编译缓存开关与目录覆盖 |

## 语义 / Semantics

- 文件缺失 → 用内置默认（与模板中的值一致）。
- 字段缺失 → 该字段用默认（`#[serde(default)]`），无需整节齐全。
- 文件存在但非法 JSON / 拼错字段名 / 类型错误 → **拒绝启动**（fail-closed），不静默回退。
- `limits.json` 只在沙箱模式生效；`--no-sandbox` 明确要求不套限制时忽略。
- 优先级：内置默认 < JSON 文件 < CLI 显式参数 < 库 API 显式赋值。
- 安全授出（开网络 / 解封文件系统 / 预打开）不在 JSON 中——stay in code / CLI（deny-by-default）。

## 值域约束 / Value bounds（越界 → 拒绝启动）

- `run.poll_ms`：`1..=10000`，且**不得大于** `run.shutdown_grace_ms`（轮询先睡满一个间隔才判宽限，poll > grace 会让有界等待超跑）。
- `run.shutdown_grace_ms`：`> 0`。
- `logging.per_file_bytes`：`>= 4096`（过小会在每条日志上触发轮换，造成文件/目录膨胀）。
- `logging.retention_days`：`1..=365`。
- `caching.directory`：若设置必须是绝对路径；该目录仍会经 `protect_dir` 收紧为属主专用。