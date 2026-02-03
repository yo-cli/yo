// yo-auto: Task scheduler with TTS and Web UI (Windows only)

use colored::Colorize;
use std::env;
use yo_lib::commands::{AutoCommand, TestCommand, VeCommand};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn show_version() {
    println!("yo-auto version {}", VERSION);
}

fn show_usage() {
    println!("Usage: yo-auto [OPTION] | [COMMAND]");
    println!("Options:");
    println!("  -v, --version                    Show version information");
    println!("  --web                            Start with Web UI (port 9999)");
    println!("  --web [port]                     Start with Web UI on custom port");
    println!("  --autostart                      Install autostart (Windows only)");
    println!("  --autostart remove               Remove autostart");
    println!("  --autostart status               Show autostart status");
    println!("  test                             Test hourly chime playback");
    println!("  ve                               Test Volcengine TTS synthesis");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        // Default: run scheduler without web
        show_version();
        println!();

        if let Err(e) = AutoCommand::execute() {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
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

    // --autostart
    if arg1 == "--autostart" {
        let action = args.get(2).map(|s| s.as_str()).unwrap_or("install");

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

    // --web
    if arg1 == "--web" {
        let port = args
            .get(2)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(9999);

        if let Err(e) = AutoCommand::execute_with_web(port) {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    // test
    if arg1 == "test" {
        if let Err(e) = TestCommand::execute() {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    // ve
    if arg1 == "ve" {
        if let Err(e) = VeCommand::execute() {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    show_usage();
    std::process::exit(1);
}
