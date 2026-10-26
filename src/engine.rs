//! Indexing with an mtime/size cache, shared by the CLI and the MCP server.
//! The server keeps one `Engine` alive, so repeat queries re-parse only the
//! files that changed since the last call.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use tree_sitter::Parser;

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
        let found = discover(root, &opts.exclude, &opts.languages)?;
        let base = opts.display_base.as_deref().unwrap_or(root);
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
