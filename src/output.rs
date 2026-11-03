//! Render a run file as text or JSON.

use std::fmt::{self, Write};
use std::path::Path;

use crate::record::{Decision, Kind, Outcome, Record, RunKind};
use crate::size::human;
use crate::sys;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Text,
    Json,
    Html,
}

pub fn render(record: &Record, format: Format, home: &Path) -> String {
    match format {
        Format::Text => text(record, home, sys::color()),
        Format::Json => json(record),
        Format::Html => unreachable!("App::new rejects html"),
    }
}

fn json(record: &Record) -> String {
    serde_json::to_string_pretty(record).expect("a record is always valid JSON") + "\n"
}

/// Lines under an item line start here.
const INDENT: &str = "             ";

fn text(record: &Record, home: &Path, color: bool) -> String {
    let mut out = String::new();
    write_text(&mut out, record, home, color).expect("writing to a String cannot fail");
    out
}

/// A table: free space, then the items grouped by decision and result.
fn write_text(out: &mut String, record: &Record, home: &Path, color: bool) -> fmt::Result {
    let bold = |s: &str| {
        if color {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    };
    let opt = |b: Option<u64>| b.map_or("?".into(), human);
    let command = match record.kind {
        RunKind::Survey => "survey",
        RunKind::DryRun => "cleanup --dry-run",
        RunKind::Cleanup => "cleanup",
    };

    writeln!(
        out,
        "{}",
        bold(&format!(
            "maid {command} on {} at {}",
            record.host, record.finished_at
        ))
    )?;
    writeln!(
        out,
        "Free space: {} before, {} after",
        opt(record.free_bytes_before),
        opt(record.free_bytes_after)
    )?;

    let mut items: Vec<_> = record.items.iter().collect();
    items.sort_by_key(|i| (i.decision, i.result, std::cmp::Reverse(i.size_bytes)));
    for list in items.chunk_by(|a, b| (a.decision, a.result) == (b.decision, b.result)) {
        let (decision, result) = (list[0].decision, list[0].result);
        let total: u64 = list.iter().filter_map(|i| i.size_bytes).sum();
        let freed: u64 = list.iter().filter_map(|i| i.freed_bytes).sum();
        let mut head = format!(
            "{} · {} ({} items, {}",
            label(decision),
            result_label(result),
            list.len(),
            human(total)
        );
        if result == Outcome::Done {
            write!(head, ", {} freed", human(freed))?;
        }
        head.push(')');
        writeln!(out, "\n{}", bold(&head))?;
        for item in list {
            let kind = match item.kind {
                Kind::Target => "target",
                Kind::Worktree => "worktree",
                Kind::Unknown => "unknown",
            };
            let place = item
                .path
                .as_deref()
                .or(item.repo.as_deref())
                .map(|p| sys::tilde(p, home));
            let mut line = format!("  {:>9}  {kind:<8}  {}", opt(item.size_bytes), item.name);
            if let Some(place) = place {
                write!(line, "  {place}")?;
            }
            writeln!(out, "{line}")?;
            if let Some(freed) = item.freed_bytes {
                writeln!(out, "{INDENT}freed {}", human(freed))?;
            }
            if let Some(reason) = &item.reason {
                writeln!(out, "{INDENT}{reason}")?;
            }
            if matches!(result, Outcome::Planned | Outcome::Done | Outcome::Failed) {
                for cmd in &item.commands {
                    writeln!(out, "{INDENT}$ {cmd}")?;
                }
            }
            if let Some(error) = &item.error {
                for line in error.lines() {
                    writeln!(out, "{INDENT}! {line}")?;
                }
            }
        }
    }
    if !record.warnings.is_empty() {
        writeln!(
            out,
            "\n{}",
            bold(&format!("Warnings ({})", record.warnings.len()))
        )?;
        for w in &record.warnings {
            writeln!(out, "  {w}")?;
        }
    }
    Ok(())
}

fn label(d: Decision) -> &'static str {
    match d {
        Decision::Clean => "CLEAN",
        Decision::Ask => "ASK",
        Decision::Keep => "KEEP",
    }
}

fn result_label(r: Outcome) -> &'static str {
    match r {
        Outcome::Failed => "failed",
        Outcome::Done => "done",
        Outcome::Planned => "planned",
        Outcome::Skipped => "skipped",
    }
}
