//! Git worktrees: find them, match them to a rule, and check them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::{Rule, Worktrees};
use crate::exec;

/// How deep to look for main checkouts under a repos folder.
const MAX_DEPTH: usize = 4;

/// Folders not searched for main checkouts.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "deps",
    "_build",
];

/// Ignored files may be inside these folders, because a build makes them again.
/// Keep this list strict: `tmp`, `log` and similar folders can hold real data.
const BUILD_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "_build",
    "deps",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".next",
    ".turbo",
    ".parcel-cache",
    ".gradle",
    "coverage",
    ".elixir_ls",
];

#[derive(Deserialize)]
struct WtList {
    items: Vec<WtItem>,
}

#[derive(Deserialize)]
struct WtItem {
    branch: Option<String>,
    worktree: Option<WtTree>,
    display: Option<WtDisplay>,
}

#[derive(Deserialize)]
struct WtTree {
    path: String,
    main: bool,
    changes: Option<Changes>,
    prunable: Option<Prunable>,
}

#[derive(Deserialize)]
struct Prunable {
    reason: Option<String>,
}

#[derive(Deserialize)]
struct WtDisplay {
    state: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Changes {
    staged: bool,
    modified: bool,
    untracked: bool,
    renamed: bool,
    deleted: bool,
    conflicted: bool,
}

#[derive(Clone, Debug)]
pub struct Worktree {
    /// The main checkout of the repo.
    pub repo: PathBuf,
    pub path: String,
    pub branch: Option<String>,
    pub state: Option<String>,
    pub changes: Option<Changes>,
    /// Set when git says the worktree can be pruned.
    pub prunable: Option<String>,
}

/// Find main checkouts (folders with a `.git` folder) under `root`.
pub fn checkouts(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, 0, &mut found);
    found.sort();
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if dir.join(".git").is_dir() {
        found.push(dir.to_path_buf());
        return;
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // file_type does not follow symlinks, so links are not walked.
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && !SKIP_DIRS.contains(&name.as_ref()) && name != ".git" {
            walk(&entry.path(), depth + 1, found);
        }
    }
}

/// List the worktrees of a repo, without the main worktree.
pub fn list(repo: &Path) -> Result<Vec<Worktree>> {
    let repo_s = repo.to_string_lossy();
    let out = exec::run(&["wt", "-C", &repo_s, "list", "--format=json"])?;
    if !out.status.success() {
        bail!("wt list failed: {}", out.failure());
    }
    parse(&out.stdout, repo)
}

fn parse(json: &str, repo: &Path) -> Result<Vec<Worktree>> {
    let list: WtList = serde_json::from_str(json).context("cannot read wt list output")?;
    Ok(list
        .items
        .into_iter()
        .filter_map(|item| {
            let tree = item.worktree?;
            (!tree.main).then(|| Worktree {
                repo: repo.to_path_buf(),
                path: tree.path,
                branch: item.branch,
                state: item.display.and_then(|d| d.state),
                changes: tree.changes,
                prunable: tree
                    .prunable
                    .map(|p| p.reason.unwrap_or_else(|| "prunable".into())),
            })
        })
        .collect())
}

/// The rule for a worktree path, and the values the pattern captured.
pub fn rule_for<'a>(
    config: &'a Worktrees,
    path: &str,
    home: &Path,
) -> (&'a Rule, Vec<(String, String)>) {
    // wt prints real paths, so also try HOME with its links resolved.
    let real_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    for rule in &config.overrides {
        for home in [home, real_home.as_path()] {
            let pattern = crate::sys::expand(&rule.path, home);
            if let Some(captures) = matches(&pattern.to_string_lossy(), path) {
                return (rule, captures);
            }
        }
    }
    (&config.default, Vec::new())
}

/// Match a path against a pattern. `*` matches one folder name, `{name}`
/// matches one folder name and captures it.
fn matches(pattern: &str, path: &str) -> Option<Vec<(String, String)>> {
    let pat: Vec<&str> = pattern.trim_end_matches('/').split('/').collect();
    let segs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    if pat.len() != segs.len() {
        return None;
    }
    let mut captures = Vec::new();
    for (p, s) in pat.iter().zip(&segs) {
        if *p == "*" {
            if s.is_empty() {
                return None;
            }
        } else if let Some(name) = p.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
            if s.is_empty() {
                return None;
            }
            captures.push((name.to_string(), s.to_string()));
        } else if p != s {
            return None;
        }
    }
    Some(captures)
}

