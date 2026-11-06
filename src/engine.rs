//! Indexing with an mtime/size cache, shared by the CLI and the MCP server.
//! The server keeps one `Engine` alive, so repeat queries re-parse only the
//! files that changed since the last call.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use tree_sitter::Parser;

use crate::baseline::{Baseline, Scope};
use crate::detect::{Occ, RawGroup, detect};
use crate::filter::{PathFilter, PathRules};
use crate::index::{FileIndex, MIN_STORED_TOKENS, index_source, normalize_line_endings};
use crate::report::{Analysis, fingerprint};
use crate::variation::{self, Kind, Variation};
use crate::walk::{Found, MAX_FILE_BYTES, discover, looks_generated};

#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub min_tokens: u32,
    /// Exact-text matching only: identifiers and literals must match too.
    pub strict: bool,
    /// Language indices into `LANGS`; empty means every language that is
    /// not opt-in.
    pub languages: Vec<usize>,
    pub exclude: Vec<String>,
    /// Config files' path patterns, each anchored at its own directory.
    pub rules: Vec<PathRules>,
    /// Fingerprints of groups to leave out: duplication kept on purpose.
    pub accept: Vec<String>,
    /// For `near`, which also applies `[near] exclude` from configs found
    /// inside the scanned tree.
    pub near: bool,
    /// Leave configs inside the scanned tree unread: `--config` and
    /// `--no-config` name the one config that counts.
    pub skip_nested: bool,
    /// Groups needing more parameters than this per 100 tokens (and more
    /// than [`FREE_PARAMS`]) are dropped: they are tables of data or mapping
    /// boilerplate, not a helper waiting to be extracted. Which parameters
    /// count is [`counts_as_parameter`].
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
            rules: Vec::new(),
            accept: Vec::new(),
            near: false,
            skip_nested: false,
            max_param_density: 8.0,
            display_base: None,
        }
    }
}

impl ScanOptions {
    /// These options restricted to `lang` when they name no languages, for
    /// a question about one file or snippet: its clones are all in its
    /// language, and an opt-in language (YAML) is then scanned because it
    /// was asked about.
    pub fn narrowed_to(&self, lang: Option<usize>) -> Self {
        let mut opts = self.clone();
        if let Some(lang) = lang
            && opts.languages.is_empty()
        {
            opts.languages = vec![lang];
        }
        opts
    }
}

struct Cached {
    stamp: (SystemTime, u64),
    index: Option<Arc<FileIndex>>,
}

/// Root, file, strict, included: what the indexed result hangs on besides
/// the file's stamp. `included` counts because a config can start naming a
/// generated file without the file itself changing.
type CacheKey = (PathBuf, PathBuf, bool, bool);

#[derive(Default)]
pub struct Engine {
    cache: Mutex<FxHashMap<CacheKey, Cached>>,
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
        let filter = PathFilter::new(&root, opts)?;
        let found = discover(&root, &filter, &opts.languages)?;
        let base = base.as_deref().unwrap_or(&root);
        let indexed: Vec<_> = found
            .par_iter()
            .map_init(Parser::new, |parser, file| {
                self.index_cached(parser, base, file, opts.strict)
            })
            .collect();
        Ok(indexed.into_iter().flatten().collect())
    }

    fn index_cached(
        &self,
        parser: &mut Parser,
        root: &Path,
        file: &Found,
        strict: bool,
    ) -> Option<Arc<FileIndex>> {
        let path = file.path.as_path();
        let meta = std::fs::metadata(path).ok()?;
        if meta.len() > MAX_FILE_BYTES {
            return None;
        }
        let stamp = (meta.modified().ok()?, meta.len());
        let key = (
            root.to_path_buf(),
            path.to_path_buf(),
            strict,
            file.included,
        );
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap()
            .get(&key)
            .filter(|c| c.stamp == stamp)
        {
            return hit.index.clone();
        }
        let index = read_and_index(parser, root, file, strict);
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
    file: &Found,
    strict: bool,
) -> Option<Arc<FileIndex>> {
    let text = normalize_line_endings(String::from_utf8(std::fs::read(&file.path).ok()?).ok()?);
    if looks_generated(&text, file.included) {
        return None;
    }
    let shown = display_path(root, &file.path);
    index_source(parser, shown, file.lang, text, strict).map(Arc::new)
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
    let groups: Vec<RawGroup> = detect(&files, min_tokens, |g| {
        let allowed = (density * g.tokens as f32 / 100.0).max(FREE_PARAMS);
        let params = variation::analyze(g, &files)
            .parameters
            .iter()
            .filter(|p| counts_as_parameter(p))
            .count();
        params as f32 <= allowed
    });
    // After suppression, so the pieces inside an accepted clone stay
    // hidden with it instead of surfacing as groups of their own.
    let (accepted, mut groups): (Vec<_>, Vec<_>) = groups
        .into_iter()
        .partition(|g| opts.accept.contains(&fingerprint(g)));
    groups.sort_by(|a, b| score(b).cmp(&score(a)).then(a.hash.cmp(&b.hash)));
    Analysis {
        files,
        groups,
        accepted,
        elapsed: started.elapsed(),
    }
}

