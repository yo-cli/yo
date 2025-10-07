mod commands;
mod common;
mod github;
mod s5;

use colored::Colorize;
use commands::{GitHubInitCommand, S5Command};
use std::env;

const VERSION: &str = "1.0.0";
const PROGRAM_NAME: &str = "yo";

fn show_version() {
    println!("{} version {}", PROGRAM_NAME, VERSION);
}

fn show_usage() {
    println!("Usage: {} [OPTION] | run s5 [OPTIONS] | init @username/repo", PROGRAM_NAME);
    println!("Options:");
    println!("  -v, --version        Show version information");
    println!("  run s5               Start SOCKS5 proxy (automatic mode)");
    println!("  run s5 -i            Start SOCKS5 proxy (interactive mode)");
    println!("  run s5 --interactive Start SOCKS5 proxy (interactive mode)");
    println!("  init @username/repo  Initialize GitHub SSH keys for repository");
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
