//! maid: measure known places on disk and clean them through their owners.

mod config;
mod exec;
mod output;
mod run;
mod size;
mod sys;
mod unknown;
mod worktree;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use run::Outcome;

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
    Cleanup {
        /// Show what would run and run the dry_run commands. Changes nothing.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Clean only the target or worktree rule with this name (repeatable).
        /// "worktree" selects the worktrees that no override matches.
        #[arg(long, value_name = "NAME")]
        only: Vec<String>,
    },
    /// Print the last run.
    Report,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
    Html,
}

/// An error in how maid was called or configured: exit code 2.
#[derive(Debug)]
struct UsageError(anyhow::Error);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for UsageError {}

fn usage<T>(r: Result<T>) -> Result<T> {
    r.map_err(|e| UsageError(e).into())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match real_main(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("maid: {e:#}");
            ExitCode::from(if e.is::<UsageError>() { 2 } else { 1 })
        }
    }
}

fn real_main(cli: Cli) -> Result<ExitCode> {
    if let Format::Html = cli.format {
        return usage(Err(anyhow::anyhow!("html output is not implemented yet")));
    }
    let home = sys::home()?;
    let run_file = run::run_file(&home);
    let config = || {
        let (path, required) = config::user_path(cli.config.as_deref(), &home);
        usage(config::load(&path, required))
    };

    let run = match &cli.command {
        Command::Report => {
            print(&run::load(&run_file)?, cli.format, &home);
            return Ok(ExitCode::SUCCESS);
        }
        Command::Survey => run::survey(&config()?, &home).finish(),
        Command::Cleanup { dry_run, only } => {
            let config = config()?;
            check_only(&config, only)?;
            let mut survey = run::survey(&config, &home);
            if *dry_run {
                survey.dry_run(only);
            } else {
                survey.clean(&config, &home, only);
            }
            survey.finish()
        }
    };

    run::save(&run, &run_file)?;
    print(&run, cli.format, &home);
    let failed = run.items.iter().any(|i| i.result == Outcome::Failed);
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn check_only(config: &config::Config, only: &[String]) -> Result<()> {
    let names = config.names();
    match only.iter().find(|name| !names.contains(&name.as_str())) {
        Some(name) => usage(Err(anyhow::anyhow!(
            "--only {name:?} matches no target or worktree rule (names: {})",
            names.join(", ")
        ))),
        None => Ok(()),
    }
}

fn print(run: &run::Run, format: Format, home: &std::path::Path) {
    match format {
        Format::Text => print!("{}", output::text(run, home, sys::color())),
        Format::Json => print!("{}", output::json(run)),
        Format::Html => unreachable!("html is rejected before the run"),
    }
}