/// A name that differs in several places, in a file read without a
/// grammar, is most likely a renamed local, which the tokens cannot tell
/// from a parameter; counting those would drop every renamed copy. One
/// that differs once is counted, or same-shaped unrelated code (shell
/// lines, SQL statements) would pass the veto.
fn counts_as_parameter(p: &Variation) -> bool {
    p.kind != Kind::Name || p.uses == 1
}

/// A small clone with a couple of arguments is still a fine helper, so the
/// density cap only bites past this many parameters.
pub const FREE_PARAMS: f32 = 3.0;

pub fn score(g: &RawGroup) -> u64 {
    g.tokens as u64 * (g.occs.len() as u64 - 1)
}

pub const SNIPPET_PATH: &str = "<snippet>";

/// What a snippet search found.
pub struct Similar {
    pub analysis: Analysis,
    /// Ids of groups holding both the snippet and existing code.
    pub hits: Vec<usize>,
    /// Ids of such groups the config accepts as duplication kept on purpose.
    pub accepted: Vec<usize>,
}

impl Engine {
    /// Clone groups that include `code`: answers "does this already exist
    /// somewhere?" before an agent writes or extracts it. The parameter
    /// density limit does not apply: it ranks helpers worth extracting, and
    /// here a match with every name and literal changed is still a match.
    pub fn similar(
        &self,
        root: &Path,
        code: &str,
        lang: usize,
        opts: &ScanOptions,
    ) -> anyhow::Result<Similar> {
        let started = Instant::now();
        let scan = ScanOptions {
            max_param_density: f32::INFINITY,
            accept: Vec::new(),
            ..opts.narrowed_to(Some(lang))
        };
        let files = self.index_with_snippet(root, code, lang, &scan)?;
        let analysis = analyze(files, &scan, started);
        let (accepted, hits) = snippet_matches(&analysis, &opts.accept);
        Ok(Similar {
            analysis,
            hits,
            accepted,
        })
    }

    /// The scanned files with the snippet last.
    fn index_with_snippet(
        &self,
        root: &Path,
        code: &str,
        lang: usize,
        opts: &ScanOptions,
    ) -> anyhow::Result<Vec<Arc<FileIndex>>> {
        let mut files = self.index_tree(root, opts)?;
        let snippet = Source {
            path: SNIPPET_PATH.into(),
            lang,
            text: snippet_text(code, lang),
        };
        files.extend(index_sources(vec![snippet], opts.strict));
        Ok(files)
    }
}

/// Ids of the groups matching existing code, as (accepted, the others). The
/// snippet is the last file.
fn snippet_matches(analysis: &Analysis, accept: &[String]) -> (Vec<usize>, Vec<usize>) {
    let snippet = analysis.files.len() as u32 - 1;
    let groups = &analysis.groups;
    (1..=groups.len())
        .filter(|&id| matches_existing(&groups[id - 1], snippet))
        .partition(|&id| accept.contains(&fingerprint(&groups[id - 1])))
}

/// A group of the snippet's own repeats says nothing about the code base.
fn matches_existing(group: &RawGroup, snippet: u32) -> bool {
    let in_snippet = |o: &Occ| o.file == snippet;
    group.occs.iter().any(in_snippet) && !group.occs.iter().all(in_snippet)
}

