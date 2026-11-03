//! The run file: what a survey or cleanup found and did.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::sys;

/// What made a run: the command, and for cleanup, if it was a dry run.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum RunKind {
    Survey,
    DryRun,
    Cleanup,
}

impl RunKind {
    /// The maid command that makes this kind of run.
    pub fn command(self) -> &'static str {
        match self {
            RunKind::Survey => "survey",
            RunKind::DryRun => "cleanup --dry-run",
            RunKind::Cleanup => "cleanup",
        }
    }

    /// The name of this step in a list of steps.
    pub fn step(self) -> &'static str {
        match self {
            RunKind::Survey => "survey",
            RunKind::DryRun => "dry run",
            RunKind::Cleanup => "cleanup",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Target,
    Worktree,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Clean,
    Ask,
    Keep,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Failed,
    Done,
    Planned,
    Skipped,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Record {
    pub version: u32,
    pub host: String,
    /// Named "mode" in the run file.
    #[serde(rename = "mode")]
    pub kind: RunKind,
    /// The steps that finished, in order: the survey, then the cleanup or
    /// dry run. maid writes the run file during the run, so a run that was
    /// stopped shows how far it got.
    pub steps: Vec<RunKind>,
    pub started_at: String,
    /// None while the run goes on, or when it was stopped.
    pub finished_at: Option<String>,
    pub free_bytes_before: Option<u64>,
    pub free_bytes_after: Option<u64>,
    pub items: Vec<Item>,
    pub warnings: Vec<String>,
}

/// Finished steps as text, for example "survey, cleanup".
pub fn steps_text(steps: &[RunKind]) -> String {
    let names: Vec<_> = steps.iter().map(|s| s.step()).collect();
    if names.is_empty() {
        "nothing".into()
    } else {
        names.join(", ")
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub name: String,
    pub path: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub size_bytes: Option<u64>,
    pub decision: Decision,
    pub reason: Option<String>,
    pub commands: Vec<String>,
    pub result: Outcome,
    pub freed_bytes: Option<u64>,
    pub error: Option<String>,
    /// Programs in the target's `needs` that were not on PATH. maid still
    /// tries the target, but its failure does not fail the run.
    pub missing_programs: Vec<String>,
}

impl Item {
    pub fn new(kind: Kind, name: &str) -> Self {
        Item {
            kind,
            name: name.to_string(),
            path: None,
            repo: None,
            branch: None,
            size_bytes: None,
            decision: Decision::Keep,
            reason: None,
            commands: Vec::new(),
            result: Outcome::Skipped,
            freed_bytes: None,
            error: None,
            missing_programs: Vec::new(),
        }
    }

    pub fn decide(&mut self, decision: Decision, reason: Option<String>) {
        self.decision = decision;
        self.reason = reason;
        self.result = if decision == Decision::Clean {
            Outcome::Planned
        } else {
            Outcome::Skipped
        };
    }

    pub fn skip(&mut self, reason: String) {
        self.result = Outcome::Skipped;
        self.reason = Some(reason);
    }

    pub fn fail(&mut self, error: String) {
        self.result = Outcome::Failed;
        self.error = Some(error);
    }
}

/// How many run files maid keeps. After each run it deletes the older ones.
const KEEP: usize = 7;

/// The folder that holds one file for each run.
pub fn dir(home: &Path) -> PathBuf {
    sys::xdg("XDG_STATE_HOME", ".local/state", home).join("maid/runs")
}

/// A path for a new run file in `dir`.
pub fn new_path(dir: &Path) -> PathBuf {
    dir.join(format!("{}.json", sys::stamp()))
}

/// Write the run file: a temp file next to it, then rename.
pub fn save(record: &Record, path: &Path) -> Result<()> {
    let dir = path.parent().context("run file has no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = dir.join(format!(".run.json.{}", std::process::id()));
    let write = || -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(serde_json::to_string_pretty(record)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    write()
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .with_context(|| format!("cannot write {}", path.display()))
}

/// Delete all run files but the newest `KEEP`.
pub fn prune(dir: &Path) -> Result<()> {
    let runs = runs(dir)?;
    for path in &runs[..runs.len().saturating_sub(KEEP)] {
        std::fs::remove_file(path).with_context(|| format!("cannot delete {}", path.display()))?;
    }
    Ok(())
}

/// The newest run file.
pub fn latest(dir: &Path) -> Result<PathBuf> {
    runs(dir)?
        .pop()
        .with_context(|| format!("no run in {} (run `maid survey` first)", dir.display()))
}

/// The run files in `dir`, oldest first. Their names sort in time order.
fn runs(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut runs = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        // A temp file starts with a dot.
        if !name.starts_with('.') && name.ends_with(".json") {
            runs.push(dir.join(&*name));
        }
    }
    runs.sort();
    Ok(runs)
}

pub fn load(path: &Path) -> Result<Record> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("cannot read {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_the_newest_runs() {
        let dir = std::env::temp_dir().join(format!("maid-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names: Vec<String> = (0..10)
            .map(|n| format!("2026-11-0{n}T000000.0Z.json"))
            .collect();
        for name in names.iter().chain([&".run.json.1".to_string()]) {
            std::fs::write(dir.join(name), "{}").unwrap();
        }

        prune(&dir).unwrap();
        let left = runs(&dir).unwrap();
        let expected: Vec<PathBuf> = names[3..].iter().map(|n| dir.join(n)).collect();
        assert_eq!(left, expected);
        assert_eq!(latest(&dir).unwrap(), dir.join(&names[9]));
        assert!(dir.join(".run.json.1").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_runs_yet() {
        let err = latest(Path::new("/nonexistent/maid/runs")).unwrap_err();
        assert!(err.to_string().contains("run `maid survey` first"));
    }
}
