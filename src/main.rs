//! maid: measure known places on disk and clean them through their owners.

mod app;
mod cleanup;
mod config;
mod exec;
mod output;
mod par;
mod record;
mod report;
mod size;
mod survey;
mod sys;
mod target;
mod unknown;
mod worktree;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use app::{App, UsageError};
use output::Format;

#[derive(Parser)]
#[command(version, about = "Free disk space through the tools that own it")]
struct Cli {
    /// Config file [default: $XDG_CONFIG_HOME/maid/config.toml]
    #[arg(short, long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Output format (html is not implemented yet)
    #[arg(short, long, global = true, value_enum, default_value_t = Format::Text)]
    format: Format,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Measure targets and worktrees and decide what to clean. Changes nothing.
    Survey,
    /// Survey, then run the clean command of each item with decision "clean".
    Cleanup(cleanup::Args),
    /// Print the last run.
    Report,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("maid: {e:#}");
            ExitCode::from(if e.is::<UsageError>() { 2 } else { 1 })
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let app = App::new(cli.config, cli.format)?;
    match cli.command {
        Command::Survey => survey::run(&app),
        Command::Cleanup(args) => cleanup::run(&app, &args),
        Command::Report => report::run(&app),
    }
}
