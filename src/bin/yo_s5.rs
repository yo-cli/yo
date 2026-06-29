// yo-s5: SOCKS5 proxy service (Linux musl)

use colored::Colorize;
use std::env;
use yo_lib::commands::S5Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn show_version() {
    println!("yo-s5 version {}", VERSION);
}

fn show_usage() {
    println!("Usage: yo-s5 [OPTION]");
    println!("Options:");
    println!("  -v, --version          Show version information");
    println!("  -i, --interactive      Start in interactive mode");
    println!();
    println!("Without options, starts SOCKS5 + HTTP proxy (same port) in automatic mode.");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut interactive = false;

    if args.len() >= 2 {
        let arg1 = &args[1];

        if arg1 == "-v" || arg1 == "--version" {
            show_version();
            return;
        }

        if arg1 == "-h" || arg1 == "--help" {
            show_usage();
            return;
        }

        if arg1 == "-i" || arg1 == "--interactive" {
            interactive = true;
        } else {
            println!("{}", format!("✗ Unknown option: {}", arg1).red().bold());
            show_usage();
            std::process::exit(1);
        }
    }

    show_version();
    println!();

    if let Err(e) = S5Command::execute(interactive) {
        println!("{}", format!("✗ {}", e).red().bold());
        std::process::exit(1);
    }
}
