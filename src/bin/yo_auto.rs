// yo: Task scheduler with TTS and Web UI (Windows)
// Supports both "yo run auto" and direct "yo --web" syntax

use colored::Colorize;
use std::env;
use yo_lib::commands::{AutoCommand, TestCommand, VeCommand};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn show_version() {
    println!("yo version {}", VERSION);
}

fn show_usage() {
    println!("Usage: yo run auto [OPTIONS]");
    println!("       yo run test");
    println!("       yo run ve");
    println!();
    println!("Options:");
    println!("  -v, --version                    Show version information");
    println!("  --web                            Start with Web UI (port 9999)");
    println!("  --web [port]                     Start with Web UI on custom port");
    println!("  --autostart                      Install autostart (Windows only)");
    println!("  --autostart remove               Remove autostart");
    println!("  --autostart status               Show autostart status");
}

fn run_auto(args: &[String], offset: usize) {
    // Check for options after "auto"
    let next_arg = args.get(offset).map(|s| s.as_str());

    match next_arg {
        Some("--web") => {
            let port = args
                .get(offset + 1)
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(9999);

            if let Err(e) = AutoCommand::execute_with_web(port) {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
        }
        Some("--autostart") => {
            let action = args.get(offset + 1).map(|s| s.as_str()).unwrap_or("install");

            let result = match action {
                "remove" => AutoCommand::autostart_remove(),
                "status" => AutoCommand::autostart_status(),
                _ => AutoCommand::autostart_install(),
            };

            if let Err(e) = result {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
        }
        _ => {
            // Default: run scheduler without web
            if let Err(e) = AutoCommand::execute() {
                println!("{}", format!("✗ {}", e).red().bold());
                std::process::exit(1);
            }
        }
    }
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

    if arg1 == "-h" || arg1 == "--help" {
        show_usage();
        return;
    }

    show_version();
    println!();

    // yo run <subcommand>
    if arg1 == "run" {
        let subcommand = args.get(2).map(|s| s.as_str());

        match subcommand {
            Some("auto") => run_auto(&args, 3),
            Some("test") => {
                if let Err(e) = TestCommand::execute() {
                    println!("{}", format!("✗ {}", e).red().bold());
                    std::process::exit(1);
                }
            }
            Some("ve") => {
                if let Err(e) = VeCommand::execute() {
                    println!("{}", format!("✗ {}", e).red().bold());
                    std::process::exit(1);
                }
            }
            _ => {
                println!("{}", "✗ Unknown subcommand".red().bold());
                println!("Available: auto, test, ve");
                std::process::exit(1);
            }
        }
        return;
    }

    show_usage();
    std::process::exit(1);
}
