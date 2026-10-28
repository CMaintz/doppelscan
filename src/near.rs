//! Near-miss clones: functions that share most of their structure but not
//! all of it, such as a copy with a statement added or removed. The exact
//! scan cannot see these because their occurrences do not align 1:1.
//!
//! The approach follows dupehound (<https://github.com/Rafaelpta/dupehound>):
//! each function's normalized token stream is fingerprinted with winnowing
//! (Schleimer, Wilkerson and Aiken, "Winnowing: local algorithms for
//! document fingerprinting", SIGMOD 2003,
//! <https://theory.stanford.edu/~aiken/publications/papers/sigmod03.pdf>),
//! functions sharing fingerprints are found through an inverted index
//! rather than an all-pairs scan, compared by Jaccard similarity of their
//! fingerprint counts, and clustered with union-find.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;

use crate::engine::ScanOptions;
use crate::hash::combine;
use crate::index::{FileIndex, Frag, MIN_STORED_TOKENS};
use crate::report::Detail;

/// Tokens per k-gram: shorter shared runs never count as a match.
const K: usize = 10;
/// Winnowing window: any shared run of `W + K - 1` tokens yields at least
/// one shared fingerprint.
const W: usize = 8;
/// Fingerprints in more functions than this are idiom (`if err != nil`
/// ladders, logging boilerplate), not duplication, and are ignored.
const MIN_CULL: usize = 100;
/// Above this many table cells the per-line diff is skipped; the group is
/// still reported.
const MAX_DIFF_CELLS: usize = 4_000_000;
const BRIEF_OCCURRENCES: usize = 5;

#[derive(Clone, Copy, Debug)]
pub struct Unit {
    pub file: u32,
    pub frag: Frag,
    /// Units with the same class are exact copies of each other.
    pub class: u32,
}

pub struct RawNear {
    /// At least two exact classes, none inside another member.
    pub members: Vec<Unit>,
    /// The lowest similarity among the matched pairs that joined the group.
    pub similarity: f32,
}

pub struct NearAnalysis {
    pub files: Vec<Arc<FileIndex>>,
    /// Sorted by score, highest first; a group's id is its index + 1.
    pub groups: Vec<RawNear>,
    pub elapsed: Duration,
}

#[derive(Debug, Serialize)]
pub struct NearGroup {
    pub id: usize,
    /// Jaccard similarity of fingerprint counts, 0 to 1.
    pub similarity: f32,
    /// Size of the smallest copy.
    pub tokens: u32,
    /// tokens x (occurrences - 1), as for exact groups.
    pub score: u64,
    pub occurrences: Vec<NearOccurrence>,
}

#[derive(Debug, Serialize)]
pub struct NearOccurrence {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Line ranges with code that a copy from another exact class lacks.
    /// `None` when the copies are too long to diff.
    pub differs: Option<Vec<(u32, u32)>>,
}

#[derive(Debug, Serialize)]
pub struct NearSummary {
    pub files: usize,
    pub groups: usize,
    pub elapsed_ms: u128,
}

pub fn near_analysis(
    files: Vec<Arc<FileIndex>>,
    opts: &ScanOptions,
    threshold: f32,
    started: Instant,
) -> NearAnalysis {
    let units = units_of(&files, opts.min_tokens.max(MIN_STORED_TOKENS));
    let classes = exact_classes(&files, units);
    let edges = similar_classes(&files, &classes, threshold);
    let mut groups = without_nested(clusters(&classes, &edges));
    groups.sort_by_key(|g| std::cmp::Reverse(near_score(g)));
    NearAnalysis {
        files,
        groups,
        elapsed: started.elapsed(),
    }
}

fn units_of(files: &[Arc<FileIndex>], min_tokens: u32) -> Vec<Unit> {
    files
        .iter()
        .enumerate()
        .flat_map(|(i, f)| {
            f.units
                .iter()
                .filter(move |u| u.tokens() >= min_tokens)
                .map(move |&frag| Unit {
                    file: i as u32,
                    frag,
                    class: 0,
                })
        })
        .collect()
}

/// Units grouped by language and structure. Exact copies compare alike, so
/// only the first of each class is fingerprinted and paired; a vendored or
/// checked-out copy of a whole tree would otherwise multiply the work, and
/// the exact scan already reports copies within a class.
fn exact_classes(files: &[Arc<FileIndex>], units: Vec<Unit>) -> Vec<Vec<Unit>> {
    let mut by_key: FxHashMap<(usize, u64), Vec<Unit>> = FxHashMap::default();
    for unit in units {
        let key = (files[unit.file as usize].lang, unit.frag.hash);
        by_key.entry(key).or_default().push(unit);
    }
    let mut classes: Vec<Vec<Unit>> = by_key.into_values().collect();
    classes.sort_by_key(|c| (c[0].file, c[0].frag.start));
    for (id, class) in classes.iter_mut().enumerate() {
        class.iter_mut().for_each(|u| u.class = id as u32);
    }
    classes
}

