//! What a scan of a path would use and why, for `doppelscan config` and
//! the MCP `get_settings` tool, which show the same thing; and the
//! commented starting config `doppelscan init` writes.

use std::path::Path;

use rustc_hash::FxHashMap;
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{Config, Requested, Set};
use crate::engine::ScanOptions;
use crate::filter::{PathFilter, PathRules};
use crate::index::MIN_STORED_TOKENS;
use crate::lang::LANGS;
use crate::walk::{
    BUILT_IN_SKIPS, Found, MAX_RAW_BYTES, discover, looks_generated, scannable_text,
};

#[derive(Debug, Serialize, PartialEq)]
pub struct Effective {
    /// Config files read, outermost first.
    pub files: Vec<String>,
    pub settings: Vec<Line>,
    pub exclude: Vec<Patterns>,
    pub include: Vec<Patterns>,
    pub near_exclude: Vec<Patterns>,
    pub accepted: Vec<Accepted>,
    pub built_in_skips: Skips,
    /// What the rest does not say about the scan.
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Accepted {
    pub fingerprint: String,
    pub reason: String,
    pub file: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Line {
    pub key: &'static str,
    pub value: Value,
    /// "flag", "default", or the config file that set it.
    pub from: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Patterns {
    pub file: String,
    pub patterns: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Skips {
    pub dirs: Vec<&'static str>,
    pub names: Vec<&'static str>,
    pub generated_markers: Vec<&'static str>,
}

/// The settings `asked` (flags or tool arguments) and `config` give, with
/// `near_similarity` the threshold asked for `near`, if any.
pub fn explain(config: &Config, asked: &Requested, near_similarity: Option<f32>) -> Effective {
    let check_min = asked.check_min_tokens;
    let defaults = crate::engine::ScanOptions::default();
    let only = (!asked.only.is_empty()).then(|| asked.only.clone());
    Effective {
        files: config.files.iter().map(|p| shown(p)).collect(),
        settings: vec![
            line(
                "min-tokens",
                asked.min_tokens.map(floored),
                &floored_set(&config.min_tokens),
                defaults.min_tokens,
            ),
            line("strict", asked.strict, &config.strict, false),
            line("only", only, &config.only, Vec::new()),
            line(
                "max-param-density",
                asked.max_param_density.map(wide),
                &wide_set(&config.max_param_density),
                wide(defaults.max_param_density),
            ),
            line(
                "similarity",
                near_similarity.map(wide),
                &wide_set(&config.similarity),
                0.8,
            ),
            line("check.min-tokens", check_min, &config.check_min_tokens, 0),
        ],
        exclude: excludes(config, asked),
        include: patterns(config, &config.rules, |r| &r.include),
        near_exclude: patterns(config, &config.near_rules, |r| &r.exclude),
        accepted: config
            .accepts
            .iter()
            .map(|a| Accepted {
                fingerprint: a.fingerprint.clone(),
                reason: a.reason.clone(),
                file: shown(&a.from),
            })
            .collect(),
        built_in_skips: Skips {
            dirs: [BUILT_IN_SKIPS.dirs, BUILT_IN_SKIPS.generated_dirs].concat(),
            names: BUILT_IN_SKIPS.names.to_vec(),
            generated_markers: BUILT_IN_SKIPS.generated_markers.to_vec(),
        },
        notes: notes(config, asked),
    }
}

/// The minimum a scan really uses: nothing smaller is ever stored.
fn floored(min_tokens: u32) -> u32 {
    min_tokens.max(MIN_STORED_TOKENS)
}

fn floored_set(set: &Option<Set<u32>>) -> Option<Set<u32>> {
    set.as_ref().map(|s| Set {
        value: floored(s.value),
        from: s.from.clone(),
    })
}

fn excludes(config: &Config, asked: &Requested) -> Vec<Patterns> {
    let mut lists = patterns(config, &config.rules, |r| &r.exclude);
    if !asked.exclude.is_empty() {
        lists.push(Patterns {
            file: "flag".to_string(),
            patterns: asked.exclude.clone(),
        });
    }
    lists
}

fn notes(config: &Config, asked: &Requested) -> Vec<String> {
    let min = asked
        .min_tokens
        .or(config.min_tokens.as_ref().map(|s| s.value));
    let below_floor = min.is_some_and(|m| m < MIN_STORED_TOKENS);
    let floor = format!("min-tokens is raised to {MIN_STORED_TOKENS}, the smallest clone stored.");
    let nested = "Configs nested inside the scanned tree also apply, and are not listed.";
    [
        below_floor.then_some(floor),
        (!config.skip_nested).then(|| nested.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Fails where a scan of `root` would fail before it reads any source: a
/// missing path, a bad `--exclude`, a nested config that does not load.
/// The last takes a walk of the tree, parsing nothing.
pub fn check_scannable(root: &Path, opts: &ScanOptions) -> anyhow::Result<()> {
    let filter = PathFilter::new(root, opts)?;
    discover(root, &filter, &opts.languages).map(|_| ())
}

/// An f32 as the f64 its shortest decimal names, so 0.8 shows as 0.8 and
/// not 0.800000011920929.
pub(crate) fn wide(v: f32) -> f64 {
    v.to_string().parse().unwrap_or(v as f64)
}

fn wide_set(set: &Option<Set<f32>>) -> Option<Set<f64>> {
    set.as_ref().map(|s| Set {
        value: wide(s.value),
        from: s.from.clone(),
    })
}

fn line<T: Serialize + Clone>(
    key: &'static str,
    asked: Option<T>,
    configured: &Option<Set<T>>,
    default: T,
) -> Line {
    let (value, from) = match (asked, configured) {
        (Some(v), _) => (v, "flag".to_string()),
        (None, Some(set)) => (set.value.clone(), shown(&set.from)),
        (None, None) => (default, "default".to_string()),
    };
    Line {
        key,
        value: json!(value),
        from,
    }
}

/// `rules` runs parallel to `config.files`: `merge` adds one of each. The
/// file cannot be rebuilt from the rule's directory, since `--config` can
/// read a file of any name.
fn patterns(
    config: &Config,
    rules: &[PathRules],
    which: impl Fn(&PathRules) -> &Vec<String>,
) -> Vec<Patterns> {
    rules
        .iter()
        .zip(&config.files)
        .filter(|(r, _)| !which(r).is_empty())
        .map(|(r, file)| Patterns {
            file: shown(file),
            patterns: which(r).clone(),
        })
        .collect()
}

/// Without Windows' verbatim prefix (`\\?\`), which canonical paths carry
/// and nobody types.
pub fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

pub fn render(e: &Effective) -> String {
    let files = if e.files.is_empty() {
        "none".to_string()
    } else {
        e.files.join(", ")
    };
    let mut out = format!("Config files: {files}\n\n");
    for l in &e.settings {
        out.push_str(&format!(
            "{:<18} {:<12} {}\n",
            l.key,
            l.value.to_string(),
            l.from
        ));
    }
    for (label, lists) in [
        ("exclude", &e.exclude),
        ("include", &e.include),
        ("near exclude", &e.near_exclude),
    ] {
        for p in lists {
            out.push_str(&format!(
                "{label} ({}): {}\n",
                p.file,
                p.patterns.join(", ")
            ));
        }
    }
    for a in &e.accepted {
        out.push_str(&format!(
            "accepted [{}] ({}): {}\n",
            a.fingerprint, a.file, a.reason
        ));
    }
    for note in &e.notes {
        out.push_str(&format!("note: {note}\n"));
    }
    let skips = &e.built_in_skips;
    out.push_str(&format!(
        "\nBuilt-in skips (`include` brings back directories and marked files):\n  dirs: {}\n  names: {}\n  generated markers: {}\n",
        skips.dirs.join(", "),
        skips.names.join(", "),
        skips.generated_markers.join(", ")
    ));
    out
}

/// Test file patterns `init` suggests for `[near] exclude` when the tree
/// has files matching them.
const TEST_GLOBS: &[&str] = &[
    "**/*.test.ts",
    "**/*.spec.ts",
    "**/*.test.tsx",
    "**/*.test.js",
    "**/*.spec.js",
    "**/*_test.go",
    "**/test_*.py",
    "**/*_test.py",
    "**/src/test/",
    "**/__tests__/",
    "**/*Tests.cs",
    "**/*Test.kt",
    "**/*Test.java",
    "**/tests/",
];

/// A commented starting config for the tree at `root`: every key with its
/// default, the languages found, and the test patterns present as near's
/// excludes.
pub fn init_template(root: &Path) -> anyhow::Result<String> {
    // The tree's configs are not what `init` is describing, and one that
    // fails to load must not stop it.
    let opts = ScanOptions {
        skip_nested: true,
        ..ScanOptions::default()
    };
    let filter = PathFilter::new(root, &opts)?;
    let mut files = discover(root, &filter, &[])?;
    files.retain(is_scanned_text);
    let mut counts: FxHashMap<usize, usize> = FxHashMap::default();
    for file in &files {
        *counts.entry(file.lang).or_default() += 1;
    }
    let mut langs: Vec<(usize, usize)> = counts.into_iter().collect();
    langs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let tests = present_tests(root, files.iter().map(|f| f.path.as_path()))?;
    Ok(template(&langs, &tests))
}

fn is_scanned_text(file: &Found) -> bool {
    let fits = std::fs::metadata(&file.path).is_ok_and(|m| m.len() <= MAX_RAW_BYTES);
    let text = || scannable_text(std::fs::read(&file.path).ok()?);
    fits && text().is_some_and(|text| !looks_generated(&text, false))
}

fn present_tests<'a>(
    root: &Path,
    files: impl Iterator<Item = &'a Path>,
) -> anyhow::Result<Vec<&'static str>> {
    let matchers = TEST_GLOBS
        .iter()
        .map(|g| {
            let mut b = ignore::gitignore::GitignoreBuilder::new(root);
            b.add_line(None, g)?;
            Ok((*g, b.build()?))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let files: Vec<&Path> = files.collect();
    Ok(matchers
        .into_iter()
        .filter(|(_, m)| {
            files
                .iter()
                .any(|f| m.matched_path_or_any_parents(f, false).is_ignore())
        })
        .map(|(g, _)| g)
        .collect())
}

fn template(langs: &[(usize, usize)], tests: &[&str]) -> String {
    let found: Vec<String> = langs
        .iter()
        .map(|(l, n)| format!("{} ({n} files)", LANGS[*l].name))
        .collect();
    let names: Vec<String> = langs
        .iter()
        .map(|(l, _)| format!("\"{}\"", LANGS[*l].name))
        .collect();
    let near_exclude = if tests.is_empty() {
        "# exclude = [\"**/*.test.ts\"]".to_string()
    } else {
        let quoted: Vec<String> = tests.iter().map(|t| format!("\"{t}\"")).collect();
        format!("exclude = [{}]", quoted.join(", "))
    };
    format!(
        "#:schema https://raw.githubusercontent.com/CMaintz/doppelscan/main/doppelscan.schema.json
# doppelscan settings for this repository. Flags and MCP tool arguments
# override them; `doppelscan config` shows what a scan uses and why.

# Smallest clone reported, in syntax tokens.
# min-tokens = 50

# Match identifiers and literals exactly (copy-paste clones only).
# strict = false

# Languages to scan. Found here: {found}.
# only = [{names}]

# .gitignore patterns, relative to this file, to leave out.
# exclude = [\"legacy/\"]

# Directories the built-in skip list leaves out that should be scanned.
# include = [\"vendor/our-fork/\"]

# Parameters allowed per 100 tokens before a group counts as a data table.
# max-param-density = 8

# Stop looking for configs in parent directories.
# root = true

[near]
# Smallest similarity reported, 0 to 1.
# similarity = 0.8

# Tests are near copies of each other by design.
{near_exclude}

[check]
# Only new clone groups of at least this many tokens fail `check`; smaller
# ones are still reported. 0 leaves it to min-tokens.
# min-tokens = 0

# A clone group kept on purpose, by the fingerprint `doppelscan show` prints:
# left out of every report and never failing `check`. Changing
# the code's structure changes the fingerprint, so it comes back for another
# look; renames and changed literals do not, unless the scan is strict.
# Copies split off a group over the density limit are the exception: renaming
# their code changes their fingerprint, so accept them again after it.
# [[accept]]
# fingerprint = \"b4a94232d76e\"
# reason = \"Generated from the OpenAPI spec; regenerated, never edited.\"
",
        found = if found.is_empty() {
            "none".to_string()
        } else {
            found.join(", ")
        },
        names = names.join(", "),
    )
}
