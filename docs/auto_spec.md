# Auto Module Spec

## 概述

`yo-auto` 是一个常驻后台的任务调度器，基于 Rhai 脚本引擎驱动，支持定时任务、锁屏/解锁事件响应、TTS 语音播报、密码管理等功能，并提供 Web UI 进行管理。

## 架构

```
┌─────────────────────────────────────────────────────┐
│ AutoCommand (入口)                                   │
│  ├─ InstanceLock (单实例)                             │
│  ├─ PasswordManager::check_and_restore() (启动自愈)   │
│  └─ ctrlc handler (退出时恢复密码)                     │
├─────────────────────────────────────────────────────┤
│ RhaiScheduler (调度器)                                │
│  ├─ RhaiEngine (脚本引擎)                             │
│  │   ├─ Rhai API (时间/动作/环境变量/天气)             │
│  │   ├─ SideEffectExecutor (副作用执行器)              │
│  │   └─ State Persistence (状态持久化)                 │
│  ├─ TimeIndex (时间索引，O(1) 按小时查找)              │
│  └─ PendingExecutions (锁外执行队列)                   │
├─────────────────────────────────────────────────────┤
│ 并行子系统                                            │
│  ├─ Web Server (Axum REST API + Vue UI, async)       │
│  ├─ LockscreenMonitor (Windows WTS 消息循环)          │
│  └─ Audio Thread (rodio 播放队列)                     │
└─────────────────────────────────────────────────────┘
```

## 核心流程

### 启动流程

```
1. check_and_restore()       — 恢复上次异常退出遗留的密码状态
2. InstanceLock::try_acquire  — PID 文件单实例检查
3. RhaiScheduler::new()      — 加载 ~/.yo/rules/*.rhai，编译 AST，构建 TimeIndex
4. set_global_scheduler()    — 设置全局引用供 lock/unlock 事件回调
5. LockscreenMonitor::start  — Windows WTS 消息循环（独立线程）
6. Web Server 启动            — Axum on 127.0.0.1:9999（tokio async）
7. call_on_mount_all()       — 调用所有规则的 on_mount()
8. 进入主循环
```

### 主循环（每分钟一次）

```
loop {
    pending = scheduler.lock().prepare_tick()   // 短暂持锁
    scheduler.unlock()                           // 立即释放
    pending.execute()                            // 锁外执行（TTS/网络等耗时操作）

    if 整点 { scheduler.lock().reload() }
    sleep(60 - second)
}
```

### prepare_tick 内部

```
1. 去重检查（同一分钟只执行一次）
2. collect_time_range_transitions()
   — 遍历所有规则，检测 in_range → out_of_range 转换
   — 收集 on_destroy 调用到 PendingExecutions
3. simulate_upcoming_rules()
   — 规则开始前 1 分钟，用独立 Engine 预生成 TTS 缓存
4. 查 TimeIndex 获取当前小时的 tick 规则
5. should_execute() 检查：enabled → weekday → time_range → interval
6. 收集 on_tick 调用到 PendingExecutions
```

### Lock/Unlock 事件

```
Windows WTS 消息 → trigger_unlock_event() / trigger_lock_event()
  → scheduler.lock().prepare_unlock/lock()   // 短暂持锁，收集规则
  → scheduler.unlock()
  → pending.execute()                         // 锁外执行
```

## Rhai 脚本规范

### 脚本结构

```rhai
let name = "显示名称";
let description = "描述";

let state = #{
    counter: 0,        // 自动持久化到 ~/.yo/state/{script}.json
};

let trigger = #{
    time_range: ["HH:MM", "HH:MM"],   // 支持跨午夜如 ["22:00", "06:00"]
    interval_minutes: 5,                // 在 time_range 内每 N 分钟触发 on_tick
    events: ["tick", "unlock", "lock"], // 响应的事件类型
    weekdays: [1,2,3,4,5],             // 1=周一, 7=周日
    enabled: true,
};

fn on_mount()   {}   // 程序启动时调用一次
fn on_tick()    {}   // interval 到达时调用
fn on_unlock()  {}   // 屏幕解锁时调用（需 events 包含 "unlock"）
fn on_lock()    {}   // 屏幕锁定时调用（需 events 包含 "lock"）
fn on_destroy() {}   // 离开 time_range 时调用（用于重置状态）
```

### 生命周期

```
程序启动 → on_mount()
进入 time_range → on_tick() 开始按 interval 触发
用户解锁屏幕 → on_unlock()（仅在 time_range 内）
用户锁定屏幕 → on_lock()（仅在 time_range 内）
离开 time_range → on_destroy()
整点 → reload（重新编译脚本，不调 on_mount/on_destroy）
```

### 状态持久化

