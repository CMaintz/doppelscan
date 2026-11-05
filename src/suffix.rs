//! Maximal repeated runs over a symbol string, via suffix array + LCP.
//!
//! Every repeated run of statements, however long, comes out once as its
//! maximal extent instead of once per sliding window, which is what keeps
//! long duplicated blocks from flooding the report with sub-windows.

use libsais::SuffixArrayConstruction;

pub struct Repeat {
    pub len: usize,
    pub positions: Vec<u32>,
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
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].len, 3);
        let mut pos = reps[0].positions.clone();
        pos.sort();
        assert_eq!(pos, vec![0, 4]);
    }
}
