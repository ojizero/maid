//! The config: embedded defaults plus the user file.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const DEFAULTS: &str = include_str!("../config/default.toml");

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Clean,
    Keep,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum DeleteBranch {
    Pushed,
    Integrated,
    Never,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub name: String,
    pub action: Action,
    pub path: Option<String>,
    pub size: Option<String>,
    pub dry_run: Option<String>,
    pub clean: Option<String>,
    #[serde(default)]
    pub skip_if_running: Vec<String>,
    pub only_if: Option<String>,
    pub why: Option<String>,
}

/// What to do with a worktree: the [worktrees] default or an override.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    /// The path pattern. Empty for the [worktrees] default.
    #[serde(default)]
    pub path: String,
    pub action: Action,
    pub why: Option<String>,
    pub dry_run: Option<String>,
    pub clean: Option<String>,
}

#[derive(Debug)]
pub struct Worktrees {
    pub repos: Vec<String>,
    pub delete_branch: DeleteBranch,
    pub default: Rule,
    /// Checked in order; the first match wins.
    pub overrides: Vec<Rule>,
}

#[derive(Debug)]
pub struct Config {
    pub targets: Vec<Target>,
    pub worktrees: Worktrees,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    target: Vec<Target>,
    #[serde(default)]
    worktrees: FileWorktrees,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileWorktrees {
    repos: Option<Vec<String>>,
    delete_branch: Option<DeleteBranch>,
    action: Option<Action>,
    why: Option<String>,
    dry_run: Option<String>,
    clean: Option<String>,
    #[serde(default, rename = "override")]
    overrides: Vec<Rule>,
}

/// The user file to read, and whether it must exist.
pub fn user_path(flag: Option<&Path>, home: &Path) -> (PathBuf, bool) {
    match flag {
        Some(path) => (path.to_path_buf(), true),
        None => (
            crate::sys::xdg("XDG_CONFIG_HOME", ".config", home).join("maid/config.toml"),
            false,
        ),
    }
}

/// Load the defaults and the user file. Every error here is a config error.
pub fn load(path: &Path, required: bool) -> Result<Config> {
    let user = match std::fs::read_to_string(path) {
        Ok(text) => {
            check_owner(path)?;
            Some(text)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => None,
        Err(e) => return Err(e).with_context(|| format!("cannot read config {}", path.display())),
    };
    let defaults: File = toml::from_str(DEFAULTS).context("embedded defaults")?;
    let user: File = match user {
        Some(text) => {
            toml::from_str(&text).with_context(|| format!("config {}", path.display()))?
        }
        None => File::default(),
    };
    let config = merge(defaults, user)?;
    validate(&config).with_context(|| format!("config {}", path.display()))?;
    Ok(config)
}

/// The config holds shell commands, so only its owner may change it.
fn check_owner(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path)?;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    if meta.uid() != uid || meta.mode() & 0o022 != 0 {
        bail!(
            "config {} must be owned by you and not writable by group or others",
            path.display()
        );
    }
    Ok(())
}

fn merge(defaults: File, user: File) -> Result<Config> {
    let mut seen = HashSet::new();
    for t in &user.target {
        if !seen.insert(t.name.as_str()) {
            bail!("target {:?} is defined twice", t.name);
        }
    }
    let mut targets = defaults.target;
    for t in user.target {
        match targets.iter_mut().find(|d| d.name == t.name) {
            Some(slot) => *slot = t,
            None => targets.push(t),
        }
    }

    let (d, u) = (defaults.worktrees, user.worktrees);
    let mut overrides = u.overrides;
    overrides.extend(d.overrides);
    let worktrees = Worktrees {
        repos: u.repos.or(d.repos).unwrap_or_default(),
        delete_branch: u
            .delete_branch
            .or(d.delete_branch)
            .unwrap_or(DeleteBranch::Never),
        default: Rule {
            name: "worktree".into(),
            path: String::new(),
            action: u.action.or(d.action).unwrap_or(Action::Keep),
            why: u.why.or(d.why),
            dry_run: u.dry_run.or(d.dry_run),
            clean: u.clean.or(d.clean),
        },
        overrides,
    };
    Ok(Config { targets, worktrees })
}

fn validate(config: &Config) -> Result<()> {
    for t in &config.targets {
        let what = format!("target {:?}", t.name);
        check_action(&what, t.action, t.clean.as_deref(), t.why.as_deref())?;
        if t.path.is_some() == t.size.is_some() {
            bail!("{what}: set exactly one of path and size");
        }
    }
    let w = &config.worktrees;
    check_action(
        "[worktrees]",
        w.default.action,
        w.default.clean.as_deref(),
        w.default.why.as_deref(),
    )?;
    for o in &w.overrides {
        let what = format!("worktree override {:?}", o.name);
        check_action(&what, o.action, o.clean.as_deref(), o.why.as_deref())?;
        if !(o.path.starts_with('/') || o.path.starts_with("~/")) {
            bail!("{what}: path must start with / or ~/");
        }
    }
    Ok(())
}

fn check_action(what: &str, action: Action, clean: Option<&str>, why: Option<&str>) -> Result<()> {
    let blank = |s: Option<&str>| s.is_none_or(|s| s.trim().is_empty());
    match action {
        Action::Clean if blank(clean) => bail!("{what}: action \"clean\" needs a clean command"),
        Action::Keep if blank(why) => bail!("{what}: action \"keep\" needs why"),
        _ => Ok(()),
    }
}

impl Config {
    /// Every name that --only accepts.
    pub fn names(&self) -> Vec<&str> {
        let targets = self.targets.iter().map(|t| t.name.as_str());
        targets
            .chain(self.worktrees.overrides.iter().map(|o| o.name.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(user: &str) -> Result<Config> {
        let config = merge(toml::from_str(DEFAULTS)?, toml::from_str(user)?)?;
        validate(&config)?;
        Ok(config)
    }

    fn err(user: &str) -> String {
        format!("{:#}", parse(user).unwrap_err())
    }

    #[test]
    fn defaults_are_valid() {
        let c = parse("").unwrap();
        assert!(c.targets.iter().any(|t| t.name == "Homebrew cache"));
        assert_eq!(c.worktrees.delete_branch, DeleteBranch::Pushed);
        assert_eq!(c.worktrees.overrides[0].name, "T3");
    }

    #[test]
    fn same_name_replaces_and_new_name_adds() {
        let c = parse(
            r#"
            [[target]]
            name = "uv cache"
            action = "keep"
            path = "~/.cache/uv"
            why = "offline work"

            [[target]]
            name = "Gradle cache"
            action = "clean"
            path = "~/.gradle/caches"
            clean = "gradle --stop"
            "#,
        )
        .unwrap();
        let defaults = parse("").unwrap();
        assert_eq!(c.targets.len(), defaults.targets.len() + 1);
        let uv = c.targets.iter().find(|t| t.name == "uv cache").unwrap();
        assert_eq!(uv.action, Action::Keep);
        assert_eq!(uv.clean, None);
        assert_eq!(c.targets.last().unwrap().name, "Gradle cache");
    }

    #[test]
    fn worktree_keys_replace_and_overrides_go_first() {
        let c = parse(
            r#"
            [worktrees]
            repos = ["~/code"]
            delete_branch = "never"

            [[worktrees.override]]
            name = "Scratch"
            path = "~/scratch/*"
            action = "keep"
            why = "mine"
            "#,
        )
        .unwrap();
        let w = &c.worktrees;
        assert_eq!(w.repos, ["~/code"]);
        assert_eq!(w.delete_branch, DeleteBranch::Never);
        assert!(
            w.default
                .clean
                .as_deref()
                .unwrap()
                .starts_with("wt -C {repo} remove")
        );
        let names: Vec<_> = w.overrides.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["Scratch", "T3"]);
    }

    #[test]
    fn rejects_bad_config() {
        assert!(
            err("[[target]]\nname='x'\naction='clean'\npath='/x'\nclean='true'\ncolour='red'")
                .contains("unknown field `colour`")
        );
        assert!(
            err("[[target]]\nname='x'\naction='delete'\npath='/x'")
                .contains("unknown variant `delete`")
        );
        assert!(
            err("[[target]]\nname='x'\naction='clean'\npath='/x'")
                .contains("needs a clean command")
        );
        assert!(err("[[target]]\nname='x'\naction='keep'\npath='/x'").contains("needs why"));
        assert!(
            err("[[target]]\nname='x'\naction='keep'\nwhy='y'")
                .contains("exactly one of path and size")
        );
        assert!(
            err("[[target]]\nname='x'\naction='keep'\nwhy='y'\npath='/x'\nsize='echo 1GB'")
                .contains("exactly one")
        );
        assert!(err("[worktrees]\ndelete_branch='always'").contains("unknown variant `always`"));
        assert!(err("[worktrees]\nclean=''").contains("needs a clean command"));
        assert!(err("[worktrees]\nrepo=['~']").contains("unknown field `repo`"));
        assert!(
            err("[[worktrees.override]]\nname='x'\npath='rel/*'\naction='keep'\nwhy='y'")
                .contains("must start with")
        );
        assert!(err("[[target]]\nname='x'\naction='keep'\nwhy='y'\npath='/a'\n[[target]]\nname='x'\naction='keep'\nwhy='y'\npath='/b'").contains("twice"));
        assert!(
            err("[[target]]\nname='x'\naction='keep'\nwhy='y'\npath='/a'\nskip_if_running='brew'")
                .contains("invalid type")
        );
    }

    #[test]
    fn missing_user_file_is_fine_unless_required() {
        let missing = Path::new("/nonexistent/maid/config.toml");
        assert!(load(missing, false).is_ok());
        assert!(load(missing, true).is_err());
    }
}
