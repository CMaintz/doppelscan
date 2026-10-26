//! Cross-file grouping: whole-node clones by hash, statement-run clones by
//! maximal repeats, then subsumption so each duplication is reported once
//! at its largest extent.

use std::sync::Arc;

use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::hash::combine;
use crate::index::{FileIndex, Frag};
use crate::suffix::maximal_repeats;

#[derive(Clone, Debug)]
pub struct Occ {
    pub file: u32,
    pub start: u32,
    pub end: u32,
    pub start_line: u32,
    pub end_line: u32,
    pub scope: u32,
    /// Leaf index ranges; one per run element so separators between
    /// statements (which differ between grammars) never break alignment.
    pub leaves: Vec<(u32, u32)>,
}

impl Occ {
    fn contains(&self, other: &Occ) -> bool {
        self.file == other.file && self.start <= other.start && other.end <= self.end
    }

    pub fn leaf_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.leaves.iter().flat_map(|&(s, e)| s..e)
    }
}

#[derive(Clone, Debug)]
pub struct RawGroup {
    pub hash: u64,
    pub tokens: u32,
    /// 1 for a whole-node clone, otherwise the number of statements.
    pub run_len: usize,
    pub occs: Vec<Occ>,
}

/// `useful` vetoes a group before it is accepted; a vetoed group does not
/// shadow the smaller clones inside it.
pub fn detect(
    files: &[Arc<FileIndex>],
    min_tokens: u32,
    useful: impl Fn(&RawGroup) -> bool,
) -> Vec<RawGroup> {
    let (mut groups, runs) = rayon::join(
        || node_groups(files, min_tokens),
        || run_groups(files, min_tokens),
    );
    groups.extend(runs);
    groups.retain_mut(keep_aligned);
    suppress_subsumed(groups, useful)
}

fn node_groups(files: &[Arc<FileIndex>], min_tokens: u32) -> Vec<RawGroup> {
    let mut keyed: Vec<(u64, u32, u32)> = files
        .par_iter()
        .enumerate()
        .flat_map_iter(|(fi, f)| {
            f.nodes
                .iter()
                .enumerate()
                .filter(move |(_, n)| n.tokens() >= min_tokens)
                .map(move |(ni, n)| (n.hash, fi as u32, ni as u32))
        })
        .collect();
    keyed.par_sort_unstable();
    keyed
        .chunk_by(|a, b| a.0 == b.0)
        .filter(|c| c.len() >= 2)
        .map(|c| {
            let occs = c
                .iter()
                .map(|&(_, fi, ni)| occ_of(fi, &[files[fi as usize].nodes[ni as usize]]))
                .collect();
            RawGroup {
                hash: c[0].0,
                tokens: files[c[0].1 as usize].nodes[c[0].2 as usize].tokens(),
                run_len: 1,
                occs,
            }
        })
        .collect()
}

fn occ_of(file: u32, elems: &[Frag]) -> Occ {
    let (first, last) = (elems[0], elems[elems.len() - 1]);
    Occ {
        file,
        start: first.start,
        end: last.end,
        start_line: first.start_line,
        end_line: last.end_line,
        scope: first.scope,
        leaves: elems.iter().map(|e| (e.leaf_start, e.leaf_end)).collect(),
    }
}

/// Concatenation of every block's child sequence as dense symbols, each
/// sequence closed by a unique separator so no repeat spans two blocks.
struct RunText {
    symbols: Vec<u32>,
    /// (file, seq, element) at each position; separators have file u32::MAX.
    owner: Vec<(u32, u32, u32)>,
    token_prefix: Vec<u64>,
}

fn build_run_text(files: &[Arc<FileIndex>]) -> RunText {
    let mut ids: FxHashMap<u64, u32> = FxHashMap::default();
    let mut text = RunText {
        symbols: Vec::new(),
        owner: Vec::new(),
        token_prefix: vec![0],
    };
    let mut separators = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        for (si, seq) in f.seqs.iter().enumerate() {
            for (ei, e) in seq.iter().enumerate() {
                let next = ids.len() as u32;
                text.symbols.push(*ids.entry(e.hash).or_insert(next));
                text.owner.push((fi as u32, si as u32, ei as u32));
                text.token_prefix
                    .push(text.token_prefix.last().unwrap() + e.tokens() as u64);
            }
            separators.push(text.symbols.len());
            text.symbols.push(0);
            text.owner.push((u32::MAX, 0, 0));
            text.token_prefix.push(*text.token_prefix.last().unwrap());
        }
    }
    let base = ids.len() as u32;
    for (k, &pos) in separators.iter().enumerate() {
        text.symbols[pos] = base + k as u32;
    }
    text
}

