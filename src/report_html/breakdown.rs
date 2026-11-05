//! Pie charts of recorded item sizes and cleanup results.

use std::fmt::Write;

use crate::record::{Decision, Kind, Outcome, Record, RunKind};
use crate::size::human;

use super::escape;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Freed,
    NotCleaned,
    Review,
    Keep,
    Remaining,
    Unmeasured,
}

impl Category {
    const ALL: [Self; 6] = [
        Self::Freed,
        Self::NotCleaned,
        Self::Review,
        Self::Keep,
        Self::Remaining,
        Self::Unmeasured,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Freed => "Space freed",
            Self::NotCleaned => "Clean candidates not cleaned",
            Self::Review => "Needs review",
            Self::Keep => "Kept by policy",
            Self::Remaining => "Remaining after cleanup",
            Self::Unmeasured => "Cleanup size not measured",
        }
    }

    fn meaning(self) -> &'static str {
        match self {
            Self::Freed => "Recorded decrease in item size, including partial cleanup.",
            Self::NotCleaned => {
                "Items marked clean that were planned, skipped, or failed. Any recorded space freed is counted separately."
            }
            Self::Review => "Items marked ask. Maid did not select them for cleanup.",
            Self::Keep => "Items marked keep by the run's policy.",
            Self::Remaining => {
                "Measured space left after a completed cleanup command. It is not necessarily safe to remove."
            }
            Self::Unmeasured => {
                "Cleanup completed, but the change in item size could not be measured."
            }
        }
    }
}

#[derive(Default)]
struct Slice {
    label: String,
    category: Option<Category>,
    size: u64,
    freed: u64,
    not_cleaned: u64,
}

impl Slice {
    fn add(&mut self, size: u64, freed: u64, not_cleaned: u64) {
        self.size = self.size.saturating_add(size);
        self.freed = self.freed.saturating_add(freed);
        self.not_cleaned = self.not_cleaned.saturating_add(not_cleaned);
    }
}

struct Breakdown {
    targets: Vec<Slice>,
    outcomes: Vec<Slice>,
    unknown: usize,
}

impl Breakdown {
    fn of(record: &Record) -> Self {
        let mut targets: Vec<Slice> = Vec::new();
        let mut outcomes: Vec<Slice> = Category::ALL
            .into_iter()
            .map(|category| Slice {
                label: category.label().into(),
                category: Some(category),
                ..Slice::default()
            })
            .collect();
        let mut unknown = 0;
        for item in &record.items {
            let Some(size) = item.size_bytes else {
                unknown += 1;
                continue;
            };
            // Survey and dry-run records do not measure space freed by cleanup.
            let freed = if record.kind == RunKind::Cleanup {
                item.freed_bytes.unwrap_or(0).min(size)
            } else {
                0
            };
            let left = size - freed;
            let outcome = match item.decision {
                Decision::Ask => Category::Review,
                Decision::Keep => Category::Keep,
                Decision::Clean if item.result != Outcome::Done => Category::NotCleaned,
                Decision::Clean if item.freed_bytes.is_none() => Category::Unmeasured,
                Decision::Clean => Category::Remaining,
            };
            let not_cleaned = if outcome == Category::NotCleaned {
                left
            } else {
                0
            };
            outcomes[Category::Freed as usize].add(freed, freed, 0);
            outcomes[outcome as usize].add(left, 0, not_cleaned);
            let label = match item.kind {
                Kind::Target => item.name.as_str(),
                Kind::Worktree => "Worktrees",
                Kind::Unknown => "Other folders",
            };
            let index = targets
                .iter()
                .position(|s| s.label == label)
                .unwrap_or_else(|| {
                    targets.push(Slice {
                        label: label.into(),
                        ..Slice::default()
                    });
                    targets.len() - 1
                });
            targets[index].add(size, freed, not_cleaned);
        }
        targets.sort_by_key(|s| std::cmp::Reverse(s.size));
        // Keep small targets in the legend without adding tiny pie slices.
        if targets.len() > 6 {
            let rest = targets.split_off(6);
            let mut other = Slice {
                label: "Other targets and folders".into(),
                ..Slice::default()
            };
            for slice in rest {
                other.add(slice.size, slice.freed, slice.not_cleaned);
            }
            targets.push(other);
        }
        Self {
            targets,
            outcomes,
            unknown,
        }
    }
}