fn similar_classes(
    files: &[Arc<FileIndex>],
    classes: &[Vec<Unit>],
    threshold: f32,
) -> Vec<(u32, u32, f32)> {
    let reps: Vec<Unit> = classes.iter().map(|c| c[0]).collect();
    let prints: Vec<Prints> = reps.par_iter().map(|u| winnow(&tokens(files, u))).collect();
    similar_pairs(files, &reps, &prints, threshold)
}

fn tokens(files: &[Arc<FileIndex>], unit: &Unit) -> Vec<u32> {
    let leaves = &files[unit.file as usize].leaves;
    leaves[unit.frag.leaf_start as usize..unit.frag.leaf_end as usize]
        .iter()
        .map(|l| l.token)
        .collect()
}

/// A unit's fingerprints with how often each was selected, sorted by
/// hash. Counts matter: as a set, four `this.x = x;` lines and ten would
/// fingerprint the same.
type Prints = Vec<(u64, u32)>;

fn winnow(tokens: &[u32]) -> Prints {
    if tokens.len() < K {
        return Vec::new();
    }
    let grams: Vec<u64> = tokens
        .windows(K)
        .map(|w| w.iter().fold(0x9E37, |h, &t| combine(h, t as u64)))
        .collect();
    let mut picked = selected(&grams);
    picked.sort_unstable();
    let mut prints: Prints = Vec::new();
    for fp in picked {
        match prints.last_mut() {
            Some((last, count)) if *last == fp => *count += 1,
            _ => prints.push((fp, 1)),
        }
    }
    prints
}

/// One hash per selected position: the rightmost minimum of each window,
/// so a window sliding past a minimum keeps choosing it.
fn selected(grams: &[u64]) -> Vec<u64> {
    let mut picked = Vec::new();
    let mut last = usize::MAX;
    for start in 0..grams.len().saturating_sub(W - 1).max(1) {
        let window = &grams[start..(start + W).min(grams.len())];
        let offset = (0..window.len()).rev().min_by_key(|&i| window[i]);
        let at = start + offset.expect("non-empty window");
        if at != last {
            picked.push(grams[at]);
            last = at;
        }
    }
    picked
}

fn size(prints: &Prints) -> u32 {
    prints.iter().map(|&(_, c)| c).sum()
}

/// Pairs of unit ids at or above `threshold`, with their similarity.
fn similar_pairs(
    files: &[Arc<FileIndex>],
    units: &[Unit],
    prints: &[Prints],
    threshold: f32,
) -> Vec<(u32, u32, f32)> {
    candidates(files, units, prints)
        .into_par_iter()
        .filter_map(|(a, b)| {
            let (pa, pb) = (&prints[a as usize], &prints[b as usize]);
            // Jaccard is at most the smaller size over the larger, which
            // skips the merge for most pairs of unequal size.
            let (sa, sb) = (size(pa) as f32, size(pb) as f32);
            if sa.min(sb) < threshold * sa.max(sb) {
                return None;
            }
            let similarity = jaccard(pa, pb);
            (similarity >= threshold).then_some((a, b, similarity))
        })
        .collect()
}

/// Pairs sharing a fingerprint that is not idiom. Idiom is skipped only
/// here, where a fingerprint in thousands of functions would pair them
/// all; scoring still counts it, so a pair alike in their idiom gets
/// credit for it and a pair differing in it (4 field assignments against
/// 12) does not look identical.
fn candidates(
    files: &[Arc<FileIndex>],
    units: &[Unit],
    prints: &[Prints],
) -> FxHashSet<(u32, u32)> {
    let mut index: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    for (id, fps) in prints.iter().enumerate() {
        for &(fp, _) in fps {
            index.entry(fp).or_default().push(id as u32);
        }
    }
    let cull = MIN_CULL.max(units.len() / 200);
    let mut pairs = FxHashSet::default();
    for list in index.values().filter(|l| l.len() <= cull) {
        for (i, &a) in list.iter().enumerate() {
            for &b in &list[i + 1..] {
                if comparable(files, &units[a as usize], &units[b as usize]) {
                    pairs.insert((a, b));
                }
            }
        }
    }
    pairs
}

/// Multiset Jaccard of two fingerprint lists sorted by hash, each hash at
/// most once (as `winnow` builds them). Two empty lists give NaN, which no
/// threshold accepts.
fn jaccard(a: &Prints, b: &Prints) -> f32 {
    let (mut i, mut j, mut common) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += a[i].1.min(b[j].1);
                i += 1;
                j += 1;
            }
        }
    }
    common as f32 / (size(a) + size(b) - common) as f32
}

