// yo-git: GitHub SSH key management (Linux musl)

use colored::Colorize;
use std::env;
use yo_lib::commands::{GitHubInitCommand, InitMode};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn show_version() {
    println!("yo-git version {}", VERSION);
}

fn show_usage() {
    println!("Usage: yo-git [OPTIONS] @username/repo");
    println!();
    println!("Options:");
    println!("  -v, --version          Show version information");
    println!("  -h, --help             Show this help message");
    println!("  --https                Use HTTPS + Token instead of SSH deploy key");
    println!("  --ssh                  Use SSH deploy key (default)");
    println!();
    println!("Examples:");
    println!("  yo-git @myuser/myrepo           # SSH deploy key (default)");
    println!("  yo-git @myuser/myrepo --https   # HTTPS with token");
    println!("  yo-git @myuser/myrepo --ssh     # SSH deploy key (explicit)");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        show_usage();
        std::process::exit(1);
    }

    // Parse arguments
    let mut repo_spec: Option<String> = None;
    let mut mode = InitMode::Interactive; // Default to interactive selection
    let mut show_help = false;
    let mut show_ver = false;

    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "-v" | "--version" => show_ver = true,
            "-h" | "--help" => show_help = true,
            "--https" => mode = InitMode::Https,
            "--ssh" => mode = InitMode::Ssh,
            s if s.starts_with('@') => repo_spec = Some(s.to_string()),
            _ => {
                println!("{}", format!("✗ Unknown option: {}", arg).red().bold());
                std::process::exit(1);
            }
        }
    }

    if show_ver {
        show_version();
        return;
    }

    if show_help {
        show_usage();
        return;
    }

    show_version();
    println!();

    // Handle @username/repo format
    if let Some(spec) = repo_spec {
        if let Err(e) = GitHubInitCommand::execute(&spec, mode) {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    println!(
        "{}",
        "✗ Repository specification required".red().bold()
    );
    println!("{}", "ℹ Usage: yo-git @username/repo [--https|--ssh]".blue());
    std::process::exit(1);
}
