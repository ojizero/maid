//! Render a run file as text or JSON.

use std::fmt::Write;
use std::path::Path;

use crate::record::{Command, Decision, Kind, Outcome, Record};
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

pub fn json(record: &Record) -> String {
    serde_json::to_string_pretty(record).expect("a record is always valid JSON") + "\n"
}

/// A table: free space, then the items grouped by decision and result.
pub fn text(record: &Record, home: &Path, color: bool) -> String {
    let bold = |s: &str| {
        if color {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    };
    let opt = |b: Option<u64>| b.map_or("?".into(), human);
    let command = match record.command {
        Command::Survey => "survey",
        Command::DryRun => "cleanup --dry-run",
        Command::Cleanup => "cleanup",
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}",
        bold(&format!(
            "maid {command} on {} at {}",
            record.host, record.finished_at
        ))
    );
    let _ = writeln!(
        out,
        "Free space: {} before, {} after",
        opt(record.free_bytes_before),
        opt(record.free_bytes_after)
    );

    let mut items: Vec<_> = record.items.iter().collect();
    items.sort_by_key(|i| (i.decision, i.result, std::cmp::Reverse(i.size_bytes)));
    let mut groups: Vec<((Decision, Outcome), Vec<_>)> = Vec::new();
    for item in items {
        let key = (item.decision, item.result);
        match groups.last_mut() {
            Some((k, list)) if *k == key => list.push(item),
            _ => groups.push((key, vec![item])),
        }
    }

    for ((decision, result), list) in groups {
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
            let _ = write!(head, ", {} freed", human(freed));
        }
        head.push(')');
        let _ = writeln!(out, "\n{}", bold(&head));
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
                let _ = write!(line, "  {place}");
            }
            let _ = writeln!(out, "{line}");
            if let Some(freed) = item.freed_bytes {
                let _ = writeln!(out, "             freed {}", human(freed));
            }
            if let Some(reason) = &item.reason {
                let _ = writeln!(out, "             {reason}");
            }
            if matches!(result, Outcome::Planned | Outcome::Done | Outcome::Failed) {
                for cmd in &item.commands {
                    let _ = writeln!(out, "             $ {cmd}");
                }
            }
            if let Some(error) = &item.error {
                for line in error.lines() {
                    let _ = writeln!(out, "             ! {line}");
                }
            }
        }
    }
    if !record.warnings.is_empty() {
        let _ = writeln!(
            out,
            "\n{}",
            bold(&format!("Warnings ({})", record.warnings.len()))
        );
        for w in &record.warnings {
            let _ = writeln!(out, "  {w}");
        }
    }
    out
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