fn comparable(files: &[Arc<FileIndex>], a: &Unit, b: &Unit) -> bool {
    let same_lang = files[a.file as usize].lang == files[b.file as usize].lang;
    same_lang && !nested(a, b) && !nested(b, a)
}

fn nested(inner: &Unit, outer: &Unit) -> bool {
    inner.file == outer.file
        && outer.frag.start <= inner.frag.start
        && inner.frag.end <= outer.frag.end
}

fn strictly_inside(inner: &Unit, outer: &Unit) -> bool {
    let same = (inner.frag.start, inner.frag.end) == (outer.frag.start, outer.frag.end);
    nested(inner, outer) && !same
}

/// Groups of classes joined by similar pairs, each class expanded back to
/// all its copies.
fn clusters(classes: &[Vec<Unit>], edges: &[(u32, u32, f32)]) -> Vec<RawNear> {
    let mut parent: Vec<u32> = (0..classes.len() as u32).collect();
    for &(a, b, _) in edges {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        parent[ra as usize] = rb;
    }
    let mut by_root: FxHashMap<u32, (Vec<u32>, f32)> = FxHashMap::default();
    for &(a, _, similarity) in edges {
        let root = find(&mut parent, a);
        let entry = by_root.entry(root).or_insert((Vec::new(), 1.0));
        entry.1 = entry.1.min(similarity);
    }
    for id in 0..classes.len() as u32 {
        let root = find(&mut parent, id);
        if let Some(entry) = by_root.get_mut(&root) {
            entry.0.push(id);
        }
    }
    by_root
        .into_values()
        .map(|(ids, similarity)| expand(classes, &ids, similarity))
        .filter(|g| distinct_classes(&g.members) >= 2)
        .collect()
}

/// Only representatives were checked for nesting, so a copy expanded from
/// a class can sit inside another member; it is the same code twice.
fn expand(classes: &[Vec<Unit>], ids: &[u32], similarity: f32) -> RawNear {
    let all: Vec<Unit> = ids
        .iter()
        .flat_map(|&i| classes[i as usize].iter().copied())
        .collect();
    let members = all
        .iter()
        .filter(|m| !all.iter().any(|o| strictly_inside(m, o)))
        .copied()
        .collect();
    RawNear {
        members,
        similarity,
    }
}

fn distinct_classes(members: &[Unit]) -> usize {
    let mut classes: Vec<u32> = members.iter().map(|m| m.class).collect();
    classes.sort_unstable();
    classes.dedup();
    classes.len()
}

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        parent[x as usize] = parent[parent[x as usize] as usize];
        x = parent[x as usize];
    }
    x
}

fn without_nested(all: Vec<RawNear>) -> Vec<RawNear> {
    let keep: Vec<bool> = all
        .iter()
        .map(|g| !all.iter().any(|o| !std::ptr::eq(o, g) && carried_by(g, o)))
        .collect();
    all.into_iter()
        .zip(keep)
        .filter_map(|(g, k)| k.then_some(g))
        .collect()
}

/// Whether every member of `inner` sits in a different member of `outer`:
/// then the copies exist only because their enclosing functions were
/// copied. Two lambdas inside one function are a duplicate of their own.
fn carried_by(inner: &RawNear, outer: &RawNear) -> bool {
    let mut used = vec![false; outer.members.len()];
    inner.members.iter().all(|m| {
        let slot =
            (0..outer.members.len()).find(|&i| !used[i] && strictly_inside(m, &outer.members[i]));
        slot.map(|i| used[i] = true).is_some()
    })
}

fn near_score(g: &RawNear) -> u64 {
    smallest(g) as u64 * (g.members.len() as u64 - 1)
}

fn smallest(g: &RawNear) -> u32 {
    g.members.iter().map(|m| m.frag.tokens()).min().unwrap_or(0)
}

impl NearAnalysis {
    pub fn group(&self, id: usize) -> NearGroup {
        let raw = &self.groups[id - 1];
        let streams: Vec<Vec<u32>> = raw.members.iter().map(|m| tokens(&self.files, m)).collect();
        let occurrences = (0..raw.members.len())
            .map(|i| self.occurrence(raw, &streams, i))
            .collect();
        NearGroup {
            id,
            similarity: (raw.similarity * 100.0).round() / 100.0,
            tokens: smallest(raw),
            score: near_score(raw),
            occurrences,
        }
    }

