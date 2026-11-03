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
}

impl App {
    pub fn new(config: Option<PathBuf>, format: Format) -> Result<App> {
        if let Format::Html = format {
            return usage(Err(anyhow::anyhow!("html output is not implemented yet")));
        }
        Ok(App {
            home: sys::home()?,
            format,
            config,
        })
    }

    /// The defaults merged with the user file. An error is a usage error.
    pub fn config(&self) -> Result<Config> {
        let (path, required) = config::user_path(self.config.as_deref(), &self.home);
        usage(config::load(&path, required))
    }

    pub fn run_file(&self) -> PathBuf {
        record::path(&self.home)
    }

    pub fn print(&self, record: &Record) {
        print!("{}", output::render(record, self.format, &self.home));
    }

    /// Save the run file and print it. Exit code 1 when an item failed.
    pub fn finish(&self, record: Record) -> Result<ExitCode> {
        record::save(&record, &self.run_file())?;
        self.print(&record);
        let failed = record.items.iter().any(|i| i.result == Outcome::Failed);
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