pub(super) fn render(record: &Record) -> String {
    let data = Breakdown::of(record);
    let measured = data
        .targets
        .iter()
        .fold(0_u64, |sum, s| sum.saturating_add(s.size));
    let freed = data
        .targets
        .iter()
        .fold(0_u64, |sum, s| sum.saturating_add(s.freed));
    let mut out = String::from(
        "<section class=\"breakdown\" aria-labelledby=\"breakdown-title\"><h2 id=\"breakdown-title\">Measured space breakdown</h2>",
    );
    if record.kind == RunKind::Cleanup {
        write!(
            out,
            "<p><strong>{} freed of {} measured</strong></p>",
            human(freed),
            human(measured)
        )
        .unwrap();
    } else {
        write!(
            out,
            "<p><strong>{} measured · no cleanup in this run</strong></p>",
            human(measured)
        )
        .unwrap();
    }
    out.push_str("<p class=\"muted\">These charts add recorded item sizes, which can overlap. They do not show total disk use. Clean candidates show measured space, not a promise of space that can be freed.</p><div class=\"pie-grid\">");
    chart(&mut out, "By target", &data.targets, true);
    chart(&mut out, "By cleanup result", &data.outcomes, false);
    write!(out, "</div><p class=\"muted\">{} items have unknown sizes and are not included. Hover over a slice for details, or open its legend entry.</p></section>", data.unknown).unwrap();
    out
}

fn chart(out: &mut String, title: &str, slices: &[Slice], targets: bool) {
    let total = slices
        .iter()
        .fold(0_u64, |sum, s| sum.saturating_add(s.size));
    write!(
        out,
        "<figure class=\"pie-figure\"><figcaption>{title}</figcaption>"
    )
    .unwrap();
    if total == 0 {
        out.push_str("<p class=\"muted\">No measured space to show.</p></figure>");
        return;
    }
    write!(out, "<svg class=\"pie\" viewBox=\"0 0 240 240\" role=\"group\" aria-label=\"{title}: {} of recorded item sizes. Details are in the legend below.\">", human(total)).unwrap();
    let mut angle = -std::f64::consts::FRAC_PI_2;
    for (i, slice) in slices.iter().enumerate().filter(|(_, s)| s.size > 0) {
        let end = angle + slice.size as f64 / total as f64 * std::f64::consts::TAU;
        let description = description(slice, total, targets);
        let color = color(i, targets);
        let attrs = format!(
            "class=\"pie-slice\" tabindex=\"0\" role=\"img\" aria-label=\"{}\" data-pie-detail=\"{}\"",
            escape(&description),
            escape(&description)
        );
        if slice.size == total {
            write!(out, "<circle {attrs} cx=\"120\" cy=\"120\" r=\"104\" fill=\"var({color})\"><title>{}</title></circle>", escape(&description)).unwrap();
        } else {
            let (x1, y1) = (120.0 + 104.0 * angle.cos(), 120.0 + 104.0 * angle.sin());
            let (x2, y2) = (120.0 + 104.0 * end.cos(), 120.0 + 104.0 * end.sin());
            let large = u8::from(end - angle > std::f64::consts::PI);
            write!(out, "<path {attrs} d=\"M120 120 L{x1:.4} {y1:.4} A104 104 0 {large} 1 {x2:.4} {y2:.4} Z\" fill=\"var({color})\"><title>{}</title></path>", escape(&description)).unwrap();
        }
        angle = end;
    }
    out.push_str(
        "</svg><div class=\"pie-tooltip\" role=\"tooltip\" hidden></div><ul class=\"pie-legend\">",
    );
    for (i, slice) in slices.iter().enumerate().filter(|(_, s)| s.size > 0) {
        let color = color(i, targets);
        let value = if targets {
            format!("{} freed of {}", human(slice.freed), human(slice.size))
        } else {
            human(slice.size)
        };
        write!(out, "<li><details><summary><span class=\"pie-swatch\" style=\"background:var({color})\" aria-hidden=\"true\"></span><span class=\"pie-label\">{}</span><span class=\"pie-value\">{value}</span></summary><p>{}</p></details></li>", escape(&slice.label), escape(&description(slice, total, targets))).unwrap();
    }
    out.push_str("</ul></figure>");
}

