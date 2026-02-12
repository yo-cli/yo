# yo-ob 产品需求规格

> 本文档面向 AI 开发助手，描述 yo-ob 的产品定位、核心场景和设计决策。

---

## 1. 产品定位

**yo-ob 是一个 OceanBase 数据库部署环境准备工具。**

目标用户是需要在 Debian Linux 上部署 OceanBase 数据库的运维人员或开发者。OceanBase 对操作系统有一系列特定的配置要求（文件描述符限制、内核参数、IPv6、SSH 端口等），手动逐项配置既繁琐又容易遗漏。yo-ob 自动检测、配置并验证所有前置条件。

核心理念：**一条命令，环境就绑。**

---

## 2. 核心场景

### 2.1 一键准备环境

**需求：** 用户拿到一台新的 Debian 服务器，要部署 OceanBase，不想一项项查文档手动配置。

**行为：**
- 用户运行 `yo-ob prepare`
- 系统检查：
  - 是否为 Debian 系统
  - 是否以 root 权限运行
- 交互式配置 SSH 端口（可选择修改或保持默认）
- 配置 `/etc/hosts` 文件（确保 `127.0.0.1 {hostname}` 存在）
- 检查并安装必要软件包（`iputils-clockdiff`、`rpm2cpio`、`alien`）
- 配置 OceanBase 资源限制（`/etc/security/limits.d/99-oceanbase.conf`）：
  - nofile=655360、nproc=655360、core=unlimited、stack=unlimited
- 配置内核参数（`/etc/sysctl.d/99-oceanbase.conf`）：
  - `fs.aio-max-nr=1048576`、`vm.max_map_count=655360`
- 禁用 IPv6（`/etc/sysctl.d/99-disable-ipv6.conf`）
- 应用 sysctl 配置并验证运行时值

**冲突处理：**
- 如果配置文件已存在且内容不同，交互式提示用户选择：覆盖还是保留
- `--force` 标志跳过确认，直接覆盖
- 修改前自动备份原文件（带时间戳）

### 2.2 环境检查

**需求：** 用户已经配过环境（或者不确定配没配），想验证所有配置是否正确。

**行为：**
- 用户运行 `yo-ob check`
- 逐项检查：
  - 配置文件是否存在、内容是否正确
  - sysctl 运行时值是否与配置一致
  - 当前 session 的 ulimit 值是否满足要求（nofile、core、stack、nproc）
- 用清晰的表格展示每一项的状态（通过/不通过/冲突）

---

## 3. 设计决策

| 决策 | 理由 |
|------|------|
| 仅支持 Debian | OceanBase 官方推荐 CentOS/Debian，此工具聚焦 Debian 生态 |
| 要求 root 权限 | 修改系统限制和内核参数必须 root |
| 配置文件放 `.d` 目录 | 使用 `limits.d/` 和 `sysctl.d/` 而非修改主配置文件，更安全、可追溯 |
| 自动备份 | 每次修改前带时间戳备份，方便回滚 |
| 运行时验证 | 配置文件写入后还需 `sysctl --system` 生效，验证运行时值确保真正生效 |
| 交互式冲突解决 | 尊重用户已有配置，不静默覆盖 |

---

## 4. 当前已知限制

- **仅 Debian Linux：** 不支持 CentOS、Ubuntu 或其他发行版
- **不管理 OceanBase 本身：** 只做环境准备，不负责 OceanBase 的安装和启动
- **IPv6 一律禁用：** 不支持"保留 IPv6 但满足 OceanBase 要求"的场景
- **SSH 端口修改需手动验证：** 修改后建议用户用新端口测试连接再关闭旧会话
