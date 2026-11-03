//! `.doppelscan.toml`: a repository's own defaults for the scan settings,
//! so every run (a developer, CI, an agent through MCP) uses the same
//! ones without repeating flags. A flag or tool argument still wins.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::engine::ScanOptions;
use crate::lang::{LANGS, lang_index_by_name};

pub const FILE: &str = ".doppelscan.toml";

/// Unknown keys are an error: a misspelled setting would otherwise be
/// silently left at its default.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    pub min_tokens: Option<u32>,
    pub strict: Option<bool>,
    /// Languages, as for `--only`.
    pub only: Option<Vec<String>>,
    /// Globs relative to the scanned path, as for `--exclude`; added to
    /// those given on the command line.
    #[serde(default)]
    pub exclude: Vec<String>,
    pub max_param_density: Option<f32>,
    /// `near`'s threshold, 0 to 1.
    pub similarity: Option<f32>,
}

impl Config {
    /// The config for a scan of `start`, or the defaults when there is none.
    pub fn for_scan(start: &Path) -> anyhow::Result<Self> {
        match nearest(start, FILE) {
            Some(path) => Self::load(&path),
            None => Ok(Self::default()),
        }
    }

    /// What was asked for, with the gaps filled from this config and then
    /// from the defaults (`default_min` for the token minimum).
    pub fn options(&self, asked: Requested, default_min: u32) -> anyhow::Result<ScanOptions> {
        let defaults = ScanOptions::default();
        let only = match (asked.only.is_empty(), &self.only) {
            (true, Some(only)) => only.clone(),
            _ => asked.only,
        };
        Ok(ScanOptions {
            min_tokens: asked.min_tokens.or(self.min_tokens).unwrap_or(default_min),
            strict: asked.strict || self.strict.unwrap_or(false),
            languages: languages(&only)?,
            exclude: [self.exclude.clone(), asked.exclude].concat(),
            max_param_density: asked
                .max_param_density
                .or(self.max_param_density)
                .unwrap_or(defaults.max_param_density),
            ..defaults
        })
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// Settings from a flag or tool argument; `None` or empty when not given.
#[derive(Default)]
pub struct Requested {
    pub min_tokens: Option<u32>,
    pub strict: bool,
    pub only: Vec<String>,
    pub exclude: Vec<String>,
    pub max_param_density: Option<f32>,
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
/// root, so a file at the root covers a scan of any directory in it.
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
