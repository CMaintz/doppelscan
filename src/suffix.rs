//! Maximal repeated runs over a symbol string, via suffix array + LCP.
//!
//! Every repeated run of statements, however long, comes out once as its
//! maximal extent instead of once per sliding window, which is what keeps
//! long duplicated blocks from flooding the report with sub-windows.

use rayon::prelude::*;

pub struct Repeat {
    pub len: usize,
    pub positions: Vec<u32>,
}

/// Prefix doubling with parallel sorts. Rounds are log2 of the longest
/// repeat, which for statement runs is small, so this beats SA-IS in
/// practice while staying simple.
pub fn suffix_array(text: &[u32]) -> Vec<u32> {
    let n = text.len();
    let mut sa: Vec<u32> = (0..n as u32).collect();
    let mut rank: Vec<u64> = text.iter().map(|&s| s as u64).collect();
    let mut next = vec![0u64; n];
    let mut k = 1;
    loop {
        let key = |i: u32| {
            let i = i as usize;
            let second = if i + k < n { rank[i + k] + 1 } else { 0 };
            (rank[i] << 32) | second
        };
        sa.par_sort_unstable_by_key(|&i| key(i));
        let distinct = rerank(&sa, &key, &mut next);
        std::mem::swap(&mut rank, &mut next);
        if distinct == n || k >= n {
            return sa;
        }
        k *= 2;
    }
}

fn rerank(sa: &[u32], key: &impl Fn(u32) -> u64, out: &mut [u64]) -> usize {
    let mut r = 0u64;
    for w in 0..sa.len() {
        if w > 0 && key(sa[w]) != key(sa[w - 1]) {
            r += 1;
        }
        out[sa[w] as usize] = r;
    }
    if sa.is_empty() { 0 } else { r as usize + 1 }
}

/// Kasai: lcp[i] is the common prefix of suffixes sa[i-1] and sa[i].
pub fn lcp_array(text: &[u32], sa: &[u32]) -> Vec<u32> {
    let n = text.len();
    let mut rank = vec![0u32; n];
    for (i, &s) in sa.iter().enumerate() {
        rank[s as usize] = i as u32;
    }
    let mut lcp = vec![0u32; n];
    let mut h = 0usize;
    for i in 0..n {
        let r = rank[i] as usize;
        if r == 0 {
            h = 0;
            continue;
        }
        let j = sa[r - 1] as usize;
        while i + h < n && j + h < n && text[i + h] == text[j + h] {
            h += 1;
        }
        lcp[r] = h as u32;
        h = h.saturating_sub(1);
    }
    lcp
}

/// Enumerates LCP intervals with lcp >= `min_len` whose occurrences are
/// left-maximal (not all preceded by the same symbol); the others are
/// suffixes of a longer repeat that is reported on its own.
pub fn maximal_repeats(text: &[u32], min_len: usize) -> Vec<Repeat> {
    let sa = suffix_array(text);
    let lcp = lcp_array(text, &sa);
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (lcp, left bound)
    // i runs one past the end to flush the stack with a virtual lcp of 0.
    #[allow(clippy::needless_range_loop)]
    for i in 1..=sa.len() {
        let cur = if i < sa.len() { lcp[i] as usize } else { 0 };
        let mut lb = i - 1;
        while let Some(&(top_lcp, top_lb)) = stack.last() {
            if top_lcp <= cur {
                break;
            }
            stack.pop();
            emit(text, &sa, top_lcp, top_lb, i - 1, min_len, &mut out);
            lb = top_lb;
        }
        if stack.last().is_none_or(|&(l, _)| l < cur) {
            stack.push((cur, lb));
        }
    }
    out
}

fn emit(
    text: &[u32],
    sa: &[u32],
    len: usize,
    lb: usize,
    rb: usize,
    min_len: usize,
    out: &mut Vec<Repeat>,
) {
    if len < min_len {
        return;
    }
    let positions = &sa[lb..=rb];
    let preceding = |p: u32| (p > 0).then(|| text[p as usize - 1]);
    let first = preceding(positions[0]);
    let left_extendable = first.is_some() && positions.iter().all(|&p| preceding(p) == first);
    if !left_extendable {
        out.push(Repeat {
            len,
            positions: positions.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_sa(text: &[u32]) -> Vec<u32> {
        let mut sa: Vec<u32> = (0..text.len() as u32).collect();
        sa.sort_by(|&a, &b| text[a as usize..].cmp(&text[b as usize..]));
        sa
    }

    // Catches a wrong doubling key (rank+1 vs 0 sentinel) that mis-sorts
    // suffixes which are prefixes of each other.
    #[test]
    fn suffix_array_matches_naive_sort() {
        let text = [3, 1, 2, 1, 2, 1, 2, 3, 1, 2, 9, 1, 2, 1];
        assert_eq!(suffix_array(&text), naive_sa(&text));
    }

    // Catches reporting every right-truncated window: "1 2 3" twice must be
    // one repeat of length 3, not also "2 3".
    #[test]
    fn reports_only_maximal_runs() {
        let text = [1, 2, 3, 100, 1, 2, 3, 101];
        let reps = maximal_repeats(&text, 2);
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].len, 3);
        let mut pos = reps[0].positions.clone();
        pos.sort();
        assert_eq!(pos, vec![0, 4]);
    }
}
