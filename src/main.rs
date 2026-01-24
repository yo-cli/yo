mod auto;
mod commands;
mod common;
mod github;
mod s5;

use colored::Colorize;
use commands::{AutoCommand, CloneCommand, GitHubInitCommand, S5Command, TestCommand, VeCommand};
use std::env;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PROGRAM_NAME: &str = env!("CARGO_PKG_NAME");

fn show_version() {
    println!("{} version {}", PROGRAM_NAME, VERSION);
}

fn show_usage() {
    println!(
        "Usage: {} [OPTION] | run [COMMAND] | init @username/repo",
        PROGRAM_NAME
    );
    println!("Options:");
    println!("  -v, --version                    Show version information");
    println!("  run auto                         Start Rhai scheduler");
    println!("  run auto --web                   Start with Web UI (port 9999)");
    println!("  run auto --web [port]            Start with Web UI on custom port");
    println!("  run auto --autostart             Install autostart (Windows only)");
    println!("  run auto --autostart remove      Remove autostart");
    println!("  run auto --autostart status      Show autostart status");
    println!("  run clone                        Clone template with keyword replacement");
    println!("  run s5                           Start SOCKS5 proxy (automatic mode)");
    println!("  run s5 -i                        Start SOCKS5 proxy (interactive mode)");
    println!("  run test                         Test hourly chime playback");
    println!("  run ve                           Test Volcengine TTS synthesis");
    println!("  init @username/repo              Initialize GitHub SSH keys");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        show_usage();
        std::process::exit(1);
    }

    let arg1 = &args[1];

    if arg1 == "-v" || arg1 == "--version" {
        show_version();
        return;
    }

    show_version();
    println!();

    // run auto
    if args.len() >= 3 && arg1 == "run" && args[2] == "auto" {
        // Check for --autostart
        if args.len() >= 4 && args[3] == "--autostart" {
            let action = args.get(4).map(|s| s.as_str()).unwrap_or("install");

            let result = match action {
                "remove" => AutoCommand::autostart_remove(),
                "status" => AutoCommand::autostart_status(),
                _ => AutoCommand::autostart_install(),
            };

            if let Err(e) = result {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
            return;
        }

        // Check for --web
        if args.len() >= 4 && args[3] == "--web" {
            let port = args
                .get(4)
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(9999);

            if let Err(e) = AutoCommand::execute_with_web(port) {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
            return;
        }

        // Run scheduler without web
        if let Err(e) = AutoCommand::execute() {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    // run clone
    if args.len() >= 3 && arg1 == "run" && args[2] == "clone" {
        if let Err(e) = CloneCommand::execute() {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    // run test
    if args.len() >= 3 && arg1 == "run" && args[2] == "test" {
        if let Err(e) = TestCommand::execute() {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    // run ve
    if args.len() >= 3 && arg1 == "run" && args[2] == "ve" {
        if let Err(e) = VeCommand::execute() {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    // run s5
    if args.len() >= 3 && arg1 == "run" && args[2] == "s5" {
        let interactive = args
            .get(3)
            .map(|a| a == "-i" || a == "--interactive")
            .unwrap_or(false);

        if args.len() >= 4 && !interactive {
            println!("{}", format!("✗ Unknown option: {}", args[3]).red().bold());
            show_usage();
            std::process::exit(1);
        }

        if let Err(e) = S5Command::execute(interactive) {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    // init
    if arg1 == "init" {
        if args.len() < 3 {
            println!("{}", "✗ Repository specification required".red().bold());
            println!(
                "{}",
                format!("ℹ Usage: {} init @username/repo", PROGRAM_NAME).blue()
            );
            std::process::exit(1);
        }

        if let Err(e) = GitHubInitCommand::execute(&args[2]) {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    show_usage();
    std::process::exit(1);
}
