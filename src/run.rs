//! The run: survey, dry run and cleanup, and the run file.

use std::collections::HashSet;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{Action, Config, DeleteBranch, Rule, Target};
use crate::worktree::{self, Worktree};
use crate::{exec, size, sys, unknown};

/// How many measurements run at the same time.
const JOBS: usize = 8;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
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
pub struct Run {
    pub version: u32,
    pub host: String,
    pub mode: Mode,
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
    fn new(kind: Kind, name: &str) -> Self {
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

    fn decide(&mut self, decision: Decision, reason: Option<String>) {
        self.decision = decision;
        self.reason = reason;
        self.result = if decision == Decision::Clean {
            Outcome::Planned
        } else {
            Outcome::Skipped
        };
    }
}

/// An item and what maid needs to clean it.
struct Entry {
    item: Item,
    source: Source,
}

enum Source {
    Target(Target),
    Worktree {
        wt: Worktree,
        rule: Rule,
        captures: Vec<(String, String)>,
        is_default: bool,
    },
    None,
}

pub struct Options<'a> {
    pub mode: Mode,
    /// Clean only targets and overrides with these names. Empty means all.
    pub only: &'a [String],
}

/// Survey, then clean or dry-run when the mode asks for it.
pub fn execute(config: &Config, home: &Path, opts: &Options) -> Result<Run> {
    let started_at = sys::now();
    let mut warnings = Vec::new();
    let free_bytes_before = free(&mut warnings);

    let mut entries = survey(config, home, &mut warnings);
    if opts.mode != Mode::Survey {
        for entry in &mut entries {
            if entry.item.decision != Decision::Clean {
                continue;
            }
            if !opts.only.is_empty() && !opts.only.contains(&entry.source_name()) {
                entry.item.result = Outcome::Skipped;
                entry.item.reason = Some("not selected by --only".into());
                continue;
            }
            match opts.mode {
                Mode::DryRun => dry_run(entry),
                _ => clean(entry, config, home),
            }
        }
    }

    let free_bytes_after = free(&mut warnings);
    Ok(Run {
        version: 1,
        host: sys::hostname(),
        mode: opts.mode,
        started_at,
        finished_at: sys::now(),
        free_bytes_before,
        free_bytes_after,
        items: entries.into_iter().map(|e| e.item).collect(),
        warnings,
    })
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

fn progress(message: &str) {
    eprintln!("maid: {message}");
}

impl Entry {
    /// The name that --only matches: the target or override name.
    fn source_name(&self) -> String {
        match &self.source {
            Source::Worktree { rule, .. } => rule.name.clone(),
            _ => self.item.name.clone(),
        }
    }
}

// ── Survey ───────────────────────────────────────────────────────

fn survey(config: &Config, home: &Path, warnings: &mut Vec<String>) -> Vec<Entry> {
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
            if item.path.as_deref().is_some_and(|p| !Path::new(p).exists()) {
                skip(&mut item, "path does not exist".into());
            } else if let Some(reason) = target_blocker(t) {
                skip(&mut item, reason);
            }
        }
    }
    Entry {
        item,
        source: Source::Target(t.clone()),
    }
}

fn skip(item: &mut Item, reason: String) {
    item.result = Outcome::Skipped;
    item.reason = Some(reason);
}

fn measure_target(t: &Target, home: &Path) -> Result<Option<u64>> {
    if let Some(path) = &t.path {
        return Ok(du(&sys::expand(path, home)));
    }
    let cmd = t.size.as_deref().unwrap_or_default();
    let out = exec::sh(cmd)?;
    if !out.success {
        // Docker prints nothing useful when it is not running: the size is not known.
        return Ok(None);
    }
    size::parse(&out.stdout).with_context(|| format!("size command {cmd:?}"))
}

/// The reason not to clean a target now, if there is one.
fn target_blocker(t: &Target) -> Option<String> {
    for program in &t.skip_if_running {
        match exec::run(&["pgrep", "-x", program]) {
            Ok(out) if out.success => return Some(format!("{program} is running")),
            Ok(_) => {}
            Err(e) => return Some(format!("cannot check if {program} is running: {e:#}")),
        }
    }
    if let Some(cmd) = &t.only_if {
        match exec::sh(cmd) {
            Ok(out) if out.success => {}
            Ok(_) => return Some(format!("only_if failed: {cmd}")),
            Err(e) => return Some(format!("only_if failed: {e:#}")),
        }
    }
    None
}

/// The size of a folder in bytes, from `du -sk`. None when it does not exist.
pub fn du(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path).ok()?;
    let out = exec::run(&["du", "-sk", &path.to_string_lossy()]).ok()?;
    // du exits non-zero when some files are unreadable but still prints a total.
    let kib: u64 = out.stdout.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
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
    item.size_bytes = du(Path::new(&wt.path));
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

fn decide_worktree(entry: &mut Entry, policy: DeleteBranch) {
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

fn branch_delete(repo: &Path, branch: &str) -> String {
    format!(
        "git -C {} branch -D {}",
        exec::quote(&repo.to_string_lossy()),
        exec::quote(branch)
    )
}

// ── Dry run and cleanup ──────────────────────────────────────────

fn dry_run(entry: &mut Entry) {
    let item = &mut entry.item;
    if item.result != Outcome::Planned {
        return;
    }
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
                    item.error = Some(format!("dry_run: {e:#}"));
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
                item.error = Some(format!("dry_run failed: {}", out.failure()));
            }
        }
        Err(e) => item.error = Some(format!("dry_run: {e:#}")),
    }
}

fn clean(entry: &mut Entry, config: &Config, home: &Path) {
    if entry.item.result != Outcome::Planned {
        return;
    }
    // Check again just before cleaning: things change during a long run.
    match &mut entry.source {
        Source::Target(t) => {
            if let Some(reason) = target_blocker(t) {
                skip(&mut entry.item, reason);
                return;
            }
        }
        Source::Worktree { wt, .. } => {
            let fresh = worktree::list(&wt.repo).map(|l| l.into_iter().find(|w| w.path == wt.path));
            match fresh {
                Ok(Some(fresh)) => *wt = fresh,
                Ok(None) => return skip(&mut entry.item, "worktree is gone".into()),
                Err(e) => return skip(&mut entry.item, format!("{e:#}")),
            }
            decide_worktree(entry, config.worktrees.delete_branch);
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

    let after = match &entry.source {
        Source::Target(t) => measure_target(t, home).ok().flatten(),
        _ => item.path.as_ref().and_then(|p| du(Path::new(p))),
    };
    item.freed_bytes = item
        .size_bytes
        .map(|before| before.saturating_sub(after.unwrap_or(0)));
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
    ran.push(branch_delete(repo, branch));
    let out =
        exec::run(&["git", "-C", &repo_s, "branch", "-D", branch]).map_err(|e| format!("{e:#}"))?;
    if out.success {
        Ok(())
    } else {
        Err(out.failure())
    }
}

// ── Run file ─────────────────────────────────────────────────────

pub fn run_file(home: &Path) -> PathBuf {
    sys::xdg("XDG_STATE_HOME", ".local/state", home).join("maid/run.json")
}

/// Write the run file: a temp file next to it, then rename.
pub fn save(run: &Run, path: &Path) -> Result<()> {
    let dir = path.parent().context("run file has no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = dir.join(format!(".run.json.{}", std::process::id()));
    let write = || -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(serde_json::to_string_pretty(run)?.as_bytes())?;
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

pub fn load(path: &Path) -> Result<Run> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read {} (run `maid survey` first)", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("cannot read {}", path.display()))
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
