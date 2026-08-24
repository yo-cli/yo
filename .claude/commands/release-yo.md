Release yo-cli 仓库。只推 dev，dev → main 合并与打 tag 全部由 CI 完成，本地负责编译 Windows exe 并补传。

Execute these steps in order. If any step fails, stop immediately and report the error.

> **绝对不要手动合并 dev 到 main。** CI 的 `check-version` 是拿 dev 的 `Cargo.toml` 版本号跟 `origin/main` 比，不一致才跑 `release-stable`（`.github/workflows/release.yml`）。手动先把 main 推上去，CI 就会判定「版本没变」跳过整个 `release-stable` —— dev→main 合并、打 tag、建 stable release 全都不会发生，只剩一个 `-dev` 预发布。`release-stable` 自己就会做合并和打 tag，本地插手只会破坏它。

## 版本 & 提交

1. **Patch bump**: Bump `Cargo.toml` 中的 patch 版本号（如 1.1.41 → 1.1.42）。
2. **Cross-target 编译检查**（bump 后、commit 前）：`cargo check --all-targets && cargo build --release --bin yo-git --target x86_64-pc-windows-gnu`。**Linux 编译通过不代表 Windows 能过** —— `yo_lib` 里任何 unix-only 依赖（如 `nix`）被无条件引用，都只会在 Windows target 上炸，而 yo-git.exe 正是从这里编出来的。unix-only 代码必须按 `src/ob/system.rs` 的 `#[cfg(unix)]` / `#[cfg(not(unix))]` 配对写。
3. **Commit pending changes**: Stage all (`git add -A`) and commit with message `release: vX.Y.Z`（使用 bump 后的版本号）。
4. **Push**: Push dev branch to origin (`git push origin dev`)。到此本地的推送就结束了，不要碰 main。

## 编译 & 同步

5. **同步到 GitBash**: `mkdir -p /mnt/c/Users/DEV/bin && cp target/x86_64-pc-windows-gnu/release/yo-git.exe /mnt/c/Users/DEV/bin/yo.exe`（第 2 步已经编好了；Git Bash 的 `/etc/profile.d/env.sh` 无条件把 `~/bin` 放 PATH 最前）
6. **上传 exe 到 GitHub Release**: CI 只产 Linux 产物，Windows exe 由本地补传。先等 CI 建好 stable release（跟踪 dev 分支最新 run 直到结束：`gh run watch $(gh run list --repo yo-cli/yo --branch dev --limit 1 --json databaseId --jq '.[0].databaseId') --repo yo-cli/yo --exit-status`；若 10 分钟后 `gh release view vX.Y.Z --repo yo-cli/yo` 仍不存在，停止并提示检查 CI 的 `release-stable` 是否被跳过），然后上传：
   `GH_TOKEN=$(gh auth token --user eflogic) gh release upload vX.Y.Z target/x86_64-pc-windows-gnu/release/yo-git.exe --clobber --repo yo-cli/yo`
   （必须用 eflogic 账号——它对 yo-cli/yo 有写权限，okrxyz 只读。）
7. **验证**: 确认 stable release 的 6 个产物（yo-git / yo-s5 / yo-file / yo-ob / yo-s3 / yo-git.exe）齐全，报告新版本号、文件大小和 exe 上传结果。本地 main 分支会落后于远端（CI 在远端合的），需要时 `git fetch origin main` 即可。