/// PHP is only parsed as code after an opening tag. The tag shares the first
/// line so the snippet's line numbers stay the caller's.
fn snippet_text(code: &str, lang: usize) -> String {
    if crate::lang::LANGS[lang].name == "php" && !code.trim_start().starts_with("<?") {
        format!("<?php {code}")
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
    /// Ids of new groups smaller than the check's own minimum: shown, but
    /// they do not fail it and are not in `new`.
    pub smaller: Vec<usize>,
    /// Says why some new groups do not count, when any do not.
    pub note: Option<String>,
}

impl Engine {
    /// New duplication against `against`. New groups under `min_new_tokens`
    /// are reported but do not count, so a gate can be stricter about size
    /// than the report.
    pub fn check(
        &self,
        root: &Path,
        against: &Against,
        opts: &ScanOptions,
        min_new_tokens: u32,
    ) -> anyhow::Result<Check> {
        let (baseline, opts, scope) = match against {
            Against::File(path) => against_file(root, path, opts)?,
            // Both sides scan `root`, so all of this baseline is in view.
            Against::Rev(rev) => {
                let (base, opts) = self.scan_rev(root, rev, opts)?;
                let baseline = Baseline::from_analysis(&base, &opts);
                (baseline, opts, Scope::default())
            }
        };
        let analysis = self.scan(root, &opts)?;
        let resolved = baseline.groups.len() - baseline.pruned(&analysis, &scope).groups.len();
        let (new, small): (Vec<usize>, Vec<usize>) = baseline
            .new_groups(&analysis)
            .into_iter()
            .partition(|&id| analysis.groups[id - 1].tokens >= min_new_tokens);
        let note = (!small.is_empty()).then(|| {
            let groups = if small.len() == 1 { "group is" } else { "groups are" };
            format!(
                "{} smaller new {groups} under {min_new_tokens} tokens, listed last; they do not fail the check.",
                small.len()
            )
        });
        Ok(Check {
            analysis,
            new,
            resolved,
            smaller: small,
            note,
        })
    }

    /// The baseline in `file` without what a scan of `root` shows is gone.
    pub fn prune(&self, root: &Path, file: &Path, opts: &ScanOptions) -> anyhow::Result<Baseline> {
        let (baseline, opts, scope) = against_file(root, file, opts)?;
        Ok(baseline.pruned(&self.scan(root, &opts)?, &scope))
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
        let commit = crate::git::fork_point(repo_dir(root), rev)?;
        let revision = crate::git::read_revision(root, &commit, opts, &prefix)?;
        let mut opts = opts.clone();
        let submodules = revision.submodules.iter().map(|s| format!("{s}/**"));
        opts.exclude.extend(submodules);
        let files = index_sources(revision.sources, opts.strict);
        Ok((analyze(files, &opts, started), opts))
    }
}

/// A directory git can run in for `root`. Named file or not, the fork point
/// resolves, so `read_revision` can give the error that fits.
fn repo_dir(root: &Path) -> &Path {
    if !root.is_file() {
        return root;
    }
    let parent = root.parent().filter(|p| !p.as_os_str().is_empty());
    parent.unwrap_or(Path::new("."))
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
/// scan is shown from there whatever subdirectory is being checked. The
/// scope says whether that was all of what the baseline describes.
fn against_file(
    root: &Path,
    path: &Path,
    opts: &ScanOptions,
) -> anyhow::Result<(Baseline, ScanOptions, Scope)> {
    let baseline = Baseline::load(path)?;
    baseline.settings.require_same(opts, path)?;
    let base = anchor(root, path)?;
    let scope = Scope::of(&prefix_of(&root.canonicalize()?, &base));
    let opts = ScanOptions {
        display_base: Some(base),
        ..opts.clone()
    };
    Ok((baseline, opts, scope))
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

impl Engine {
    /// Near-miss clones: functions at least `similarity` alike (0 to 1)
    /// that are not exact clones of each other.
    pub fn near(
        &self,
        root: &Path,
        opts: &ScanOptions,
        similarity: f32,
    ) -> anyhow::Result<crate::near::NearAnalysis> {
        let started = Instant::now();
        let files = self.index_tree(root, opts)?;
        Ok(crate::near::near_analysis(files, opts, similarity, started))
    }
}
