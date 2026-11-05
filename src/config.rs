//! `.doppelscan.toml`: a repository's own defaults for the scan settings,
//! so every run (a developer, CI, an agent through MCP) uses the same
//! ones without repeating flags. A flag or tool argument still wins.
//!
//! Every config from the scanned directory up to the repository root
//! applies, the nearest winning where two set the same key, so a package
//! in a monorepo can refine the root's settings. `root = true` stops the
//! search there. Path patterns follow `.gitignore` syntax and are anchored
//! at the directory of the file that lists them.

use std::path::{Path, PathBuf};

use anyhow::Context;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::engine::ScanOptions;
use crate::filter::PathRules;
use crate::lang::{LANGS, lang_index_by_name};

pub const FILE: &str = ".doppelscan.toml";

// Unknown keys are an error: a misspelled setting would otherwise be
// silently left at its default. The doc comments here become the schema's
// descriptions, which editors show, so they are written for users.

/// doppelscan settings for a repository (.doppelscan.toml). Flags and MCP
/// tool arguments override them.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[schemars(title = "doppelscan config")]
struct File {
    /// Stop looking for configs in parent directories.
    #[serde(default)]
    root: bool,
    /// Smallest clone reported, in syntax tokens. Default 50.
    min_tokens: Option<u32>,
    /// Match identifiers and literals exactly (copy-paste clones only).
    strict: Option<bool>,
    /// Only these languages, by name or extension; yaml is scanned only
    /// when listed.
    only: Option<Vec<String>>,
    /// .gitignore patterns, relative to this file, to leave out.
    #[serde(default)]
    exclude: Vec<String>,
    /// .gitignore patterns, relative to this file, of directories the
    /// built-in skip list leaves out that should be scanned.
    #[serde(default)]
    include: Vec<String>,
    /// Groups needing more parameters than this per 100 tokens are dropped
    /// as data tables. Default 8.
    max_param_density: Option<f32>,
    /// Settings for `doppelscan near`.
    #[serde(default)]
    near: NearFile,
    /// Settings for `doppelscan check`.
    #[serde(default)]
    check: CheckFile,
    /// Clone groups the team has decided to keep, by fingerprint. They are
    /// left out of every report and never fail `check`, however many
    /// copies there are. A change to the code changes the fingerprint, so
    /// the clone comes back for another look.
    #[serde(default)]
    accept: Vec<AcceptFile>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AcceptFile {
    /// The group's fingerprint, as `doppelscan` shows it in brackets.
    fingerprint: String,
    /// Why the duplication stays.
    reason: String,
}

/// A clone group kept on purpose.
#[derive(Clone, Debug)]
pub struct Accept {
    pub fingerprint: String,
    pub reason: String,
    pub from: PathBuf,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct CheckFile {
    /// Only new clone groups of at least this many tokens fail the check;
    /// smaller ones are still reported. Default: the report's min-tokens.
    min_tokens: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct NearFile {
    /// Smallest similarity reported, 0 to 1. Default 0.8.
    similarity: Option<f32>,
    /// .gitignore patterns, relative to this file, that near leaves out
    /// (tests are near copies of each other by design).
    #[serde(default)]
    exclude: Vec<String>,
}

/// The JSON schema of a config file, for editors.
pub fn schema() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(File)).unwrap() + "\n"
}

/// A value and the config file that set it.
#[derive(Clone, Debug)]
pub struct Set<T> {
    pub value: T,
    pub from: PathBuf,
}

/// The configs that apply to one scan, merged.
#[derive(Debug, Default)]
pub struct Config {
    pub min_tokens: Option<Set<u32>>,
    pub strict: Option<Set<bool>>,
    pub only: Option<Set<Vec<String>>>,
    pub max_param_density: Option<Set<f32>>,
    pub similarity: Option<Set<f32>>,
    pub check_min_tokens: Option<Set<u32>>,
    /// From every file, outermost first.
    pub accepts: Vec<Accept>,
    /// Each file's `exclude` and `include`, outermost first.
    pub rules: Vec<PathRules>,
    /// Each file's `[near] exclude`, which only `near` applies.
    pub near_rules: Vec<PathRules>,
    /// The files read, outermost first.
    pub files: Vec<PathBuf>,
    /// Configs inside the scanned tree stay unread: the source named which
    /// config counts.
    pub skip_nested: bool,
}

/// Which config a command reads.
pub enum Source<'a> {
    /// The files that apply to this path, found by walking up from it.
    Lookup(&'a Path),
    /// Exactly this file.
    File(&'a Path),
    None,
}

impl Config {
    pub fn read(source: Source) -> anyhow::Result<Self> {
        let skip_nested = !matches!(source, Source::Lookup(_));
        let paths = match source {
            Source::Lookup(start) => chain(start)?,
            Source::File(path) => vec![
                path.canonicalize()
                    .with_context(|| format!("reading config {}", path.display()))?,
            ],
            Source::None => Vec::new(),
        };
        let mut config = Self {
            skip_nested,
            ..Self::default()
        };
        for path in paths {
            config.merge(load(&path)?, path);
        }
        Ok(config)
    }

    /// The files that apply to a scan of `start`.
    pub fn for_scan(start: &Path) -> anyhow::Result<Self> {
        Self::read(Source::Lookup(start))
    }

    fn merge(&mut self, file: File, path: PathBuf) {
        override_with(&mut self.min_tokens, file.min_tokens, &path);
        override_with(&mut self.strict, file.strict, &path);
        override_with(&mut self.only, file.only, &path);
        override_with(&mut self.max_param_density, file.max_param_density, &path);
        override_with(&mut self.similarity, file.near.similarity, &path);
        override_with(&mut self.check_min_tokens, file.check.min_tokens, &path);
        self.accepts.extend(file.accept.into_iter().map(|a| Accept {
            fingerprint: a.fingerprint,
            reason: a.reason,
            from: path.clone(),
        }));
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        self.rules.push(PathRules {
            dir: dir.clone(),
            exclude: file.exclude,
            include: file.include,
        });
        self.near_rules.push(PathRules {
            dir,
            exclude: file.near.exclude,
            include: Vec::new(),
        });
        self.files.push(path);
    }

    /// What was asked for, with the gaps filled from the config and then
    /// from the defaults.
    pub fn options(&self, asked: Requested) -> anyhow::Result<ScanOptions> {
        let defaults = ScanOptions::default();
        let only = match (asked.only.is_empty(), &self.only) {
            (true, Some(only)) => only.value.clone(),
            _ => asked.only,
        };
        Ok(ScanOptions {
            min_tokens: asked
                .min_tokens
                .or(value(&self.min_tokens))
                .unwrap_or(defaults.min_tokens),
            strict: asked.strict.or(value(&self.strict)).unwrap_or(false),
            languages: languages(&only)?,
            exclude: asked.exclude,
            rules: self.rules.clone(),
            skip_nested: self.skip_nested,
            accept: self.accepts.iter().map(|a| a.fingerprint.clone()).collect(),
            max_param_density: asked
                .max_param_density
                .or(value(&self.max_param_density))
                .unwrap_or(defaults.max_param_density),
            ..defaults
        })
    }

    /// As `options`, with `[near] exclude` added.
    pub fn near_options(&self, asked: Requested) -> anyhow::Result<ScanOptions> {
        let mut opts = self.options(asked)?;
        opts.rules.extend(self.near_rules.iter().cloned());
        opts.near = true;
        Ok(opts)
    }

    /// The size a new group needs to fail `check`: the one asked for, else
    /// the config's, else 0, so the report's minimum decides.
    pub fn check_min_tokens(&self, asked: Option<u32>) -> u32 {
        asked.or(value(&self.check_min_tokens)).unwrap_or(0)
    }

    /// `near`'s threshold: the one asked for, else the config's, else 0.8.
    pub fn similarity(&self, asked: Option<f32>) -> f32 {
        asked.or(value(&self.similarity)).unwrap_or(0.8)
    }
}

fn override_with<T>(slot: &mut Option<Set<T>>, nearer: Option<T>, from: &Path) {
    if let Some(value) = nearer {
        *slot = Some(Set {
            value,
            from: from.to_path_buf(),
        });
    }
}

fn value<T: Clone>(set: &Option<Set<T>>) -> Option<T> {
    set.as_ref().map(|s| s.value.clone())
}

/// The path patterns of a config found inside a scanned tree. Its other
/// settings are left out: one scan has one token minimum, from the
/// configs at or above where it starts.
pub(crate) fn nested_rules(path: &Path, near: bool) -> anyhow::Result<PathRules> {
    let file = load(path)?;
    let near_exclude = if near { file.near.exclude } else { Vec::new() };
    Ok(PathRules {
        dir: path.parent().unwrap_or(Path::new("")).to_path_buf(),
        exclude: [file.exclude, near_exclude].concat(),
        include: file.include,
    })
}

fn load(path: &Path) -> anyhow::Result<File> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Config files from the repository root (or the nearest `root = true`)
/// down to `start`, outermost first.
fn chain(start: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let Ok(start) = start.canonicalize() else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for dir in start.ancestors() {
        let file = dir.join(FILE);
        if file.is_file() {
            let stops = load(&file)?.root;
            found.push(file);
            if stops {
                break;
            }
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    found.reverse();
    Ok(found)
}

/// Settings from a flag or tool argument; `None` or empty when not given.
#[derive(Default)]
pub struct Requested {
    pub min_tokens: Option<u32>,
    pub strict: Option<bool>,
    pub only: Vec<String>,
    pub exclude: Vec<String>,
    pub max_param_density: Option<f32>,
    /// `check`'s own minimum for new groups.
    pub check_min_tokens: Option<u32>,
}

fn languages(names: &[String]) -> anyhow::Result<Vec<usize>> {
    names
        .iter()
        .map(|l| {
            lang_index_by_name(l)
                .with_context(|| format!("unknown language {l:?}; known: {}", known_langs()))
        })
        .collect()
}

pub fn known_langs() -> String {
    LANGS.iter().map(|l| l.name).collect::<Vec<_>>().join(", ")
}

/// `name` in `start` or the nearest parent directory, up to the repository
/// root.
pub fn nearest(start: &Path, name: &str) -> Option<PathBuf> {
    let start = start.canonicalize().ok()?;
    for dir in start.ancestors() {
        let file = dir.join(name);
        if file.exists() {
            return Some(file);
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    None
}
