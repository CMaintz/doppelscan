//! Accepted duplication, so a gate can fail only on duplication a change
//! adds. A baseline comes from a committed file or from scanning a git
//! revision; either way the comparison is the same.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::detect::RawGroup;
use crate::engine::ScanOptions;
use crate::lang::{LANGS, Syntax};
use crate::report::{Analysis, fingerprint};

pub const DEFAULT_FILE: &str = ".doppelscan-baseline.json";
/// Version 2 lists the languages it covers even when none were named:
/// version 1 stored none, which then meant every language the binary knew,
/// so a release adding languages failed every check on their existing
/// duplication.
const VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub version: u32,
    /// The scan settings the entries were found with; checks reuse them,
    /// since a different minimum size finds different groups.
    pub settings: Settings,
    pub groups: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub min_tokens: u32,
    pub strict: bool,
    pub max_param_density: f32,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub fingerprint: String,
    pub occurrences: usize,
    pub tokens: u32,
    /// `file` or `file#scope` per occurrence, sorted. Line numbers are left
    /// out because every edit above a clone would make them stale.
    pub places: Vec<String>,
}

impl Settings {
    pub fn from_options(opts: &ScanOptions) -> Self {
        Self {
            min_tokens: opts.min_tokens,
            strict: opts.strict,
            max_param_density: opts.max_param_density,
            languages: scanned_languages(&opts.languages)
                .into_iter()
                .map(|l| LANGS[l].name.to_string())
                .collect(),
            exclude: opts.exclude.clone(),
        }
    }

    /// Fails, naming each difference, unless `current` scans as these
    /// settings did: comparing scans made with different settings would
    /// report changes nobody made. The baseline records them only to
    /// check this; the settings themselves come from flags and configs.
    pub fn require_same(&self, current: &ScanOptions, file: &Path) -> anyhow::Result<()> {
        let now = Self::from_options(current);
        let differences: Vec<String> = [
            (
                "min-tokens",
                self.min_tokens.to_string(),
                now.min_tokens.to_string(),
            ),
            ("strict", self.strict.to_string(), now.strict.to_string()),
            (
                "max-param-density",
                self.max_param_density.to_string(),
                now.max_param_density.to_string(),
            ),
            ("languages", set_of(&self.languages), set_of(&now.languages)),
            ("--exclude", set_of(&self.exclude), set_of(&now.exclude)),
        ]
        .into_iter()
        .filter(|(_, then, now)| then != now)
        .map(|(key, then, now)| format!("  {key}: baseline {then}, now {now}"))
        .collect();
        anyhow::ensure!(
            differences.is_empty(),
            "{} was written with other settings:\n{}\nRewrite it with `doppelscan baseline`, or set them back.",
            crate::settings::shown(file),
            differences.join("\n")
        );
        Ok(())
    }
}

/// A list whose order does not change the scan, as one comparable value.
fn set_of(items: &[String]) -> String {
    let mut items = items.to_vec();
    items.sort();
    items.dedup();
    items.join(",")
}

/// The languages a scan with `languages` covers.
fn scanned_languages(languages: &[usize]) -> Vec<usize> {
    if languages.is_empty() {
        (0..LANGS.len()).filter(|&l| !LANGS[l].opt_in).collect()
    } else {
        languages.to_vec()
    }
}

impl Baseline {
    pub fn from_analysis(analysis: &Analysis, opts: &ScanOptions) -> Self {
        let raw = analysis.groups.iter();
        let mut groups: Vec<Entry> = raw.map(|group| entry(analysis, group)).collect();
        groups.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
        Self {
            version: VERSION,
            settings: Settings::from_options(opts),
            groups,
        }
    }

    /// The baseline file for a scan of `start`: in it or the nearest parent
    /// directory up to the repository root.
    pub fn find(start: &Path) -> Option<PathBuf> {
        crate::config::nearest(start, DEFAULT_FILE)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading baseline {}", path.display()))?;
        let mut baseline: Self = serde_json::from_str(&text)
            .with_context(|| format!("parsing baseline {}", path.display()))?;
        if baseline.version == 1 {
            baseline.upgrade_from_v1();
        }
        anyhow::ensure!(
            baseline.version == VERSION,
            "baseline {} has version {}; this doppelscan reads version {VERSION}",
            path.display(),
            baseline.version
        );
        Ok(baseline)
    }

