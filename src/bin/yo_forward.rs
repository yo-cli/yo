// yo-forward: route local traffic through an upstream SOCKS5 via a local gost

use clap::{Parser, Subcommand};
use colored::Colorize;

use yo_lib::forward::{commands, ForwardConfig};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "yo-forward")]
#[command(
    about = format!("Local forward-proxy client: route traffic through an upstream SOCKS5 (v{})", VERSION),
    long_about = None
)]
#[command(version = VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Upstream socks5 exit host:port (default 127.0.0.1:30999)
    #[arg(long, global = true)]
    upstream: Option<String>,

    /// Local http proxy port (default 8888)
    #[arg(long, global = true)]
    port: Option<u16>,
}

#[derive(Subcommand)]
enum Commands {
    /// Configure and start the local forward proxy (default action)
    Up {
        /// Skip all confirmations
        #[arg(short, long)]
        force: bool,
    },
    /// Check each link in the proxy chain
    Check,
    /// Stop and remove the local forward proxy
    Down,
}

fn main() {
    let cli = Cli::parse();

    println!("{} {}\n", "yo-forward version:".cyan(), VERSION);

    let config = ForwardConfig::new(cli.upstream, cli.port);

    let result = match cli.command {
        Some(Commands::Up { force }) => commands::up::run(config, force),
        Some(Commands::Check) => commands::check::run(config),
        Some(Commands::Down) => commands::down::run(),
        None => commands::up::run(config, false), // 无子命令 = up（零思考默认）
    };

    if let Err(e) = result {
        eprintln!("{}", format!("✗ {}", e).red().bold());
        std::process::exit(1);
    }
}
