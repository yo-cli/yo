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

4. **`auto/`** - Task scheduler subsystem (Rhai-based)
   - `rhai/` - Rhai scripting engine
     - `engine.rs` - Rhai engine initialization, API registration, script loading
     - `scheduler.rs` - Main scheduler with 30s polling loop
     - `api.rs` - Exposed Rhai APIs (speak, lock_screen, shutdown, etc.)
     - `types.rs` - Rule, Trigger, GlobalState definitions
     - `default_rules.rs` - Default script templates
   - `screen/` - Lockscreen monitoring
     - `monitor.rs` - Windows session state tracking via WTS APIs
   - `startup/` - Windows autostart management
     - `manager.rs` - VBS script creation in Startup folder
   - `tts/` - Text-to-speech
     - `client.rs` - Volcengine TTS API client
     - `player.rs` - Audio playback via rodio
   - `state/` - State management
     - `instance_lock.rs` - Single instance enforcement via PID file
   - `web/` - Web UI server
     - `server.rs` - Axum-based REST API and static file serving
     - `types.rs` - WebState definition
   - `ui/` - Web UI frontend
     - `web_ui.html` - Vue 3 Composition API frontend with script editor

5. **`common/`** - Shared utilities
   - `crypto_utils.rs` - AES-256-CBC encryption using machine-specific MAC address as key derivation input

### Key Design Patterns

**Encryption Strategy**:
- GitHub tokens are encrypted using AES-256-CBC
- Key derivation: SHA-256(MAC_address + SALT)
- Stored in `~/.yo/github_token.enc`

**Rhai Scripting Engine**:
- Scripts are stored in `~/.yo/rules/*.rhai`
- Each script defines a `trigger` configuration and event handlers
- Trigger options: `time_range`, `interval_minutes`, `events`, `weekdays`, `enabled`
- Event types: `tick` (30s interval), `lock`, `unlock`
- Available APIs:
  - `speak(text)` - TTS playback
  - `lock_screen()` - Lock workstation
  - `shutdown(delay_secs)` - Delayed shutdown
  - `chime(hour)` - Hourly chime playback
  - `current_hour()`, `current_minute()` - Time utilities
  - `is_weekend()`, `is_workday()` - Day type checks
  - `get_counter(name)`, `set_counter(name, val)` - Persistent counters
  - `get_flag(name)`, `set_flag(name, val)` - Persistent flags
  - `log(msg)` - Console logging

**Task Scheduler Architecture**:
- Persistent process (does NOT exit after launch)
- 30-second polling loop triggers `on_tick` for rules with matching time/weekday
- Lock/unlock events trigger `on_lock` and `on_unlock` handlers
- Windows: Uses `WTSRegisterSessionNotification` for lockscreen detection
- Web UI: Axum server with script editing capability
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

- `~/.yo/rules/*.rhai` - Rhai script rules (created on first run with default examples)
- `~/.yo/yo-auto.pid` - PID file for single instance lock (auto command)
- `~/.yo/github_token.enc` - Encrypted GitHub personal access token
- `~/.ssh/config` - Modified by `init` command to add deploy key aliases
- `voice/` directory - Generated TTS audio files
- `%APPDATA%\...\Startup\yo-auto-web.vbs` - Windows autostart script (when installed)

## Rhai Script Example

```rhai
// ~/.yo/rules/night_lockscreen.rhai

// Trigger configuration
let trigger = #{
    time_range: ["21:30", "05:00"],
    interval_minutes: 5,
    events: ["tick"],
    weekdays: [1, 2, 3, 4, 5, 6, 7],  // All days
    enabled: true,
};

// Called every 30 seconds when in time range
fn on_tick() {
    speak("Time to rest");
    lock_screen();
}

// Called when screen is unlocked
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

When writing tests:
- Unit tests go in same file as `#[cfg(test)] mod tests`
- Integration tests for GitHub API should mock HTTP responses
- Crypto tests should verify encrypt/decrypt round-trips
- Task scheduler tests should use fixed time mocks (avoid sleep-based tests)

## Platform-Specific Code

**Windows-specific**:
- `Cargo.toml` includes `windows` crate with session monitoring features
- `screen/monitor.rs` uses Win32 APIs for session state
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
- **Rhai scripting**: Task logic is defined in `.rhai` scripts, enabling runtime customization without recompilation
- **Async runtime**: Tokio is used for async operations, primarily for Web UI
