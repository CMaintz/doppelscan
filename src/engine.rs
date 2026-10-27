//! Indexing with an mtime/size cache, shared by the CLI and the MCP server.
//! The server keeps one `Engine` alive, so repeat queries re-parse only the
//! files that changed since the last call.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use tree_sitter::Parser;

use crate::baseline::{Baseline, Settings};
use crate::detect::{RawGroup, detect};
use crate::index::{FileIndex, MIN_STORED_TOKENS, index_source};
use crate::report::Analysis;
use crate::variation;
use crate::walk::{MAX_FILE_BYTES, discover, looks_generated};

#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub min_tokens: u32,
    /// Exact-text matching only: identifiers and literals must match too.
    pub strict: bool,
    /// Language indices into `LANGS`; empty means all.
    pub languages: Vec<usize>,
    pub exclude: Vec<String>,
    /// Groups needing more parameters than this per 100 tokens (and more
    /// than [`FREE_PARAMS`]) are dropped: they are tables of data or mapping
    /// boilerplate, not a helper waiting to be extracted.
    pub max_param_density: f32,
    /// Result paths are shown relative to this; default is the scanned path.
    pub display_base: Option<PathBuf>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            min_tokens: 50,
            strict: false,
            languages: Vec::new(),
            exclude: Vec::new(),
            max_param_density: 8.0,
            display_base: None,
        }
    }
}

struct Cached {
    stamp: (SystemTime, u64),
    index: Option<Arc<FileIndex>>,
}

#[derive(Default)]
pub struct Engine {
    cache: Mutex<FxHashMap<(PathBuf, PathBuf, bool), Cached>>,
}

/// An in-memory file, for tests and for snippets an agent asks about.
pub struct Source {
    pub path: String,
    pub lang: usize,
    pub text: String,
}

impl Engine {
    pub fn scan(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<Analysis> {
        let started = Instant::now();
        let files = self.index_tree(root, opts)?;
        Ok(analyze(files, opts, started))
    }

    pub fn index_tree(
        &self,
        root: &Path,
        opts: &ScanOptions,
    ) -> anyhow::Result<Vec<Arc<FileIndex>>> {
        let (root, base) = comparable(root, opts)?;
        let found = discover(&root, &opts.exclude, &opts.languages)?;
        let base = base.as_deref().unwrap_or(&root);
        let indexed: Vec<_> = found
            .par_iter()
            .map_init(Parser::new, |parser, (path, lang)| {
                self.index_cached(parser, base, path, *lang, opts.strict)
            })
            .collect();
        Ok(indexed.into_iter().flatten().collect())
    }

    fn index_cached(
        &self,
        parser: &mut Parser,
        root: &Path,
        path: &Path,
        lang: usize,
        strict: bool,
    ) -> Option<Arc<FileIndex>> {
        let meta = std::fs::metadata(path).ok()?;
        if meta.len() > MAX_FILE_BYTES {
            return None;
        }
        let stamp = (meta.modified().ok()?, meta.len());
        let key = (root.to_path_buf(), path.to_path_buf(), strict);
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap()
            .get(&key)
            .filter(|c| c.stamp == stamp)
        {
            return hit.index.clone();
        }
        let index = read_and_index(parser, root, path, lang, strict);
        self.cache.lock().unwrap().insert(
            key,
            Cached {
                stamp,
                index: index.clone(),
            },
        );
        index
    }
}

fn read_and_index(
    parser: &mut Parser,
    root: &Path,
    path: &Path,
    lang: usize,
    strict: bool,
) -> Option<Arc<FileIndex>> {
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    if looks_generated(&text) {
        return None;
    }
    index_source(parser, display_path(root, path), lang, text, strict).map(Arc::new)
}

fn display_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let shown = if rel.as_os_str().is_empty() {
        path
    } else {
        rel
    };
    shown.to_string_lossy().replace('\\', "/")
}

pub fn index_sources(sources: Vec<Source>, strict: bool) -> Vec<Arc<FileIndex>> {
    sources
        .into_par_iter()
        .map_init(Parser::new, |parser, s| {
            index_source(parser, s.path, s.lang, s.text, strict)
        })
        .flatten()
        .map(Arc::new)
        .collect()
}

pub fn analyze(files: Vec<Arc<FileIndex>>, opts: &ScanOptions, started: Instant) -> Analysis {
    let min_tokens = opts.min_tokens.max(MIN_STORED_TOKENS);
    let density = opts.max_param_density;
    let mut groups: Vec<RawGroup> = detect(&files, min_tokens, |g| {
        let allowed = (density * g.tokens as f32 / 100.0).max(FREE_PARAMS);
        variation::analyze(g, &files).parameters.len() as f32 <= allowed
    });
    groups.sort_by(|a, b| score(b).cmp(&score(a)).then(a.hash.cmp(&b.hash)));
    Analysis {
        files,
        groups,
        elapsed: started.elapsed(),
    }
}

/// A small clone with a couple of arguments is still a fine helper, so the
/// density cap only bites past this many parameters.
pub const FREE_PARAMS: f32 = 3.0;

pub fn score(g: &RawGroup) -> u64 {
    g.tokens as u64 * (g.occs.len() as u64 - 1)
}

pub const SNIPPET_PATH: &str = "<snippet>";

