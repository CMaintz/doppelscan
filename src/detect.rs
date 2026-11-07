//! Cross-file grouping: whole-node clones by hash, statement-run clones by
//! maximal repeats, then subsumption so each duplication is reported once
//! at its largest extent.

use std::fmt;
use std::sync::Arc;

use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::hash::combine;
use crate::index::{FileIndex, Frag};
use crate::suffix::{Repeat, maximal_repeats};

#[derive(Clone, Debug)]
pub struct Occ {
    pub file: u32,
    pub start: u32,
    pub end: u32,
    pub start_line: u32,
    pub end_line: u32,
    pub scope: u32,
    leaf_count: u32,
    elems: Elems,
}

/// The syntax elements an occurrence covers, each with its own leaf range so
/// separators between siblings (which differ between grammars) never break
/// alignment. A run points into its file rather than holding a copy: between
/// two equal tables every length has a group of its own until the longest
/// subsumes them, which is quadratic in the table when copied.
#[derive(Clone)]
enum Elems {
    Node {
        file: Arc<FileIndex>,
        index: u32,
    },
    Run {
        file: Arc<FileIndex>,
        seq: u32,
        start: u32,
        len: u32,
    },
}

impl Elems {
    fn frags(&self) -> &[Frag] {
        match self {
            Elems::Node { file, index } => std::slice::from_ref(&file.nodes[*index as usize]),
            Elems::Run {
                file,
                seq,
                start,
                len,
            } => &file.seqs.get(*seq as usize)[*start as usize..(start + len) as usize],
        }
    }
}

impl fmt::Debug for Elems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.frags()).finish()
    }
}

impl Occ {
    /// By token span, not bytes: a wrapper and the node it wraps (a file
    /// holding one function) cover the same tokens and contain each other,
    /// so whichever is accepted first suppresses the other.
    fn contains(&self, other: &Occ) -> bool {
        let (lo, hi) = self.leaf_span();
        let (other_lo, other_hi) = other.leaf_span();
        self.file == other.file && lo <= other_lo && other_hi <= hi
    }

    fn leaf_span(&self) -> (u32, u32) {
        let frags = self.elems.frags();
        (frags[0].leaf_start, frags[frags.len() - 1].leaf_end)
    }

    pub fn leaf_indices(&self) -> impl Iterator<Item = u32> + '_ {
        let frags = self.elems.frags().iter();
        frags.flat_map(|e| e.leaf_start..e.leaf_end)
    }
}

#[derive(Clone, Debug)]
pub struct RawGroup {
    pub hash: u64,
    pub tokens: u32,
    /// 1 for a whole-node clone, otherwise the number of siblings in the run.
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
                .map(|&(_, fi, ni)| {
                    let elems = Elems::Node {
                        file: Arc::clone(&files[fi as usize]),
                        index: ni,
                    };
                    occ_of(files, fi, elems)
                })
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

fn occ_of(files: &[Arc<FileIndex>], file: u32, elems: Elems) -> Occ {
    let frags = elems.frags();
    let (first, last) = (frags[0], frags[frags.len() - 1]);
    let (start_line, end_line) = files[file as usize].lines(first.start, last.end);
    Occ {
        file,
        start: first.start,
        end: last.end,
        start_line,
        end_line,
        scope: first.scope,
        leaf_count: frags.iter().map(Frag::tokens).sum(),
        elems,
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
    let repeats = maximal_repeats(&text.symbols, 2);
    let block_until = repeats.alike_until(|p| block_of(p, &text));
    let run_of = |rep: &Repeat| Run {
        len: rep.len,
        positions: repeats.positions(rep),
        one_block: repeats.all_alike(rep, &block_until),
    };
    repeats
        .found
        .par_iter()
        .filter_map(|rep| run_group(files, &text, run_of(rep), min_tokens))
        .collect()
}

/// A repeat's occurrences, in suffix order and not yet sorted.
struct Run<'a> {
    len: usize,
    positions: &'a [u32],
    one_block: bool,
}

impl Run<'_> {
    /// Repetition inside one block only, with no copy elsewhere: the shape
    /// of a wall. Decided before the positions are sorted, because a long
    /// table has one such run per length.
    fn is_only_a_wall(&self, text: &RunText) -> bool {
        let (a, b) = (self.positions[0] as usize, self.positions[1] as usize);
        let overlap_proves_it = 2 * a.abs_diff(b) <= self.len;
        self.one_block && (overlap_proves_it || is_tandem(&text.symbols[a..a + self.len]))
    }
}

fn run_group(
    files: &[Arc<FileIndex>],
    text: &RunText,
    run: Run,
    min_tokens: u32,
) -> Option<RawGroup> {
    let Run { len, positions, .. } = run;
    let p0 = positions[0] as usize;
    let tokens = (text.token_prefix[p0 + len] - text.token_prefix[p0]) as u32;
    if tokens < min_tokens || run.is_only_a_wall(text) {
        return None;
    }
    let mut sorted = positions.to_vec();
    sorted.sort_unstable();
    let kept = distinct_positions(files, text, &sorted, len)?;
    let occs: Vec<Occ> = kept.iter().map(|&p| run_occ(files, text, p, len)).collect();
    (occs.len() >= 2).then(|| RawGroup {
        hash: run_hash(run_elems(files, text, p0 as u32, len)),
        tokens,
        run_len: len,
        occs,
    })
}

