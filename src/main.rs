mod auto;
mod commands;
mod common;
mod github;
mod s5;

use colored::Colorize;
use commands::{AutoCommand, CloneCommand, GitHubInitCommand, S5Command};
use std::env;

const VERSION: &str = "1.0.0";
const PROGRAM_NAME: &str = "yo";

fn show_version() {
    println!("{} version {}", PROGRAM_NAME, VERSION);
}

fn show_usage() {
    println!("Usage: {} [OPTION] | run [COMMAND] | init @username/repo", PROGRAM_NAME);
    println!("Options:");
    println!("  -v, --version          Show version information");
    println!("  run auto               Start task scheduler (runs continuously)");
    println!("  run auto --web         Start task scheduler with Web UI (default port: 9999)");
    println!("  run auto --web [port]  Start task scheduler with Web UI on custom port");
    println!("  run clone              Clone template with keyword replacement");
    println!("  run s5                 Start SOCKS5 proxy (automatic mode)");
    println!("  run s5 -i              Start SOCKS5 proxy (interactive mode)");
    println!("  run s5 --interactive   Start SOCKS5 proxy (interactive mode)");
    println!("  init @username/repo    Initialize GitHub SSH keys for repository");
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

    // 处理 run auto 命令
    if args.len() >= 3 && arg1 == "run" && args[2] == "auto" {
        // 检查是否有 --web 参数
        let mut web_enabled = false;
        let mut web_port = 9999u16;

        if args.len() >= 4 && args[3] == "--web" {
            web_enabled = true;
            // 检查是否指定了端口
            if args.len() >= 5 {
                if let Ok(port) = args[4].parse::<u16>() {
                    web_port = port;
                } else {
                    println!("{}", format!("✗ Invalid port number: {}", args[4]).red().bold());
                    std::process::exit(1);
                }
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
