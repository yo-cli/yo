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
