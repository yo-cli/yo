# Linux - yo-git

```bash
# Build
cargo build --release --bin yo-git --no-default-features

# Install
sudo cp target/release/yo-git /usr/local/bin/yo
```


# Windows - yo-auto (Git Bash)

```bash
export CARGO_TARGET_DIR="C:/Users/DEV/.cargo-target"

# Build
cargo build --release --bin yo-auto

# Install
cp C:/Users/DEV/.cargo-target/release/yo-auto.exe ~/.cargo/bin/yo.exe
```
