# Auto Module 待改进项

基于 spec 梳理的问题和优化建议，按优先级排列。

---

## P0 - 逻辑缺陷

### 1. 非 Web 模式 run() 死锁
**文件**: `scheduler.rs` `run()` + `auto_command.rs` `execute()`
**问题**: `execute()` 中 `scheduler_arc.lock().unwrap().run()` 持锁进入无限循环，`trigger_unlock_event()` 永远拿不到锁，lock/unlock 事件完全失效。
**修复**: `execute()` 应和 `execute_with_web()` 一样使用 `run_scheduler_loop` 模式（短暂持锁 prepare → 释放 → execute）。

### 2. reload 丢失 on_destroy 时机
**文件**: `scheduler.rs` `reload()`
**问题**: 整点 reload 时替换 rules 向量，如果某规则正在 time_range 内被 reload，旧规则的 on_destroy 不会被调用（因为旧规则已不在 self.rules 中）。若该规则调用了 change_password，密码不会被恢复。
**修复**: reload 前对已移除/修改的规则执行 on_destroy。

### 3. on_mount 只在启动时调用一次
**文件**: `scheduler.rs`
**问题**: 规则进入 time_range 时没有入口回调。当前 on_mount 仅在程序启动时调用，不是"进入时间范围"时调用。如果程序一直运行，on_mount 只执行一次。脚本中的 `state.unlocks = 0` 重置逻辑实际依赖 on_destroy（离开时重置），但如果程序在 time_range 内重启，unlocks 不会重置（因为 clear_all_states 清了磁盘状态，但 on_mount 里的重置也执行了，所以这个其实 OK）。
**状态**: 可接受，但建议明确区分 on_mount（启动）和 on_enter（进入时间范围）语义。

### 4. simulate_upcoming_rules 只检查 time_range 起始分钟
**文件**: `scheduler.rs` `simulate_upcoming_rules()`
**问题**: 只在 `future_mins == start_mins` 时触发预热，如果程序恰好在那一分钟没运行（睡眠恢复等），TTS 缓存不会预热。
**影响**: 不致命，首次 speak 会有延迟（需实时合成）。

---

## P1 - 健壮性

### 5. ctrlc handler 可能不捕获 Windows 关机事件
**文件**: `auto_command.rs`
**问题**: `ctrlc` crate 在 Windows 上处理 CTRL_C_EVENT 和 CTRL_CLOSE_EVENT，但不一定处理 CTRL_SHUTDOWN_EVENT 和 CTRL_LOGOFF_EVENT。`shutdown(30)` 触发的系统关机可能不经过 ctrlc handler。
**影响**: 密码可能残留为 LOCK_PASSWORD。
**缓解**: `check_and_restore()` 在下次启动时恢复 + 自愈逻辑。
**改进**: 注册 Windows console handler 覆盖所有关闭事件类型。

### 6. 脚本编译错误导致整个 reload 可能不一致
**文件**: `engine.rs` `load_rules()`
**问题**: 单个脚本编译失败只打印警告继续，但如果核心脚本（如锁屏规则）编译失败，调度器继续运行但缺少关键规则。
**改进**: 编译失败时保留旧规则版本。

### 7. 天气 API 阻塞调度器
**文件**: `weather/client.rs`
**问题**: `get_weather()` 是同步 HTTP 请求，在 Rhai 脚本中调用时阻塞当前规则执行。如果网络慢或超时，会延迟后续规则执行。
**改进**: 加超时限制（当前 reqwest 默认 30s）；或缓存天气结果避免重复请求。

---

## P2 - 代码质量

### 8. extract_script_metadata 用字符串解析代替 AST
**文件**: `web/server.rs` `extract_script_metadata()`
**问题**: 用 `find('"')` 解析 `let name = "..."` 和 `let description = "..."`。不支持：
- 模板字符串 `` `...` ``（如带 `${max_unlocks}` 的 description）
- 单引号字符串
- 多行值
**影响**: Web UI 中部分脚本的 description 显示为空。
**修复**: 用 Rhai Engine 编译脚本后从 Scope 提取变量（和 engine.rs 的 extract_metadata 一样）。

### 9. 重复的 Rhai Map ↔ JSON 转换代码
**文件**: `engine.rs` 和 `web/server.rs`
**问题**: 两处都有 `rhai_map_to_json` / `dynamic_to_json`，逻辑完全相同。
**修复**: 提取到 types.rs 或 common utils。

### 10. TimeIndex 不支持 pre_notify 时间窗口
**文件**: `index.rs`
**问题**: 规则如 night_lockscreen 从 21:50 开始，但实际锁屏在 22:20。21:50-22:20 之间只是提醒。TimeIndex 把整个 21:50-06:00 都索引了，这没问题但不精确。
**影响**: 无功能影响，只是索引粒度粗。
**状态**: 可接受。

---

## P3 - 可选优化

### 11. speak() 阻塞可优化
**现状**: `speak()` 阻塞到播放完成。多次 speak 顺序执行。
**场景**: 早安播报文本很长，一次 speak 可能持续 30 秒+。
**可选**: 将合成和播放分离，合成完成即返回（播放在队列线程完成）。保持顺序性但减少调用线程阻塞时间。

### 12. 整点 reload 可能中断正在执行的规则
**现状**: reload 在主循环中 prepare_tick 之后检查。如果 pending.execute() 正在执行（如 TTS 播放中），reload 要等下一轮才执行。这其实是安全的。
**状态**: 当前行为正确，无需改动。

### 13. 测试覆盖不足
**缺失测试**:
- RhaiScheduler 状态机（时间范围转换、interval 计算）
- TimeIndex 构建（跨午夜范围）
- should_execute 各分支
- PendingExecutions 流程
- Web API 端点

### 14. 密码明文存储
**现状**: 原始 Windows 密码存在 `~/.yo/config.json` 明文中。
**风险**: 本机其他用户或恶意软件可读取。
**可选**: 使用 AES 加密（项目已有 common/crypto 模块）。
