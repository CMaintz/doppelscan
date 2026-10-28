//! The agent-facing result model and its two renderings: JSON, and a
//! compact text form that costs an LLM few tokens to read.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::detect::RawGroup;
use crate::engine::{Against, Check, score};
use crate::index::FileIndex;
use crate::variation::{Kind, Variation, analyze, template};

/// Ranked clone groups over a set of indexed files.
pub struct Analysis {
    pub files: Vec<Arc<FileIndex>>,
    /// Sorted by score, highest first; a group's id is its index + 1.
    pub groups: Vec<RawGroup>,
    pub elapsed: Duration,
}

#[derive(Debug, Serialize)]
pub struct Location {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Group {
    pub id: usize,
    /// Stable across runs while the duplicated code is unchanged.
    pub fingerprint: String,
    /// "node" for one syntax node, "run" for consecutive siblings
    /// (statements, members or arguments).
    pub kind: &'static str,
    /// Siblings in a run; 1 for a node.
    pub elements: usize,
    pub tokens: u32,
    pub lines: u32,
    /// tokens x (occurrences - 1): the volume extraction would remove.
    pub score: u64,
    pub occurrences: Vec<Location>,
    /// Values that differ per occurrence and become helper arguments.
    pub parameters: Vec<Variation>,
    /// Locals the fragment binds itself; they differ but need no argument.
    pub renames: Vec<Variation>,
    /// Names the fragment declares for other code (methods, fields,
    /// classes): keeping one means updating the callers of the others.
    pub declares: Vec<Variation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub files: usize,
    pub groups: usize,
    pub duplicated_lines: u64,
    pub elapsed_ms: u128,
}

impl Analysis {
    pub fn group(&self, id: usize, with_template: bool) -> Group {
        let raw = &self.groups[id - 1];
        let vars = analyze(raw, &self.files);
        let template = with_template.then(|| template(raw, &self.files, &vars));
        let (declares, renames) = vars
            .renames
            .into_iter()
            .partition(|v| v.kind == Kind::Declared);
        Group {
            id,
            fingerprint: fingerprint(raw),
            kind: if raw.run_len == 1 { "node" } else { "run" },
            elements: raw.run_len,
            tokens: raw.tokens,
            lines: raw.occs[0].end_line - raw.occs[0].start_line + 1,
            score: score(raw),
            occurrences: raw.occs.iter().map(|o| self.location(o)).collect(),
            parameters: vars.parameters,
            renames,
            declares,
            template,
        }
    }

    fn location(&self, o: &crate::detect::Occ) -> Location {
        let file = &self.files[o.file as usize];
        Location {
            file: file.path.clone(),
            start_line: o.start_line,
            end_line: o.end_line,
            scope: file.scope_name(o.scope).map(str::to_string),
        }
    }

    /// Accepts a numeric id or a fingerprint (or a unique prefix of one).
    pub fn resolve(&self, key: &str) -> Option<usize> {
        let key = key.trim().trim_start_matches('#');
        if let Ok(id) = key.parse::<usize>()
            && (1..=self.groups.len()).contains(&id)
        {
            return Some(id);
        }
        let key = key.to_ascii_lowercase();
        let mut hits =
            (1..=self.groups.len()).filter(|&i| fingerprint(&self.groups[i - 1]).starts_with(&key));
        let first = hits.next()?;
        hits.next().is_none().then_some(first)
    }

    /// Groups with an occurrence overlapping `file` lines `[from, to]`.
    pub fn overlapping(&self, file: &str, from: u32, to: u32) -> Vec<usize> {
        let file = file.replace('\\', "/");
        (1..=self.groups.len())
            .filter(|&id| {
                self.groups[id - 1].occs.iter().any(|o| {
                    let path = &self.files[o.file as usize].path;
                    (path == &file
                        || path.ends_with(&format!("/{file}"))
                        || file.ends_with(&format!("/{path}")))
                        && o.start_line <= to
                        && from <= o.end_line
                })
            })
            .collect()
    }

    pub fn summary(&self) -> Summary {
        let duplicated_lines = self
            .groups
            .iter()
            .map(|g| {
                g.occs
                    .iter()
                    .skip(1)
                    .map(|o| (o.end_line - o.start_line + 1) as u64)
                    .sum::<u64>()
            })
            .sum();
        Summary {
            files: self.files.len(),
            groups: self.groups.len(),
            duplicated_lines,
            elapsed_ms: self.elapsed.as_millis(),
        }
    }
}

/// Stable across runs while the duplicated code keeps its structure.
pub fn fingerprint(group: &RawGroup) -> String {
    format!("{:012x}", group.hash >> 16)
}

const MAX_VALUE_CHARS: usize = 48;

pub fn render_summary(s: &Summary, shown: usize) -> String {
    format!(
        "{} clone groups ({} duplicated lines) in {} files, {} ms. Showing {}.\n",
        s.groups, s.duplicated_lines, s.files, s.elapsed_ms, shown
    )
}

/// How much of a group to render. Listings use `Brief` so a page of
/// groups stays cheap for an agent to read; `Full` gives every occurrence
/// and the per-occurrence value columns needed to rewrite call sites.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Brief,
    Full,
}

