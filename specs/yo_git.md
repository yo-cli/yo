# yo-git 产品需求规格

> 只写代码说不出来的东西：定位、设计取舍、做不到的。命令行为看 `yo-git --help` 与 `src/commands/github_init.rs`。

---

## 1. 产品定位

**yo-git 是一个 GitHub 仓库访问初始化工具。**

目标用户是需要频繁在新机器或新环境上配置 GitHub 仓库访问权限的开发者。传统流程（生成 SSH 密钥、添加 Deploy Key、配置 SSH Config、或者手动管理 HTTPS Token）步骤繁琐且容易出错。yo-git 把这些步骤压缩为一条命令。

核心理念：**一条命令，仓库就能 clone。**

---

## 2. 设计决策

| 决策 | 理由 |
|------|------|
| 按仓库隔离 SSH 密钥 | 每个仓库独立密钥对，吊销某个仓库的访问不影响其他仓库 |
| SSH Config Host 别名 | `github.com.{user}.{repo}` 格式避免多密钥冲突，Git 原生支持 |
| AES-256 加密 Token | 比明文安全，比系统密钥链跨平台性更好 |
| Deploy Key 而非账户级 SSH Key | 最小权限原则，Deploy Key 只作用于单个仓库 |
| `credential.useHttpPath=true` | HTTPS 模式下区分不同仓库的 Token，避免串用 |
| 自动备份 SSH Config | 修改 `~/.ssh/config` 前先备份，防止配置被破坏 |

---

## 3. 当前已知限制

- **Token 需要手动创建：** 用户仍需在 GitHub 网页上创建 Personal Access Token，yo-git 不做 OAuth 流程
- **Deploy Key 带写权限：** 以 `read_only: false` 添加，能推送；只作用于该仓库，收回靠在仓库设置里删掉这把 key
- **加密密钥固定：** AES 密钥由固定 salt 派生，安全性依赖于本地文件系统访问控制
