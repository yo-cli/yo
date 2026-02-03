// yo-file: File utilities (template cloning, etc.)

use colored::Colorize;
use std::env;
use yo_lib::commands::CloneCommand;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn show_version() {
    println!("yo-file version {}", VERSION);
}

fn show_usage() {
    println!("Usage: yo-file [OPTION] | [COMMAND]");
    println!("Options:");
    println!("  -v, --version          Show version information");
    println!("Commands:");
    println!("  clone                  Clone template with keyword replacement");
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

    // clone
    if arg1 == "clone" {
        if let Err(e) = CloneCommand::execute() {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    println!("{}", format!("✗ Unknown command: {}", arg1).red().bold());
    show_usage();
    std::process::exit(1);
}