const BRIEF_OCCURRENCES: usize = 5;
const BRIEF_VALUES: usize = 4;

pub fn render_group(g: &Group, detail: Detail) -> String {
    let mut out = String::new();
    let shape = if g.kind == "node" {
        String::new()
    } else {
        format!(", run of {}", g.elements)
    };
    let _ = writeln!(
        out,
        "#{} [{}] x{} | {} tokens, {} lines{} | score {}",
        g.id,
        g.fingerprint,
        g.occurrences.len(),
        g.tokens,
        g.lines,
        shape,
        g.score
    );
    render_locations(&mut out, &g.occurrences, detail);
    render_variations(&mut out, "params", &g.parameters, true, detail);
    render_variations(&mut out, "renames", &g.renames, false, detail);
    render_variations(&mut out, "declares", &g.declares, false, detail);
    if let Some(t) = &g.template {
        let _ = writeln!(out, "  template:\n{}", indent(t));
    }
    out
}

fn render_locations(out: &mut String, locations: &[Location], detail: Detail) {
    let shown = if detail == Detail::Brief {
        BRIEF_OCCURRENCES
    } else {
        usize::MAX
    };
    for loc in locations.iter().take(shown) {
        let scope = loc
            .scope
            .as_deref()
            .map(|s| format!("  ({s})"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {}:{}-{}{}",
            loc.file, loc.start_line, loc.end_line, scope
        );
    }
    if locations.len() > shown {
        let _ = writeln!(out, "  ... and {} more", locations.len() - shown);
    }
}

fn render_variations(
    out: &mut String,
    label: &str,
    vars: &[Variation],
    numbered: bool,
    detail: Detail,
) {
    if vars.is_empty() {
        return;
    }
    let items: Vec<String> = vars
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let values = render_values(&v.values, detail);
            let label = match v.kind {
                Kind::Type => ":type",
                Kind::Function => ":function",
                Kind::Member => ":member",
                Kind::Name => ":name",
                // Plain values and literals (which show their quotes or
                // digits) stay unlabelled to keep listings short.
                _ => "",
            };
            if numbered {
                format!("${}{label}={}", i + 1, values)
            } else {
                values
            }
        })
        .collect();
    let _ = writeln!(out, "  {label}: {}", items.join(";  "));
}

/// Full keeps one value per occurrence, in occurrence order; Brief lists
/// each distinct value once.
fn render_values(values: &[String], detail: Detail) -> String {
    if detail == Detail::Full {
        return values
            .iter()
            .map(|s| clip(s))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    let mut distinct: Vec<&String> = Vec::new();
    for v in values {
        if !distinct.contains(&v) {
            distinct.push(v);
        }
    }
    let mut shown: Vec<String> = distinct
        .iter()
        .take(BRIEF_VALUES)
        .map(|s| clip(s))
        .collect();
    if distinct.len() > BRIEF_VALUES {
        shown.push(format!("+{} more", distinct.len() - BRIEF_VALUES));
    }
    shown.join(" | ")
}

fn clip(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX_VALUE_CHARS {
        return one_line;
    }
    one_line
        .chars()
        .take(MAX_VALUE_CHARS - 3)
        .collect::<String>()
        + "..."
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// New groups in full (an agent has to see every copy to decide which one
/// to keep), then a hint when the baseline file can shrink.
pub fn render_check(check: &Check, against: &Against) -> String {
    let what = against.label();
    let mut out = if check.new.is_empty() {
        format!("No new duplication against {what}.\n")
    } else {
        format!(
            "{} new clone group(s) against {what}. Reuse or extract the existing \
             code; `doppelscan show <fingerprint>` gives the helper template.\n",
            check.new.len()
        )
    };
    if let Some(note) = &check.note {
        out.push_str(&format!("{note}\n"));
    }
    for &id in &check.new {
        let group = check.analysis.group(id, false);
        out.push_str(&format!("\n{}", render_group(&group, Detail::Full)));
    }
    if check.resolved > 0 && matches!(against, Against::File(_)) {
        let entries = if check.resolved == 1 {
            "entry"
        } else {
            "entries"
        };
        out.push_str(&format!(
            "\n{} baseline {entries} no longer match; `doppelscan baseline --prune` shrinks it.\n",
            check.resolved
        ));
    }
    out
}
