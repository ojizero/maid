//! A target: measure it, and check if it can be cleaned now.

use std::path::Path;

use anyhow::{Context, Result};

use crate::config::Target;
use crate::{exec, size, sys};

/// The size in bytes: `du` of the path, or the output of the size command.
pub fn measure(t: &Target, home: &Path) -> Result<Option<u64>> {
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
pub fn blocker(t: &Target, home: &Path) -> Option<String> {
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
