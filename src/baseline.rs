//! Accepted duplication, so a gate can fail only on duplication a change
//! adds. A baseline comes from a committed file or from scanning a git
//! revision; either way the comparison is the same.

use std::path::{Path, PathBuf};

use anyhow::Context;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::detect::RawGroup;
use crate::engine::ScanOptions;
use crate::lang::LANGS;
use crate::report::{Analysis, fingerprint};

pub const DEFAULT_FILE: &str = ".doppelscan-baseline.json";
/// Bumped whenever fingerprints change, since an older file's entries would
/// then match nothing and unchanged code would read as new. Version 3 follows
/// language and lexer changes. Version 2 lists the languages it covers even
/// when none were named: version 1 stored none, which then meant every
/// language the binary knew, so a release adding languages failed every check
/// on their existing duplication.
const VERSION: u32 = 3;
/// All of these stay one byte in JSON.
const SHAPE_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";

#[derive(Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub version: u32,
    /// The scan settings the entries were found with; checks reuse them,
    /// since a different minimum size finds different groups.
    pub settings: Settings,
    /// The folder the entries describe, relative to the file's directory
    /// with a trailing slash; empty for that directory itself. Clones depend
    /// on what is in view, so comparisons have to scan exactly this.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scanned: String,
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
    /// The clone's normalized tokens, one character each, for telling code
    /// it held from new code beside it: a place only says where the clone
    /// sat, and an edit leaves a shrunk clone with a new fingerprint. Empty
    /// in baselines written before this was recorded.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shape: String,
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
            scanned: String::new(),
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
        let baseline: Self = serde_json::from_str(&text)
            .with_context(|| format!("parsing baseline {}", path.display()))?;
        anyhow::ensure!(
            baseline.version >= VERSION,
            "baseline {} has version {}, whose fingerprints this doppelscan does not produce; \
             write it again with `doppelscan baseline`",
            path.display(),
            baseline.version
        );
        anyhow::ensure!(
            baseline.version == VERSION,
            "baseline {} has version {}; this doppelscan reads version {VERSION}",
            path.display(),
            baseline.version
        );
        Ok(baseline)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// Ids of the groups in `now` that this baseline does not account for.
    pub fn new_groups(&self, now: &Analysis) -> Vec<usize> {
        let index = Index::new(&self.groups);
        (1..=now.groups.len())
            .filter(|&id| !index.excuses(&entry(now, &now.groups[id - 1])))
            .collect()
    }

    /// The entries still needed to excuse `now`, with counts lowered and
    /// places cut to what remains. Never adds an entry, a place or a copy,
    /// so pruning cannot accept new duplication. `now` must be a scan of the
    /// baseline's whole directory: recorded places go stale as soon as a
    /// copy moves, so from part of the tree a copy that moved out of it looks
    /// exactly like one that was deleted.
    pub fn pruned(&self, now: &Analysis) -> Self {
        let found = now.groups.iter().chain(&now.accepted);
        let current: Vec<Entry> = found.map(|group| entry(now, group)).collect();
        let excused = Index::new(&self.groups).excused_groups(&current);
        let kept = self.groups.iter().zip(&excused);
        let groups = kept.filter_map(|(e, by)| e.pruned(by)).collect();
        Self {
            version: VERSION,
            settings: self.settings.clone(),
            scanned: self.scanned.clone(),
            groups,
        }
    }
}

/// Entries by what a group has to share with them to match, so a large
/// baseline costs a lookup per group rather than a pass over every entry.
struct Index<'a> {
    entries: &'a [Entry],
    by_fingerprint: FxHashMap<&'a str, Vec<usize>>,
    by_place: FxHashMap<&'a str, Vec<usize>>,
}

impl<'a> Index<'a> {
    /// Places without a `#` are left out: the shrink rule never uses them.
    fn new(entries: &'a [Entry]) -> Self {
        let mut by_fingerprint: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
        let mut by_place: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
        for (i, e) in entries.iter().enumerate() {
            by_fingerprint.entry(&e.fingerprint).or_default().push(i);
            for place in e.places.iter().filter(|p| p.contains('#')) {
                by_place.entry(place).or_default().push(i);
            }
        }
        Self {
            entries,
            by_fingerprint,
            by_place,
        }
    }

