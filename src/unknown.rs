//! Find large folders that no target or worktree covers.

use std::path::{Path, PathBuf};

use crate::size::{GIB, du};
use crate::survey::par_map;

/// Folders this size or larger are reported.
const MIN_SIZE: u64 = GIB;

/// Folders whose children are scanned, relative to HOME.
const DEEP_ROOTS: &[&str] = &[
    "Library/Caches",
    "Library/Developer",
    ".cache",
    ".local/share",
];

/// Folders in HOME that macOS guards with a permission prompt, or that hold
/// the deep roots. They are not scanned as a whole.
const SKIP_TOP: &[&str] = &[
    "Library",
    "Desktop",
    "Documents",
    "Downloads",
    "Movies",
    "Music",
    "Pictures",
    ".Trash",
    ".cache",
    ".local",
];

pub struct Found {
    pub path: PathBuf,
    /// The size without the known items inside it.
    pub size: u64,
}

/// `known` holds the paths and sizes of targets and worktrees.
pub fn scan(
    home: &Path,
    known: &[(PathBuf, Option<u64>)],
    warnings: &mut Vec<String>,
) -> Vec<Found> {
    let mut candidates = Vec::new();
    let mut add_children = |dir: &Path, skip: &[&str]| match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name();
                // file_type does not follow symlinks.
                if entry.file_type().is_ok_and(|t| t.is_dir())
                    && !skip.contains(&name.to_string_lossy().as_ref())
                {
                    candidates.push(entry.path());
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            let message = format!("cannot scan {}: {e}", dir.display());
            eprintln!("maid: warning: {message}");
            warnings.push(message);
        }
    };
    add_children(home, SKIP_TOP);
    for root in DEEP_ROOTS {
        add_children(&home.join(root), &[]);
    }
    candidates.sort();

    let todo: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|c| !known.iter().any(|(k, _)| c.starts_with(k)))
        .collect();
    let sizes = par_map(&todo, |dir| du(dir));

    let mut found = Vec::new();
    for (path, size) in todo.into_iter().zip(sizes) {
        let inside: u64 = known
            .iter()
            .filter(|(k, _)| k.starts_with(&path))
            .filter_map(|(_, s)| *s)
            .sum();
        let size = size.unwrap_or(0).saturating_sub(inside);
        if size >= MIN_SIZE {
            found.push(Found { path, size });
        }
    }
    found
}
