Release yo-cli 仓库。使用 dev → main 合并流程，编译后同步到 GitBash。

Execute these steps in order. If any step fails, stop immediately and report the error.

## 版本 & 提交

1. **Patch bump**: Bump `Cargo.toml` 中的 patch 版本号（如 1.1.41 → 1.1.42）。
2. **Commit pending changes**: Stage all (`git add -A`) and commit with message `release: vX.Y.Z`（使用 bump 后的版本号）。
3. **Push**: Push dev branch to origin (`git push origin dev`).
4. **Merge to main**: Run `git checkout main && git pull origin main && git merge dev && git push origin main && git checkout dev`.

## 编译 & 同步

5. **Build release**: `cargo build --release --bin yo-git --target x86_64-pc-windows-gnu`
6. **同步到 GitBash**: `cp target/x86_64-pc-windows-gnu/release/yo-git.exe /mnt/c/Users/DEV/.cargo/bin/yo.exe`
7. **验证**: 报告新版本号和文件大小。