    /// An entry accounts for a group when it is the same clone with at
    /// least as many copies (moved or renamed code keeps its fingerprint),
    /// or when it is a smaller clone in places one entry already covered:
    /// an edit that shrinks a clone changes its fingerprint but adds nothing.
    fn excuses(&self, group: &Entry) -> bool {
        let copies = |&i: &usize| self.entries[i].occurrences;
        self.same(group)
            .iter()
            .any(|i| group.occurrences <= copies(i))
            || !self.shrunk_from(group).is_empty()
    }

    /// For each entry, the groups in `current` that lean on it.
    fn excused_groups<'g>(&self, current: &'g [Entry]) -> Vec<Vec<&'g Entry>> {
        let mut excused = vec![Vec::new(); self.entries.len()];
        for group in current {
            let same = self.same(group).iter().copied();
            for i in same.chain(self.shrunk_from(group)) {
                excused[i].push(group);
            }
        }
        excused
    }

    /// Fingerprints are truncated hashes, so several entries can share one.
    fn same(&self, group: &Entry) -> &[usize] {
        let found = self.by_fingerprint.get(group.fingerprint.as_str());
        found.map_or(&[], Vec::as_slice)
    }

    /// The entries `group` is a shrunk form of. Every place of such a group
    /// pairs with a place of the entry, so holding something that encloses
    /// its first place narrows the candidates without missing any.
    fn shrunk_from(&self, group: &Entry) -> Vec<usize> {
        let first = group.places.iter().take(1);
        let enclosed = first.flat_map(|place| enclosing(place));
        let held = enclosed.filter_map(|place| self.by_place.get(place));
        let mut candidates: Vec<usize> = held.flatten().copied().collect();
        candidates.sort_unstable();
        candidates.dedup();
        candidates.retain(|&i| is_shrunk_from(group, &self.entries[i]));
        candidates
    }
}

impl Entry {
    /// What `excused`, the groups leaning on this entry, still need of it,
    /// or `None` when there are none.
    fn pruned(&self, excused: &[&Entry]) -> Option<Self> {
        if excused.is_empty() {
            return None;
        }
        // Same and shrunk groups both lean on this entry at once, so what
        // only one of them holds has to stay; moved copies hold no place,
        // which is why the same group's own count also counts.
        let places = self.places_held_by(excused);
        let same = excused.iter().filter(|c| c.fingerprint == self.fingerprint);
        let copies = same.map(|c| c.occurrences);
        let remaining = copies.max().unwrap_or(0).max(places.len());
        Some(self.narrowed(places, remaining))
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

/// A shrunk clone is code the entry held, in places it held it. An entry
/// without a shape can only go by places, and there only named ones count:
/// two files sharing one top-level clone must not excuse every later
/// top-level clone between them.
fn is_shrunk_from(group: &Entry, accepted: &Entry) -> bool {
    let held = if accepted.shape.is_empty() {
        group.places.iter().all(|p| p.contains('#'))
    } else {
        accepted.shape.contains(&group.shape)
    };
    group.tokens <= accepted.tokens && held && is_sub_multiset(&group.places, &accepted.places)
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
        shape: shape(analysis, raw),
    }
}

/// Six bits a token keep the file small, and a false match would still take
/// a whole clone's worth of tokens lining up. Separators are left out
/// because a run holds only its elements while the whole node around them
/// holds what sits between them too, and a run cut from that node must still
/// read as part of it.
fn shape(analysis: &Analysis, raw: &RawGroup) -> String {
    let occ = &raw.occs[0];
    let file = &analysis.files[occ.file as usize];
    let leaves = occ.leaf_indices().map(|i| &file.leaves[i as usize]);
    let tokens = leaves.filter(|leaf| !matches!(file.leaf_text(leaf), "," | ";"));
    tokens
        .map(|leaf| SHAPE_CHARS[leaf.token as usize % SHAPE_CHARS.len()] as char)
        .collect()
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

/// Every `outer` for which `encloses(outer, place)` holds. Cuts at each `.`
/// rather than parsing out the scope, since `encloses` does not parse either
/// and a path can hold both a `.` and a `#`.
fn enclosing(place: &str) -> impl Iterator<Item = &str> {
    let cuts = place.match_indices('.').map(|(i, _)| &place[..i]);
    cuts.filter(|outer| outer.contains('#'))
        .chain(std::iter::once(place))
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
