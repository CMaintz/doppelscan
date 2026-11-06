# Benchmark

doppelscan, [dupehound](https://crates.io/crates/dupehound) and
[jscpd](https://github.com/kucherenko/jscpd) on the same large repository,
run four times as doppelscan changed. The latest run comes first; every
earlier result is kept below it.

## Setup

| | |
| --- | --- |
| Machine | AMD Ryzen 7 3800X (8 cores, 16 threads), 32 GB RAM, Windows 10 Pro |
| Repository | Elasticsearch at `6aae768f2d1d`: 51,122 tracked files, 33,386 of them Java |
| Planted clones | 80, added in one commit, in code no tool had flagged before |
| Timing | Wall time and peak working set of the process. 3 runs on the unplanted repo, median reported, unless noted |
| Minimum size | 50 tokens wherever a tool has one |

The tools were run as:

| Config | Command |
| --- | --- |
| doppelscan exact | `doppelscan R --min-tokens 50 --format json --limit 0` |
| doppelscan near | `doppelscan near R --min-tokens 50 --similarity 0.8 --format json --limit 0` |
| doppelscan yaml | `doppelscan R --only yaml --min-tokens 50 --format json --limit 0` |
| dupehound 0.1.2 | `dupehound scan R --min-tokens 50 --json --all` |
| jscpd 5.3.0 default | `jscpd R -k 50 -r json -o DIR -s --no-tips` |
| jscpd 5.3.0 type-2/3 | the default plus `--ignore-identifiers --ignore-literals --max-gap-lines 2` |

"doppelscan all" is the union of its exact, near and yaml runs.

Each planted clone copies existing code, at least 100 tokens long:

| Kind | How the copy differs | n |
| --- | --- | --- |
| exact | verbatim method, same module | 10 |
| renamed | 3 to 9 parameters or locals renamed, up to 2 literals changed | 10 |
| near-add | 1 or 2 statements inserted mid-body | 10 |
| near-remove | one statement deleted | 10 |
| near-change | one statement replaced | 10 |
| block | 6 consecutive statements put at the top of an unrelated method | 10 |
| cross-module | 5 exact and 5 renamed, copied into another Gradle module | 10 |
| lexical | Gradle blocks, shell functions and YAML blocks, appended to a file of the same type | 10 |

A plant counts as caught when one group has an occurrence overlapping the
source and another overlapping the copy. That is generous to window-based
finders: a near plant counts when only its untouched half is reported. So
each recall table has a stricter partner where the occurrences must cover
at least 80% of both ranges.

## Run 4, 2026-11-06: after the audit

doppelscan at `d416925` ("now"), against the builds from runs 1 ("old") and
2 ("mid"), measured together in one sitting.

### Speed

| Tool | Runs (s) | Median | Peak memory | Output |
| --- | --- | --- | --- | --- |
| doppelscan now exact | 11.8, 11.8, 11.6 | **11.8 s** | 2.0 GB | 53,030 groups |
| doppelscan now near | 21.1, 21.0, 21.8 | **21.1 s** | 1.9 GB | 6,104 groups |
| doppelscan now yaml | 2.5 | 2.5 s | 0.1 GB | |
| doppelscan mid exact | 19.2, 17.9, 17.9 | 17.9 s | 3.0 GB | 54,662 groups |
| doppelscan mid near | 72.9 | 72.9 s | 9.4 GB | 6,381 groups (run 2) |
| doppelscan old exact | 19.5, 19.3, 18.6 | 19.3 s | 3.1 GB | 61,902 groups |
| doppelscan old near | 164.1, 189.9, 190.9 | 189.9 s | 11.8 GB | 500 groups |
| dupehound | 37.1, 15.6, 16.6 | **16.6 s** | 0.2 GB | 11,512 clusters |
| jscpd default | 508.0, 497.6, 443.5 | **497.6 s** | 14.4 GB | |
| jscpd type-2/3 | 687.8 | **687.8 s** | 15.9 GB | 1 GB of JSON |

The machine was not idle: other sessions held about 15 GB, and two of the
three "mid near" runs failed at startup while memory was short, so that row
has one run. The first dupehound run followed a run that pushed the checkout out
of the file cache, which is why it is slower than the next two.

Group counts fall partly because fewer files are scanned: 37,153 in "old",
34,249 in "mid" and 32,769 in "now", which skips generated-source directories
by default. The rest of the drop was not investigated.

### Recall, any overlap

| Kind | old all | mid all | now exact | now near | now all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| exact | 10 | 10 | 10 | 0 | 10 | 10 | 10 | 10 |
| renamed | 10 | 10 | 10 | 0 | 10 | 10 | 0 | 10 |
| near-add | 10 | 10 | 10 | 5 | 10 | 2 | 10 | 10 |
| near-remove | 10 | 10 | 10 | 10 | 10 | 8 | 10 | 10 |
| near-change | 10 | 10 | 10 | 5 | 10 | 6 | 10 | 10 |
| block | 10 | 10 | 10 | 0 | 10 | 0 | 10 | 10 |
| cross-module | 10 | 10 | 10 | 0 | 10 | 10 | 6 | 10 |
| lexical | 4 | 9 | 7 | 0 | 9 | 0 | 9 | 9 |
| **total** | 74 | 79 | 77 | 20 | **79** | 46 | 65 | **79** |

No tool caught lexical-09: a comment-heavy YAML block that falls under 50
tokens once comments are removed, so it is a weak plant.

"now near" sits low by design here: exact clones are left to the exact scan,
so near only reports what is genuinely near.

### Recall, 80% coverage

| Kind | old all | mid all | now exact | now near | now all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| exact | 9 | 10 | 10 | 0 | 10 | 10 | 10 | 10 |
| renamed | 10 | 10 | 10 | 0 | 10 | 10 | 0 | 9 |
| near-add | 1 | 5 | 0 | 5 | 5 | 2 | 3 | 8 |
| near-remove | 4 | 10 | 4 | 10 | 10 | 8 | 5 | 8 |
| near-change | 4 | 6 | 4 | 5 | 6 | 6 | 6 | 10 |
| block | 10 | 10 | 10 | 0 | 10 | 0 | 10 | 10 |
| cross-module | 10 | 10 | 10 | 0 | 10 | 10 | 5 | 10 |
| lexical | 4 | 9 | 7 | 0 | 9 | 0 | 9 | 9 |
| **total** | 52 | 70 | 55 | 20 | **70** | 46 | 48 | **74** |

doppelscan's remaining near misses are 5 near-add and 4 near-change plants:
one inserted or replaced statement changes enough fingerprints to push a short
method under 0.8.

### Verdict

Recall did not move from run 2, but the time and memory did. A full doppelscan
pass takes about 35 s and 2 GB, against nearly 4 minutes and 12 GB when this
benchmark started. It ties jscpd type-2/3 on any-overlap recall in a twentieth
of the time, and is 4 plants behind at 80% coverage. dupehound is still the
lightest by far, but sees only whole functions in languages it supports.

Precision was not resampled on this build; the latest sample is run 2's.

After this run, near stopped pairing a lambda with the function around it
(6,104 groups to 6,086 on the same tree). All 64 spans it reported in planted
files are still reported, and near's time is unchanged.

## Run 3, 2026-11-02: memory and speed work

Exact scans on unplanted Elasticsearch while the indexing changes landed.
JSON output was byte-identical to `main` apart from `elapsed_ms`, which was the
bar for these changes.

| Build | Runs (s) | Peak memory |
| --- | --- | --- |
| `main` before | 23.2, 27.2, 26.3 | 2.9 GB |
| compact index, step 1 | 25.1, 24.4, 27.9 | 2.8 GB |
| compact index, step 2 | 28.3, 26.6, 23.8 | 2.5 GB |
| compact index, step 3 | 30.8, 25.3, 29.5 | 2.2 GB |
| compact index, step 4 | 26.7, 28.2, 25.0 | 2.2 GB |
| compact index, step 5 | 24.0, 22.6, 23.7 | 2.1 GB |
| SA-IS suffix array, before | 29.5, 27.7, 23.9 | 2.1 GB |
| SA-IS suffix array, after | 19.9, 23.5, 17.5 | 2.1 GB |

Near scoring was also reworked to score pairs as they are found instead of
holding them all, which took a near scan from 257 s and 10 GB to about 23 to
39 s and 2.5 GB, then to 1.9 GB after the same memory work.

## Run 2, 2026-11-01: near scoring and dot-directories

"new" is `main` plus two fixes: near scoring counts common fingerprints on both
sides of the score, and dot-directories are scanned. jscpd was rerun on `main`
only, in default mode: its other runs were stopped twice when memory ran out,
so its recall columns below are run 1's.

| Tool | Median wall | Peak memory | Groups |
| --- | --- | --- | --- |
| doppelscan old exact | 47.1 s | 3.1 GB | 61,902 |
| doppelscan new exact | 22.7 s | 2.9 GB | 54,662 |
| doppelscan old near | 247.5 s | 12.6 GB | 500 |
| doppelscan new near | 94.4 s | 9.4 GB | 6,381 |
| dupehound | 16.9 s | 0.2 GB | |
| jscpd default | 581.1 s | 15.1 GB | |

The machine was short on memory, so the old build's times are noisier than in
run 1, where its exact scan took 21 s.

Recall, any overlap:

| Kind | old all | new exact | new near | new all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| exact | 10 | 10 | 3 | 10 | 10 | 10 | 10 |
| renamed | 10 | 10 | 0 | 10 | 10 | 0 | 10 |
| near-add | 10 | 10 | 5 | 10 | 2 | 10 | 10 |
| near-remove | 10 | 10 | 10 | 10 | 8 | 10 | 10 |
| near-change | 10 | 10 | 5 | 10 | 6 | 10 | 10 |
| block | 10 | 10 | 0 | 10 | 0 | 10 | 10 |
| cross-module | 10 | 10 | 4 | 10 | 10 | 6 | 10 |
| lexical | 4 | 7 | 0 | 9 | 0 | 9 | 9 |
| **total** | 74 | 77 | 27 | **79** | 46 | 65 | **79** |

Recall, 80% coverage:

| Kind | old all | new all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- |
| exact | 9 | 10 | 10 | 10 | 10 |
| renamed | 10 | 10 | 10 | 0 | 9 |
| near-add | 1 | 5 | 2 | 3 | 8 |
| near-remove | 4 | 10 | 8 | 5 | 8 |
| near-change | 4 | 6 | 6 | 6 | 10 |
| block | 10 | 10 | 0 | 10 | 10 |
| cross-module | 10 | 10 | 10 | 5 | 10 |
| lexical | 4 | 9 | 0 | 9 | 9 |
| **total** | 52 | **70** | 46 | 48 | **74** |

Precision of near, 20 random groups labelled by reading the code (labels as in
run 1):

| Build | Extractable | Real, not worth extracting | Wrong | Not wrong |
| --- | --- | --- | --- | --- |
| old near | 4 | 14 | 2 | 90% |
| new near | 6 | 14 | 0 | 100% |

Near-miss scoring went from 1 of 30 plants to 20 of 30 with no wrong hits in
the sample.

## Run 1, 2026-10-28: first comparison

Speed on unplanted `main`:

| Tool | Runs (s) | Median | Peak memory | Output |
| --- | --- | --- | --- | --- |
| doppelscan exact | 62.0, 21.2, 21.3 | 21.3 s | 3.1 GB | 61,902 groups, 106 MB JSON |
| doppelscan near | 196.0, 244.3, 196.9 | 196.9 s | 8.5 to 11.7 GB | 500 groups |
| doppelscan yaml | 3.5 | 3.5 s | 0.2 GB | |
| dupehound | 38.6, 14.9, 16.8 | 16.8 s | 0.2 GB | 11,512 clusters |
| jscpd default | 672.1, 560.1, 506.9 | 560.1 s | 9.2 to 11.7 GB | 235,094 pairs, 336 MB JSON |
| jscpd type-2/3 | 774.9 | 774.9 s | 12.6 GB | 703,346 pairs |

Recall, any overlap:

| Kind | doppelscan exact | doppelscan near | doppelscan all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- | --- |
| exact | 10 | 0 | 10 | 10 | 10 | 10 |
| renamed | 10 | 0 | 10 | 10 | 0 | 10 |
| near-add | 10 | 1 | 10 | 2 | 10 | 10 |
| near-remove | 10 | 0 | 10 | 8 | 10 | 10 |
| near-change | 10 | 0 | 10 | 6 | 10 | 10 |
| block | 10 | 0 | 10 | 0 | 10 | 10 |
| cross-module | 10 | 0 | 10 | 10 | 6 | 10 |
| lexical | 4 | 0 | 4 | 0 | 9 | 9 |
| **total** | **74** | 1 | **74** | 46 | 65 | **79** |

Recall, 80% coverage:

| Kind | doppelscan all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- |
| exact | 9 | 10 | 10 | 10 |
| renamed | 10 | 10 | 0 | 9 |
| near-add | 1 | 2 | 3 | 8 |
| near-remove | 4 | 8 | 5 | 8 |
| near-change | 4 | 6 | 6 | 10 |
| block | 10 | 0 | 10 | 10 |
| cross-module | 10 | 10 | 5 | 10 |
| lexical | 4 | 0 | 9 | 9 |
| **total** | 52 | 46 | 48 | **74** |

Precision, 20 random groups per tool on unplanted `main`, one labeller reading
the code:

| Tool | Extractable | Real, not worth extracting | Wrong | Not wrong |
| --- | --- | --- | --- | --- |
| doppelscan exact | 7 | 13 | 0 | 100% |
| doppelscan near | 4 | 14 | 2 | 90% |
| dupehound | 4 | 16 | 0 | 100% |
| jscpd default | 3 | 2 | 15 | 25% |

jscpd's wrong hits were 13 license-header, `package` or `import` blocks and 2
generated visitor stubs. "Real, not worth extracting" covers equals/hashCode,
per-type overloads, generated sources and tests that repeat by design.

Findings that drove later runs: near found 1 of 30 near-miss plants because
common fingerprints diluted every score on a repository this size, and the
exact scan skipped dot-directories, which hid 5 lexical plants.

## Limits of this benchmark

It is one repository, mostly Java, on one machine with other work running. The
plants and the precision labels come from the doppelscan side, and the labels
had no second opinion. The scripts and raw output are not in
this repository.
