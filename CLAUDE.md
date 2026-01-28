# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Yo** is a multi-functional command-line tool written in pure Rust that provides:
- GitHub SSH key management (automatic deploy key setup)
- SOCKS5 proxy service (Docker + GOST based)
- Task scheduler with Rhai scripting, lockscreen automation and TTS reminders
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

# Debug build and run
cargo run -- --version
cargo run -- run auto
```

## Running the Application

```bash
# GitHub SSH key setup
yo init @username/repository

# SOCKS5 proxy (automatic / interactive mode)
yo run s5
yo run s5 -i

# Task scheduler (persistent service)
yo run auto

# Task scheduler with Web UI (default port 9999 / custom port)
yo run auto --web
yo run auto --web 8080

# Windows autostart management
yo run auto --web --autostart           # Install autostart
yo run auto --web --autostart remove    # Remove autostart
yo run auto --web --autostart status    # Show status

# Template cloning with keyword replacement
yo run clone

# Test hourly chime / Volcengine TTS
yo run test
yo run ve
```

## Architecture

### Module Structure

The codebase follows a domain-driven structure with 5 main modules under `src/`:

1. **`commands/`** - Command entry points that orchestrate business logic (one file per command)
2. **`github/`** - GitHub integration: encrypted token storage, Ed25519 key generation, REST API client
3. **`s5/`** - SOCKS5 proxy: Docker management, GOST proxy lifecycle, network utilities
4. **`auto/`** - Task scheduler subsystem (largest module), containing:
   - `rhai/` - Scripting engine, scheduler loop, API registration, rule types, time indexing
   - `config/` - GlobalConfig for environment variable management (`~/.yo/config.json`)
   - `screen/` - Lockscreen monitoring (Windows WTS APIs, cross-platform stubs)
   - `startup/` - Windows autostart via VBS script in Startup folder
   - `tts/` - Volcengine TTS client and rodio audio playback
   - `state/` - Single instance enforcement via PID file
   - `web/` - Axum-based REST API + static file serving for Web UI
   - `ui/` - Vue 3 Composition API frontend (`web_ui.html`)
5. **`common/`** - Shared utilities (AES-256-CBC encryption with MAC-address-derived key)

### Key Design Patterns

**CLI Dispatch**: Manual argument parsing in `main.rs` (no clap/structopt). Pattern matching on positional args dispatches to command structs. All commands return `Result` with errors displayed in bold red.

**Encryption Strategy**:
- GitHub tokens encrypted with AES-256-CBC
- Key derivation: SHA-256(MAC_address + SALT)
- Stored in `~/.yo/github_token.enc`

**Rhai Scripting Engine**:
- Scripts stored in `~/.yo/rules/*.rhai`
- Each script defines a `trigger` map and event handler functions
- Trigger options: `time_range`, `interval_minutes`, `events`, `weekdays`, `enabled`
- Script lifecycle functions: `on_mount()`, `on_tick()`, `on_lock()`, `on_unlock()`, `on_destroy()`
- Event types: `tick` (30s interval), `lock`, `unlock`
- Available Rhai APIs:
  - **Time**: `hour()`, `minute()`, `second()`, `weekday()`, `time_str()`, `date_str()`, `in_time_range(start, end)`, `is_weekend()`, `is_workday()`
  - **Actions**: `speak(text)`, `lock_screen()`, `shutdown(delay_secs)`, `chime(hour)`, `log(msg)`
  - **Screen**: `screen_locked()` - check if screen is currently locked
  - **TTS config**: `configure_tts(api_key, voice)`
  - **Environment**: `get_env(name)`, `has_env(name)` - read from GlobalConfig
  - **Calendar**: `generate_script_events(script_name)` - simulate and persist events to `events.json`

**Task Scheduler Architecture**:
- Persistent process (does NOT exit after launch)
- 30-second polling loop triggers `on_tick` for rules with matching time/weekday
- TimeIndex (`rhai/index.rs`) provides efficient time-based rule lookups
- Lock/unlock events trigger `on_lock` and `on_unlock` handlers
- Windows: Uses `WTSRegisterSessionNotification` for lockscreen detection
- Single instance: PID-based lock prevents multiple scheduler instances

**Template Cloning Flow** (`clone_command.rs`):
1. Interactive prompts for source directory and keywords
2. Parse keywords in any format (kebab-case, snake_case, PascalCase, camelCase)
3. Generate all variants: kebab-case, snake_case, PascalCase, camelCase, SCREAMING_SNAKE
4. Walk directory tree and replace in both filenames and file contents

**SOCKS5 Proxy Flow**:
1. Check Docker installation (auto-install if missing)
2. Pull `ginuerzh/gost` image
3. Generate random port (30000-40000) + password (20 chars)
4. Start container with SOCKS5 listener
5. Validate connectivity via `socket2` probe

**Error Handling**:
- Uses `thiserror` for structured error types per module
- Commands return `anyhow::Result<()>` for flexibility
- Errors propagate to `main.rs` for user-friendly display

## Configuration Files

- `~/.yo/rules/*.rhai` - Rhai script rules (default examples created on first run)
- `~/.yo/config.json` - Global environment variables for Rhai scripts
- `~/.yo/events.json` - Generated calendar events from script simulation
- `~/.yo/yo-auto.pid` - PID file for single instance lock
- `~/.yo/github_token.enc` - Encrypted GitHub personal access token
- `~/.ssh/config` - Modified by `init` command to add deploy key aliases
- `voice/` directory - Generated TTS audio files
- `%APPDATA%\...\Startup\yo-auto-web.vbs` - Windows autostart script

## Rhai Script Example

```rhai
// ~/.yo/rules/night_lockscreen.rhai

let trigger = #{
    time_range: ["21:30", "05:00"],
    interval_minutes: 5,
    events: ["tick"],
    weekdays: [1, 2, 3, 4, 5, 6, 7],
    enabled: true,
};

fn on_tick() {
    speak("Time to rest");
    lock_screen();
}

fn on_unlock() {
    let count = get_counter("unlock_count") + 1;
    set_counter("unlock_count", count);

    if count >= 3 {
        speak("Maximum unlocks reached, shutting down");
        shutdown(30);
    } else {
        speak(`Unlock ${count} of 3`);
    }
}
```

## Testing Strategy

- Unit tests go in same file as `#[cfg(test)] mod tests`
- Integration tests for GitHub API should mock HTTP responses
- Crypto tests should verify encrypt/decrypt round-trips
- Task scheduler tests should use fixed time mocks (avoid sleep-based tests)
- Some TTS tests are marked `#[ignore]` since they require API keys

## Platform-Specific Code

All platform-specific code uses `#[cfg(target_os = "...")]` conditional compilation:

- **Windows**: `windows` crate for WTS session monitoring, `rundll32.exe` for lockscreen, registry queries for Git Bash detection, `tasklist` for PID checks
- **Linux**: `/proc/{pid}` for process checks, `loginctl lock-session` for lockscreen, `/sys/class/net/` for MAC address
- **macOS**: `pmset displaysleepnow` for lockscreen

## Important Notes

- **Pure Rust crypto**: Uses `aes` + `cbc` crates (NOT OpenSSL) for portability
- **reqwest**: Configured with `rustls-tls` (not native-tls) to avoid OpenSSL dependency
- **Release profile**: Aggressive optimization with `opt-level = 3`, LTO and symbol stripping
- **Task scheduler**: Designed to run indefinitely - do NOT add auto-exit logic
- **SSH keys**: Always generates Ed25519 (not RSA)
- **Colored output**: Extensive use of `colored` crate (green=success, red=error, yellow=warning, blue=info, cyan=action)
- **Interactive prompts**: Uses `inquire` crate for user input dialogs
- **Async runtime**: Tokio used for Web UI server; most other code is synchronous
- **Concurrency**: `Arc<Mutex<T>>` for shared state in scheduler, `lazy_static` for globals
