//! `maid doctor`: check that maid can run on this computer and that every
//! target in the config can be used. It prints each check and a summary of
//! the newest run, as text or JSON, and exits with 1 when a check fails.

use std::fmt::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use serde::Serialize;

use crate::app::App;
use crate::config::Config;
use crate::output::Format;
use crate::record::{self, Decision, Outcome, Record, RunKind};
use crate::{size, sys};

pub fn run(app: &App) -> Result<ExitCode> {
    let health = Health::of(app);
    match app.format {
        Format::Text => print!("{}", health.text()),
        Format::Json => println!("{}", serde_json::to_string_pretty(&health)?),
        Format::Html => unreachable!("only report accepts html"),
    }
    Ok(if health.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// What doctor found. This is its JSON output.
#[derive(Serialize)]
struct Health {
    /// False when a check failed.
    ok: bool,
    checks: Vec<Check>,
    /// The newest run, if there is one.
    last_run: Option<LastRun>,
}

#[derive(Serialize)]
struct Check {
    level: Level,
    check: &'static str,
    detail: String,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Level {
    Ok,
    Warn,
    Fail,
}

/// The newest run, in short.
#[derive(Serialize)]
struct LastRun {
    file: String,
    mode: RunKind,
    steps: Vec<RunKind>,
    started_at: String,
    finished_at: Option<String>,
    done: usize,
    failed: usize,
    ask: usize,
    freed_bytes: u64,
}

impl Health {
    fn of(app: &App) -> Health {
        let mut checks = Checks {
            app,
            list: Vec::new(),
        };
        let config = checks.config();
        checks.state();
        let last_run = checks.last_run();
        // survey and cleanup cannot work without these.
        for program in ["sh", "du", "pgrep"] {
            match sys::find_program(program) {
                Some(path) => checks.add(Level::Ok, program, checks.tilde(&path)),
                None => checks.add(Level::Fail, program, "not found on PATH".into()),
            }
        }
        if let Some(config) = &config {
            checks.targets(config);
            checks.worktrees(config);
        }
        Health {
            ok: !checks.list.iter().any(|c| c.level == Level::Fail),
            checks: checks.list,
            last_run,
        }
    }

    fn text(&self) -> String {
        let mut out = String::new();
        for c in &self.checks {
            let level = match c.level {
                Level::Ok => "ok",
                Level::Warn => "warn",
                Level::Fail => "FAIL",
            };
            writeln!(out, "{level:<5} {:<9} {}", c.check, c.detail).expect("writing to a String");
        }
        out
    }
}

struct Checks<'a> {
    app: &'a App,
    list: Vec<Check>,
}

impl Checks<'_> {
    fn add(&mut self, level: Level, check: &'static str, detail: String) {
        self.list.push(Check {
            level,
            check,
            detail,
        });
    }

    fn config(&mut self) -> Option<Config> {
        match self.app.config() {
            Ok(config) => {
                self.add(Level::Ok, "config", self.tilde(&self.app.config_path()));
                Some(config)
            }
            Err(e) => {
                self.add(Level::Fail, "config", format!("{e:#}"));
                None
            }
        }
    }

    /// maid must be able to write the run files.
    fn state(&mut self) {
        let dir = self.app.runs_dir();
        match writable(&dir) {
            Ok(()) => self.add(Level::Ok, "state", self.tilde(&dir)),
            Err(e) => {
                let detail = format!("cannot write {}: {e}", self.tilde(&dir));
                self.add(Level::Fail, "state", detail);
            }
        }
    }

    fn last_run(&mut self) -> Option<LastRun> {
        let Ok(path) = record::latest(&self.app.runs_dir()) else {
            self.add(Level::Ok, "last run", "none yet".into());
            return None;
        };
        let run = match record::load(&path) {
            Ok(run) => run,
            Err(e) => {
                self.add(Level::Warn, "last run", format!("{e:#}"));
                return None;
            }
        };
        let last = LastRun::of(&path, run);
        let (level, detail) = last.check();
        self.add(level, "last run", detail);
        Some(last)
    }

    /// Every target must be usable: the programs it needs are on PATH.
    fn targets(&mut self, config: &Config) {
        for t in &config.targets {
            let missing = t.missing_programs();
            if t.needs.is_none() {
                let detail = format!(
                    "{}: no `needs`, so doctor cannot check it (use `needs = []` if it needs no program)",
                    t.name
                );
                self.add(Level::Warn, "target", detail);
            } else if missing.is_empty() {
                self.add(Level::Ok, "target", t.name.clone());
            } else {
                let detail = format!(
                    "{}: {} not on PATH (install it, or remove the target)",
                    t.name,
                    missing.join(", ")
                );
                self.add(Level::Fail, "target", detail);
            }
        }
    }

    /// Without wt and git, survey asks about each repo and cleans no worktree.
    fn worktrees(&mut self, config: &Config) {
        let repos = &config.worktrees.repos;
        if repos.is_empty() {
            return;
        }
        for program in ["git", "wt"] {
            match sys::find_program(program) {
                Some(path) => self.add(Level::Ok, program, self.tilde(&path)),
                None => {
                    let detail = "not found on PATH: maid cannot clean worktrees".into();
                    self.add(Level::Warn, program, detail);
                }
            }
        }
        for repo in repos {
            let dir = sys::expand(repo, &self.app.home);
            if dir.is_dir() {
                self.add(Level::Ok, "repos", self.tilde(&dir));
            } else {
                self.add(Level::Warn, "repos", format!("{repo} is not a folder"));
            }
        }
    }

    fn tilde(&self, path: &Path) -> String {
        sys::tilde(&path.to_string_lossy(), &self.app.home)
    }
}

impl LastRun {
    fn of(path: &Path, run: Record) -> LastRun {
        let results = |r: Outcome| run.items.iter().filter(|i| i.result == r).count();
        LastRun {
            file: path.to_string_lossy().into_owned(),
            done: results(Outcome::Done),
            failed: results(Outcome::Failed),
            ask: run
                .items
                .iter()
                .filter(|i| i.decision == Decision::Ask)
                .count(),
            freed_bytes: run.items.iter().filter_map(|i| i.freed_bytes).sum(),
            mode: run.kind,
            steps: run.steps,
            started_at: run.started_at,
            finished_at: run.finished_at,
        }
    }

    /// A stopped run or a failed item is a warning: see `maid report`.
    fn check(&self) -> (Level, String) {
        let command = self.mode.command();
        let Some(at) = &self.finished_at else {
            let detail = format!(
                "{command} started at {} did not finish (ran: {})",
                self.started_at,
                record::steps_text(&self.steps)
            );
            return (Level::Warn, detail);
        };
        let mut detail = format!("{command} at {at}: {} done", self.done);
        if self.done > 0 {
            write!(detail, " ({} freed)", size::human(self.freed_bytes))
                .expect("writing to a String");
        }
        write!(detail, ", {} failed, {} ask", self.failed, self.ask).expect("writing to a String");
        if self.failed > 0 {
            detail.push_str(" (see `maid report`)");
            return (Level::Warn, detail);
        }
        (Level::Ok, detail)
    }
}

/// Create the folder if needed, then write and delete a test file in it.
fn writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    // A name with a dot first, so it is never taken for a run file.
    let probe = dir.join(format!(".doctor.{}", std::process::id()));
    std::fs::write(&probe, "")?;
    std::fs::remove_file(&probe)
}