fn description(slice: &Slice, total: u64, targets: bool) -> String {
    let share = slice.size as f64 / total as f64 * 100.0;
    if targets {
        format!(
            "{}: {} freed of {} measured ({share:.1}% of chart). {} in clean candidates not cleaned. Other space can include kept items, review items, and space remaining after cleanup.",
            slice.label,
            human(slice.freed),
            human(slice.size),
            human(slice.not_cleaned)
        )
    } else {
        let meaning = slice
            .category
            .expect("outcome slices have a category")
            .meaning();
        format!(
            "{}: {} ({share:.1}% of chart). {meaning}",
            slice.label,
            human(slice.size)
        )
    }
}

fn color(index: usize, targets: bool) -> &'static str {
    if targets {
        [
            "--chart-1",
            "--chart-2",
            "--chart-3",
            "--chart-4",
            "--chart-5",
            "--chart-6",
            "--chart-7",
        ][index]
    } else {
        [
            "--success",
            "--warning",
            "--chart-3",
            "--chart-4",
            "--chart-5",
            "--muted-foreground",
        ][index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Item;

    fn record(items: Vec<Item>, kind: RunKind) -> Record {
        Record {
            version: 2,
            host: "test".into(),
            kind,
            steps: vec![],
            started_at: "now".into(),
            finished_at: None,
            free_bytes_before: None,
            free_bytes_after: None,
            items,
            warnings: vec![],
        }
    }

    #[test]
    fn partitions_partial_failed_skipped_and_unknown_items() {
        let mut done = Item::new(Kind::Target, "cache");
        done.decision = Decision::Clean;
        done.result = Outcome::Done;
        done.size_bytes = Some(100);
        done.freed_bytes = Some(60);
        let mut failed = done.clone();
        failed.result = Outcome::Failed;
        failed.freed_bytes = Some(20);
        let mut skipped = failed.clone();
        skipped.result = Outcome::Skipped;
        skipped.freed_bytes = None;
        let unknown = Item::new(Kind::Target, "unknown");
        let data = Breakdown::of(&record(
            vec![done, failed, skipped, unknown],
            RunKind::Cleanup,
        ));
        assert_eq!(data.unknown, 1);
        assert_eq!(data.outcomes[0].size, 80);
        assert_eq!(data.outcomes[1].size, 180);
        assert_eq!(data.outcomes[4].size, 40);
        assert_eq!(data.outcomes.iter().map(|s| s.size).sum::<u64>(), 300);
        assert_eq!(data.targets[0].not_cleaned, 180);
    }

    #[test]
    fn survey_has_candidates_but_no_space_freed() {
        let mut item = Item::new(Kind::Target, "cache");
        item.decide(Decision::Clean, None);
        item.size_bytes = Some(100);
        let data = Breakdown::of(&record(vec![item], RunKind::Survey));
        assert_eq!(data.outcomes[0].size, 0);
        assert_eq!(data.outcomes[1].size, 100);
    }

    #[test]
    fn grouping_small_targets_keeps_the_measured_total() {
        let items = (1..=8)
            .map(|size| {
                let mut item = Item::new(Kind::Target, &format!("cache {size}"));
                item.size_bytes = Some(size);
                item
            })
            .collect();
        let data = Breakdown::of(&record(items, RunKind::Survey));
        assert_eq!(data.targets.len(), 7);
        assert_eq!(data.targets[6].size, 3);
        assert_eq!(data.targets.iter().map(|s| s.size).sum::<u64>(), 36);
        assert_eq!(data.outcomes[3].size, 36);
    }

    #[test]
    fn handles_empty_and_single_slice_charts() {
        let empty = render(&record(vec![], RunKind::Survey));
        assert!(!empty.contains("<svg"));
        let mut item = Item::new(Kind::Target, "<cache>");
        item.size_bytes = Some(100);
        let html = render(&record(vec![item], RunKind::Survey));
        assert!(html.contains("<circle"));
        assert!(html.contains("&lt;cache&gt;"));
        assert!(!html.contains("NaN"));
    }
}
