# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Yo** is a multi-functional command-line tool written in pure Rust that provides:
- GitHub SSH key management (automatic deploy key setup)
- SOCKS5 proxy service (Docker + GOST based)
- Task scheduler with lockscreen automation and TTS reminders
- Template cloning with keyword replacement

The tool is designed for Windows/Linux/macOS cross-platform support with a focus on security (AES-256-CBC encryption for sensitive data).

## Build Commands

```bash
# Development build
cargo build

# Release build (optimized with LTO and stripped)
cargo build --release

# Install locally to ~/.cargo/bin/yo
cargo install --path .

# Run tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test module
cargo test scheduler_tests
cargo test crypto_utils

# Lint
cargo clippy

# Format
cargo fmt

# Check without building
cargo check

# Generate and open documentation
cargo doc --open

# Debug build and run (useful for development)
cargo run -- --version
cargo run -- run auto
```

## Running the Application

```bash
# GitHub SSH key setup
yo init @username/repository

# SOCKS5 proxy (automatic mode)
yo run s5

# SOCKS5 proxy (interactive mode)
yo run s5 -i

# Task scheduler (runs as persistent service)
yo run auto

# Task scheduler with Web UI (default port 9999)
yo run auto --web

# Task scheduler with Web UI on custom port
yo run auto --web 8080

# Windows autostart management (adds VBS script to Startup folder)
yo run auto --web --autostart           # Install autostart and start Web UI
yo run auto --web --autostart remove    # Remove autostart
yo run auto --web --autostart status    # Show autostart status

# Template cloning with keyword replacement
yo run clone

# Test hourly chime playback
yo run test

# Test Volcengine TTS synthesis and playback
yo run ve

# Version info
yo --version
```

## Architecture

### Module Structure

The codebase follows a domain-driven structure with 5 main modules:

1. **`commands/`** - Command entry points that orchestrate business logic
   - `github_init.rs` - SSH key generation and GitHub API interaction
   - `s5_command.rs` - SOCKS5 proxy setup orchestration
   - `auto_command.rs` - Task scheduler entry point
   - `clone_command.rs` - Template cloning workflow
   - `test_command.rs` - Hourly chime playback test
   - `ve_command.rs` - Volcengine TTS test

2. **`github/`** - GitHub integration layer
   - `token_manager.rs` - Encrypted storage/retrieval of GitHub tokens
   - `ssh_key_manager.rs` - Ed25519 key generation and SSH config management
   - `api_client.rs` - GitHub REST API client (deploy keys, etc.)

3. **`s5/`** - SOCKS5 proxy management
   - `docker_manager.rs` - Docker installation detection and container management
   - `proxy_manager.rs` - GOST proxy configuration and lifecycle
   - `network_utils.rs` - Port availability checking and network testing

4. **`auto/`** - Task scheduler subsystem
   - `scheduler.rs` - Main event loop (30s polling interval)
   - `scheduler_async.rs` - Async version with Web UI support
   - `config.rs` - Task configuration deserialization (`~/.yo/auto_config.json`)
   - `task_executor.rs` - Task type dispatcher (lockscreen/command/tts)
   - `task_executor_async.rs` - Async version for Web UI integration
   - `lockscreen_monitor.rs` - Windows-specific session state tracking
   - `lockscreen_state.rs` - Cross-platform lockscreen state management
   - `tts.rs` - Volcengine TTS API integration
   - `tts_async.rs` - Async version for Web UI integration
   - `shared_state.rs` - Thread-safe state sharing between scheduler and web server
   - `web_server.rs` - Axum-based web server for task management UI
   - `autostart.rs` - Windows autostart management (VBS script in Startup folder)
   - `instance_lock.rs` - Single instance enforcement via PID file lock

5. **`common/`** - Shared utilities
   - `crypto_utils.rs` - AES-256-CBC encryption using machine-specific MAC address as key derivation input

### Key Design Patterns

**Encryption Strategy**:
- GitHub tokens are encrypted using AES-256-CBC
- Key derivation: SHA-256(MAC_address + SALT)
- Stored in `~/.yo/github_token.enc`

