# Yo - 多功能命令行工具

> 🦀 **纯 Rust 实现** - 高性能、内存安全的命令行工具

一个集成了 GitHub SSH 密钥管理和 SOCKS5 代理的多功能命令行工具。

## ✨ 特性

- 🔐 **GitHub SSH 密钥管理** - 自动生成、配置和部署 SSH 密钥
- 🌐 **SOCKS5 代理** - 基于 Docker 和 GOST 的一键代理服务
- 🔒 **安全加密** - AES-256-CBC 加密存储敏感信息
- 🎨 **友好界面** - 彩色终端输出,交互式配置
- 🦀 **纯 Rust** - 完全使用 Rust 编写,安全高效

## 🚀 快速开始

### 安装

```bash
# 克隆项目
git clone <your-repo>
cd yo

# 使用 cargo install 安装
cargo install --path .
```

Cargo 会自动:
- ✅ 编译项目
- ✅ 安装到 ~/.cargo/bin/yo
- ✅ PATH 已配置(Rust 安装时自动添加)
- ✅ 全局可用

### 使用

```bash
# GitHub SSH 密钥管理
yo init @username/repository

# SOCKS5 代理 (自动模式)
yo run s5

# SOCKS5 代理 (交互模式)
yo run s5 -i

# 查看版本
yo --version
```

### 卸载

```bash
# 卸载程序
cargo uninstall yo

# 删除配置文件(可选)
rm -rf ~/.yo
```

## 🎯 核心功能

### GitHub SSH 密钥管理

```bash
yo init @username/repository
```

自动完成:
1. 生成 Ed25519 SSH 密钥
2. 加密存储 GitHub Token (AES-256-CBC)
3. 添加 Deploy Key 到 GitHub
4. 配置 SSH config 文件
5. 生成 git clone 命令

### SOCKS5 代理服务

```bash
yo run s5
```

自动完成:
1. 检测并安装 Docker (如需要)
2. 拉取 GOST 镜像
3. 生成随机端口和密码
4. 启动 SOCKS5 代理
5. 测试连接
6. 显示配置信息

## 🔧 技术栈

- **语言**: Rust 1.70+
- **HTTP**: reqwest
- **JSON**: serde + serde_json
- **加密**: openssl + aes + cbc
- **终端**: colored
- **错误**: thiserror

## 📦 项目结构

```
yo/
├── Cargo.toml              # Rust 项目配置
├── README.md               # 项目说明
├── 使用说明.md             # 使用指南
├── src/                    # Rust 源代码
│   ├── main.rs            # 主程序
│   ├── common/            # 通用模块 (加密)
│   ├── github/            # GitHub 管理
│   ├── commands/          # 命令实现
│   └── s5/                # SOCKS5 代理
└── bak/                   # C++ 版本备份
```

## 🔨 开发

```bash
# 编译
cargo build --release

# 运行测试
cargo test

# 代码检查
cargo clippy

# 格式化
cargo fmt

# 生成文档
cargo doc --open
```

## 🤝 贡献

欢迎提交 Issue 和 PR!

## 📄 许可证

MIT License

## 🙏 致谢

- 感谢 Rust 社区提供的优秀工具和库
- 感谢 GOST 项目提供的 SOCKS5 代理实现

---

**Made with 🦀 Rust**