    /// Diffed against a copy from another class: an exact twin would show
    /// no difference at all.
    fn occurrence(&self, raw: &RawNear, streams: &[Vec<u32>], i: usize) -> NearOccurrence {
        let m = &raw.members[i];
        let other = (0..raw.members.len())
            .find(|&j| raw.members[j].class != m.class)
            .expect("a group spans two classes");
        let file = &self.files[m.file as usize];
        let scope = file.own_scope(m.frag.start, m.frag.end, m.frag.scope);
        NearOccurrence {
            file: file.path.clone(),
            start_line: m.frag.start_line,
            end_line: m.frag.end_line,
            scope: file.scope_name(scope).map(str::to_string),
            differs: differing_lines(file, m, &streams[i], &streams[other]),
        }
    }

    pub fn summary(&self) -> NearSummary {
        NearSummary {
            files: self.files.len(),
            groups: self.groups.len(),
            elapsed_ms: self.elapsed.as_millis(),
        }
    }
}

/// Line ranges of the tokens in `own` that a longest common subsequence
/// with `other` leaves unmatched.
fn differing_lines(
    file: &FileIndex,
    unit: &Unit,
    own: &[u32],
    other: &[u32],
) -> Option<Vec<(u32, u32)>> {
    if own.len() * other.len() > MAX_DIFF_CELLS {
        return None;
    }
    let leaves = &file.leaves[unit.frag.leaf_start as usize..];
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for (i, _) in unmatched(own, other)
        .iter()
        .enumerate()
        .filter(|(_, u)| **u)
    {
        let line = file.line_of(leaves[i].start);
        match ranges.last_mut() {
            Some(last) if line <= last.1 + 1 => last.1 = last.1.max(line),
            _ => ranges.push((line, line)),
        }
    }
    Some(ranges)
}

/// For each token of `a`, whether a longest common subsequence with `b`
/// leaves it out. The cell cap keeps the table's u16 lengths in range.
fn unmatched(a: &[u32], b: &[u32]) -> Vec<bool> {
    let (n, m) = (a.len(), b.len());
    let mut table = vec![0u16; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[at(i, j)] = if a[i] == b[j] {
                table[at(i + 1, j + 1)] + 1
            } else {
                table[at(i + 1, j)].max(table[at(i, j + 1)])
            };
        }
    }
    let mut out = vec![true; n];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out[i] = false;
            i += 1;
            j += 1;
        } else if table[at(i + 1, j)] >= table[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

pub fn render_near(g: &NearGroup, detail: Detail) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "#{} x{} near, {:.0}% similar | {} tokens | score {}",
        g.id,
        g.occurrences.len(),
        g.similarity * 100.0,
        g.tokens,
        g.score
    );
    let shown = match detail {
        Detail::Brief => BRIEF_OCCURRENCES,
        Detail::Full => usize::MAX,
    };
    for o in g.occurrences.iter().take(shown) {
        let scope = o.scope.as_deref().map(|s| format!("  ({s})"));
        let _ = writeln!(
            out,
            "  {}:{}-{}{}{}",
            o.file,
            o.start_line,
            o.end_line,
            scope.unwrap_or_default(),
            render_differs(&o.differs)
        );
    }
    if g.occurrences.len() > shown {
        let _ = writeln!(out, "  ... and {} more", g.occurrences.len() - shown);
    }
    out
}

fn render_differs(differs: &Option<Vec<(u32, u32)>>) -> String {
    let Some(ranges) = differs else {
        return "  differs: (too long to diff)".into();
    };
    if ranges.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = ranges
        .iter()
        .map(|&(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect();
    format!("  differs: {}", lines.join(", "))
}

pub fn render_near_summary(a: &NearAnalysis, shown: usize) -> String {
    format!(
        "{} near-miss groups in {} files, {} ms. Showing {}.\n",
        a.groups.len(),
        a.files.len(),
        a.elapsed.as_millis(),
        shown
    )
}

#[cfg(test)]
mod tests {
    use super::{K, W, winnow};

    // Catches a winnowing window or k-gram size that breaks the guarantee
    // the threshold rests on: any shared run of W + K - 1 tokens is seen.
    #[test]
    fn a_shared_run_of_the_guaranteed_length_shares_a_fingerprint() {
        let run: Vec<u32> = (1000..1000 + (W + K - 1) as u32).collect();
        for offset in 0..12 {
            let a: Vec<u32> = (0..offset)
                .chain(run.iter().copied())
                .chain(500..520)
                .collect();
            let b: Vec<u32> = (200..217).chain(run.iter().copied()).collect();
            let (fa, fb) = (winnow(&a), winnow(&b));
            let shared = fa.iter().any(|(f, _)| fb.iter().any(|(g, _)| f == g));
            assert!(shared, "offset {offset}");
        }
    }
}