impl Engine {
    /// Clone groups that include `code`: answers "does this already exist
    /// somewhere?" before an agent writes or extracts it.
    pub fn similar(
        &self,
        root: &Path,
        code: &str,
        lang: usize,
        opts: &ScanOptions,
    ) -> anyhow::Result<(Analysis, Vec<usize>)> {
        let started = Instant::now();
        let mut files = self.index_tree(root, opts)?;
        let text = snippet_text(code, lang);
        files.extend(index_sources(
            vec![Source {
                path: SNIPPET_PATH.into(),
                lang,
                text,
            }],
            opts.strict,
        ));
        let analysis = analyze(files, opts, started);
        let snippet = analysis.files.len() as u32 - 1;
        let hits = (1..=analysis.groups.len())
            .filter(|&id| {
                analysis.groups[id - 1]
                    .occs
                    .iter()
                    .any(|o| o.file == snippet)
            })
            .collect();
        Ok((analysis, hits))
    }
}

/// PHP is only parsed as code after an opening tag.
fn snippet_text(code: &str, lang: usize) -> String {
    if crate::lang::LANGS[lang].name == "php" && !code.trim_start().starts_with("<?") {
        format!("<?php\n{code}")
    } else {
        code.to_string()
    }
}

/// What a check compares the working tree against.
pub enum Against {
    /// A baseline file written by `doppelscan baseline`.
    File(PathBuf),
    /// The same scan at a git revision.
    Rev(String),
}

impl Against {
    pub fn label(&self) -> String {
        match self {
            // Canonical Windows paths carry a `\\?\` prefix nobody wants to read.
            Against::File(path) => {
                let shown = path.display().to_string();
                shown.strip_prefix(r"\\?\").unwrap_or(&shown).to_string()
            }
            Against::Rev(rev) => rev.clone(),
        }
    }
}

pub struct Check {
    pub analysis: Analysis,
    /// Ids of groups in `analysis` the baseline does not account for.
    pub new: Vec<usize>,
    /// Baseline entries nothing in the working tree matches any more.
    pub resolved: usize,
    /// Set when a baseline file's stored settings replaced the requested ones.
    pub note: Option<String>,
}

impl Engine {
    pub fn check(
        &self,
        root: &Path,
        against: &Against,
        opts: &ScanOptions,
    ) -> anyhow::Result<Check> {
        let (baseline, opts, note) = match against {
            Against::File(path) => against_file(root, path, opts)?,
            Against::Rev(rev) => {
                let (base, opts) = self.scan_rev(root, rev, opts)?;
                (Baseline::from_analysis(&base, &opts), opts, None)
            }
        };
        let analysis = self.scan(root, &opts)?;
        let new = baseline.new_groups(&analysis);
        let resolved = baseline.groups.len() - baseline.pruned(&analysis).groups.len();
        Ok(Check {
            analysis,
            new,
            resolved,
            note,
        })
    }

    /// `root` at the fork point of `rev`, with paths shown as a working-tree
    /// scan with the same options would show them. Also returns those
    /// options, narrowed to leave out submodules on both sides.
    pub fn scan_rev(
        &self,
        root: &Path,
        rev: &str,
        opts: &ScanOptions,
    ) -> anyhow::Result<(Analysis, ScanOptions)> {
        let started = Instant::now();
        let prefix = match &opts.display_base {
            Some(_) => {
                let (root, base) = comparable(root, opts)?;
                prefix_of(&root, base.as_deref().unwrap_or(&root))
            }
            None => String::new(),
        };
        let commit = crate::git::fork_point(root, rev)?;
        let revision = crate::git::read_revision(root, &commit, opts, &prefix)?;
        let mut opts = opts.clone();
        let submodules = revision.submodules.iter().map(|s| format!("{s}/**"));
        opts.exclude.extend(submodules);
        let files = index_sources(revision.sources, opts.strict);
        Ok((analyze(files, &opts, started), opts))
    }
}

/// The scanned root and `display_base` in the same canonical form, or
/// `./src` would never strip against `/repo` to `src`.
fn comparable(root: &Path, opts: &ScanOptions) -> anyhow::Result<(PathBuf, Option<PathBuf>)> {
    Ok(match &opts.display_base {
        Some(base) => (root.canonicalize()?, Some(base.canonicalize()?)),
        None => (root.to_path_buf(), None),
    })
}

fn prefix_of(root: &Path, base: &Path) -> String {
    let rel = root.strip_prefix(base).unwrap_or(Path::new(""));
    let rel = rel.to_string_lossy().replace('\\', "/");
    if rel.is_empty() { rel } else { rel + "/" }
}

/// Places in a baseline are relative to the file's own directory, so the
/// scan is shown from there whatever subdirectory is being checked.
fn against_file(
    root: &Path,
    path: &Path,
    opts: &ScanOptions,
) -> anyhow::Result<(Baseline, ScanOptions, Option<String>)> {
    let baseline = Baseline::load(path)?;
    let mut applied = baseline.settings.apply(opts)?;
    applied.display_base = Some(anchor(root, path)?);
    let note = (Settings::from_options(opts) != baseline.settings).then(|| {
        format!(
            "Using the settings stored in {} (min_tokens {}); \
             rewrite the baseline to change them.",
            path.display(),
            baseline.settings.min_tokens
        )
    });
    Ok((baseline, applied, note))
}

/// The directory a baseline file describes; `root` must lie inside it.
pub fn anchor(root: &Path, baseline: &Path) -> anyhow::Result<PathBuf> {
    let dir = baseline.parent().filter(|d| !d.as_os_str().is_empty());
    let dir = dir.unwrap_or(Path::new(".")).canonicalize()?;
    let root = root.canonicalize()?;
    anyhow::ensure!(
        root.starts_with(&dir),
        "{} is outside {}, the directory the baseline describes",
        root.display(),
        dir.display()
    );
    Ok(dir)
}
