// yo-s3: burn a specified amount of AWS cost, controllably, by writing large
// objects to S3 with Cross-Region Replication as the cost engine.

use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing_subscriber::EnvFilter;

use yo_lib::s3::commands::args::{CleanupArgs, RunArgs, SetupCrrArgs};
use yo_lib::s3::commands::{cleanup, run, setup_crr};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "yo-s3")]
#[command(
    about = format!("按预算可控地消耗 AWS 成本:S3 大对象写入 + 跨区复制流量 (v{})", VERSION),
    long_about = None
)]
#[command(version = VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Commands {
    /// 按预算烧钱(默认动作)
    Run(RunArgs),
    /// 一键配置跨区复制(目标桶 + 版本控制 + 复制角色和规则)
    SetupCrr(SetupCrrArgs),
    /// 手动清理:abort 残留分段上传 + 物理删除本工具前缀下的对象
    Cleanup(CleanupArgs),
}

#[tokio::main]
async fn main() {
    // Runtime events go through tracing (RUST_LOG-controlled, default warn so
    // the progress bar stays clean); user-facing output uses plain println.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with_writer(std::io::stderr)
        .init();

    println!("{} {}\n", "yo-s3 version:".cyan(), VERSION);

    let cli = Cli::parse();
    let result = match cli.command {
        Some(Commands::Run(args)) => run::run(args).await,
        Some(Commands::SetupCrr(args)) => setup_crr::run(args).await,
        Some(Commands::Cleanup(args)) => cleanup::run(args).await,
        None => run::run(cli.run).await, // 无子命令 = run(零思考默认)
    };

    if let Err(e) = result {
        eprintln!("{}", format!("✗ {:#}", e).red().bold());
        std::process::exit(1);
    }
}
