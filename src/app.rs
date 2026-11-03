//! What every command shares: the home folder, the output format and the
//! config.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;

use crate::config::{self, Config};
use crate::output::{self, Format};
use crate::record::{self, Outcome, Record};
use crate::sys;

pub struct App {
    pub home: PathBuf,
    pub format: Format,
    /// The -c file, if given.
    config: Option<PathBuf>,
    /// The run file of this run.
    run_file: PathBuf,
}

impl App {
    pub fn new(config: Option<PathBuf>, format: Format) -> Result<App> {
        if let Format::Html = format {
            return usage(Err(anyhow::anyhow!("html output is not implemented yet")));
        }
        let home = sys::home()?;
        Ok(App {
            run_file: record::new_path(&record::dir(&home)),
            home,
            format,
            config,
        })
    }

    pub fn config_path(&self) -> PathBuf {
        config::path(self.config.as_deref(), &self.home)
    }

    /// The config file, checked. An error is a usage error.
    pub fn config(&self) -> Result<Config> {
        usage(config::load(&self.config_path()))
    }

    pub fn runs_dir(&self) -> PathBuf {
        record::dir(&self.home)
    }

    pub fn print(&self, record: &Record) {
        print!("{}", output::render(record, self.format, &self.home));
    }

    /// Save the run file of this run, and delete old run files.
    pub fn save(&self, record: &Record) -> Result<()> {
        record::save(record, &self.run_file)?;
        if let Err(e) = record::prune(&self.runs_dir()) {
            eprintln!("maid: warning: {e:#}");
        }
        Ok(())
    }

    /// Save the run file and print it. Exit code 1 when an item failed,
    /// unless a program in its `needs` was missing.
    pub fn finish(&self, record: Record) -> Result<ExitCode> {
        self.save(&record)?;
        self.print(&record);
        // A target that misses a program in `needs` was expected to fail:
        // the run warns about it, and the item says to run `maid doctor`.
        let failed = record
            .items
            .iter()
            .any(|i| i.result == Outcome::Failed && i.missing_programs.is_empty());
        Ok(if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }
}

/// Progress goes to stderr, so stdout holds only the result.
pub fn progress(message: &str) {
    eprintln!("maid: {message}");
}

/// A warning goes to stderr and to the run file.
pub fn warn(warnings: &mut Vec<String>, message: String) {
    eprintln!("maid: warning: {message}");
    warnings.push(message);
}

/// An error in how maid was called or configured: exit code 2.
#[derive(Debug)]
pub struct UsageError(anyhow::Error);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for UsageError {}

pub fn usage<T>(r: Result<T>) -> Result<T> {
    r.map_err(|e| UsageError(e).into())
}
