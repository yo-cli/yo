// yo-ob: OceanBase environment preparation tool (Linux only)

use clap::{Parser, Subcommand};
use colored::Colorize;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "yo-ob")]
#[command(about = format!("OceanBase operations tool (v{})", VERSION), long_about = None)]
#[command(version = VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Prepare OceanBase environment
    Prepare {
        /// Force mode, skip all confirmations
        #[arg(short, long)]
        force: bool,
    },
    /// Check OceanBase configuration
    Check,
}

fn main() {
    let cli = Cli::parse();

    println!("{} {}\n", "yo-ob version:".cyan(), VERSION);

    let result = match cli.command {
        Commands::Prepare { force } => yo_lib::ob::commands::prepare::run(force),
        Commands::Check => yo_lib::ob::commands::check::run(),
    };

    if let Err(e) = result {
        eprintln!("{}", format!("✗ {}", e).red().bold());
        std::process::exit(1);
    }
}