    /// Version 1 baselines predate languages read without a grammar, so
    /// "all languages" meant the grammar ones.
    fn upgrade_from_v1(&mut self) {
        if self.settings.languages.is_empty() {
            self.settings.languages = LANGS
                .iter()
                .filter(|l| matches!(l.syntax, Syntax::Tree(_)))
                .map(|l| l.name.to_string())
                .collect();
        }
        self.version = VERSION;
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// Ids of the groups in `now` that this baseline does not account for.
    pub fn new_groups(&self, now: &Analysis) -> Vec<usize> {
        (1..=now.groups.len())
            .filter(|&id| !self.excuses(&entry(now, &now.groups[id - 1])))
            .collect()
    }

    /// The entries still needed to excuse `now`, with counts lowered and
    /// places cut to what remains. Never adds an entry, a place or a copy,
    /// so pruning cannot accept new duplication.
    pub fn pruned(&self, now: &Analysis) -> Self {
        let found = now.groups.iter().chain(&now.accepted);
        let current: Vec<Entry> = found.map(|group| entry(now, group)).collect();
        let groups = self
            .groups
            .iter()
            .filter_map(|e| e.pruned(&current))
            .collect();
        Self {
            version: VERSION,
            settings: self.settings.clone(),
            groups,
        }
    }

    /// An entry accounts for a group when it is the same clone with at
    /// least as many copies (moved or renamed code keeps its fingerprint),
    /// or when it is a smaller clone in places one entry already covered:
    /// an edit that shrinks a clone changes its fingerprint but adds nothing.
    fn excuses(&self, group: &Entry) -> bool {
        self.groups.iter().any(|e| {
            (e.fingerprint == group.fingerprint && group.occurrences <= e.occurrences)
                || is_shrunk_from(group, e)
        })
    }
}

impl Entry {
    /// What the groups found now still need of this entry, or `None` when
    /// they need none of it.
    fn pruned(&self, current: &[Entry]) -> Option<Self> {
        let same: Vec<&Entry> = current
            .iter()
            .filter(|c| c.fingerprint == self.fingerprint)
            .collect();
        if let Some(remaining) = same.iter().map(|c| c.occurrences).max() {
            return Some(self.narrowed(self.places_held_by(&same), remaining));
        }
        let shrunk: Vec<&Entry> = current.iter().filter(|c| is_shrunk_from(c, self)).collect();
        (!shrunk.is_empty()).then(|| {
            let places = self.places_held_by(&shrunk);
            let remaining = places.len();
            self.narrowed(places, remaining)
        })
    }

    fn narrowed(&self, places: Vec<String>, remaining: usize) -> Self {
        Self {
            occurrences: remaining.min(self.occurrences),
            places,
            ..self.clone()
        }
    }

    /// Only ever a subset of `places`, even for code that moved: following
    /// it would let the shrink rule excuse clones in places never accepted.
    fn places_held_by(&self, groups: &[&Entry]) -> Vec<String> {
        let mut held = vec![false; self.places.len()];
        let paired = groups
            .iter()
            .flat_map(|g| pairings(&g.places, &self.places));
        for i in paired.flatten() {
            held[i] = true;
        }
        let kept = self.places.iter().zip(held).filter(|(_, held)| *held);
        kept.map(|(place, _)| place.clone()).collect()
    }
}

/// Only named places count: two files sharing one top-level clone must not
/// excuse every later top-level clone between them.
fn is_shrunk_from(group: &Entry, accepted: &Entry) -> bool {
    group.tokens <= accepted.tokens
        && group.places.iter().all(|p| p.contains('#'))
        && is_sub_multiset(&group.places, &accepted.places)
}

fn entry(analysis: &Analysis, raw: &RawGroup) -> Entry {
    let mut places: Vec<String> = raw
        .occs
        .iter()
        .map(|o| {
            let file = &analysis.files[o.file as usize];
            match file.scope_name(file.own_scope(o.start, o.end, o.scope)) {
                Some(scope) => format!("{}#{scope}", file.path),
                None => file.path.clone(),
            }
        })
        .collect();
    places.sort();
    Entry {
        fingerprint: fingerprint(raw),
        occurrences: raw.occs.len(),
        tokens: raw.tokens,
        places,
    }
}

fn is_sub_multiset(small: &[String], big: &[String]) -> bool {
    pairings(small, big).all(|slot| slot.is_some())
}

/// For each place in `small`, the index of a distinct place in `big` that
/// is the same or encloses it (`a.ts#Cart` encloses `a.ts#Cart.total`).
/// Each place takes the deepest free match, so an exact match is never
/// spent on a place an enclosing scope could have covered.
fn pairings<'a>(
    small: &'a [String],
    big: &'a [String],
) -> impl Iterator<Item = Option<usize>> + 'a {
    let mut used = vec![false; big.len()];
    small.iter().map(move |s| {
        let slot = (0..big.len())
            .filter(|&i| !used[i] && encloses(&big[i], s))
            .max_by_key(|&i| big[i].len());
        slot.inspect(|&i| used[i] = true)
    })
}

fn encloses(outer: &str, inner: &str) -> bool {
    inner == outer
        || (outer.contains('#')
            && inner
                .strip_prefix(outer)
                .is_some_and(|rest| rest.starts_with('.')))
}

#[cfg(test)]
mod tests {
    use super::is_sub_multiset;

    fn places(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    // Catches first-fit matching: `C.m` taking the enclosing `C` would
    // leave nothing for `C.n`, though a matching exists.
    #[test]
    fn matching_keeps_enclosing_places_for_places_without_an_exact_match() {
        let small = places(&["a.ts#C.m", "a.ts#C.n"]);
        assert!(is_sub_multiset(&small, &places(&["a.ts#C", "a.ts#C.m"])));
        assert!(!is_sub_multiset(&small, &places(&["a.ts#C.m", "a.ts#D"])));
    }
}