```
脚本定义 state 默认值（每次重启从脚本重新读取）
运行时修改的值持久化到 ~/.yo/state/{script}.json
只保存与默认值不同的字段（差异保存）
程序重启时：clear all states → 从脚本重新获取默认值
```

### 可用 API

| 分类 | API | 说明 |
|------|-----|------|
| 时间 | `hour()`, `minute()`, `second()` | 当前时间（模拟模式返回模拟时间） |
| 时间 | `weekday()` | 1-7，1=周一 |
| 时间 | `time_str()`, `date_str()` | "HH:MM", "YYYY-MM-DD" |
| 时间 | `in_time_range(start, end)` | 是否在时间范围内 |
| 时间 | `is_weekend()`, `is_workday()` | 判断工作日 |
| 时间 | `weekday_name()` | "星期一" 等 |
| 动作 | `speak(text)` | TTS 语音播报（阻塞直到播放完成） |
| 动作 | `speak(text, pause_ms)` | 带自定义停顿 |
| 动作 | `lock_screen()` | 锁定屏幕 |
| 动作 | `enter_sleep()` | 进入睡眠 |
| 动作 | `shutdown(delay_secs)` | 计划关机 |
| 动作 | `chime(hour)` | 整点报时音 + 语音 |
| 动作 | `log(msg)` | 日志输出 |
| 密码 | `change_password()` | 改为 LOCK_PASSWORD（幂等，有自愈） |
| 密码 | `restore_password()` | 恢复原密码（幂等） |
| 屏幕 | `screen_locked()` | 检查屏幕是否锁定 |
| 环境 | `get_env(key)`, `has_env(key)` | 读取 GlobalConfig |
| 日历 | `get_today_festival()` | 今日节日 |
| 日历 | `get_today_solar_term()` | 今日节气 |
| 日历 | `get_today_special()` | 节日优先，否则节气 |
| 日历 | `days_until_spring_festival()` | 距春节天数 |
| 天气 | `get_weather(city)` | 返回 Map: weather/temp/feels_like/humidity/wind_dir/wind_scale |
| 事件 | `generate_script_events(name)` | 模拟生成日历事件到 events.json |

### 运行模式（RunMode）

| 模式 | 用途 | speak 行为 | 副作用 |
|------|------|-----------|--------|
| Real | 正常运行 | 合成 + 播放 | 全部执行 |
| CacheTts | TTS 预热 | 只合成不播放 | 全部跳过 |
| GenerateEvents | 事件生成 | 只收集文本 | 全部跳过 |

## 子系统

### TTS（火山引擎）

- API: `https://openspeech.bytedance.com/api/v1/tts`
- 配置: `TTS_API_KEY = "appid|token"`, `TTS_VOICE = "voice_id"`
- 缓存: `SHA256(text+speaker)` → `~/.yo/voice/cache/{hash}.mp3`
- 播放: 单线程 rodio 队列，按顺序播放
- 预热: 规则激活前 1 分钟用独立 Engine 模拟执行，提前生成缓存

### 密码管理

- 存储: `~/.yo/config.json` 中 `WINDOWS_PASSWORD`
- LOCK_PASSWORD: `"zyxwvutsrqponmlkjihgfedcba"`（固定）
- 标记文件: `~/.yo/password_changed`（存在=已改密）
- 自愈: `change_password` 遇 error 86 时尝试 LOCK→原 再 原→LOCK
- 恢复时机: on_destroy / 程序启动 check_and_restore / ctrlc handler

### 锁屏监控（Windows）

- 使用 `WTSRegisterSessionNotification` 监听会话变化
- `WM_WTSSESSION_CHANGE` wparam 0x7=lock, 0x8=unlock
- 进程检测: `CreateToolhelp32Snapshot` 查找 LogonUI.exe

### Web UI

- 绑定: `127.0.0.1:{port}`（默认 9999，仅本机访问）
- 前端: 内嵌 Vue 3 单文件 HTML
- 功能: 规则列表/编辑/创建/删除/重命名/导入导出、事件日历、环境变量配置、脚本模拟

### 单实例

- PID 文件: `~/.yo/yo-auto.pid`
- 启动时检查旧 PID 是否存活，死进程自动清理

## 文件布局

```
~/.yo/
├── config.json              — 全局环境变量
├── password_changed         — 密码修改标记
├── yo-auto.pid              — 单实例锁
├── events.json              — 日历事件
├── rules/                   — Rhai 脚本
│   ├── 01_night_lockscreen.rhai
│   ├── 02_lunch_lockscreen.rhai
│   └── ...
├── state/                   — 脚本运行时状态（差异存储）
│   └── {script_name}.json
└── voice/
    ├── cache/               — TTS 缓存（SHA256 命名）
    │   └── {hash}.mp3
    └── clock/
        └── Hour_Chime_from_Clock.mp3
```