fn run_hash(elems: &[Frag]) -> u64 {
    let len = elems.len() as u64;
    elems.iter().fold(len, |h, e| combine(h, e.hash))
}

fn run_occ(files: &[Arc<FileIndex>], text: &RunText, pos: u32, len: usize) -> Occ {
    let (fi, si, ei) = text.owner[pos as usize];
    let elems = Elems::Run {
        file: Arc::clone(&files[fi as usize]),
        seq: si,
        start: ei,
        len: len as u32,
    };
    occ_of(files, fi, elems)
}

/// Disjoint positions worth reporting, in text order. Inside a wall of
/// near-identical members a tandem repeat fits at every multiple of its
/// period, which is the wall's shape and no clone, so such a run counts only
/// through a copy elsewhere, at the wall positions spelled exactly like it.
fn distinct_positions(
    files: &[Arc<FileIndex>],
    text: &RunText,
    sorted: &[u32],
    len: usize,
) -> Option<Vec<u32>> {
    let kept: Vec<u32> = non_overlapping(sorted, len, text, |_| true).collect();
    let first = sorted[0] as usize;
    if !is_tandem(&text.symbols[first..first + len]) || !shares_a_block(&kept, text) {
        return Some(kept);
    }
    let copy = alone_in_its_block(&kept, text)?;
    let same_text = |p| spelled_alike(files, text, p, copy, len);
    Some(non_overlapping(sorted, len, text, same_text).collect())
}

fn alone_in_its_block(sorted: &[u32], text: &RunText) -> Option<u32> {
    sorted
        .chunk_by(|&a, &b| same_block(a, b, text))
        .find(|block| block.len() == 1)
        .map(|block| block[0])
}

fn spelled_alike(files: &[Arc<FileIndex>], text: &RunText, a: u32, b: u32, len: usize) -> bool {
    let spelling = |pos: u32| {
        let file = &files[text.owner[pos as usize].0 as usize];
        let elems = run_elems(files, text, pos, len).iter();
        let leaves = elems.flat_map(|e| &file.leaves[e.leaf_start as usize..e.leaf_end as usize]);
        leaves.map(|leaf| file.leaf_text(leaf))
    };
    spelling(a).eq(spelling(b))
}

fn run_elems<'f>(files: &'f [Arc<FileIndex>], text: &RunText, pos: u32, len: usize) -> &'f [Frag] {
    let (fi, si, ei) = text.owner[pos as usize];
    &files[fi as usize].seqs.get(si as usize)[ei as usize..ei as usize + len]
}

fn shares_a_block(sorted: &[u32], text: &RunText) -> bool {
    sorted.windows(2).any(|w| same_block(w[0], w[1], text))
}

fn same_block(a: u32, b: u32, text: &RunText) -> bool {
    block_of(a, text) == block_of(b, text)
}

fn block_of(pos: u32, text: &RunText) -> (u32, u32) {
    let (file, seq, _) = text.owner[pos as usize];
    (file, seq)
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
/// block; keep a greedy left-to-right set of disjoint ones among those
/// `wanted` allows. `wanted` is asked only about positions clear of the last
/// one taken, which keeps a full text comparison per taken position from
/// turning quadratic on a long table.
fn non_overlapping<'a>(
    sorted: &'a [u32],
    len: usize,
    text: &'a RunText,
    wanted: impl Fn(u32) -> bool + 'a,
) -> impl Iterator<Item = u32> + 'a {
    let mut taken_end: Option<(u32, u32, usize)> = None;
    sorted.iter().copied().filter(move |&p| {
        let (fi, si, _) = text.owner[p as usize];
        let overlaps =
            matches!(taken_end, Some((f, s, end)) if f == fi && s == si && (p as usize) < end);
        let take = !overlaps && wanted(p);
        if take {
            taken_end = Some((fi, si, p as usize + len));
        }
        take
    })
}

/// Guards against a 64-bit hash collision pairing fragments of different
/// shape: every occurrence must have as many leaves as the first.
fn keep_aligned(group: &mut RawGroup) -> bool {
    let expected = group.occs[0].leaf_count;
    group.occs.retain(|o| o.leaf_count == expected);
    group.occs.len() >= 2
}

/// Largest groups first (among equal token counts the tightest byte span,
/// so a function wins over the file wrapping it). A group is dropped when
/// each of its occurrences already sits inside an accepted occurrence,
/// since extracting the outer clone removes the inner one too.
fn suppress_subsumed(
    mut groups: Vec<RawGroup>,
    useful: impl Fn(&RawGroup) -> bool,
) -> Vec<RawGroup> {
    groups.sort_by(|a, b| {
        b.tokens
            .cmp(&a.tokens)
            .then(b.occs.len().cmp(&a.occs.len()))
            .then(byte_span(a).cmp(&byte_span(b)))
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

fn byte_span(g: &RawGroup) -> u32 {
    g.occs[0].end - g.occs[0].start
}
