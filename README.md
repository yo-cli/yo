cargo install --path .

# Yo - 多功能命令行工具

> 🦀 **纯 Rust 实现** - 高性能、内存安全的命令行工具

一个集成了 GitHub SSH 密钥管理、SOCKS5 代理和定时任务调度的多功能命令行工具。

## ✨ 特性

- 🔐 **GitHub SSH 密钥管理** - 自动生成、配置和部署 SSH 密钥
- 🌐 **SOCKS5 代理** - 基于 Docker 和 GOST 的一键代理服务
- ⏰ **定时任务调度** - 常驻进程执行定时任务（锁屏、自定义命令等）
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

# 定时任务调度 (持续运行)
yo run auto

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

### 定时任务调度

```bash
yo run auto
```

功能特点:
1. 启动后显示当前时间和任务列表
2. 持续运行，不会自动退出
3. 每30秒检查一次任务执行时间
4. 默认任务：22:00-06:00 期间每 5 分钟锁屏一次
5. 支持时间区间重复执行
6. 配置文件：`~/.yo/auto_config.json`

**配置文件示例**:
```json
{
  "tasks": [
    {
      "name": "night_lockscreen",
      "task_type": "lockscreen_repeated",
      "start_time": "22:00",
      "end_time": "06:00",
      "interval_minutes": 5,
      "enabled": true,
      "description": "Lock screen every 5 minutes from 22:00 to 06:00"
    },
    {
      "name": "work_break",
      "task_type": "command",
      "start_time": "09:00",
      "end_time": "18:00",
      "interval_minutes": 60,
      "enabled": true,
      "command": "notify-send 'Break Time' 'Take a 5-minute break!'",
      "description": "Hourly break reminder during work hours"
    },
    {
      "name": "lunch_reminder_tts",
      "task_type": "tts_command",
      "start_time": "12:00",
      "end_time": "12:01",
      "interval_minutes": 1440,
      "enabled": true,
      "tts_text": "该吃午饭了",
      "tts_voice": "zh_female_wanwanxiaohe_moon_bigtts",
      "tts_api_key": "your-volcengine-api-key",
      "description": "12:00 午餐提醒（湾湾小何音色）"
    }
  ]
}
```

**任务字段说明**:
- `name`: 任务名称（唯一标识）
- `task_type`: 任务类型（`lockscreen_repeated`、`command` 或 `tts_command`）
- `start_time`: 开始时间（HH:MM 格式）
- `end_time`: 结束时间（HH:MM 格式，支持跨午夜）
- `interval_minutes`: 执行间隔（分钟）
- `enabled`: 是否启用任务
- `command`: 自定义命令（仅 `command` 类型需要）
- `description`: 任务描述（可选）
- `tts_text`: 语音合成文本（仅 `tts_command` 类型需要）
- `tts_voice`: 语音模型/音色（仅 `tts_command` 类型需要）
- `tts_api_key`: 火山引擎 API Key（仅 `tts_command` 类型需要）

**任务类型**:
- `lockscreen_repeated`: 重复锁定屏幕（支持 Linux/macOS/Windows）
- `command`: 执行自定义 shell 命令
- `tts_command`: 文字转语音提醒（支持火山引擎 TTS API）

**时间区间说明**:
- 支持跨午夜时间段（如 22:00-06:00）
- 结束时间不执行（如 06:00 不会触发任务）
- 执行时间点：start_time, start_time+interval, start_time+2*interval...

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
│   ├── s5/                # SOCKS5 代理
│   └── auto/              # 定时任务调度
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