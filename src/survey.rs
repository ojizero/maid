//! `maid survey`: measure every target, worktree and unknown large folder,
//! and decide what to do with each. Changes nothing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;

use crate::app::{App, progress, warn};
use crate::config::{Action, Config, DeleteBranch, Rule, Target};
use crate::record::{Decision, Item, Kind, Record, RunKind};
use crate::worktree::{self, Worktree};
use crate::{exec, par, size, sys, target, unknown};

pub fn run(app: &App) -> Result<ExitCode> {
    let survey = survey(&app.config()?, &app.home);
    app.finish(survey.finish(RunKind::Survey))
}

/// An item and what maid needs to clean it.
pub struct Entry {
    pub item: Item,
    pub source: Source,
}

pub enum Source {
    Target(Target),
    Worktree {
        wt: Worktree,
        rule: Rule,
        captures: Vec<(String, String)>,
        is_default: bool,
    },
    None,
}

/// What a survey found. Other commands can act on its entries.
pub struct Survey {
    started_at: String,
    free_bytes_before: Option<u64>,
    pub entries: Vec<Entry>,
    warnings: Vec<String>,
    /// The steps that finished. The survey is the first.
    steps: Vec<RunKind>,
}

/// Measure every target, worktree and unknown large folder, and decide
/// what to do with each. Changes nothing.
pub fn survey(config: &Config, home: &Path) -> Survey {
    let started_at = sys::now();
    let mut warnings = Vec::new();
    let free_bytes_before = free(&mut warnings);
    let entries = survey_entries(config, home, &mut warnings);
    Survey {
        started_at,
        free_bytes_before,
        entries,
        warnings,
        steps: vec![RunKind::Survey],
    }
}

impl Survey {
    /// Mark a step after the survey as finished.
    pub fn ran(&mut self, step: RunKind) {
        self.steps.push(step);
    }

    /// The record of a `kind` run that is not finished yet.
    pub fn record(&self, kind: RunKind) -> Record {
        Record {
            version: 2,
            host: sys::hostname(),
            kind,
            steps: self.steps.clone(),
            started_at: self.started_at.clone(),
            finished_at: None,
            free_bytes_before: self.free_bytes_before,
            free_bytes_after: None,
            items: self.entries.iter().map(|e| e.item.clone()).collect(),
            warnings: self.warnings.clone(),
        }
    }

    /// Measure free space again and make the record of the finished run.
    pub fn finish(mut self, kind: RunKind) -> Record {
        let free_bytes_after = free(&mut self.warnings);
        Record {
            finished_at: Some(sys::now()),
            free_bytes_after,
            ..self.record(kind)
        }
    }
}

fn free(warnings: &mut Vec<String>) -> Option<u64> {
    sys::free_bytes()
        .map_err(|e| warn(warnings, format!("cannot read free space: {e:#}")))
        .ok()
}

fn survey_entries(config: &Config, home: &Path, warnings: &mut Vec<String>) -> Vec<Entry> {
    progress(&format!("measuring {} targets", config.targets.len()));
    let mut entries = par::map(&config.targets, |t| survey_target(t, home));
    for t in &config.targets {
        let missing = t.missing_programs();
        if !missing.is_empty() {
            let programs = missing.join(", ");
            warn(
                warnings,
                format!(
                    "{}: {programs} not on PATH (run `maid doctor` for details)",
                    t.name
                ),
            );
        }
    }
    for e in &entries {
        if let Some(err) = &e.item.error {
            warn(warnings, format!("{}: {err}", e.item.name));
        }
    }

    let (worktrees, skipped) = find_worktrees(config, home);
    progress(&format!("checking {} worktrees", worktrees.len()));
    entries.extend(par::map(&worktrees, |wt| {
        survey_worktree(wt.clone(), config, home)
    }));
    entries.extend(skipped);

    progress("looking for unknown large folders");
    for found in unknown::scan(home, &covered(&entries, config, home), warnings) {
        let mut item = Item::new(Kind::Unknown, "unknown folder");
        item.path = Some(found.path.to_string_lossy().into_owned());
        item.size_bytes = Some(found.size);
        item.decide(
            Decision::Ask,
            Some("no target or worktree covers it".into()),
        );
        entries.push(Entry {
            item,
            source: Source::None,
        });
    }
    entries
}

/// The paths that the unknown scan must not report, with their sizes when
/// known. The repos folders are covered: their worktrees are items, and
/// maid does not clean the main checkouts.
fn covered(entries: &[Entry], config: &Config, home: &Path) -> Vec<(PathBuf, Option<u64>)> {
    let items = entries
        .iter()
        .filter_map(|e| Some((PathBuf::from(e.item.path.as_ref()?), e.item.size_bytes)));
    let repos = config
        .worktrees
        .repos
        .iter()
        .map(|root| (sys::expand(root, home), None));
    items.chain(repos).collect()
}