fn run_groups(files: &[Arc<FileIndex>], min_tokens: u32) -> Vec<RawGroup> {
    let text = build_run_text(files);
    maximal_repeats(&text.symbols, 2)
        .into_par_iter()
        .filter_map(|rep| run_group(files, &text, rep.len, rep.positions, min_tokens))
        .collect()
}

fn run_group(
    files: &[Arc<FileIndex>],
    text: &RunText,
    len: usize,
    mut positions: Vec<u32>,
    min_tokens: u32,
) -> Option<RawGroup> {
    let p0 = positions[0] as usize;
    let tokens = (text.token_prefix[p0 + len] - text.token_prefix[p0]) as u32;
    if tokens < min_tokens {
        return None;
    }
    positions.sort_unstable();
    let kept: Vec<u32> = non_overlapping(&positions, len, text).collect();
    if is_tandem(&text.symbols[p0..p0 + len]) && shares_a_block(&kept, text) {
        return None;
    }
    let occs: Vec<Occ> = kept
        .iter()
        .map(|&p| occ_of(text.owner[p as usize].0, run_elems(files, text, p, len)))
        .collect();
    let hash = run_elems(files, text, p0 as u32, len)
        .iter()
        .fold(len as u64, |h, e| combine(h, e.hash));
    (occs.len() >= 2).then_some(RawGroup {
        hash,
        tokens,
        run_len: len,
        occs,
    })
}

fn run_elems<'f>(files: &'f [Arc<FileIndex>], text: &RunText, pos: u32, len: usize) -> &'f [Frag] {
    let (fi, si, ei) = text.owner[pos as usize];
    &files[fi as usize].seqs[si as usize][ei as usize..ei as usize + len]
}

fn shares_a_block(sorted: &[u32], text: &RunText) -> bool {
    sorted.windows(2).any(|w| {
        let (a, b) = (text.owner[w[0] as usize], text.owner[w[1] as usize]);
        (a.0, a.1) == (b.0, b.1)
    })
}

fn is_tandem(run: &[u32]) -> bool {
    let mut fail = vec![0usize; run.len()];
    let mut k = 0;
    for i in 1..run.len() {
        while k > 0 && run[i] != run[k] {
            k = fail[k - 1];
        }
        if run[i] == run[k] {
            k += 1;
        }
        fail[i] = k;
    }
    let period = run.len() - fail[run.len() - 1];
    period * 2 <= run.len()
}

/// A tandem repeat ("s s s s") yields occurrences that overlap within one
/// block; keep a greedy left-to-right set of disjoint ones.
fn non_overlapping<'a>(
    sorted: &'a [u32],
    len: usize,
    text: &'a RunText,
) -> impl Iterator<Item = u32> + 'a {
    let mut taken_end: Option<(u32, u32, usize)> = None;
    sorted.iter().copied().filter(move |&p| {
        let (fi, si, _) = text.owner[p as usize];
        let overlaps =
            matches!(taken_end, Some((f, s, end)) if f == fi && s == si && (p as usize) < end);
        if !overlaps {
            taken_end = Some((fi, si, p as usize + len));
        }
        !overlaps
    })
}

/// Guards against a 64-bit hash collision pairing fragments of different
/// shape: every occurrence must have as many leaves as the first.
fn keep_aligned(group: &mut RawGroup) -> bool {
    let count = |o: &Occ| o.leaves.iter().map(|&(s, e)| e - s).sum::<u32>();
    let expected = count(&group.occs[0]);
    group.occs.retain(|o| count(o) == expected);
    group.occs.len() >= 2
}

/// Largest groups first; a group is dropped when each of its occurrences
/// already sits inside an accepted occurrence, since extracting the outer
/// clone removes the inner one too.
fn suppress_subsumed(
    mut groups: Vec<RawGroup>,
    useful: impl Fn(&RawGroup) -> bool,
) -> Vec<RawGroup> {
    groups.sort_by(|a, b| {
        b.tokens
            .cmp(&a.tokens)
            .then(b.occs.len().cmp(&a.occs.len()))
            .then(a.hash.cmp(&b.hash))
    });
    let mut accepted_by_file: FxHashMap<u32, Vec<Occ>> = FxHashMap::default();
    let mut kept = Vec::new();
    for g in groups {
        let covered = g.occs.iter().all(|o| {
            accepted_by_file
                .get(&o.file)
                .is_some_and(|acc| acc.iter().any(|a| a.contains(o)))
        });
        if covered || !useful(&g) {
            continue;
        }
        for o in &g.occs {
            accepted_by_file.entry(o.file).or_default().push(o.clone());
        }
        kept.push(g);
    }
    kept
}
