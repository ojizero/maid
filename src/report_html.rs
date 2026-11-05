//! The offline HTML view of one saved run.

use std::fmt::Write;

use crate::record::{Decision, Item, Outcome, Record, RunKind};
use crate::size::human;

mod breakdown;

pub fn render(record: &Record) -> String {
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>maid report</title><style>",
    );
    out.push_str(include_str!("report_html.css"));
    out.push_str("</style></head><body><main>");
    let status = if record.finished_at.is_none() {
        "Not finished"
    } else if record.items.iter().any(|i| i.result == Outcome::Failed) {
        "Finished with failed items"
    } else {
        "Finished"
    };
    write!(
        out,
        "<header><p class=\"eyebrow\">maid · saved run</p><h1>{}</h1><p>{} · {}</p></header>",
        escape(&format!("maid {}", record.kind.command())),
        escape(&record.host),
        status
    )
    .unwrap();
    write!(out, "<dl class=\"metadata\"><div><dt>Started (UTC)</dt><dd>{}</dd></div><div><dt>Finished (UTC)</dt><dd>{}</dd></div><div><dt>Completed steps</dt><dd>{}</dd></div></dl>", escape(&record.started_at), record.finished_at.as_deref().map(escape).unwrap_or_else(|| "—".into()), escape(&crate::record::steps_text(&record.steps))).unwrap();
    if record.kind != RunKind::Cleanup {
        out.push_str("<p class=\"notice\">This was a survey or dry run. Planned commands are not completed cleanup.</p>");
    }
    if record.finished_at.is_none() {
        out.push_str("<p class=\"notice\">This run did not finish. Results show only the progress saved so far.</p>");
    }
    out.push_str(
        "<section aria-labelledby=\"space\"><h2 id=\"space\">Space</h2><dl class=\"metrics\">",
    );
    metric(&mut out, "Free before", bytes(record.free_bytes_before));
    metric(&mut out, "Free after", bytes(record.free_bytes_after));
    // A filesystem delta and the sum of item measurements can differ.
    let freed: Option<u64> = record
        .items
        .iter()
        .filter_map(|i| i.freed_bytes)
        .reduce(u64::saturating_add);
    metric(&mut out, "Item space freed", bytes(freed));
    out.push_str("</dl>");
    if let (Some(before), Some(after)) = (record.free_bytes_before, record.free_bytes_after) {
        let max = before.max(after).max(1);
        out.push_str("<div class=\"space-chart\" role=\"img\" aria-label=\"Filesystem free space before and after, on the same scale\">");
        for (label, value) in [("Before", before), ("After", after)] {
            write!(out, "<div><span>{label}</span><span class=\"track\"><span class=\"bar\" style=\"width:{:.2}%\"></span></span><span>{}</span></div>", value as f64 / max as f64 * 100.0, human(value)).unwrap();
        }
        out.push_str("</div>");
    }
    out.push_str("<p class=\"muted\">Item measurements can overlap. Item space freed can differ from the change in filesystem free space. Unknown values are shown as —.</p></section>");
    out.push_str(&breakdown::render(record));
    out.push_str("<section aria-labelledby=\"results\"><h2 id=\"results\">Recorded results</h2><div class=\"counts\">");
    for (result, label) in [
        (Outcome::Done, "Done"),
        (Outcome::Failed, "Failed"),
        (Outcome::Planned, "Planned"),
        (Outcome::Skipped, "Skipped"),
    ] {
        let count = record.items.iter().filter(|i| i.result == result).count();
        write!(out, "<span><strong>{count}</strong> {label}</span>").unwrap();
    }
    out.push_str("</div><p class=\"muted\">Decisions and results describe this run. A skipped item is not proof that its files remain.</p></section>");
    if !record.warnings.is_empty() {
        write!(
            out,
            "<section class=\"notice\"><h2>Warnings ({})</h2><ul>",
            record.warnings.len()
        )
        .unwrap();
        for warning in &record.warnings {
            write!(out, "<li>{}</li>", escape(warning)).unwrap();
        }
        out.push_str("</ul></section>");
    }
    out.push_str("<p class=\"muted\">Item rows show the measured size before cleanup. Open an item for its reason, commands and space freed.</p>");
    for (decision, label) in [
        (Decision::Clean, "Clean"),
        (Decision::Ask, "Ask"),
        (Decision::Keep, "Keep"),
    ] {
        let mut items: Vec<_> = record
            .items
            .iter()
            .filter(|i| i.decision == decision)
            .collect();
        if items.is_empty() {
            continue;
        }
        items.sort_by_key(|i| (i.result, std::cmp::Reverse(i.size_bytes)));
        write!(
            out,
            "<section><h2>{label} <small>({})</small></h2>",
            items.len()
        )
        .unwrap();
        for item in items {
            write_item(&mut out, item);
        }
        out.push_str("</section>");
    }
    out.push_str("<footer>Source: saved maid run · No scan or cleanup runs when you open this report.</footer></main><script>");
    out.push_str(include_str!("report_html.js"));
    out.push_str("</script></body></html>\n");
    out
}

