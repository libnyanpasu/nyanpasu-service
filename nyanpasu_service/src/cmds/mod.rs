use clap::{Parser, Subcommand};

use crate::logging;

mod install;
mod restart;
mod start;
mod stop;
mod uninstall;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[clap(short, long)]
    debug: bool,
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
    Server, // The main entry point for the service, other commands are the control plane for the service
    Status,
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
    IO(#[from] std::io::Error),
    #[error("other error: {0}")]
    Other(#[from] anyhow::Error),
}

pub async fn process() -> Result<(), CommandError> {
    let cli = Cli::parse();
    if !matches!(cli.command, Some(Commands::Status) | None)
        && !crate::utils::must_check_elevation()
    {
        return Err(CommandError::PermissionDenied);
    }
    if matches!(cli.command, Some(Commands::Server)) {
        logging::init(cli.debug, true)?;
    } else {
        logging::init(cli.debug, false)?;
    }

    match cli.command {
        Some(Commands::Install(ctx)) => {
            Ok(tokio::task::spawn_blocking(move || install::install(ctx)).await??)
        }
        Some(Commands::Uninstall) => Ok(tokio::task::spawn_blocking(uninstall::uninstall).await??),
        Some(Commands::Start) => Ok(tokio::task::spawn_blocking(start::start).await??),
        Some(Commands::Stop) => Ok(tokio::task::spawn_blocking(stop::stop).await??),
        Some(Commands::Restart) => Ok(tokio::task::spawn_blocking(restart::restart).await??),
        None => {
            eprintln!("No command specified");
            Ok(())
        }
        _ => {
            unimplemented!("Command not implemented");
        }
    }
}
