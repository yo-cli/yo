# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`yo` is a multi-functional CLI tool suite written in Rust (edition 2021). It consists of 4 independent binaries sharing a common library (`yo_lib`):

- **yo-git** — GitHub SSH key management (Linux)
- **yo-file** — File utilities and template cloning
- **yo-s5** — SOCKS5 proxy service via Docker (Linux)
- **yo-ob** — OceanBase environment preparation (Linux)

Product specs for each tool live in `specs/`.

## Build Commands

```bash
# Build individual binaries
cargo build --release --bin yo-git
cargo build --release --bin yo-file
cargo build --release --bin yo-s5
cargo build --release --bin yo-ob

# Check all code compiles
cargo check

# Run a specific binary during development
cargo run --bin yo-git
cargo run --bin yo-file
```

There are no tests or linting configured in this project. The release profile uses `opt-level = 3`, `lto = true`, `strip = true`.

## Architecture

```
src/
├── lib.rs              # Shared library re-exporting all modules
├── bin/                # Binary entry points (thin wrappers calling into commands/)
├── commands/           # Core command logic for each binary
├── github/             # GitHub API client, SSH key gen, encrypted token storage
├── ob/                 # OceanBase commands and config
├── s5/                 # SOCKS5 Docker/proxy management
└── common/             # Shared crypto utilities (AES-256-CBC)
```

## Key Patterns

- **Binary → Command delegation**: Each `src/bin/*.rs` parses CLI args (Clap derive) then delegates to a struct in `src/commands/`.
- **Pure Rust crypto**: No OpenSSL dependency — uses `aes`/`cbc`/`sha2` crates. Reqwest uses `rustls`.
- **Config/state storage**: Encrypted GitHub tokens at `~/.yo/github/{username}/token`.
- **Colored terminal output**: Uses the `colored` crate with consistent symbols (✓ green, ✗ red, ⚠ yellow, ℹ blue, 📊 cyan).
- **Async runtime**: Tokio with full features throughout. Reqwest for HTTP.

## Git Workflow

- **Main branch**: `main`
- **Development branch**: `dev`

## 开发原则

### 问题解决

- **Root Cause 优先**：永远从根源修复问题（修改工具/框架代码），不逐个修补项目文件。不用临时方案绕过问题，不要表层最快方案假装解决
- **三思而后行**：方案选择时要搜索、对比，选择最优最佳实践，不要第一个能跑的方案就用
- **技术结论必须验证**：说"X 方案有 Y 缺点"之前，先确认 Y 是否真实存在。不确定就说不确定，不要编一个听起来合理的理由。用模糊的"复杂度"当借口回避工作 = 偷懒
- **用 log 调试难题**：遇到难以定位的问题，添加调试日志输出定位根因，不要靠猜测反复尝试
- **逐步确认不想当然**：每一步修复后都要验证结果，不要假设已经修好就继续下一步

### 代码质量

- **单文件不要过大**：单个源文件超过 500 行时应考虑拆分。过大的文件难以理解、难以 review、增量编译效率低
- **最小改动**：只做直接请求的改动，不加无关的重构、注释、docstring
- **不过度工程**：不加 feature flag、不设计假想需求、不为一次性操作创建抽象
- **降低心智负担**：API/配置/命令设计要让用户零思考即可使用，合理默认值、自动推断、约定大于配置
- **删除即删除**：废弃代码直接删掉，不留 `// removed` 注释或 `_unused` 变量
- **先读后改**：修改代码前必须先读懂现有代码，不凭印象写代码
- **全面深度阅读**：读文档/代码时要完整深入，不要只看开头几行就下结论，避免遗漏关键细节
- **融入项目风格**：成熟项目中做迭代时，先观察项目现有的代码风格、命名惯例、架构模式，保持一致

### 安全与规范

- **不引入安全漏洞**：注意命令注入、XSS、SQL 注入等 OWASP Top 10
- **不跳过检查**：不用 `--no-verify`、`--force` 等跳过安全检查的参数
- **只验证边界**：只在系统边界（用户输入、外部 API）做验证，内部代码信任框架保证
- **凭据先校验、后持久化**：绝不在确认有效前把凭据落盘。坏凭据（粘错的 token / 输错的密码）一旦提前写入，会被后续运行静默复用，造成"一次输错、之后永久失败"且用户无感知的陷阱。做法：先调校验接口（如 GitHub `/user`）确认有效再写文件，校验不过则不落盘、下次自动重新提示；任何凭据相关失败（鉴权/权限/404）都打印凭据存储路径 + 可直接复制的清除/更换命令（`rm <path> && <重跑命令>`），并带出"当前认证身份"等线索，让用户一眼分辨是凭据存错还是目标写错

### 工作方式

- **先搜索最佳实践**：动手前先搜索业界最佳实践和成熟方案，不闭门造车
- **方案对比决策**：有多个方案时，列表对比各方案优缺点，选出最佳方案再动手
- **先搜索再造轮子**：GitHub、npm/PyPI/crates.io 搜索现有实现，优先采用成熟方案
- **Spec 先行**：新功能必须先写 spec，讨论充分后再实现
- **逐条实现**：对照 spec 实现时，列出 checklist 逐条完成，不凭印象跳步
- **问题分类逐个击破**：处理多个问题时，先将问题归类列表，然后逐个解决并确认，不混在一起处理
- **优先使用项目 skill**：操作其他项目时，先检查该项目下是否有可用的 slash command / skill，优先使用而非手动操作
- **主动发现反模式并提案新原则**：发现自己或代码中违反应成为原则的模式时，主动指出问题、提炼为一句原则、提醒用户确认后加入 init-principle 持久化
- **memory 写入时询问持久化**：写入 memory 时，如果内容有普适价值，询问用户是否同时写入 init-principle。memory 会丢，principle 存 GitHub 永不丢
