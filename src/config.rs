//! The config file. `maid init` writes the defaults to it.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const DEFAULTS: &str = include_str!("../config/default.toml");

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
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
    /// Programs that must be on PATH. `init` and `doctor` check them.
    /// None when the config does not say; `[]` when it needs none.
    pub needs: Option<Vec<String>>,
    pub action: Action,
    pub path: Option<String>,
    /// A command that prints the path, for tools whose folder differs per
    /// platform or setting.
    pub locate: Option<String>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    target: Vec<Target>,
    worktrees: FileWorktrees,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWorktrees {
    repos: Vec<String>,
    delete_branch: DeleteBranch,
    action: Action,
    why: Option<String>,
    dry_run: Option<String>,
    clean: Option<String>,
    #[serde(default, rename = "override")]
    overrides: Vec<Rule>,
}

/// The config file: the -c file, else the one in the XDG config folder.
pub fn path(flag: Option<&Path>, home: &Path) -> PathBuf {
    match flag {
        Some(path) => path.to_path_buf(),
        None => crate::sys::xdg("XDG_CONFIG_HOME", ".config", home).join("maid/config.toml"),
    }
}

/// Load the config file. Every error here is a config error.
pub fn load(path: &Path) -> Result<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("no config at {} (run `maid init`)", path.display())
        }
        Err(e) => return Err(e).with_context(|| format!("cannot read config {}", path.display())),
    };
    check_owner(path)?;
    parse(&text).with_context(|| format!("config {}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    let file: File = toml::from_str(text)?;
    let w = file.worktrees;
    let config = Config {
        targets: file.target,
        worktrees: Worktrees {
            repos: w.repos,
            delete_branch: w.delete_branch,
            default: Rule {
                name: "worktree".into(),
                path: String::new(),
                action: w.action,
                why: w.why,
                dry_run: w.dry_run,
                clean: w.clean,
            },
            overrides: w.overrides,
        },
    };
    validate(&config)?;
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

fn validate(config: &Config) -> Result<()> {
    let mut seen = HashSet::new();
    for t in &config.targets {
        if !seen.insert(&t.name) {
            bail!("target {:?} is defined twice", t.name);
        }
        let what = format!("target {:?}", t.name);
        check_action(&what, t.action, t.clean.as_deref(), t.why.as_deref())?;
        let set = [&t.path, &t.locate, &t.size];
        if set.iter().filter(|f| f.is_some()).count() != 1 {
            bail!("{what}: set exactly one of path, locate and size");
        }
    }
    let w = &config.worktrees;
    check_rule("[worktrees]", &w.default)?;
    for o in &w.overrides {
        let what = format!("worktree override {:?}", o.name);
        check_rule(&what, o)?;
        if !(o.path.starts_with('/') || o.path.starts_with("~/")) {
            bail!("{what}: path must start with / or ~/");
        }
    }
    Ok(())
}

fn check_rule(what: &str, rule: &Rule) -> Result<()> {
    check_action(
        what,
        rule.action,
        rule.clean.as_deref(),
        rule.why.as_deref(),
    )?;
    for t in [&rule.clean, &rule.dry_run].into_iter().flatten() {
        crate::worktree::check_template(t).with_context(|| what.to_string())?;
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

impl Target {
    /// The programs in `needs` that are not on PATH.
    pub fn missing_programs(&self) -> Vec<&str> {
        self.needs
            .iter()
            .flatten()
            .map(String::as_str)
            .filter(|program| crate::sys::find_program(program).is_none())
            .collect()
    }
}

impl Config {
    /// Every name that --only accepts: targets, worktree overrides, and
    /// "worktree" for the [worktrees] default.
    pub fn names(&self) -> Vec<&str> {
        let w = &self.worktrees;
        let rules = w.overrides.iter().chain(std::iter::once(&w.default));
        let targets = self.targets.iter().map(|t| t.name.as_str());
        targets.chain(rules.map(|r| r.name.as_str())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults with one change: `old` becomes `new`.
    fn defaults_with(old: &str, new: &str) -> String {
        assert!(DEFAULTS.contains(old), "{old}");
        DEFAULTS.replacen(old, new, 1)
    }

    fn err(text: &str) -> String {
        format!("{:#}", parse(text).unwrap_err())
    }

    /// The error for the defaults plus `extra` at the end.
    fn err_with(extra: &str) -> String {
        err(&format!("{DEFAULTS}\n{extra}"))
    }

    #[test]
    fn defaults_are_valid() {
        let c = parse(DEFAULTS).unwrap();
        assert!(c.targets.iter().any(|t| t.name == "Homebrew cache"));
        assert_eq!(c.worktrees.delete_branch, DeleteBranch::Pushed);
        assert_eq!(c.worktrees.overrides[0].name, "T3");
        assert_eq!(c.worktrees.default.name, "worktree");
    }

    #[test]
    fn the_file_is_the_whole_config() {
        let c = parse(
            r#"
            [[target]]
            name = "uv cache"
            action = "keep"
            path = "~/.cache/uv"
            why = "offline work"

            [worktrees]
            repos = ["~/code"]
            delete_branch = "never"
            action = "keep"
            why = "mine"
            "#,
        )
        .unwrap();
        assert_eq!(c.targets.len(), 1);
        assert_eq!(c.worktrees.repos, ["~/code"]);
        assert_eq!(c.worktrees.default.clean, None);
        assert!(c.worktrees.overrides.is_empty());
    }

    #[test]
    fn rejects_bad_config() {
        assert!(err("").contains("missing field `worktrees`"));
        assert!(
            err_with("[[target]]\nname='x'\naction='clean'\npath='/x'\nclean='true'\ncolour='red'")
                .contains("unknown field `colour`")
        );
        assert!(
            err_with("[[target]]\nname='x'\naction='delete'\npath='/x'")
                .contains("unknown variant `delete`")
        );
        assert!(
            err_with("[[target]]\nname='x'\naction='clean'\npath='/x'")
                .contains("needs a clean command")
        );
        assert!(err_with("[[target]]\nname='x'\naction='keep'\npath='/x'").contains("needs why"));
        assert!(
            err_with("[[target]]\nname='x'\naction='keep'\nwhy='y'")
                .contains("exactly one of path, locate and size")
        );
        assert!(
            err_with("[[target]]\nname='x'\naction='keep'\nwhy='y'\npath='/x'\nlocate='echo /x'")
                .contains("exactly one")
        );
        assert!(
            err_with("[[target]]\nname='x'\naction='keep'\nwhy='y'\npath='/x'\nsize='echo 1GB'")
                .contains("exactly one")
        );
        assert!(
            err_with("[[target]]\nname='uv cache'\naction='keep'\nwhy='y'\npath='/a'")
                .contains("twice")
        );
        assert!(
            err_with(
                "[[target]]\nname='x'\naction='keep'\nwhy='y'\npath='/a'\nskip_if_running='brew'"
            )
            .contains("invalid type")
        );
        assert!(
            err_with("[[worktrees.override]]\nname='x'\npath='rel/*'\naction='keep'\nwhy='y'")
                .contains("must start with")
        );

        let pushed = r#"delete_branch = "pushed""#;
        assert!(
            err(&defaults_with(pushed, r#"delete_branch = "always""#))
                .contains("unknown variant `always`")
        );
        let clean =
            r#"clean         = "wt -C {repo} remove --foreground --yes --no-delete-branch {path}""#;
        assert!(err(&defaults_with(clean, "clean = ''")).contains("needs a clean command"));
        assert!(
            err(&defaults_with(clean, r#"clean = 'rm -rf "{path}"'"#)).contains("inside quotes")
        );
        let repos = r#"repos         = ["~/workspace"]"#;
        assert!(err(&defaults_with(repos, "repo = ['~']")).contains("unknown field `repo`"));
    }

    #[test]
    fn missing_file_points_to_init() {
        let missing = Path::new("/nonexistent/maid/config.toml");
        assert!(format!("{:#}", load(missing).unwrap_err()).contains("run `maid init`"));
    }
}
