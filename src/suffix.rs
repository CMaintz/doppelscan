//! Maximal repeated runs over a symbol string, via suffix array + LCP.
//!
//! Every repeated run of statements, however long, comes out once as its
//! maximal extent instead of once per sliding window, which is what keeps
//! long duplicated blocks from flooding the report with sub-windows.

use libsais::SuffixArrayConstruction;

/// A repeated run of `len` symbols. Its occurrences stay a range of the suffix
/// array because LCP intervals nest: on a periodic text n intervals average
/// n/2 occurrences, so a copy per interval is quadratic in the text.
pub struct Repeat {
    pub len: usize,
    first_rank: u32,
    last_rank: u32,
}

pub struct Repeats {
    sa: Vec<u32>,
    pub found: Vec<Repeat>,
}

impl Repeats {
    /// Start of each occurrence, in suffix order.
    pub fn positions(&self, repeat: &Repeat) -> &[u32] {
        &self.sa[repeat.first_rank as usize..=repeat.last_rank as usize]
    }

    /// Answers which occurrences share a key, for [`Repeats::all_alike`].
    pub fn alike_until<K: Eq>(&self, key: impl Fn(u32) -> K) -> Vec<u32> {
        alike_until(&self.sa, key)
    }

    /// Whether `alike_until` found one key across all of `repeat`'s
    /// occurrences, in one lookup: a walk over them for every interval is
    /// quadratic on a periodic text.
    pub fn all_alike(&self, repeat: &Repeat, alike_until: &[u32]) -> bool {
        alike_until[repeat.first_rank as usize] >= repeat.last_rank
    }
}

/// SA-IS via libsais: linear in the text whatever the repeat lengths,
/// where prefix doubling needs log2(longest repeat) full sorts and
/// generated or vendored code makes that repeat long.
pub fn suffix_array(text: &[u32]) -> Vec<u32> {
    if text.is_empty() {
        return Vec::new();
    }
    // The integer-alphabet entry point takes i32 and may permute the text
    // while it works; symbols are dense ids below the text length, so they
    // fit. Single-threaded because libsais threads only through OpenMP,
    // which would add a native runtime to every build target.
    let mut symbols: Vec<i32> = text.iter().map(|&s| s as i32).collect();
    let sa = SuffixArrayConstruction::for_text_mut(&mut symbols)
        .in_owned_buffer32()
        .single_threaded()
        .run()
        .expect("libsais rejects only negative symbols or a text over i32::MAX");
    sa.into_vec().into_iter().map(|p| p as u32).collect()
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
pub fn maximal_repeats(text: &[u32], min_len: usize) -> Repeats {
    let sa = suffix_array(text);
    let lcp = lcp_array(text, &sa);
    let same_left = alike_until(&sa, |p| (p > 0).then(|| text[p as usize - 1]));
    let left_extendable = |lb: usize, rb: usize| sa[lb] > 0 && same_left[lb] as usize >= rb;
    let mut found = Vec::new();
    for_each_lcp_interval(&lcp, |len, lb, rb| {
        if len >= min_len && !left_extendable(lb, rb) {
            found.push(Repeat {
                len,
                first_rank: lb as u32,
                last_rank: rb as u32,
            });
        }
    });
    Repeats { sa, found }
}

/// Calls `visit(lcp, left bound, right bound)` for each interval, children
/// before the interval holding them.
fn for_each_lcp_interval(lcp: &[u32], mut visit: impl FnMut(usize, usize, usize)) {
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (lcp, left bound)
    // i runs one past the end to flush the stack with a virtual lcp of 0.
    for i in 1..=lcp.len() {
        let cur = lcp.get(i).map_or(0, |&l| l as usize);
        let mut lb = i - 1;
        while let Some(&(top_lcp, top_lb)) = stack.last().filter(|top| top.0 > cur) {
            stack.pop();
            visit(top_lcp, top_lb, i - 1);
            lb = top_lb;
        }
        if stack.last().is_none_or(|&(l, _)| l < cur) {
            stack.push((cur, lb));
        }
    }
}

/// For each rank, the last rank up to which every suffix has the same `key`.
fn alike_until<K: Eq>(sa: &[u32], key: impl Fn(u32) -> K) -> Vec<u32> {
    let mut until: Vec<u32> = (0..sa.len() as u32).collect();
    for rank in (1..sa.len()).rev() {
        if key(sa[rank - 1]) == key(sa[rank]) {
            until[rank - 1] = until[rank];
        }
    }
    until
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

    // Small alphabets and periodic texts are where an induced sort goes wrong:
    // equal LMS substrings, suffixes that prefix each other, and runs of one
    // symbol all hit the bucket-boundary and recursion paths.
    #[test]
    fn suffix_array_matches_naive_sort_on_random_texts() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..2000 {
            let len = (next() % 48) as usize;
            let alphabet = 1 + (next() % 4) as u32;
            let mut text: Vec<u32> = (0..len)
                .map(|_| (next() % alphabet as u64) as u32)
                .collect();
            if case % 3 == 0 && len > 4 {
                let period = 1 + (next() as usize % 3);
                for i in period..len {
                    text[i] = text[i - period];
                }
            }
            assert_eq!(suffix_array(&text), naive_sa(&text), "text {text:?}");
        }
    }

    // Catches reporting every right-truncated window: "1 2 3" twice must be
    // one repeat of length 3, not also "2 3".
    #[test]
    fn reports_only_maximal_runs() {
        let text = [1, 2, 3, 100, 1, 2, 3, 101];
        let reps = maximal_repeats(&text, 2);
        assert_eq!(reps.found.len(), 1);
        assert_eq!(reps.found[0].len, 3);
        let mut pos = reps.positions(&reps.found[0]).to_vec();
        pos.sort();
        assert_eq!(pos, vec![0, 4]);
    }
}
