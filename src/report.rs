//! The agent-facing result model and its two renderings: JSON, and a
//! compact text form that costs an LLM few tokens to read.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::detect::RawGroup;
use crate::engine::score;
use crate::index::FileIndex;
use crate::variation::{Variation, analyze, template};

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
    /// Names the fragment binds itself; they differ but need no argument.
    pub renames: Vec<Variation>,
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
        Group {
            id,
            fingerprint: format!("{:012x}", raw.hash >> 16),
            kind: if raw.run_len == 1 { "node" } else { "run" },
            elements: raw.run_len,
            tokens: raw.tokens,
            lines: raw.occs[0].end_line - raw.occs[0].start_line + 1,
            score: score(raw),
            occurrences: raw.occs.iter().map(|o| self.location(o)).collect(),
            parameters: vars.parameters,
            renames: vars.renames,
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
        let mut hits = (1..=self.groups.len())
            .filter(|&i| format!("{:012x}", self.groups[i - 1].hash >> 16).starts_with(&key));
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

const MAX_VALUE_CHARS: usize = 48;

pub fn render_summary(s: &Summary, shown: usize) -> String {
    format!(
        "{} clone groups ({} duplicated lines) in {} files, {} ms. Showing {}.\n",
        s.groups, s.duplicated_lines, s.files, s.elapsed_ms, shown
    )
}

pub fn render_group(g: &Group) -> String {
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
    for loc in &g.occurrences {
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
    render_variations(&mut out, "params", &g.parameters, true);
    render_variations(&mut out, "renames", &g.renames, false);
    if let Some(t) = &g.template {
        let _ = writeln!(out, "  template:\n{}", indent(t));
    }
    out
}

fn render_variations(out: &mut String, label: &str, vars: &[Variation], numbered: bool) {
    if vars.is_empty() {
        return;
    }
    let items: Vec<String> = vars
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let values = v
                .values
                .iter()
                .map(|s| clip(s))
                .collect::<Vec<_>>()
                .join(" | ");
            if numbered {
                format!("${}={}", i + 1, values)
            } else {
                values
            }
        })
        .collect();
    let _ = writeln!(out, "  {label}: {}", items.join(";  "));
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
