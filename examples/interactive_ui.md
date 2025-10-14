# CLI 交互界面示例

## 方案对比

### 1. Dialoguer (推荐用于 yo 项目)
**优点**：
- 轻量级，依赖少
- 与现有的 colored 风格一致
- API 简单直观
- 适合表单和确认对话框

**示例代码**：
```rust
use dialoguer::{theme::ColorfulTheme, Select, Confirm, Input};

pub struct InteractiveTaskManager;

impl InteractiveTaskManager {
    pub fn add_task_interactive() -> Result<Task, Box<dyn std::error::Error>> {
        let theme = ColorfulTheme::default();

        // 任务类型选择
        let task_types = vec!["锁屏 (lockscreen)", "自定义命令 (command)"];
        let task_type_idx = Select::with_theme(&theme)
            .with_prompt("选择任务类型")
            .items(&task_types)
            .default(0)
            .interact()?;

        let task_type = if task_type_idx == 0 { "lockscreen" } else { "command" };

        // 任务名称
        let name: String = Input::with_theme(&theme)
            .with_prompt("任务名称")
            .interact_text()?;

        // 执行时间
        let time: String = Input::with_theme(&theme)
            .with_prompt("执行时间 (HH:MM)")
            .default("22:00".to_string())
            .interact_text()?;

        // 如果是自定义命令
        let command = if task_type == "command" {
            Some(Input::with_theme(&theme)
                .with_prompt("要执行的命令")
                .interact_text()?)
        } else {
            None
        };

        // 描述
        let description: String = Input::with_theme(&theme)
            .with_prompt("任务描述")
            .allow_empty(true)
            .interact_text()?;

        // 确认
        let confirmed = Confirm::with_theme(&theme)
            .with_prompt("确认创建此任务?")
            .default(true)
            .interact()?;

        if !confirmed {
            return Err("用户取消操作".into());
        }

        Ok(Task {
            name,
            task_type: task_type.to_string(),
            time,
            enabled: true,
            command,
            description: if description.is_empty() { None } else { Some(description) },
        })
    }
}
```

**运行效果**：
```
? 选择任务类型 ›
  ❯ 锁屏 (lockscreen)
    自定义命令 (command)

? 任务名称 › morning_reminder

? 执行时间 (HH:MM) › 08:00

? 要执行的命令 › echo "Good morning!" > ~/reminder.txt

? 任务描述 › Daily morning reminder

? 确认创建此任务? (Y/n) › Yes

✓ 任务创建成功！
```

### 2. Ratatui (适合复杂界面)
**优点**：
- 功能最强大
- 可以创建完整的 TUI 应用
- 支持实时更新、多窗口

**适用场景**：
- 实时监控任务状态
- 任务列表管理界面
- 日志查看器

**示例代码**：
```rust
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

pub struct TaskMonitor {
    tasks: Vec<Task>,
    selected: usize,
}

impl TaskMonitor {
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // 设置终端
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        loop {
            terminal.draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Min(0),
                        Constraint::Length(3),
                    ])
                    .split(f.size());

                // 标题
                let title = Paragraph::new("🤖 Yo 任务监控")
                    .style(Style::default().fg(Color::Cyan))
                    .block(Block::default().borders(Borders::ALL));
                f.render_widget(title, chunks[0]);

                // 任务列表
                let items: Vec<ListItem> = self.tasks
                    .iter()
                    .enumerate()
                    .map(|(i, task)| {
                        let status = if task.enabled { "✓" } else { "✗" };
                        let content = format!("{} [{}] {} - {}",
                            status, task.time, task.name, task.task_type);
                        ListItem::new(content)
                            .style(if i == self.selected {
                                Style::default().bg(Color::DarkGray)
                            } else {
                                Style::default()
                            })
                    })
                    .collect();

                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("任务列表"));
                f.render_widget(list, chunks[1]);

                // 帮助信息
                let help = Paragraph::new("↑/↓: 选择 | Enter: 编辑 | Space: 启用/禁用 | a: 添加 | d: 删除 | q: 退出")
                    .style(Style::default().fg(Color::Gray));
                f.render_widget(help, chunks[2]);
            })?;

            // 处理键盘事件
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Up => {
                        if self.selected > 0 {
                            self.selected -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if self.selected < self.tasks.len() - 1 {
                            self.selected += 1;
                        }
                    }
                    KeyCode::Char(' ') => {
                        self.tasks[self.selected].enabled = !self.tasks[self.selected].enabled;
                    }
                    _ => {}
                }
            }
        }

        // 恢复终端
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        Ok(())
    }
}
```

**运行效果**：
```
┌─ 🤖 Yo 任务监控 ────────────────────────┐
│                                         │
└─────────────────────────────────────────┘
┌─ 任务列表 ──────────────────────────────┐
│ ✓ [22:00] night_lockscreen - lockscreen│
│ ✓ [08:00] morning_reminder - command   │
│ ✗ [12:00] lunch_break - command        │
└─────────────────────────────────────────┘
↑/↓: 选择 | Enter: 编辑 | Space: 启用/禁用 | a: 添加 | d: 删除 | q: 退出
```

### 3. 系统原生通知
```rust
// Cargo.toml
notify-rust = "4.10"

use notify_rust::Notification;

// 显示系统通知
Notification::new()
    .summary("Yo 任务提醒")
    .body("锁屏任务将在 1 分钟后执行")
    .icon("clock")
    .timeout(5000)
    .show()?;
```

## 推荐方案

对于 `yo` 项目，我建议：

1. **基础交互** - 使用 **dialoguer**
   - 添加任务时的交互式表单
   - 删除/编辑任务的确认对话框
   - 简单的选择菜单

2. **高级功能** (可选) - 使用 **ratatui**
   - `yo run auto --monitor` 实时监控模式
   - 可视化任务管理界面

3. **系统通知** - 使用 **notify-rust**
   - 任务执行前的提醒
   - 任务执行结果通知

## 实现建议

可以添加新命令：
```bash
yo run auto           # 原有的后台运行模式
yo run auto --monitor # 新的 TUI 监控模式
yo task add           # 交互式添加任务
yo task list          # 列出所有任务
yo task edit <name>   # 编辑任务
yo task delete <name> # 删除任务
```
