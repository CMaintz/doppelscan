//! Inline markers that take code out of matching, for duplication a team
//! has decided to keep. They are found by text, not by parsing comments,
//! so they work the same in every language, grammar or not.

const START: &str = "doppelscan-ignore-start";
const END: &str = "doppelscan-ignore-end";
const FILE: &str = "doppelscan-ignore-file";

/// Byte ranges to leave out, sorted and disjoint: from the start of a line
/// holding a start marker to the end of the next line holding an end
/// marker, or to the end of the file when none follows. A file marker on
/// any line covers the whole file.
pub fn ignored_spans(source: &str, line_starts: &[u32]) -> Vec<(u32, u32)> {
    if !source.contains("doppelscan-ignore") {
        return Vec::new();
    }
    let len = source.len() as u32;
    let mut spans = Vec::new();
    let mut open = None;
    for (i, &start) in line_starts.iter().enumerate() {
        let end = line_starts.get(i + 1).copied().unwrap_or(len);
        let line = &source[start as usize..end as usize];
        if line.contains(FILE) {
            return vec![(0, len)];
        }
        if open.is_none() && line.contains(START) {
            open = Some(start);
        }
        // Checked after the start, so a start and end on one line close at once.
        if let Some(from) = open
            && line.contains(END)
        {
            spans.push((from, end));
            open = None;
        }
    }
    spans.extend(open.map(|from| (from, len)));
    spans
}

/// Whether `[start, end)` shares a byte with any of `spans`.
pub fn overlaps(spans: &[(u32, u32)], start: u32, end: u32) -> bool {
    let first_after = spans.partition_point(|&(_, e)| e <= start);
    spans.get(first_after).is_some_and(|&(s, _)| s < end)
}

#[cfg(test)]
mod tests {
    use super::{ignored_spans, overlaps};
    use crate::index::line_starts;

    fn spans(source: &str) -> Vec<(u32, u32)> {
        ignored_spans(source, &line_starts(source))
    }

    // Catches checking for an end only on later lines: a start and end on
    // one line would leave the rest of the file ignored.
    #[test]
    fn a_start_and_end_on_one_line_close_there() {
        let source = "a\n/* doppelscan-ignore-start */ b /* doppelscan-ignore-end */\nc\n";
        assert_eq!(spans(source), [(2, 62)]);
    }

    // Catches an inclusive end: a fragment that starts where an ignored
    // span ends shares no byte with it.
    #[test]
    fn touching_a_span_is_not_overlapping_it() {
        let spans = [(10, 20), (30, 40)];
        assert!(!overlaps(&spans, 20, 30));
        assert!(!overlaps(&spans, 0, 10));
        assert!(overlaps(&spans, 19, 21));
        assert!(overlaps(&spans, 0, 50));
    }
}
