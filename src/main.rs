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

use run::{Mode, Outcome};

#[derive(Parser)]
#[command(version, about = "Free disk space through the tools that own it")]
struct Cli {
    /// Config file [default: $XDG_CONFIG_HOME/maid/config.toml]
    #[arg(short, long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Measure targets and worktrees and decide what to clean. Changes nothing.
    Survey {
        /// Output format
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Survey, then run the clean command of each item with decision "clean".
    Cleanup {
        /// Output format
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Show what would run and run the dry_run commands. Changes nothing.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Clean only the target or worktree override with this name (repeatable)
        #[arg(long, value_name = "NAME")]
        only: Vec<String>,
    },
    /// Print the last run.
    Report {
        /// Output format (html is not implemented yet)
        #[arg(short, long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
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
    let home = sys::home()?;
    let run_file = run::run_file(&home);

    let (format, mode, only) = match cli.command {
        Command::Report { format } => {
            let format = match format {
                ReportFormat::Text => Format::Text,
                ReportFormat::Json => Format::Json,
                ReportFormat::Html => {
                    return usage(Err(anyhow::anyhow!("html output is not implemented yet")));
                }
            };
            print(&run::load(&run_file)?, format, &home);
            return Ok(ExitCode::SUCCESS);
        }
        Command::Survey { format } => (format, Mode::Survey, Vec::new()),
        Command::Cleanup {
            format,
            dry_run,
            only,
        } => (
            format,
            if dry_run { Mode::DryRun } else { Mode::Cleanup },
            only,
        ),
    };

    let (path, required) = config::user_path(cli.config.as_deref(), &home);
    let config = usage(config::load(&path, required))?;
    let names = config.names();
    for name in &only {
        if !names.contains(&name.as_str()) {
            return usage(Err(anyhow::anyhow!(
                "--only {name:?} matches no target or override (names: {})",
                names.join(", ")
            )));
        }
    }

    let run = run::execute(&config, &home, &run::Options { mode, only: &only })?;
    run::save(&run, &run_file)?;
    print(&run, format, &home);
    let failed = run.items.iter().any(|i| i.result == Outcome::Failed);
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn print(run: &run::Run, format: Format, home: &std::path::Path) {
    match format {
        Format::Text => print!("{}", output::text(run, home, sys::color())),
        Format::Json => print!("{}", output::json(run)),
    }
}
