//! `maid init`: write a config with the known targets that this computer
//! can use.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::app::{App, usage};
use crate::config::{self, DEFAULTS, Target};
use crate::sys;

pub fn run(app: &App) -> Result<ExitCode> {
    let path = app.config_path();
    let known = Known::new();
    let mut text = known.head.to_string();
    let mut added = 0;
    let mut left_out = Vec::new();
    for (block, target) in &known.targets {
        let missing = target.missing_programs();
        if missing.is_empty() {
            text.push_str(block);
            added += 1;
        } else {
            left_out.push(format!("{} (needs {})", target.name, missing.join(", ")));
        }
    }
    text.push_str(known.tail);
    config::parse(&text).context("the config that init built is not valid")?;

    write_new(&path, &text)?;
    println!(
        "Wrote {} with {added} targets.",
        sys::tilde(&path.to_string_lossy(), &app.home)
    );
    if !left_out.is_empty() {
        println!("Left out, because a program is not on PATH:");
        for line in &left_out {
            println!("  {line}");
        }
    }
    println!("Edit the file, then run `maid doctor`.");
    Ok(ExitCode::SUCCESS)
}

/// The default config in parts: the text before the first target, one
/// block of text for each known target, and the [worktrees] text after
/// the last target. Together they are the whole default config.
struct Known {
    head: &'static str,
    targets: Vec<(&'static str, Target)>,
    tail: &'static str,
}

#[derive(Deserialize)]
struct Block {
    target: Vec<Target>,
}

impl Known {
    fn new() -> Known {
        let start = DEFAULTS
            .find("\n[[target]]")
            .expect("default.toml has targets")
            + 1;
        let end = DEFAULTS
            .find("\n# ── Worktrees")
            .expect("default.toml has a worktrees part")
            + 1;
        let mut bounds = vec![start];
        bounds.extend(
            DEFAULTS[start..end]
                .match_indices("\n[[target]]")
                .map(|(i, _)| start + i + 1),
        );
        bounds.push(end);
        let targets = bounds
            .windows(2)
            .map(|w| {
                let block = &DEFAULTS[w[0]..w[1]];
                let mut parsed: Block = toml::from_str(block).expect("a known target is valid");
                (block, parsed.target.remove(0))
            })
            .collect();
        Known {
            head: &DEFAULTS[..start],
            targets,
            tail: &DEFAULTS[end..],
        }
    }
}

/// Write a new file with mode 0600: the config holds shell commands, see
/// config::check_owner. A file that exists is a usage error.
fn write_new(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path);
    let mut file = match file {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return usage(Err(anyhow::anyhow!(
                "config {} exists already; edit it, or delete it and run `maid init` again",
                path.display()
            )));
        }
        Err(e) => return Err(e).with_context(|| format!("cannot create {}", path.display())),
    };
    file.write_all(text.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_parts_are_the_whole_default_config() {
        let known = Known::new();
        let blocks: String = known.targets.iter().map(|(b, _)| *b).collect();
        assert_eq!(format!("{}{blocks}{}", known.head, known.tail), DEFAULTS);
        let all = config::parse(DEFAULTS).unwrap();
        let names: Vec<_> = known.targets.iter().map(|(_, t)| &t.name).collect();
        let expected: Vec<_> = all.targets.iter().map(|t| &t.name).collect();
        assert_eq!(names, expected);
    }

    /// init and doctor can check a known target only through its needs.
    #[test]
    fn every_known_target_has_needs() {
        for (_, t) in Known::new().targets {
            assert!(t.needs.is_some(), "{} has no needs", t.name);
        }
    }

    #[test]
    fn config_without_known_targets_is_valid() {
        let known = Known::new();
        let c = config::parse(&format!("{}{}", known.head, known.tail)).unwrap();
        assert!(c.targets.is_empty());
    }
}