/// Put values into a command. Each `{name}` must have a value; the value is
/// shell-quoted. Other braces stay as they are.
pub fn fill(template: &str, vars: &[(String, String)]) -> Result<String> {
    substitute(template, |name| {
        match vars.iter().find(|(k, _)| k == name) {
            Some((_, value)) => Ok(exec::quote(value)),
            None => bail!("no value for {{{name}}} in {template:?}"),
        }
    })
}

/// Check a command template when the config loads.
pub fn check_template(template: &str) -> Result<()> {
    substitute(template, |_| Ok(String::new())).map(drop)
}

/// Replace each `{name}` with `value(name)`. A placeholder inside shell
/// quotes is an error: a quoted value inside "..." would still run `$(...)`.
fn substitute(template: &str, mut value: impl FnMut(&str) -> Result<String>) -> Result<String> {
    let mut out = String::new();
    let mut open: Option<char> = None; // the open quote character
    let mut escaped = false;
    let mut rest = template;
    while let Some(c) = rest.chars().next() {
        if !escaped && let Some((name, after)) = placeholder(rest) {
            if open.is_some() {
                bail!("{{{name}}} is inside quotes in {template:?}; maid quotes values itself");
            }
            out.push_str(&value(name)?);
            rest = after;
            continue;
        }
        match (c, open) {
            _ if escaped => escaped = false,
            ('\\', Some('"') | None) => escaped = true,
            ('\'' | '"', None) => open = Some(c),
            (q, Some(o)) if q == o => open = None,
            _ => {}
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    Ok(out)
}

/// The `{name}` placeholder at the start of `text`, if there is one: its
/// name and the text after it.
fn placeholder(text: &str) -> Option<(&str, &str)> {
    let (name, after) = text.strip_prefix('{')?.split_once('}')?;
    let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some((name, after))
}

/// The values a worktree command can use.
pub fn vars(wt: &Worktree, captures: &[(String, String)]) -> Vec<(String, String)> {
    let mut vars = vec![
        ("repo".to_string(), wt.repo.to_string_lossy().into_owned()),
        ("path".to_string(), wt.path.clone()),
    ];
    if let Some(branch) = &wt.branch {
        vars.push(("branch".to_string(), branch.clone()));
    }
    vars.extend_from_slice(captures);
    vars
}

/// Checks 1 and 2, from the `wt list` data: integrated and no changes.
pub fn check_state(wt: &Worktree) -> Result<(), String> {
    if let Some(reason) = &wt.prunable {
        return Err(format!("prunable: {reason}"));
    }
    let Some(c) = &wt.changes else {
        return Err("changes are not known".into());
    };
    let flags = [
        ("staged", c.staged),
        ("modified", c.modified),
        ("untracked", c.untracked),
        ("renamed", c.renamed),
        ("deleted", c.deleted),
        ("conflicted", c.conflicted),
    ];
    let set: Vec<&str> = flags
        .iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| *name)
        .collect();
    if !set.is_empty() {
        return Err(format!("has changes: {}", set.join(", ")));
    }
    // Changes first: wt does not call a branch with changes "integrated",
    // and "has changes" is the more useful reason.
    // "empty" is a branch at the same commit as the default branch.
    match wt.state.as_deref() {
        Some("integrated" | "empty") => Ok(()),
        Some(state) => Err(format!("not integrated (state: {state})")),
        None => Err("not integrated (no state)".into()),
    }
}

/// Check 3: every ignored file is build output.
pub fn check_ignored(path: &Path) -> Result<(), String> {
    let path_s = path.to_string_lossy();
    let out = exec::run(&[
        "git",
        "-C",
        &path_s,
        "status",
        "--porcelain",
        "--ignored",
        "-z",
    ])
    .map_err(|e| format!("git status failed: {e:#}"))?;
    if !out.status.success() {
        return Err(format!("git status failed: {}", out.failure()));
    }
    check_status(&out.stdout)
}

/// Check `git status --porcelain --ignored -z` output. Any entry that is
/// not ignored is a change. An ignored entry must be inside a build folder
/// or be a `.DS_Store` file.
fn check_status(out: &str) -> Result<(), String> {
    let mut bad = Vec::new();
    for entry in out.split('\0') {
        if entry.is_empty() {
            continue;
        }
        let Some(file) = entry.strip_prefix("!! ") else {
            return Err(format!("has changes: {entry}"));
        };
        // -z gives raw names: no quotes or escapes to undo.
        // A folder ends with "/"; it has no file name.
        let (dirs, name) = match file.strip_suffix('/') {
            Some(dir) => (dir, ""),
            None => file.rsplit_once('/').unwrap_or(("", file)),
        };
        let in_build_dir = dirs.split('/').any(|d| BUILD_DIRS.contains(&d));
        if !(in_build_dir || name == ".DS_Store") {
            bad.push(file);
        }
    }
    match bad.as_slice() {
        [] => Ok(()),
        [file] => Err(format!("ignored file is not build output: {file}")),
        [a, b, c, rest @ ..] if !rest.is_empty() => Err(format!(
            "ignored files are not build output: {a}, {b}, {c} and {} more",
            rest.len()
        )),
        files => Err(format!(
            "ignored files are not build output: {}",
            files.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    const FIXTURE: &str = include_str!("../tests/fixtures/wt-list.json");
    const REPO: &str = "/Users/dev/src/acme/api";

    fn fixture() -> Vec<Worktree> {
        parse(FIXTURE, Path::new(REPO)).unwrap()
    }

    fn find<'a>(all: &'a [Worktree], suffix: &str) -> &'a Worktree {
        all.iter().find(|w| w.path.ends_with(suffix)).unwrap()
    }

    #[test]
    fn parse_drops_main_worktree() {
        let all = fixture();
        assert_eq!(all.len(), 13);
        assert!(all.iter().all(|w| w.path != REPO));
    }

    #[test]
    fn state_checks_from_fixture() {
        let all = fixture();
        assert_eq!(check_state(find(&all, "api.chore-docs")), Ok(()));
        assert_eq!(
            check_state(find(&all, "api.feat-search")),
            Err("not integrated (state: diverged)".into())
        );
        assert_eq!(
            check_state(find(&all, "api.fix-retry")),
            Err("not integrated (state: would_conflict)".into())
        );
        let prunable = check_state(find(&all, "/private/tmp/api-build-1")).unwrap_err();
        assert!(prunable.starts_with("prunable:"), "{prunable}");
        // Detached, but integrated and clean.
        assert_eq!(check_state(find(&all, "eval/base")), Ok(()));
    }

    #[test]
    fn changes_fail_even_when_integrated() {
        let mut wt = find(&fixture(), "api.chore-docs").clone();
        wt.changes.as_mut().unwrap().untracked = true;
        wt.changes.as_mut().unwrap().staged = true;
        assert_eq!(
            check_state(&wt),
            Err("has changes: staged, untracked".into())
        );
        wt.changes = None;
        assert_eq!(check_state(&wt), Err("changes are not known".into()));
    }

    #[test]
    fn decisions_from_fixture() {
        let home = Path::new("/Users/dev");
        let cfg = config::parse(config::DEFAULTS).unwrap();
        let all = fixture();
        let t3 = find(&all, "/.t3/worktrees/api/t3code-1a2b3c4d");
        let (rule, _) = rule_for(&cfg.worktrees, &t3.path, home);
        assert_eq!(rule.name, "T3");
        assert_eq!(rule.action, config::Action::Keep);

        let integrated: Vec<_> = all
            .iter()
            .filter(|w| rule_for(&cfg.worktrees, &w.path, home).0.name == "worktree")
            .filter(|w| check_state(w).is_ok())
            .map(|w| w.path.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(
            integrated,
            [
                "api.chore-docs",
                "api.feat-billing-ids",
                "api.fix-log-filter",
                "api.chore-bump",
                "base"
            ]
        );
    }

    #[test]
    fn patterns() {
        assert_eq!(
            matches("/h/.t3/worktrees/*/*", "/h/.t3/worktrees/api/t3code-1"),
            Some(vec![])
        );
        assert_eq!(
            matches("/h/.t3/worktrees/*/*", "/h/.t3/worktrees/api"),
            None
        );
        assert_eq!(
            matches("/h/.t3/worktrees/*/*", "/h/.t3/worktrees/api/x/y"),
            None
        );
        assert_eq!(
            matches("/h/trees/{project}/{id}", "/h/trees/api/ab12"),
            Some(vec![
                ("project".into(), "api".into()),
                ("id".into(), "ab12".into())
            ])
        );
        assert_eq!(
            matches("/h/trees/{project}/{id}", "/h/trees/api/x/ab12"),
            None
        );
    }

    #[test]
    fn fill_quotes_values() {
        let vars = vec![
            ("repo".to_string(), "/r".to_string()),
            ("path".to_string(), "/w/it's; rm -rf ~".to_string()),
        ];
        assert_eq!(
            fill("wt -C {repo} remove {path}", &vars).unwrap(),
            r"wt -C '/r' remove '/w/it'\''s; rm -rf ~'"
        );
        assert_eq!(
            fill("docker ps --format {{.Name}} { y }", &vars).unwrap(),
            "docker ps --format {{.Name}} { y }"
        );
        assert!(
            fill("git branch -D {branch}", &vars)
                .unwrap_err()
                .to_string()
                .contains("{branch}")
        );
    }

    #[test]
    fn placeholders_inside_quotes_are_errors() {
        let vars = vec![("path".to_string(), "/a/$(touch x)".to_string())];
        for t in [
            r#"rm "{path}""#,
            "rm '{path}'",
            r#"sh -c "cd {path} && ls""#,
        ] {
            assert!(
                fill(t, &vars)
                    .unwrap_err()
                    .to_string()
                    .contains("inside quotes"),
                "{t}"
            );
            assert!(check_template(t).is_err(), "{t}");
        }
        assert_eq!(
            fill(r#"echo "a" {path} 'b'"#, &vars).unwrap(),
            r#"echo "a" '/a/$(touch x)' 'b'"#
        );
        assert_eq!(
            fill(r#"echo \"{path}"#, &vars).unwrap(),
            r#"echo \"'/a/$(touch x)'"#
        );
        assert!(check_template("wt -C {repo} remove {path}").is_ok());
    }

    #[test]
    fn filled_command_is_one_word_to_the_shell() {
        let vars = vec![("path".to_string(), "/a b/$(touch x)/c".to_string())];
        let cmd = fill("printf '%s|' {path}", &vars).unwrap();
        assert_eq!(exec::sh(&cmd).unwrap().stdout, "/a b/$(touch x)/c|");
    }

    #[test]
    fn ignored_build_output_passes() {
        let out = "!! node_modules/\0!! packages/web/.next/\0!! target/\0!! .DS_Store\0!! src/.DS_Store\0!! api/_build/dev/lib/\0!! web/node_modules/.cache/x.json";
        assert_eq!(check_status(out), Ok(()));
        assert_eq!(check_status(""), Ok(()));
    }

    #[test]
    fn ignored_data_fails() {
        let fail = |out: &str| check_status(out).unwrap_err();
        assert_eq!(
            fail("!! .env\0!! tmp/"),
            "ignored files are not build output: .env, tmp/"
        );
        assert_eq!(
            fail("!! a\0!! b\0!! node_modules/\0!! c\0!! d\0!! e"),
            "ignored files are not build output: a, b, c and 2 more"
        );
        assert_eq!(
            fail("!! node_modules/\0!! .env"),
            "ignored file is not build output: .env"
        );
        assert_eq!(
            fail("!! tmp/notes.md"),
            "ignored file is not build output: tmp/notes.md"
        );
        assert_eq!(fail("!! tmp/"), "ignored file is not build output: tmp/");
        assert_eq!(fail("!! log/"), "ignored file is not build output: log/");
        assert_eq!(
            fail("!! db/dev.sqlite3"),
            "ignored file is not build output: db/dev.sqlite3"
        );
        assert_eq!(
            fail("!! certs/server.key"),
            "ignored file is not build output: certs/server.key"
        );
        assert_eq!(
            fail("!! public/app.js"),
            "ignored file is not build output: public/app.js"
        );
        // A file named like a build folder is not a build folder.
        assert_eq!(fail("!! build"), "ignored file is not build output: build");
        // -z gives raw names; quote characters are part of the name.
        assert_eq!(
            fail("!! .DS_Store\""),
            "ignored file is not build output: .DS_Store\""
        );
        assert_eq!(
            fail("!! \"target/"),
            "ignored file is not build output: \"target/"
        );
        assert_eq!(
            fail("!! .DS_Store/"),
            "ignored file is not build output: .DS_Store/"
        );
    }

    #[test]
    fn changes_in_status_fail() {
        assert_eq!(
            check_status("?? new.txt"),
            Err("has changes: ?? new.txt".into())
        );
        assert_eq!(
            check_status(" M src/a.rs"),
            Err("has changes:  M src/a.rs".into())
        );
    }
}
