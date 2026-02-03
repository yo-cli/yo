// yo-ob: Placeholder for future features (Linux musl)

use std::env;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn show_version() {
    println!("yo-ob version {}", VERSION);
}

fn show_usage() {
    println!("yo-ob v{}", VERSION);
    println!();
    println!("This binary is a placeholder for future features.");
    println!("Stay tuned for updates!");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() >= 2 {
        let arg1 = &args[1];

        if arg1 == "-v" || arg1 == "--version" {
            show_version();
            return;
        }
    }

    show_usage();
}
