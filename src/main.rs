mod auto;
mod commands;
mod common;
mod github;
mod s5;

use colored::Colorize;
use commands::{AutoCommand, CloneCommand, GitHubInitCommand, S5Command, TestCommand, VeCommand};
use std::env;

// 从 Cargo.toml 自动读取版本号和项目名称
const VERSION: &str = env!("CARGO_PKG_VERSION");
const PROGRAM_NAME: &str = env!("CARGO_PKG_NAME");

fn show_version() {
    println!("{} version {}", PROGRAM_NAME, VERSION);
}

fn show_usage() {
    println!("Usage: {} [OPTION] | run [COMMAND] | init @username/repo", PROGRAM_NAME);
    println!("Options:");
    println!("  -v, --version                    Show version information");
    println!("  run auto                         Start task scheduler (runs continuously)");
    println!("  run auto --web                   Start task scheduler with Web UI (default port: 9999)");
    println!("  run auto --web [port]            Start task scheduler with Web UI on custom port");
    println!("  run auto --web --autostart         Install autostart and start Web UI (Windows only)");
    println!("  run auto --web --autostart remove  Remove autostart (Windows only)");
    println!("  run auto --web --autostart status  Show autostart status (Windows only)");
    println!("  run clone                        Clone template with keyword replacement");
    println!("  run s5                           Start SOCKS5 proxy (automatic mode)");
    println!("  run s5 -i                        Start SOCKS5 proxy (interactive mode)");
    println!("  run s5 --interactive             Start SOCKS5 proxy (interactive mode)");
    println!("  run test                         Test hourly chime playback");
    println!("  run ve                           Test Volcengine TTS synthesis and playback");
    println!("  init @username/repo              Initialize GitHub SSH keys for repository");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        show_usage();
        std::process::exit(1);
    }

    let arg1 = &args[1];

    // 处理版本标志
    if arg1 == "-v" || arg1 == "--version" {
        show_version();
        return;
    }

    // 在执行任何命令前打印版本信息
    show_version();
    println!(); // 空行分隔

    // 处理 run auto 命令
    if args.len() >= 3 && arg1 == "run" && args[2] == "auto" {
        // 解析参数
        let mut web_enabled = false;
        let mut web_port = 9999u16;
        let mut autostart_action: Option<String> = None; // None, Some("install"), Some("remove"), Some("status")

        let mut i = 3;
        while i < args.len() {
            match args[i].as_str() {
                "--web" => {
                    web_enabled = true;
                    // 检查下一个参数是否是端口号
                    if i + 1 < args.len() {
                        if let Ok(port) = args[i + 1].parse::<u16>() {
                            web_port = port;
                            i += 1;
                        }
                    }
                }
                "--autostart" => {
                    // 检查下一个参数是否是 remove 或 status
                    if i + 1 < args.len() {
                        match args[i + 1].as_str() {
                            "remove" => {
                                autostart_action = Some("remove".to_string());
                                i += 1;
                            }
                            "status" => {
                                autostart_action = Some("status".to_string());
                                i += 1;
                            }
                            _ => {
                                // 默认是 install
                                autostart_action = Some("install".to_string());
                            }
                        }
                    } else {
                        autostart_action = Some("install".to_string());
                    }
                }
                _ => {
                    println!("{}", format!("✗ Unknown option: {}", args[i]).red().bold());
                    show_usage();
                    std::process::exit(1);
                }
            }
            i += 1;
        }

        // 处理 autostart 相关操作
        if let Some(action) = autostart_action {
            if !web_enabled {
                println!("{}", "✗ --autostart requires --web option".red().bold());
                std::process::exit(1);
            }

            match action.as_str() {
                "remove" => {
                    match AutoCommand::autostart_remove() {
                        Ok(_) => {}
                        Err(e) => {
                            println!("{}", format!("✗ {}", e).red().bold());
                            std::process::exit(1);
                        }
                    }
                    return;
                }
                "status" => {
                    match AutoCommand::autostart_status() {
                        Ok(_) => {}
                        Err(e) => {
                            println!("{}", format!("✗ {}", e).red().bold());
                            std::process::exit(1);
                        }
                    }
                    return;
                }
                "install" => {
                    // 先安装 autostart，然后继续启动
                    match AutoCommand::autostart_install(web_port) {
                        Ok(_) => {}
                        Err(e) => {
                            println!("{}", format!("✗ {}", e).red().bold());
                            std::process::exit(1);
                        }
                    }
                    // 安装后继续启动 Web UI
                }
                _ => {}
            }
        }

        if web_enabled {
            // 使用异步版本（带 Web UI）
            match AutoCommand::execute_with_web(web_port) {
                Ok(_) => {}
                Err(e) => {
                    println!("{}", format!("✗ {}", e).red().bold());
                    std::process::exit(1);
                }
            }
        } else {
            // 使用同步版本（不带 Web UI）
            match AutoCommand::execute() {
                Ok(_) => {}
                Err(e) => {
                    println!("{}", format!("✗ {}", e).red().bold());
                    std::process::exit(1);
                }
            }
        }

        return;
    }

    // 处理 run clone 命令
    if args.len() >= 3 && arg1 == "run" && args[2] == "clone" {
        match CloneCommand::execute() {
            Ok(_) => {}
            Err(e) => {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
        }

        return;
    }

    // 处理 run test 命令
    if args.len() >= 3 && arg1 == "run" && args[2] == "test" {
        match TestCommand::execute() {
            Ok(_) => {}
            Err(e) => {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
        }

        return;
    }

    // 处理 run ve 命令（Volcengine TTS 测试）
    if args.len() >= 3 && arg1 == "run" && args[2] == "ve" {
        match VeCommand::execute() {
            Ok(_) => {}
            Err(e) => {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
        }

        return;
    }

    // 处理 run s5 命令
    if args.len() >= 3 && arg1 == "run" && args[2] == "s5" {
        let mut interactive = false;

        // 检查交互模式标志
        if args.len() >= 4 {
            let arg3 = &args[3];
            if arg3 == "-i" || arg3 == "--interactive" {
                interactive = true;
            } else {
                println!("{}", format!("✗ Unknown option: {}", arg3).red().bold());
                show_usage();
                std::process::exit(1);
            }
        }

        match S5Command::execute(interactive) {
            Ok(_) => {}
            Err(e) => {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
        }

        return;
    }

    // 处理 init 命令
    if arg1 == "init" {
        if args.len() < 3 {
            println!("{}", "✗ Repository specification required".red().bold());
            println!(
                "{}",
                format!("ℹ Usage: {} init @username/repo", PROGRAM_NAME)
                    .blue()
                    .bold()
            );
            std::process::exit(1);
        }

        let repo_spec = &args[2];
        match GitHubInitCommand::execute(repo_spec) {
            Ok(_) => {}
            Err(e) => {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
        }

        return;
    }

    // 无效参数
    show_usage();
    std::process::exit(1);
}
