//! `maid cleanup`: survey, then run the clean command of each item with
//! decision clean, one at a time.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;

use crate::app::{App, progress, usage};
use crate::config::{Config, DeleteBranch};
use crate::record::{Decision, Outcome, RunKind};
use crate::survey::{self, Entry, Source};
use crate::{exec, size, target, worktree};

#[derive(clap::Args)]
pub struct Args {
    /// Show what would run and run the dry_run commands. Changes nothing.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Clean only the target or worktree rule with this name (repeatable).
    /// "worktree" selects the worktrees that no override matches.
    #[arg(long, value_name = "NAME")]
    only: Vec<String>,
}

pub fn run(app: &App, args: &Args) -> Result<ExitCode> {
    let config = app.config()?;
    check_only(&config, &args.only)?;
    let mut survey = survey::survey(&config, &app.home);
    // A planned item has decision clean and passed its checks in the survey.
    let planned = survey
        .entries
        .iter_mut()
        .filter(|e| e.item.result == Outcome::Planned);
    for entry in planned {
        // A worktree item has the name of its rule, so this matches rules too.
        if !args.only.is_empty() && !args.only.contains(&entry.item.name) {
            entry.item.skip("not selected by --only".into());
        } else if args.dry_run {
            dry_run(entry);
        } else {
            clean(entry, &config, &app.home);
        }
    }
    let kind = if args.dry_run {
        RunKind::DryRun
    } else {
        RunKind::Cleanup
    };
    app.finish(survey.record(kind))
}

fn check_only(config: &Config, only: &[String]) -> Result<()> {
    let names = config.names();
    match only.iter().find(|name| !names.contains(&name.as_str())) {
        Some(name) => usage(Err(anyhow::anyhow!(
            "--only {name:?} matches no target or worktree rule (names: {})",
            names.join(", ")
        ))),
        None => Ok(()),
    }
}

fn dry_run(entry: &mut Entry) {
    let item = &mut entry.item;
    let size = item.size_bytes.map_or("size unknown".into(), size::human);
    progress(&format!(
        "would clean {} ({size}): {}",
        item.name,
        item.path.as_deref().unwrap_or("-")
    ));
    for cmd in &item.commands {
        progress(&format!("  would run: {cmd}"));
    }
    let cmd = match dry_run_command(&entry.source) {
        Ok(Some(cmd)) => cmd,
        Ok(None) => return,
        Err(e) => return item.fail(format!("dry_run: {e:#}")),
    };
    progress(&format!("  dry run: {cmd}"));
    match exec::sh(&cmd) {
        Ok(out) => {
            for line in exec::tail(&out.stdout, 20).lines() {
                progress(&format!("    {line}"));
            }
            if !out.status.success() {
                item.fail(format!("dry_run failed: {}", out.failure()));
            }
        }
        Err(e) => item.fail(format!("dry_run: {e:#}")),
    }
}

/// The item's dry_run command, with the placeholders filled.
fn dry_run_command(source: &Source) -> Result<Option<String>> {
    match source {
        Source::Target(t) => Ok(t.dry_run.clone()),
        Source::Worktree {
            wt, rule, captures, ..
        } => rule
            .dry_run
            .as_deref()
            .map(|t| worktree::fill(t, &worktree::vars(wt, captures)))
            .transpose(),
        Source::None => Ok(None),
    }
}

fn clean(entry: &mut Entry, config: &Config, home: &Path) {
    // Check again just before cleaning: things change during a long run.
    match &mut entry.source {
        Source::Target(t) => {
            if let Some(reason) = target::blocker(t, home) {
                return entry.item.skip(reason);
            }
        }
        Source::Worktree { wt, .. } => {
            let fresh = worktree::list(&wt.repo).map(|l| l.into_iter().find(|w| w.path == wt.path));
            match fresh {
                Ok(Some(fresh)) => *wt = fresh,
                Ok(None) => return entry.item.skip("worktree is gone".into()),
                Err(e) => return entry.item.skip(format!("{e:#}")),
            }
            survey::decide_worktree(entry, config.worktrees.delete_branch);
            if entry.item.decision != Decision::Clean {
                return;
            }
        }
        Source::None => return,
    }

    let item = &mut entry.item;
    progress(&format!(
        "cleaning {}: {}",
        item.name,
        item.path.as_deref().unwrap_or("-")
    ));
    // A clean item has its clean command first, then the branch delete
    // when the survey planned one.
    let (remove, delete) = (item.commands[0].clone(), item.commands.get(1).cloned());
    let mut ran = vec![remove.clone()];
    let mut error = failure(exec::sh(&remove));
    if error.is_none()
        && let Some(delete) = delete
        && let Source::Worktree { wt, .. } = &entry.source
        && let Some(branch) = &wt.branch
    {
        let allowed = match config.worktrees.delete_branch {
            DeleteBranch::Pushed => on_remote(&wt.repo, branch),
            _ => Ok(true),
        };
        error = match allowed {
            Ok(false) => None,
            Ok(true) => {
                ran.push(delete.clone());
                failure(exec::sh(&delete))
            }
            Err(e) => Some(e),
        }
        .map(|e| format!("worktree removed, but the branch was not deleted: {e}"));
    }

    // A failed measurement leaves freed_bytes unknown; a path that is gone is 0.
    let after = match (&entry.source, &item.path) {
        (Source::Target(t), None) => target::measure(t, home).ok().flatten(),
        (_, Some(p)) if std::fs::symlink_metadata(p).is_err() => Some(0),
        (_, Some(p)) => size::du(Path::new(p)),
        (_, None) => None,
    };
    item.freed_bytes = item
        .size_bytes
        .zip(after)
        .map(|(before, after)| before.saturating_sub(after));
    item.commands = ran;
    item.result = if error.is_some() {
        Outcome::Failed
    } else {
        Outcome::Done
    };
    item.error = error;
}

/// The error text of a command that did not succeed.
fn failure(result: Result<exec::Output>) -> Option<String> {
    match result {
        Ok(out) if out.status.success() => None,
        Ok(out) => Some(out.failure()),
        Err(e) => Some(format!("{e:#}")),
    }
}

/// True when a remote branch contains the branch's last commit.
fn on_remote(repo: &Path, branch: &str) -> Result<bool, String> {
    let repo = repo.to_string_lossy();
    match exec::run(&["git", "-C", &repo, "branch", "-r", "--contains", branch]) {
        Ok(out) if out.status.success() => Ok(!out.stdout.trim().is_empty()),
        Ok(out) => Err(out.failure()),
        Err(e) => Err(format!("{e:#}")),
    }
}
