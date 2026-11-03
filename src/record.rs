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
    pub started_at: String,
    pub finished_at: String,
    pub free_bytes_before: Option<u64>,
    pub free_bytes_after: Option<u64>,
    pub items: Vec<Item>,
    pub warnings: Vec<String>,
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

pub fn path(home: &Path) -> PathBuf {
    sys::xdg("XDG_STATE_HOME", ".local/state", home).join("maid/run.json")
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

pub fn load(path: &Path) -> Result<Record> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read {} (run `maid survey` first)", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("cannot read {}", path.display()))
}
