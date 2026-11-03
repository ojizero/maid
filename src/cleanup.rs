//! `maid cleanup`: survey, then run the clean command of each item with
//! decision clean, one at a time.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;

use crate::app::{App, progress, usage};
use crate::config::{Config, DeleteBranch};
use crate::record::{Command, Decision, Outcome};
use crate::survey::{self, Entry, Source};
use crate::{exec, size, worktree};

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
    let command = if args.dry_run {
        Command::DryRun
    } else {
        Command::Cleanup
    };
    app.finish(survey.record(command))
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
    let dry = match &entry.source {
        Source::Target(t) => t.dry_run.clone(),
        Source::Worktree {
            wt, rule, captures, ..
        } => match &rule.dry_run {
            Some(t) => match worktree::fill(t, &worktree::vars(wt, captures.clone())) {
                Ok(cmd) => Some(cmd),
                Err(e) => {
                    item.fail(format!("dry_run: {e:#}"));
                    None
                }
            },
            None => None,
        },
        Source::None => None,
    };
    let Some(cmd) = dry else { return };
    progress(&format!("  dry run: {cmd}"));
    match exec::sh(&cmd) {
        Ok(out) => {
            for line in exec::tail(&out.stdout, 20).lines() {
                progress(&format!("    {line}"));
            }
            if !out.success {
                item.fail(format!("dry_run failed: {}", out.failure()));
            }
        }
        Err(e) => item.fail(format!("dry_run: {e:#}")),
    }
}

fn clean(entry: &mut Entry, config: &Config, home: &Path) {
    // Check again just before cleaning: things change during a long run.
    match &mut entry.source {
        Source::Target(t) => {
            if let Some(reason) = survey::target_blocker(t, home) {
                entry.item.skip(reason);
                return;
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
    let Some(cmd) = item.commands.first().cloned() else {
        return;
    };
    let mut ran = vec![cmd.clone()];
    let result = exec::sh(&cmd);
    let mut error = match result {
        Ok(out) if out.success => None,
        Ok(out) => Some(out.failure()),
        Err(e) => Some(format!("{e:#}")),
    };
    if error.is_none()
        && let Source::Worktree {
            wt,
            is_default: true,
            ..
        } = &entry.source
        && let Some(branch) = &wt.branch
    {
        error = delete_branch(&wt.repo, branch, config.worktrees.delete_branch, &mut ran)
            .err()
            .map(|e| format!("worktree removed, but the branch was not deleted: {e}"));
    }

    // A failed measurement leaves freed_bytes unknown; a path that is gone is 0.
    let after = match (&entry.source, &item.path) {
        (Source::Target(t), None) => survey::measure_target(t, home).ok().flatten(),
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

fn delete_branch(
    repo: &Path,
    branch: &str,
    policy: DeleteBranch,
    ran: &mut Vec<String>,
) -> Result<(), String> {
    let repo_s = repo.to_string_lossy();
    if policy == DeleteBranch::Never {
        return Ok(());
    }
    if policy == DeleteBranch::Pushed {
        let out = exec::run(&["git", "-C", &repo_s, "branch", "-r", "--contains", branch])
            .map_err(|e| format!("{e:#}"))?;
        if !out.success {
            return Err(out.failure());
        }
        if out.stdout.trim().is_empty() {
            return Ok(());
        }
    }
    ran.push(survey::branch_delete(repo, branch));
    let out =
        exec::run(&["git", "-C", &repo_s, "branch", "-D", branch]).map_err(|e| format!("{e:#}"))?;
    if out.success {
        Ok(())
    } else {
        Err(out.failure())
    }
}
