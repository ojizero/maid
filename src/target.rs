//! A target: measure it, and check if it can be cleaned now.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Target;
use crate::{exec, size, sys};

/// The target's folder: its path, or what its locate command prints.
/// None for a target that has a size command.
pub fn locate(t: &Target, home: &Path) -> Result<Option<PathBuf>> {
    if let Some(path) = &t.path {
        return Ok(Some(sys::expand(path, home)));
    }
    let Some(cmd) = &t.locate else {
        return Ok(None);
    };
    let out = exec::sh(cmd).with_context(|| format!("locate {cmd:?}"))?;
    if !out.status.success() {
        bail!("locate {cmd:?} failed: {}", out.failure());
    }
    let path = sys::expand(out.stdout.trim(), home);
    if !path.is_absolute() {
        bail!(
            "locate {cmd:?} printed {:?}, not an absolute path",
            out.stdout.trim()
        );
    }
    Ok(Some(path))
}

/// The size in bytes: `du` of the folder that `locate` found, or the
/// output of the size command.
pub fn measure(t: &Target, path: Option<&Path>) -> Result<Option<u64>> {
    if let Some(path) = path {
        return Ok(size::du(path));
    }
    let cmd = t.size.as_deref().unwrap_or_default();
    let out = exec::sh(cmd)?;
    if !out.status.success() {
        // Docker prints nothing useful when it is not running: the size is not known.
        return Ok(None);
    }
    size::parse(&out.stdout).with_context(|| format!("size command {cmd:?}"))
}

/// The reason not to clean a target now, if there is one. `path` is the
/// folder that `locate` found.
pub fn blocker(t: &Target, path: Option<&Path>) -> Option<String> {
    if let Some(path) = path {
        match std::fs::symlink_metadata(path) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn located(cmd: &str) -> Result<Option<PathBuf>> {
        let t = Target {
            name: "x".into(),
            needs: None,
            action: crate::config::Action::Keep,
            path: None,
            locate: Some(cmd.into()),
            size: None,
            dry_run: None,
            clean: None,
            skip_if_running: Vec::new(),
            only_if: None,
            why: Some("y".into()),
        };
        locate(&t, Path::new("/home/me"))
    }

    #[test]
    fn locate_runs_the_command() {
        assert_eq!(located("echo /x/y").unwrap(), Some(PathBuf::from("/x/y")));
        assert_eq!(
            located("echo '~/c'").unwrap(),
            Some(PathBuf::from("/home/me/c"))
        );
    }

    #[test]
    fn locate_fails_without_an_absolute_path() {
        let err = |cmd| format!("{:#}", located(cmd).unwrap_err());
        assert!(err("echo oops; exit 3").contains("failed"));
        assert!(err("echo cache").contains("not an absolute path"));
        assert!(err("true").contains("not an absolute path"));
    }
}
