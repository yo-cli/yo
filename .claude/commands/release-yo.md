Release yo-cli 仓库。使用 dev → main 合并流程，编译后同步到 GitBash。

Execute these steps in order. If any step fails, stop immediately and report the error.

## 版本 & 提交

1. **Patch bump**: Bump `Cargo.toml` 中的 patch 版本号（如 1.1.41 → 1.1.42）。
2. **Commit pending changes**: Stage all (`git add -A`) and commit with message `release: vX.Y.Z`（使用 bump 后的版本号）。
3. **Push**: Push dev branch to origin (`git push origin dev`).
4. **Merge to main**: Run `git checkout main && git pull origin main && git merge dev && git push origin main && git checkout dev`.

## 编译 & 同步

5. **Build release**: `cargo build --release --bin yo-git --target x86_64-pc-windows-gnu`
6. **同步到 GitBash**: `mkdir -p /mnt/c/Users/DEV/bin && cp target/x86_64-pc-windows-gnu/release/yo-git.exe /mnt/c/Users/DEV/bin/yo.exe`（Git Bash 的 `/etc/profile.d/env.sh` 无条件把 `~/bin` 放 PATH 最前）
7. **上传 exe 到 GitHub Release**: CI 只产 Linux 产物，Windows exe 由本地补传。先等 CI 建好 stable release（跟踪 dev 分支最新 run 直到结束：`gh run watch $(gh run list --repo yo-cli/yo --branch dev --limit 1 --json databaseId --jq '.[0].databaseId') --repo yo-cli/yo --exit-status`；若 10 分钟后 `gh release view vX.Y.Z --repo yo-cli/yo` 仍不存在，停止并提示检查 CI），然后上传：
   `GH_TOKEN=$(gh auth token --user eflogic) gh release upload vX.Y.Z target/x86_64-pc-windows-gnu/release/yo-git.exe --clobber --repo yo-cli/yo`
   （必须用 eflogic 账号——它对 yo-cli/yo 有写权限，okrxyz 只读。）
8. **验证**: 报告新版本号、文件大小和 exe 上传结果。
