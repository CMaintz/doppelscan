//! Which paths a scan leaves out: `--exclude` globs (relative to the
//! scanned path), config excludes (`.gitignore` patterns relative to the
//! config that lists them), and the built-in skip list, apart from what a
//! config's `include` brings back.
//!
//! Configs above the scanned path come in with the scan's settings. A
//! config inside the scanned tree is read as the walk enters its
//! directory, so a package's excludes hold whether the scan starts at the
//! package or at the repository root, unless the command named its config
//! (`--config`, `--no-config`), which then is the only one that counts.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::overrides::{Override, OverrideBuilder};
use rustc_hash::FxHashMap;

use crate::config::{FILE, nested_rules};
use crate::engine::ScanOptions;
use crate::walk::is_excluded_dir;

/// A config file's `exclude` and `include`, as `.gitignore` patterns
/// anchored at `dir` (canonical).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathRules {
    pub dir: PathBuf,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

#[derive(Clone)]
pub struct PathFilter {
    /// The scanned path, canonical, to anchor config patterns against.
    root: PathBuf,
    pub(crate) overrides: Override,
    rules: Arc<Vec<Compiled>>,
    nested: Arc<Nested>,
}

/// Configs found inside the scanned tree, by canonical directory, and the
/// first one that failed to load.
#[derive(Default)]
struct Nested {
    near: bool,
    skip: bool,
    found: RwLock<FxHashMap<PathBuf, Compiled>>,
    error: Mutex<Option<anyhow::Error>>,
}

struct Compiled {
    dir: PathBuf,
    exclude: Gitignore,
    include: Gitignore,
    /// Where each anchored include starts to need wildcards; the walk must
    /// enter skipped directories on the way there and below it.
    include_roots: Vec<PathBuf>,
}

impl PathFilter {
    pub fn new(root: &Path, opts: &ScanOptions) -> anyhow::Result<Self> {
        let rules = opts.rules.iter().map(compile);
        Ok(Self {
            root: root.canonicalize()?,
            overrides: exclude_overrides(root, &opts.exclude)?,
            rules: Arc::new(rules.collect::<anyhow::Result<_>>()?),
            nested: Arc::new(Nested {
                near: opts.near,
                skip: opts.skip_nested,
                ..Nested::default()
            }),
        })
    }

    /// `rel` is relative to the scanned path. A directory the scan enters
    /// has its own config read here, before anything inside it is judged.
    pub fn skips_dir(&self, rel: &Path) -> bool {
        let abs = self.absolute(rel);
        let builtin = rel.components().any(|c| is_excluded_dir(c.as_os_str()));
        let wanted = self.matches(&abs, true, |c| &c.include) || self.reaches_include(&abs);
        let skipped = (builtin && !wanted) || self.matches(&abs, true, |c| &c.exclude);
        if !skipped && !rel.as_os_str().is_empty() {
            self.read_config_in(&abs);
        }
        skipped
    }

    /// `rel` is relative to the scanned path.
    pub fn skips_file(&self, rel: &Path) -> bool {
        let abs = self.absolute(rel);
        let parents = rel.parent().into_iter().flat_map(Path::components);
        let builtin = parents.into_iter().any(|c| is_excluded_dir(c.as_os_str()));
        let included = self.matches(&abs, false, |c| &c.include);
        (builtin && !included) || self.matches(&abs, false, |c| &c.exclude)
    }

    /// A config inside the tree that could not be read, once the walk is
    /// done.
    pub fn take_error(&self) -> Option<anyhow::Error> {
        self.nested.error.lock().unwrap().take()
    }

    /// Joined part by part: a listed path uses `/`, which a verbatim
    /// Windows root (`\\?\C:\...`) would keep as part of one name.
    fn absolute(&self, rel: &Path) -> PathBuf {
        rel.components().fold(self.root.clone(), |p, c| p.join(c))
    }

    fn matches(&self, abs: &Path, is_dir: bool, which: impl Fn(&Compiled) -> &Gitignore) -> bool {
        let hit = |c: &Compiled| {
            abs.starts_with(&c.dir)
                && which(c)
                    .matched_path_or_any_parents(abs, is_dir)
                    .is_ignore()
        };
        self.rules.iter().any(hit) || self.nested_above(abs, hit)
    }

    fn reaches_include(&self, abs: &Path) -> bool {
        let reaches = |c: &Compiled| {
            c.include_roots
                .iter()
                .any(|r| r.starts_with(abs) || abs.starts_with(r))
        };
        self.rules.iter().any(reaches) || self.nested_above(abs, reaches)
    }

    fn nested_above(&self, abs: &Path, test: impl Fn(&Compiled) -> bool) -> bool {
        let found = self.nested.found.read().unwrap();
        !found.is_empty()
            && abs
                .ancestors()
                .take_while(|a| a.starts_with(&self.root) && *a != self.root)
                .any(|a| found.get(a).is_some_and(&test))
    }

    fn read_config_in(&self, dir: &Path) {
        let file = dir.join(FILE);
        if self.nested.skip || !file.is_file() {
            return;
        }
        match nested_rules(&file, self.nested.near).and_then(|r| compile(&r)) {
            Ok(compiled) => {
                let mut found = self.nested.found.write().unwrap();
                found.insert(dir.to_path_buf(), compiled);
            }
            Err(e) => {
                let mut error = self.nested.error.lock().unwrap();
                error.get_or_insert(e);
            }
        }
    }
}

fn compile(rules: &PathRules) -> anyhow::Result<Compiled> {
    let matcher = |patterns: &[String]| -> anyhow::Result<Gitignore> {
        let mut builder = GitignoreBuilder::new(&rules.dir);
        for p in patterns {
            builder.add_line(None, p)?;
        }
        Ok(builder.build()?)
    };
    Ok(Compiled {
        dir: rules.dir.clone(),
        exclude: matcher(&rules.exclude)?,
        include: matcher(&rules.include)?,
        include_roots: rules
            .include
            .iter()
            .filter_map(|p| include_root(&rules.dir, p))
            .collect(),
    })
}

/// The path an anchored pattern names before its first wildcard segment:
/// `vendor/ours/**` and `vendor/ours/` give `dir/vendor/ours`,
/// `vendor/*/src` gives `dir/vendor`. A pattern without an inner slash
/// (`build`) matches at any depth, as in `.gitignore`, so it names no one
/// path; inside a skipped directory it only works anchored.
fn include_root(dir: &Path, pattern: &str) -> Option<PathBuf> {
    if !pattern.trim_end_matches('/').contains('/') {
        return None;
    }
    let literal = pattern
        .split('/')
        .filter(|s| !s.is_empty())
        .take_while(|s| !s.contains(['*', '?', '[']));
    Some(literal.fold(dir.to_path_buf(), |p, s| p.join(s)))
}

fn exclude_overrides(root: &Path, exclude: &[String]) -> anyhow::Result<Override> {
    let mut overrides = OverrideBuilder::new(root);
    for glob in exclude {
        overrides.add(&format!("!{glob}"))?;
    }
    Ok(overrides.build()?)
}
