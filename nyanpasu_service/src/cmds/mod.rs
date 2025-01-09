use crate::logging;
use clap::{Parser, Subcommand};

mod install;
mod restart;
mod rpc;
mod server;
mod start;
mod status;
mod stop;
mod uninstall;
mod update;

/// Nyanpasu Service, a privileged service for managing the core service.
///
/// The main entry point for the service, Other commands are the control plane for the service.
///
/// rpc subcommands are shortcuts for client rpc calls,
/// It is useful for testing and debugging service rpc calls.
#[derive(Parser)]
#[command(version, author, about, long_about, disable_version_flag = true)]
struct Cli {
    /// Enable verbose logging
    #[clap(short = 'V', long, default_value = "false")]
    verbose: bool,

    /// Print the version
    #[clap(short, long, default_value = "false")]
    version: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Install the service
    Install(install::InstallCommand),
    /// Uninstall the service
    Uninstall,
    /// Start the service
    Start,
    /// Stop the service
    Stop,
    /// Restart the service
    Restart,
    /// Run the server. It should be called by the service manager.
    Server(server::ServerContext), // The main entry point for the service, other commands are the control plane for the service
    /// Get the status of the service
    Status(status::StatusCommand),
    /// Update the service
    Update,
    /// RPC commands, a shortcut for client rpc calls
    #[command(subcommand)]
    Rpc(rpc::RpcCommand),
}

#[derive(thiserror::Error, Debug)]
pub enum CommandError {
    #[error("permission denied")]
    PermissionDenied,
    #[error("service not installed")]
    ServiceNotInstalled,
    #[error("service not running")]
    ServiceAlreadyInstalled,
    #[error("service not running")]
    ServiceAlreadyStopped,
    #[error("service already running")]
    ServiceAlreadyRunning,
    #[error("join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("io error: {0}")]
    IO(
        #[from]
        #[backtrace]
        std::io::Error,
    ),
    #[error("serde error: {0}")]
    SimdError(
        #[from]
        #[backtrace]
        simd_json::Error,
    ),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub async fn process() -> Result<(), CommandError> {
    let cli = Cli::parse();
    if cli.version {
        print_version();
    }

    if !matches!(
        cli.command,
        Some(Commands::Status(_)) | Some(Commands::Rpc(_)) | None
    ) && !crate::utils::must_check_elevation()
    {
        return Err(CommandError::PermissionDenied);
    }
    if matches!(cli.command, Some(Commands::Server(_))) {
        logging::init(cli.verbose, true)?;
    } else {
        logging::init(cli.verbose, false)?;
    }

    match cli.command {
        Some(Commands::Install(ctx)) => {
            Ok(tokio::task::spawn_blocking(move || install::install(ctx)).await??)
        }
        Some(Commands::Uninstall) => Ok(tokio::task::spawn_blocking(uninstall::uninstall).await??),
        Some(Commands::Start) => Ok(tokio::task::spawn_blocking(start::start).await??),
        Some(Commands::Stop) => Ok(tokio::task::spawn_blocking(stop::stop).await??),
        Some(Commands::Restart) => Ok(tokio::task::spawn_blocking(restart::restart).await??),
        Some(Commands::Server(ctx)) => {
            server::server(ctx).await?;
            Ok(())
        }
        Some(Commands::Status(ctx)) => Ok(status::status(ctx).await?),
        Some(Commands::Update) => {
            update::update().await?;
            Ok(())
        }
        Some(Commands::Rpc(ctx)) => {
            rpc::rpc(ctx).await?;
            Ok(())
        }
        None => {
            eprintln!("No command specified");
            Ok(())
        }
    }
}

pub fn print_version() {
    use crate::consts::*;
    use ansi_str::AnsiStr;
    use chrono::{DateTime, Utc};
    use colored::*;
    use timeago::Formatter;

    let now = Utc::now();
    let formatter = Formatter::new();
    let commit_time =
        formatter.convert_chrono(DateTime::parse_from_rfc3339(COMMIT_DATE).unwrap(), now);
    let commit_time_width = commit_time.len() + COMMIT_DATE.len() + 3;
    let build_time =
        formatter.convert_chrono(DateTime::parse_from_rfc3339(BUILD_DATE).unwrap(), now);
    let build_time_width = build_time.len() + BUILD_DATE.len() + 3;
    let commit_info_width = COMMIT_HASH.len() + COMMIT_AUTHOR.len() + 4;
    let col_width = commit_info_width
        .max(commit_time_width)
        .max(build_time_width)
        .max(BUILD_PLATFORM.len())
        .max(RUSTC_VERSION.len())
        .max(LLVM_VERSION.len())
        + 2;
    let header_width = col_width + 16;
    println!(
        "{} v{} ({} Build)\n",
        APP_NAME,
        APP_VERSION,
        BUILD_PROFILE.yellow()
    );
    println!("╭{:─^width$}╮", " Build Information ", width = header_width);

    let mut line = format!("{} by {}", COMMIT_HASH.green(), COMMIT_AUTHOR.blue());
    let mut pad = col_width - line.ansi_strip().len();
    println!("│{:>14}: {}{}│", "Commit Info", line, " ".repeat(pad));

    line = format!("{} ({})", commit_time.red(), COMMIT_DATE.cyan());
    pad = col_width - line.ansi_strip().len();
    println!("│{:>14}: {}{}│", "Commit Time", line, " ".repeat(pad));

    line = format!("{} ({})", build_time.red(), BUILD_DATE.cyan());
    pad = col_width - line.ansi_strip().len();
    println!("│{:>14}: {}{}│", "Build Time", line, " ".repeat(pad));

    println!(
        "│{:>14}: {:<col_width$}│",
        "Build Target",
        BUILD_PLATFORM.bright_yellow()
    );
    println!(
        "│{:>14}: {:<col_width$}│",
        "Rust Version",
        RUSTC_VERSION.bright_yellow()
    );
    println!(
        "│{:>14}: {:<col_width$}│",
        "LLVM Version",
        LLVM_VERSION.bright_yellow()
    );
    println!("╰{:─^width$}╯", "", width = header_width);
    std::process::exit(0);
}
