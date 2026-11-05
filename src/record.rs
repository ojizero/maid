//! The run file: what a survey or cleanup found and did.

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

/// Serialize run writes, pruning and report generation in this state folder.
/// The returned file holds the lock until it is dropped.
pub fn lock(dir: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join(".lock"))
        .with_context(|| format!("cannot open state lock in {}", dir.display()))?;
    file.lock()
        .with_context(|| format!("cannot lock {}", dir.display()))?;
    Ok(file)
}

/// Write the run file: a temp file next to it, then rename.
pub fn save(record: &Record, path: &Path) -> Result<()> {
    let dir = path.parent().context("run file has no folder")?;
    let _lock = lock(dir)?;
    remove_html(path)?;
    sys::write_atomic(
        path,
        (serde_json::to_string_pretty(record)? + "\n").as_bytes(),
    )
}

/// Remove the cached report when its run changes or expires.
fn remove_html(path: &Path) -> Result<()> {
    let html = path.with_extension("html");
    match std::fs::remove_file(&html) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("cannot delete {}", html.display())),
    }
}

/// Delete all run files but the newest `KEEP`.
pub fn prune(dir: &Path) -> Result<()> {
    let _lock = lock(dir)?;
    let runs = runs(dir)?;
    for path in &runs[..runs.len().saturating_sub(KEEP)] {
        remove_html(path)?;
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

/// Find a retained run by its file name, with or without the .json suffix.
pub fn find(dir: &Path, id: &str) -> Result<PathBuf> {
    runs(dir)?
        .into_iter()
        .find(|path| {
            path.file_stem().is_some_and(|name| name == id)
                || path.file_name().is_some_and(|name| name == id)
        })
        .with_context(|| format!("no run {id:?} in {}", dir.display()))
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

    /// A new folder that is removed when the test ends, also on a panic.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prune_keeps_the_newest_runs() {
        let dir = std::env::temp_dir().join(format!("maid-prune-{}", std::process::id()));
        // create_dir, not create_dir_all: it fails on a folder or symlink
        // that is there already, so the test deletes only its own files.
        std::fs::create_dir(&dir).unwrap();
        let _scratch = Scratch(dir.clone());
        let names: Vec<String> = (0..10)
            .map(|n| format!("2026-11-0{n}T000000.0Z.json"))
            .collect();
        for name in names.iter().chain([&".run.json.1".to_string()]) {
            std::fs::write(dir.join(name), "{}").unwrap();
        }
        for name in &names {
            std::fs::write(dir.join(name).with_extension("html"), "cached").unwrap();
        }

        prune(&dir).unwrap();
        let left = runs(&dir).unwrap();
        let expected: Vec<PathBuf> = names[3..].iter().map(|n| dir.join(n)).collect();
        assert_eq!(left, expected);
        assert_eq!(latest(&dir).unwrap(), dir.join(&names[9]));
        assert!(dir.join(".run.json.1").exists());
        for (index, name) in names.iter().enumerate() {
            assert_eq!(dir.join(name).with_extension("html").exists(), index >= 3);
        }
    }

    #[test]
    fn progress_save_waits_for_report_generation() {
        let dir = std::env::temp_dir().join(format!("maid-lock-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let _scratch = Scratch(dir.clone());
        let record = Record {
            version: 2,
            host: "new progress".into(),
            kind: RunKind::Cleanup,
            steps: vec![RunKind::Survey],
            started_at: "now".into(),
            finished_at: None,
            free_bytes_before: None,
            free_bytes_after: None,
            items: vec![],
            warnings: vec![],
        };
        let path = dir.join("run.json");
        let report_lock = lock(&dir).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            done_tx.send(save(&record, &worker_path)).unwrap();
        });
        ready_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        std::fs::write(path.with_extension("html"), "old report").unwrap();
        drop(report_lock);
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        assert_eq!(load(&path).unwrap().host, "new progress");
        assert!(!path.with_extension("html").exists());
    }

    #[test]
    fn saving_progress_removes_the_cached_html() {
        let dir = std::env::temp_dir().join(format!("maid-progress-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let _scratch = Scratch(dir.clone());
        let path = dir.join("run.json");
        let record = Record {
            version: 2,
            host: "test".into(),
            kind: RunKind::Cleanup,
            steps: vec![RunKind::Survey],
            started_at: "now".into(),
            finished_at: None,
            free_bytes_before: None,
            free_bytes_after: None,
            items: vec![],
            warnings: vec![],
        };
        save(&record, &path).unwrap();
        std::fs::write(path.with_extension("html"), "old progress").unwrap();
        save(&record, &path).unwrap();
        assert!(!path.with_extension("html").exists());
        assert!(load(&path).unwrap().finished_at.is_none());
    }

    #[test]
    fn no_runs_yet() {
        let err = latest(Path::new("/nonexistent/maid/runs")).unwrap_err();
        assert!(err.to_string().contains("run `maid survey` first"));
    }
}
