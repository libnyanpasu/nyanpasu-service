use anyhow::Result;
use clap::{Parser, Subcommand};

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
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
}

pub fn parse() -> Result<()> {
    let cli = Cli::parse();
    Ok(())
}
