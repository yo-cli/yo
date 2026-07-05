# yo-forward 产品需求规格

> 本文档面向 AI 开发助手，描述 yo-forward 的产品定位、核心场景和设计决策。

---

## 1. 产品定位

**yo-forward 是一个一键把本机接入上游代理、让本地程序全部走代理出海的工具。**

目标用户是在 WSL2 / 本地 Linux 开发机上工作、需要让 `curl`/`pnpm`/`git`/`gh` 等命令行工具透明走代理的开发者。传统方式需要手动下载 gost、写 systemd 服务、改 `.bashrc` 环境变量、测连通，yo-forward 把这些全部自动化。

核心理念：**一条命令，本机就能翻墙。**

它在本机起一个 gost，把 `http://:8888` 的流量转发给一个**已有的上游 SOCKS5 出口**（机场客户端，通常跑在 Windows 宿主机的 `127.0.0.1:30999`），并把 `http_proxy` 等环境变量写进 shell 配置。gost 在这里是「http 入口 ↔ socks5 转发」的协议适配层，**本身不翻墙**——真正的出海能力来自上游。

---

## 2. 与 yo-s5 的关系（镜像对偶）

yo-forward 与已有的 yo-s5 是同一主题（都基于 GOST）下角色相反的一对，边界必须划清、互不合并：

| 维度 | yo-s5（代理服务端） | yo-forward（代理客户端） |
|------|--------------------|--------------------------|
| 角色 | 把本机变成代理服务器给别人连 | 让本机接上游代理、自己出海 |
| gost 模式 | `auto://:port` 监听（带认证） | `http://:8888 -F socks5://上游` 转发 |
| 部署 | Docker `gogost/gost:3` | 裸二进制 + systemd |
| 环境 | 原生 Linux 服务器，**拒绝 WSL** | **主场是 WSL2 / 本地开发机** |
| 上游 | 无（自己是源头） | 依赖已有上游 socks5 出口 |
| 产出 | 打印代理配置 JSON | 配好 systemd + shell 环境变量 |

因为二者的 WSL 策略相反、部署模型无重叠，yo-forward 作为**独立 binary** 存在，不并入 yo-s5。

---

## 3. 核心场景

### 3.1 `yo-forward up` —— 一键接入（默认动作）

**需求：** 用户在一台 WSL2 开发机上，宿主机已经有机场客户端在 `127.0.0.1:30999` 提供 socks5 出口，想让本机命令行全部走代理。

**行为：**
- 检测 systemd 是否可用（WSL 需 `/etc/wsl.conf` 开启 `[boot] systemd=true`），不可用则明确提示开启方法后终止
- 检测本机是否已装 gost：已装且版本满足则跳过；否则下载 GOST v3 裸二进制装到 `/usr/local/bin/gost`
- 写入 systemd 服务 `gost.service`：`ExecStart=/usr/local/bin/gost -L http://:8888 -F "socks5://<上游>?nodelay=false"`，`Restart=always`
- `systemctl daemon-reload && systemctl enable --now gost`
- 幂等注入 shell 环境变量到 `~/.bashrc`（改前自动备份）：`http_proxy` / `https_proxy` / `all_proxy` 指向 `http://127.0.0.1:8888`，`no_proxy=localhost,127.0.0.1`
- 连通性测试：经 `http://127.0.0.1:8888` 访问外网，打印出口 IP
- 全程幂等：每一步先检查现状，已就位则跳过，可安全重复运行

**默认值（零思考）：** 上游 `127.0.0.1:30999`、本地端口 `8888`。可用 `--upstream <host:port>` / `--port <n>` 覆盖，`-i` 进入交互式确认，`-f` 跳过所有确认。

### 3.2 `yo-forward check` —— 体检

**需求：** 用户想知道当前代理链路是否健康、哪一环断了。

**行为：** 逐项检查并用 ✓/✗/⚠ 呈现——
- gost 是否安装及版本
- `gost.service` 是否 `active (running)`
- 本地 `8888` 是否在监听
- 上游 socks5（默认 `127.0.0.1:30999`）是否可达
- 经 `8888` 的出口 IP（验证是否真的翻出去）
- systemd 是否可用
- `~/.bashrc` 是否已注入代理变量

### 3.3 `yo-forward down` —— 停用

**需求：** 用户想临时/彻底关掉本机代理。

