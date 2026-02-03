// yo-git: GitHub SSH key management (Linux musl)

use colored::Colorize;
use std::env;
use yo_lib::commands::GitHubInitCommand;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn show_version() {
    println!("yo-git version {}", VERSION);
}

fn show_usage() {
    println!("Usage: yo-git [OPTION] | @username/repo");
    println!("Options:");
    println!("  -v, --version          Show version information");
    println!("  @username/repo         Initialize GitHub SSH deploy key");
    println!();
    println!("Example:");
    println!("  yo-git @myuser/myrepo");
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

    // Handle @username/repo format
    if arg1.starts_with('@') {
        if let Err(e) = GitHubInitCommand::execute(arg1) {
            println!("{}", format!("✗ {}", e).red().bold());
            std::process::exit(1);
        }
        return;
    }

    println!(
        "{}",
        "✗ Repository specification required".red().bold()
    );
    println!("{}", "ℹ Usage: yo-git @username/repo".blue());
    std::process::exit(1);
}