fn metric(out: &mut String, label: &str, value: String) {
    write!(out, "<div><dt>{label}</dt><dd>{value}</dd></div>").unwrap();
}

fn bytes(value: Option<u64>) -> String {
    value.map_or_else(|| "—".into(), human)
}

fn write_item(out: &mut String, item: &Item) {
    let result = match item.result {
        Outcome::Done => "done",
        Outcome::Failed => "failed",
        Outcome::Planned => "planned",
        Outcome::Skipped => "skipped",
    };
    let kind = match item.kind {
        crate::record::Kind::Target => "target",
        crate::record::Kind::Worktree => "worktree",
        crate::record::Kind::Unknown => "unknown",
    };
    let name = if item.kind == crate::record::Kind::Worktree {
        item.branch
            .as_deref()
            .or(item.path.as_deref())
            .unwrap_or(&item.name)
    } else {
        &item.name
    };
    write!(out, "<details><summary><span class=\"item-name\">{}</span><span class=\"result {result}\">{result}</span><span class=\"size\">{}</span></summary><div class=\"item-detail\"><p>{kind}</p>", escape(name), bytes(item.size_bytes)).unwrap();
    for (label, value) in [
        ("Path", &item.path),
        ("Repository", &item.repo),
        ("Branch", &item.branch),
        ("Reason", &item.reason),
    ] {
        if let Some(value) = value {
            write!(out, "<p><strong>{label}:</strong> {}</p>", escape(value)).unwrap();
        }
    }
    if let Some(freed) = item.freed_bytes {
        write!(out, "<p><strong>Space freed:</strong> {}</p>", human(freed)).unwrap();
    }
    if !item.commands.is_empty() {
        out.push_str("<p>Recorded commands</p><pre><code>");
        for command in &item.commands {
            writeln!(out, "{}", escape(command)).unwrap();
        }
        out.push_str("</code></pre>");
    }
    if let Some(error) = &item.error {
        write!(out, "<pre class=\"error\">{}</pre>", escape(error)).unwrap();
    }
    if !item.missing_programs.is_empty() {
        write!(
            out,
            "<p>Missing programs: {}</p>",
            escape(&item.missing_programs.join(", "))
        )
        .unwrap();
    }
    out.push_str("</div></details>");
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Kind;

    #[test]
    fn escapes_saved_text_and_marks_incomplete_runs() {
        let mut item = Item::new(Kind::Target, "<script>alert('x')</script>");
        item.error = Some("<img src=x onerror=alert(1)>".into());
        item.commands.push("echo '<hello>' & exit".into());
        let record = Record {
            version: 2,
            host: "<host>".into(),
            kind: RunKind::DryRun,
            steps: vec![RunKind::Survey],
            started_at: "now".into(),
            finished_at: None,
            free_bytes_before: None,
            free_bytes_after: None,
            items: vec![item],
            warnings: vec!["<warning>".into()],
        };
        let html = render(&record);
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("<img"));
        assert!(html.contains("&lt;host&gt;"));
        assert!(html.contains("&lt;warning&gt;"));
        assert!(html.contains("&lt;hello&gt;"));
        assert!(html.contains("This run did not finish"));
        assert!(html.contains("Planned commands are not completed cleanup"));
        assert!(html.contains("<dd>—</dd>"));
    }
}