**行为：**
- `systemctl disable --now gost` 并删除 `gost.service`
- 从 `~/.bashrc` 移除注入的代理变量块（改前自动备份）
- 提示用户 `unset http_proxy https_proxy all_proxy` 或重开终端使当前会话生效
- 不误删用户手写的其他代理配置：仅移除本工具带标记注入的块

---

## 4. 设计决策

| 决策 | 理由 |
|------|------|
| 独立 binary，不并入 yo-s5 | 二者 WSL 策略相反（s5 拒绝 WSL / forward 主场是 WSL）、部署模型无重叠（Docker 服务端 / 裸二进制客户端），合并只会让逻辑自相矛盾、增加认知负担 |
| 裸二进制 + systemd，不用 Docker | 客户端本地转发不需要环境隔离；systemd 轻量、开机自启、崩溃自拉起，且与用户现状一致；Docker 在 WSL 里访问宿主机 `127.0.0.1` 上游反而有网络坑 |
| 形态参照 yo-ob | yo-ob 是「裸操作宿主系统：装东西 + 写配置 + 检查」的本地环境准备工具，yo-forward 属于同一类，直接复用其 clap subcommand 结构与声明式幂等配置模式 |
| gost 优先复用本机已有 | 装 gost 需访问 GitHub，但此刻可能还没代理（鸡生蛋）；先检测 `/usr/local/bin/gost`，已装则跳过下载 |
| 下载走镜像兜底 | 需下载时直连 GitHub 可能超时，用 `ghfast.top` 之类镜像前缀兜底（镜像域名可能变，仅作 fallback） |
| 锁定 GOST v3 | `-L http://:8888 -F socks5://...` 是 v3 语法；与 yo-s5 的 `gogost/gost:3` 保持同一大版本 |
| bashrc 注入带标记 + 备份 | 用带注释标记的块包裹注入内容，便于 `down` 精确移除；改前 `backup_file` 备份，遵守 repo「配置修改先备份」惯例 |
| 不主动探测 WSL mirrored 网络 | mirrored 探测复杂且脆弱；改为通过「上游 socks5 连通性测试」间接覆盖——上游不可达时，在排查提示里列出「WSL 非 mirrored 网络」这一可能原因 |
| 连通性测试收口所有失败 | 不预判每种网络问题，统一以「经 8888 能否出海」为最终判据，失败时打印结构化排查清单（上游没起 / 端口不对 / 非 mirrored / systemd 没开） |

---

## 5. 复用与新增

**复用现有代码（不重造）：**
- `s5::network_utils::S5NetworkUtils::{is_wsl, is_port_available}` —— WSL 检测、`8888` 占用检测
- `s5::network_utils` 的 `probe_proxy` 思路 —— 新增一个无认证变体（本地 gost 不需要账密），复用测出海逻辑
- `ob::utils::{backup_file, read_file_safe, write_file}` —— bashrc 读写与备份
- `reqwest`（已有 rustls 特性）下载 gost；`colored` / `inquire` 沿用输出符号与交互风格
- 备注：`is_wsl` / `is_port_available` / 文件工具本质是通用工具，当前分别锁在 `s5` / `ob` 域；首版可直接跨域 `use`，如需解耦可后续提到 `common`（实现时二选一，不在本 spec 强制）

**新增文件：**
```
src/bin/yo_forward.rs            # clap 入口（仿 yo_ob.rs），子命令 up / check / down
src/forward/
  mod.rs
  commands/{mod,up,check,down}.rs
  gost_installer.rs              # 检测/下载/安装 gost 二进制
  systemd_unit.rs                # 写入/删除 gost.service
  shell_env.rs                   # 幂等注入/移除 bashrc 代理变量块
  probe.rs                       # 连通性探测（无认证，返回出口 IP，供 up/check 收口）
```
`src/lib.rs` 增 `pub mod forward;`；`Cargo.toml` 增 `[[bin]] name = "yo-forward"`；`CLAUDE.md` binary 列表补一行。

---

## 6. 当前已知限制

- **仅 Linux / WSL2 支持：** 依赖 systemd 与 Linux shell 环境
- **需要上游出口：** yo-forward 不提供翻墙能力，必须已有一个可达的上游 socks5（机场客户端）；上游不存在时只能配好链路但无法出海
- **需要 systemd：** WSL 未开启 systemd 时无法安装服务，会提示开启方法后终止（不做无 systemd 的降级启动）
- **仅 bash：** 首版只注入 `~/.bashrc`；zsh/fish 用户需手动配置或后续扩展