**Task Scheduler Architecture**:
- Persistent process (does NOT exit after launch)
- 30-second polling loop checks all enabled tasks
- Supports time range crossing midnight (e.g., 22:00-06:00)
- Task types: `lockscreen_repeated`, `command`, `tts_command`, `adaptive_lockscreen`, `hourly_chime`
- Adaptive lockscreen: dynamically adjusts interval based on user unlock behavior
- Optional `max_unlocks` + `shutdown_on_exceed` for forced shutdown after repeated unlocks
- Windows: Uses `WTSRegisterSessionNotification` for lockscreen detection
- Hourly config reload at minute 0 for dynamic updates
- State persistence via `~/.yo/state_{task_name}.json` for adaptive tasks
- Web UI: Optional Axum-based server for real-time task monitoring and control
- Single instance: PID-based lock prevents multiple scheduler instances

**Windows Autostart**:
- Creates VBS script in `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup`
- Script launches Git Bash with `yo run auto --web` command
- Auto-detects Git Bash via registry, common paths, or PATH environment

**Template Cloning Flow** (`clone_command.rs`):
1. Interactive prompts for source directory and keywords
2. Parse keywords in any format (kebab-case, snake_case, PascalCase, camelCase)
3. Generate all variants: kebab-case, snake_case, PascalCase, camelCase, SCREAMING_SNAKE
4. Walk directory tree and replace in both filenames and file contents
5. Smart replacement using regex to preserve word boundaries

**SOCKS5 Proxy Flow**:
1. Check Docker installation → auto-install if missing
2. Pull `ginuerzh/gost` image
3. Generate random port + password
4. Start container with SOCKS5 listener
5. Validate connectivity via `socket2` probe

**Error Handling**:
- Uses `thiserror` for structured error types per module
- Commands return `anyhow::Result<()>` for flexibility
- All errors propagate to `main.rs` for user-friendly display

## Configuration Files

- `~/.yo/auto_config.json` - Task scheduler configuration (created on first run with default night_lockscreen task)
- `~/.yo/state_{task_name}.json` - Per-task state for adaptive_lockscreen tasks
- `~/.yo/yo-auto.pid` - PID file for single instance lock (auto command)
- `~/.yo/github_token.enc` - Encrypted GitHub personal access token
- `~/.ssh/config` - Modified by `init` command to add deploy key aliases
- `voice/` directory - Generated TTS audio files for tts_command tasks
- `%APPDATA%\...\Startup\yo-auto-web.vbs` - Windows autostart script (when installed)

## Testing Strategy

When writing tests:
- Unit tests go in same file as `#[cfg(test)] mod tests`
- Integration tests for GitHub API should mock HTTP responses
- Crypto tests should verify encrypt/decrypt round-trips
- Task scheduler tests should use fixed time mocks (avoid sleep-based tests)

## Platform-Specific Code

**Windows-specific**:
- `Cargo.toml` includes `windows` crate with session monitoring features
- `lockscreen_monitor.rs` uses Win32 APIs for session state
- Audio playback uses `rodio` crate

**Cross-platform lockscreen**:
- Linux: `loginctl lock-session` or `gnome-screensaver-command`
- macOS: `pmset displaysleepnow`
- Windows: `rundll32.exe user32.dll,LockWorkStation`

## Important Notes

- **Pure Rust crypto**: Uses `aes` + `cbc` crates (NOT OpenSSL) for portability
- **reqwest**: Configured with `rustls-tls` (not native-tls) to avoid OpenSSL dependency
- **Release profile**: Aggressive optimization with LTO and symbol stripping
- **Task scheduler**: Designed to run indefinitely - do NOT add auto-exit logic
- **SSH keys**: Always generates Ed25519 (not RSA) for better security and performance
- **Main entry point**: All commands are dispatched through `main.rs` using pattern matching on CLI args
- **Colored output**: Extensive use of `colored` crate for user feedback (green for success, red for errors, yellow for warnings, blue for info)
- **Interactive prompts**: Uses `inquire` crate for multi-select, text input, and confirmation dialogs
- **Task state management**: Adaptive lockscreen uses `Arc<Mutex<LockscreenState>>` for thread-safe state sharing between scheduler and monitor
- **Async runtime**: Tokio is used for async operations, primarily for Web UI and async TTS
- **Dual scheduler modes**: Sync version (`scheduler.rs`) for CLI-only, async version (`scheduler_async.rs`) for Web UI support
