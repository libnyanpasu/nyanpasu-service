use clap::{Parser, Subcommand};

use crate::logging;

mod install;

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
    #[error("other error: {0}")]
    Other(#[from] anyhow::Error),
}

pub fn parse() -> Result<(), CommandError> {
    let cli = Cli::parse();
    if !matches!(cli.command, Some(Commands::Status) | None) && !crate::utils::must_check_elevation() {
        return Err(CommandError::PermissionDenied);
    }
    if matches!(cli.command, Some(Commands::Server)) {
        logging::init(cli.debug, true)?;
    } else {
        logging::init(cli.debug, false)?;
    }

    match cli.command {
        Some(Commands::Install(ctx)) => Ok(install::install(ctx)?),
        None => {
            eprintln!("No command specified");
            Ok(())
        }
        _ => {
            unimplemented!("Command not implemented");
        }
    }
}
