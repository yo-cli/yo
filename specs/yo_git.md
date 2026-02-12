# yo-git 产品需求规格

> 本文档面向 AI 开发助手，描述 yo-git 的产品定位、核心场景和设计决策。

---

## 1. 产品定位

**yo-git 是一个 GitHub 仓库访问初始化工具。**

目标用户是需要频繁在新机器或新环境上配置 GitHub 仓库访问权限的开发者。传统流程（生成 SSH 密钥、添加 Deploy Key、配置 SSH Config、或者手动管理 HTTPS Token）步骤繁琐且容易出错。yo-git 把这些步骤压缩为一条命令。

核心理念：**一条命令，仓库就能 clone。**

---

## 2. 核心场景

### 2.1 SSH Deploy Key 方式初始化

**需求：** 用户拿到一台新机器，想通过 SSH 方式 clone 某个 GitHub 私有仓库，不想手动折腾密钥和配置。

**行为：**
- 用户运行 `yo init @username/repo --ssh`
- 系统获取（或复用已保存的）GitHub Personal Access Token
- Token 通过 GitHub API 验证身份
- 自动生成 Ed25519 SSH 密钥对，存放在 `~/.yo/github/{user}/keys/{repo}/`
- 通过 API 将公钥作为 Deploy Key 添加到目标仓库
- 自动配置 `~/.ssh/config`，为该仓库创建 Host 别名（`github.com.{user}.{repo}`），实现多仓库多密钥共存
- 输出可直接使用的 `git clone` 命令

### 2.2 HTTPS Token 方式初始化

**需求：** 有些环境不方便用 SSH（如企业防火墙限制 22 端口），用户希望用 HTTPS + Token 的方式访问仓库。

**行为：**
- 用户运行 `yo init @username/repo --https`
- 系统获取（或复用已保存的）GitHub Personal Access Token
- Token 通过 GitHub API 验证身份
- 配置 `~/.git-credentials`，写入带 Token 的 HTTPS URL
- 设置 `credential.useHttpPath=true`，确保不同仓库可以使用不同 Token
- 输出可直接使用的 `git clone` 命令

### 2.3 交互式选择

**需求：** 用户不确定用 SSH 还是 HTTPS，想让工具引导选择。

**行为：**
- 用户运行 `yo init @username/repo`（不带 --ssh 或 --https）
- 交互式提示选择认证方式
- 后续流程同上

### 2.4 Token 安全存储

**需求：** Token 是敏感信息，用户不想每次都手动输入，但也不想明文存在磁盘上。

**行为：**
- 首次使用时提示输入 GitHub Personal Access Token
- Token 使用 AES-256-CBC 加密后存储在 `~/.yo/github/{user}/token`
- 后续使用自动读取并解密，无需重复输入
- 每个 GitHub 用户名对应独立的 Token 存储

---

## 3. 设计决策

| 决策 | 理由 |
|------|------|
| 按仓库隔离 SSH 密钥 | 每个仓库独立密钥对，吊销某个仓库的访问不影响其他仓库 |
| SSH Config Host 别名 | `github.com.{user}.{repo}` 格式避免多密钥冲突，Git 原生支持 |
| AES-256 加密 Token | 比明文安全，比系统密钥链跨平台性更好 |
| Deploy Key 而非账户级 SSH Key | 最小权限原则，Deploy Key 只能访问单个仓库 |
| `credential.useHttpPath=true` | HTTPS 模式下区分不同仓库的 Token，避免串用 |
| 自动备份 SSH Config | 修改 `~/.ssh/config` 前先备份，防止配置被破坏 |

---

## 4. 当前已知限制

- **Token 需要手动创建：** 用户仍需在 GitHub 网页上创建 Personal Access Token，yo-git 不做 OAuth 流程
- **Deploy Key 默认只读：** 添加的 Deploy Key 为 read-only，若需写权限需手动修改
- **加密密钥固定：** AES 密钥由固定 salt 派生，安全性依赖于本地文件系统访问控制
