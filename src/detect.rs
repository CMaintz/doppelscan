//! Cross-file grouping: whole-node clones by hash, statement-run clones by
//! maximal repeats, then subsumption so each duplication is reported once
//! at its largest extent.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::hash::{combine, hash_bytes};
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
    spelling_prefix: OnceLock<Vec<u64>>,
}

const SPELLING_BASE: u64 = 0x9E37_79B9_7F4A_7C15;

impl RunText {
    /// The text of the `len` elements at `pos`, as a hash. Prefix sums make
    /// this one lookup, since a tandem run compares every position of a table
    /// with its copies, at every length. Built on first use: it reads every
    /// leaf, and a scan with no tandem run in two places never asks.
    fn spelling(&self, files: &[Arc<FileIndex>], pos: u32, len: usize) -> u64 {
        let prefix = self
            .spelling_prefix
            .get_or_init(|| spelling_prefix(files, self));
        let (start, end) = (prefix[pos as usize], prefix[pos as usize + len]);
        end.wrapping_sub(start.wrapping_mul(SPELLING_BASE.wrapping_pow(len as u32)))
    }
}

fn spelling_prefix(files: &[Arc<FileIndex>], text: &RunText) -> Vec<u64> {
    let mut prefix = Vec::with_capacity(text.symbols.len() + 1);
    prefix.push(0u64);
    for &owner in &text.owner {
        let next = prefix[prefix.len() - 1].wrapping_mul(SPELLING_BASE);
        prefix.push(next.wrapping_add(element_spelling(files, owner)));
    }
    prefix
}

fn element_spelling(files: &[Arc<FileIndex>], (file, seq, element): (u32, u32, u32)) -> u64 {
    if file == u32::MAX {
        return 0;
    }
    let file = &files[file as usize];
    let e = &file.seqs.get(seq as usize)[element as usize];
    let leaves = &file.leaves[e.leaf_start as usize..e.leaf_end as usize];
    leaves.iter().fold(0, |h, leaf| {
        combine(h, hash_bytes(file.leaf_text(leaf).as_bytes()))
    })
}

fn build_run_text(files: &[Arc<FileIndex>]) -> RunText {
    let mut ids: FxHashMap<u64, u32> = FxHashMap::default();
    let mut text = RunText {
        symbols: Vec::new(),
        owner: Vec::new(),
        token_prefix: vec![0],
        spelling_prefix: OnceLock::new(),
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
    let found = repeats
        .found
        .par_iter()
        .filter_map(|rep| run_group(files, &text, run_of(rep), min_tokens))
        .collect();
    without_nested_copies(found)
}

/// A group from a run, with the positions (each the run's length long) where
/// a tandem run sits alone in its block. Empty for any other run.
struct RunFind {
    group: RawGroup,
    copies: Vec<u32>,
}

/// Drops a tandem run whose copies all sit inside a longer run's. A shorter
/// table fits a periodic one at every length, each tiling it differently, so
/// subsumption, which needs one occurrence inside another, keeps them all.
/// Blocks end in separators, so positions compare across them, and what is
/// held never nests: the longest come first and a nested copy is not added.
fn without_nested_copies(mut found: Vec<RunFind>) -> Vec<RawGroup> {
    found.sort_by_key(|f| Reverse(f.group.run_len));
    let mut held: BTreeMap<u32, u32> = BTreeMap::new();
    found.retain(|f| {
        let len = f.group.run_len as u32;
        let is_held =
            |p: u32| matches!(held.range(..=p).next_back(), Some((_, &end)) if p + len <= end);
        let fresh: Vec<u32> = f.copies.iter().copied().filter(|&p| !is_held(p)).collect();
        held.extend(fresh.iter().map(|&p| (p, p + len)));
        f.copies.is_empty() || !fresh.is_empty()
    });
    found.into_iter().map(|f| f.group).collect()
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
) -> Option<RunFind> {
    let Run { len, positions, .. } = run;
    let p0 = positions[0] as usize;
    let tokens = (text.token_prefix[p0 + len] - text.token_prefix[p0]) as u32;
    if tokens < min_tokens || run.is_only_a_wall(text) {
        return None;
    }
    let mut sorted = positions.to_vec();
    sorted.sort_unstable();
    let (kept, copies) = distinct_positions(files, text, &sorted, len)?;
    let occs: Vec<Occ> = kept.iter().map(|&p| run_occ(files, text, p, len)).collect();
    let group = (occs.len() >= 2).then(|| RawGroup {
        hash: run_hash(run_elems(files, text, p0 as u32, len)),
        tokens,
        run_len: len,
        occs,
    })?;
    Some(RunFind { group, copies })
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
/// through copies elsewhere, at the wall positions spelled exactly like one.
/// Returns the positions and, for a tandem run, its copies.
fn distinct_positions(
    files: &[Arc<FileIndex>],
    text: &RunText,
    sorted: &[u32],
    len: usize,
) -> Option<(Vec<u32>, Vec<u32>)> {
    let kept: Vec<u32> = non_overlapping(sorted, len, text, |_| true).collect();
    let first = sorted[0] as usize;
    if !is_tandem(&text.symbols[first..first + len]) {
        return Some((kept, Vec::new()));
    }
    if !shares_a_block(&kept, text) {
        return Some((kept.clone(), kept));
    }
    spelled_like_copies(files, text, sorted, &kept, len)
}

fn spelled_like_copies(
    files: &[Arc<FileIndex>],
    text: &RunText,
    sorted: &[u32],
    kept: &[u32],
    len: usize,
) -> Option<(Vec<u32>, Vec<u32>)> {
    let mut copies = alone_in_their_blocks(kept, text);
    if copies.is_empty() {
        return None;
    }
    let wanted: FxHashSet<u64> = spellings(&copies, files, text, len).collect();
    let spelled_alike = |p| wanted.contains(&text.spelling(files, p, len));
    let taken = non_overlapping(sorted, len, text, spelled_alike).collect();
    let positions = held_twice(taken, files, text, len);
    copies.retain(|p| positions.binary_search(p).is_ok());
    Some((positions, copies))
}

fn alone_in_their_blocks(sorted: &[u32], text: &RunText) -> Vec<u32> {
    let blocks = sorted.chunk_by(|&a, &b| same_block(a, b, text));
    blocks
        .filter(|block| block.len() == 1)
        .map(|block| block[0])
        .collect()
}

fn spellings<'a>(
    positions: &'a [u32],
    files: &'a [Arc<FileIndex>],
    text: &'a RunText,
    len: usize,
) -> impl Iterator<Item = u64> + 'a {
    let spelling = move |&p: &u32| text.spelling(files, p, len);
    positions.iter().map(spelling)
}

/// A copy spelled unlike anything else, such as a renamed part of a wall,
/// has nothing to be a clone of here.
fn held_twice(
    mut positions: Vec<u32>,
    files: &[Arc<FileIndex>],
    text: &RunText,
    len: usize,
) -> Vec<u32> {
    let mut holders: FxHashMap<u64, u32> = FxHashMap::default();
    for spelling in spellings(&positions, files, text, len) {
        *holders.entry(spelling).or_default() += 1;
    }
    positions.retain(|&p| holders[&text.spelling(files, p, len)] >= 2);
    positions
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
