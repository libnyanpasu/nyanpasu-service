use crate::logging;
use clap::{Parser, Subcommand};
use std::backtrace::Backtrace;

mod install;
mod restart;
mod server;
mod start;
mod status;
mod stop;
mod uninstall;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[clap(long, default_value = "false")]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Install(install::InstallCommand),
    Uninstall,
    Start,
    Stop,
    Restart,
    Server(server::ServerContext), // The main entry point for the service, other commands are the control plane for the service
    Status(status::StatusCommand),
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
    #[error("io error: {source}")]
    IO {
        #[from]
        source: std::io::Error,
        backtrace: Backtrace,
    },
    #[error("serde error: {0}")]
    SimdError(#[from] simd_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub async fn process() -> Result<(), CommandError> {
    let cli = Cli::parse();
    if !matches!(cli.command, Some(Commands::Status(_)) | None)
        && !crate::utils::must_check_elevation()
    {
        return Err(CommandError::PermissionDenied);
    }
    if matches!(cli.command, Some(Commands::Server(_))) {
        logging::init(cli.verbose, true)?;
    } else {
        logging::init(cli.verbose, false)?;
    }

    ctrlc::set_handler(|| {
        tracing::info!("ctrl-c received, exiting...");
        std::process::exit(0);
    })
    .unwrap();

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
        None => {
            eprintln!("No command specified");
            Ok(())
        }
    }
}