fn survey_target(t: &Target, home: &Path) -> Entry {
    let mut item = Item::new(Kind::Target, &t.name);
    item.path = t
        .path
        .as_ref()
        .map(|p| sys::expand(p, home).to_string_lossy().into_owned());
    item.missing_programs = t.missing_programs().iter().map(|p| p.to_string()).collect();
    match target::measure(t, home) {
        Ok(size) => item.size_bytes = size,
        Err(e) => item.error = Some(format!("{e:#}")),
    }
    match t.action {
        Action::Keep => item.decide(Decision::Keep, t.why.clone()),
        Action::Clean => {
            item.commands = t.clean.iter().cloned().collect();
            item.decide(Decision::Clean, None);
            if let Some(reason) = target::blocker(t, home) {
                item.skip(reason);
            }
        }
    }
    Entry {
        item,
        source: Source::Target(t.clone()),
    }
}

/// Every worktree in the repos folders, and an item for each repo that
/// could not be listed.
fn find_worktrees(config: &Config, home: &Path) -> (Vec<Worktree>, Vec<Entry>) {
    let mut checkouts: Vec<PathBuf> = Vec::new();
    for root in &config.worktrees.repos {
        checkouts.extend(worktree::checkouts(&sys::expand(root, home)));
    }
    checkouts.sort();
    checkouts.dedup();
    progress(&format!("listing worktrees of {} repos", checkouts.len()));

    let listed = par::map(&checkouts, |repo| (repo.clone(), worktree::list(repo)));
    let mut seen = HashSet::new();
    let (mut found, mut skipped) = (Vec::new(), Vec::new());
    for (repo, result) in listed {
        match result {
            Ok(list) => found.extend(list.into_iter().filter(|wt| seen.insert(wt.path.clone()))),
            Err(e) => {
                let mut item = Item::new(Kind::Worktree, "wt list");
                item.repo = Some(repo.to_string_lossy().into_owned());
                item.decide(Decision::Ask, Some(format!("repo skipped: {e:#}")));
                skipped.push(Entry {
                    item,
                    source: Source::None,
                });
            }
        }
    }
    (found, skipped)
}

fn survey_worktree(wt: Worktree, config: &Config, home: &Path) -> Entry {
    let (rule, captures) = worktree::rule_for(&config.worktrees, &wt.path, home);
    let is_default = std::ptr::eq(rule, &config.worktrees.default);
    let mut item = Item::new(Kind::Worktree, &rule.name);
    item.path = Some(wt.path.clone());
    item.repo = Some(wt.repo.to_string_lossy().into_owned());
    item.branch = wt.branch.clone();
    item.size_bytes = size::du(Path::new(&wt.path));
    let mut entry = Entry {
        item,
        source: Source::Worktree {
            wt,
            rule: rule.clone(),
            captures,
            is_default,
        },
    };
    decide_worktree(&mut entry, config.worktrees.delete_branch);
    entry
}

pub fn decide_worktree(entry: &mut Entry, policy: DeleteBranch) {
    let Source::Worktree {
        wt,
        rule,
        captures,
        is_default,
    } = &entry.source
    else {
        return;
    };
    let item = &mut entry.item;
    item.commands.clear();
    if rule.action == Action::Keep {
        item.decide(Decision::Keep, rule.why.clone());
        return;
    }
    let checks =
        worktree::check_state(wt).and_then(|()| worktree::check_ignored(Path::new(&wt.path)));
    if let Err(reason) = checks {
        item.decide(Decision::Ask, Some(reason));
        return;
    }
    let vars = worktree::vars(wt, captures);
    match worktree::fill(rule.clean.as_deref().unwrap_or_default(), &vars) {
        Ok(cmd) => item.commands.push(cmd),
        Err(e) => {
            item.decide(Decision::Ask, Some(format!("{e:#}")));
            return;
        }
    }
    let mut reason = "integrated, no changes, ignored files are build output".to_string();
    let delete = match policy {
        DeleteBranch::Never => None,
        DeleteBranch::Pushed => Some("; branch deleted if a remote branch contains it"),
        DeleteBranch::Integrated => Some("; branch deleted"),
    };
    // Only the default rule keeps the branch (--no-delete-branch).
    if *is_default
        && let Some(branch) = &wt.branch
        && let Some(note) = delete
    {
        reason.push_str(note);
        item.commands.push(branch_delete(&wt.repo, branch));
    }
    item.decide(Decision::Clean, Some(reason));
}

fn branch_delete(repo: &Path, branch: &str) -> String {
    format!(
        "git -C {} branch -D {}",
        exec::quote(&repo.to_string_lossy()),
        exec::quote(branch)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_folders_are_covered() {
        let config = crate::config::parse(crate::config::DEFAULTS).unwrap();
        let home = Path::new("/home/me");
        let mut item = Item::new(Kind::Target, "uv cache");
        item.path = Some("/home/me/.cache/uv".into());
        item.size_bytes = Some(5);
        let entries = [Entry {
            item,
            source: Source::None,
        }];
        assert_eq!(
            covered(&entries, &config, home),
            [
                (PathBuf::from("/home/me/.cache/uv"), Some(5)),
                (PathBuf::from("/home/me/workspace"), None),
            ]
        );
    }
}
