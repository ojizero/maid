//! `maid survey`: measure every target, worktree and unknown large folder,
//! and decide what to do with each. Changes nothing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};

use crate::app::{App, progress};
use crate::config::{Action, Config, DeleteBranch, Rule, Target};
use crate::record::{Command, Decision, Item, Kind, Record};
use crate::worktree::{self, Worktree};
use crate::{exec, size, sys, unknown};

/// How many measurements run at the same time.
const JOBS: usize = 8;

pub fn run(app: &App) -> Result<ExitCode> {
    let survey = survey(&app.config()?, &app.home);
    app.finish(survey.record(Command::Survey))
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
    }
}

impl Survey {
    /// Measure free space again and make the record of this command.
    pub fn record(mut self, command: Command) -> Record {
        let free_bytes_after = free(&mut self.warnings);
        Record {
            version: 1,
            host: sys::hostname(),
            command,
            started_at: self.started_at,
            finished_at: sys::now(),
            free_bytes_before: self.free_bytes_before,
            free_bytes_after,
            items: self.entries.into_iter().map(|e| e.item).collect(),
            warnings: self.warnings,
        }
    }
}

fn free(warnings: &mut Vec<String>) -> Option<u64> {
    sys::free_bytes()
        .map_err(|e| warn(warnings, format!("cannot read free space: {e:#}")))
        .ok()
}

fn warn(warnings: &mut Vec<String>, message: String) {
    eprintln!("maid: warning: {message}");
    warnings.push(message);
}

fn survey_entries(config: &Config, home: &Path, warnings: &mut Vec<String>) -> Vec<Entry> {
    progress(&format!("measuring {} targets", config.targets.len()));
    let mut entries = par_map(&config.targets, |t| survey_target(t, home));
    for e in &entries {
        if let Some(err) = &e.item.error {
            warn(warnings, format!("{}: {err}", e.item.name));
        }
    }

    let (worktrees, skipped) = find_worktrees(config, home);
    progress(&format!("checking {} worktrees", worktrees.len()));
    entries.extend(par_map(&worktrees, |wt| {
        survey_worktree(wt.clone(), config, home)
    }));
    entries.extend(skipped);

    progress("looking for unknown large folders");
    let known: Vec<(PathBuf, Option<u64>)> = entries
        .iter()
        .filter_map(|e| Some((PathBuf::from(e.item.path.as_ref()?), e.item.size_bytes)))
        .collect();
    for found in unknown::scan(home, &known, warnings) {
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

fn survey_target(t: &Target, home: &Path) -> Entry {
    let mut item = Item::new(Kind::Target, &t.name);
    item.path = t
        .path
        .as_ref()
        .map(|p| sys::expand(p, home).to_string_lossy().into_owned());
    match measure_target(t, home) {
        Ok(size) => item.size_bytes = size,
        Err(e) => item.error = Some(format!("{e:#}")),
    }
    match t.action {
        Action::Keep => item.decide(Decision::Keep, t.why.clone()),
        Action::Clean => {
            item.commands = t.clean.iter().cloned().collect();
            item.decide(Decision::Clean, None);
            if let Some(reason) = target_blocker(t, home) {
                item.skip(reason);
            }
        }
    }
    Entry {
        item,
        source: Source::Target(t.clone()),
    }
}

pub fn measure_target(t: &Target, home: &Path) -> Result<Option<u64>> {
    if let Some(path) = &t.path {
        return Ok(size::du(&sys::expand(path, home)));
    }
    let cmd = t.size.as_deref().unwrap_or_default();
    let out = exec::sh(cmd)?;
    if !out.status.success() {
        // Docker prints nothing useful when it is not running: the size is not known.
        return Ok(None);
    }
    size::parse(&out.stdout).with_context(|| format!("size command {cmd:?}"))
}

/// The reason not to clean a target now, if there is one.
pub fn target_blocker(t: &Target, home: &Path) -> Option<String> {
    if let Some(path) = &t.path {
        match std::fs::symlink_metadata(sys::expand(path, home)) {
            Err(_) => return Some("path does not exist".into()),
            // A link can point the clean command somewhere else.
            Ok(meta) if meta.file_type().is_symlink() => return Some("path is a symlink".into()),
            Ok(_) => {}
        }
    }
    for program in &t.skip_if_running {
        // pgrep exits 0 when it finds a process, 1 when it finds none.
        match exec::run(&["pgrep", "-x", program]) {
            Ok(out) if out.status.code() == Some(0) => {
                return Some(format!("{program} is running"));
            }
            Ok(out) if out.status.code() == Some(1) => {}
            Ok(out) => {
                return Some(format!(
                    "cannot check if {program} is running: {}",
                    out.failure()
                ));
            }
            Err(e) => return Some(format!("cannot check if {program} is running: {e:#}")),
        }
    }
    if let Some(cmd) = &t.only_if {
        match exec::sh(cmd) {
            Ok(out) if out.status.success() => {}
            Ok(_) => return Some(format!("only_if failed: {cmd}")),
            Err(e) => return Some(format!("only_if failed: {e:#}")),
        }
    }
    None
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

    let listed = par_map(&checkouts, |repo| (repo.clone(), worktree::list(repo)));
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
    let vars = worktree::vars(wt, captures.clone());
    match worktree::fill(rule.clean.as_deref().unwrap_or_default(), &vars) {
        Ok(cmd) => item.commands.push(cmd),
        Err(e) => {
            item.decide(Decision::Ask, Some(format!("{e:#}")));
            return;
        }
    }
    let mut reason = "integrated, no changes, ignored files are build output".to_string();
    if let (true, Some(branch)) = (*is_default, &wt.branch) {
        match policy {
            DeleteBranch::Never => {}
            DeleteBranch::Pushed => {
                reason.push_str("; branch deleted if a remote branch contains it")
            }
            DeleteBranch::Integrated => reason.push_str("; branch deleted"),
        }
        if policy != DeleteBranch::Never {
            item.commands.push(branch_delete(&wt.repo, branch));
        }
    }
    item.decide(Decision::Clean, Some(reason));
}

pub fn branch_delete(repo: &Path, branch: &str) -> String {
    format!(
        "git -C {} branch -D {}",
        exec::quote(&repo.to_string_lossy()),
        exec::quote(branch)
    )
}

/// Map in parallel with a few threads, keeping the order.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..JOBS.min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    results.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.unwrap())
        .collect()
}
