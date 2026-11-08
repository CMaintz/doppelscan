# Benchmark

doppelscan, [dupehound](https://crates.io/crates/dupehound) and
[jscpd](https://github.com/kucherenko/jscpd) on large repositories, run five
times as doppelscan changed: Elasticsearch in every run, Apache Camel from run
5. The latest run comes first; every
earlier result is kept below it.

## Setup

| | |
| --- | --- |
| Machine | AMD Ryzen 7 3800X (8 cores, 16 threads), 32 GB RAM, Windows 10 Pro |
| Repositories | Elasticsearch at `6aae768f2d1d`: 51,122 tracked files, 33,386 of them Java. Apache Camel at `91e44cf13c8c` from run 5 |
| Planted clones | 80, added in one commit, in code no tool had flagged before |
| Timing | Wall time and peak working set, median reported. Runs 1-4: 3 runs on the unplanted repo unless noted. Run 5: 15 unplanted and 9 planted runs per config |
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

## Run 5, 2026-11-08: two repositories, three passes

doppelscan at `5b5d42f`, after the audit fixes, on Elasticsearch and Apache
Camel at `91e44cf13c8c` (45,124 tracked files, 29,926 of them Java). Camel's
80 plants follow the same recipe, copied across Maven modules; its lexical
plants are 5 shell functions and 5 YAML blocks, since no Groovy file gave a
usable block.

Each tool ran alone on an otherwise idle machine, in three full passes hours
apart: 5 runs per config on the unplanted tree and 3 on the planted one per
pass. Every run was tracked in a Windows job object, so CPU, I/O and peak
commit cover the whole process tree, with commit capped at 22 GB. jscpd ran
first in each pass, then dupehound, then doppelscan. Camel's first-pass jscpd
and dupehound runs were taken a day earlier with the same binaries. No run
failed, timed out or hit the cap. Every tool gave the same output on every
run, timing fields aside; the hashes of Camel's first-pass jscpd and
dupehound runs still included those fields, so they cannot be compared.

"Peak RAM" is the peak working set, "peak commit" the most memory the job
had reserved. The `check` rows run on the planted tree against a baseline
written from the unplanted one, so they exit 1 by design.

### Speed and resources

#### Camel

| Repo state | Config | Runs | Median wall | Range | CPU s | Cores used | Peak RAM MB | Peak commit MB | Read MB | Written MB | Page faults |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| unplanted | jscpd default | 15 | 184.89 s | 163.19-193.68 | 276.5 | 1.5 | 13554 | 15093 | 182 | 341 | 5835009 |
| unplanted | jscpd type-2/3 | 15 | 304.43 s | 283.97-328.37 | 397.3 | 1.3 | 15510 | 17862 | 193 | 841 | 7516311 |
| unplanted | dupehound | 15 | 9.02 s | 8.96-9.49 | 65.5 | 7.2 | 114 | 108 | 189 | 9 | 63140 |
| unplanted | doppelscan exact | 15 | 5.81 s | 5.74-6.00 | 60.3 | 10.3 | 958 | 1398 | 146 | 59 | 326009 |
| unplanted | doppelscan near | 15 | 5.54 s | 5.50-5.58 | 70.1 | 12.7 | 880 | 1288 | 146 | 4 | 302649 |
| unplanted | doppelscan yaml | 15 | 0.35 s | 0.33-0.42 | 3.6 | 10.4 | 29 | 133 | 1 | 0 | 7088 |
| unplanted | doppelscan baseline | 15 | 5.48 s | 5.44-5.55 | 59.8 | 10.9 | 992 | 1413 | 146 | 18 | 332501 |
| planted | jscpd default | 9 | 182.16 s | 173.00-196.41 | 273.4 | 1.5 | 13554 | 15104 | 182 | 341 | 5835115 |
| planted | jscpd type-2/3 | 9 | 305.75 s | 289.56-312.85 | 394.8 | 1.3 | 15533 | 17873 | 194 | 841 | 7512971 |
| planted | dupehound | 9 | 9.02 s | 9.00-9.18 | 65.8 | 7.3 | 100 | 105 | 189 | 9 | 62417 |
| planted | doppelscan exact | 9 | 5.81 s | 5.79-5.85 | 60.5 | 10.4 | 962 | 1388 | 147 | 59 | 327000 |
| planted | doppelscan near | 9 | 5.55 s | 5.47-5.61 | 70.0 | 12.6 | 879 | 1292 | 147 | 4 | 296693 |
| planted | doppelscan yaml | 9 | 0.35 s | 0.34-0.36 | 3.7 | 10.5 | 28 | 131 | 1 | 0 | 6840 |
| planted | doppelscan check | 9 | 5.70 s | 5.66-5.74 | 60.1 | 10.5 | 998 | 1446 | 183 | 0 | 340919 |
| planted | doppelscan check --base main | 9 | 14.63 s | 14.37-15.50 | 116.7 | 8.0 | 1053 | 1452 | 299 | 152 | 685042 |

| Config | Groups | Occurrences | Files | Lines covered |
| --- | --- | --- | --- | --- |
| doppelscan exact | 23,321 | 81,929 | 15,216 | 836,011 |
| doppelscan near | 3,045 | 11,146 | 5,243 | 206,756 |
| doppelscan yaml | 95 | 256 | 75 | 4,548 |
| dupehound | 7,681 | 22,724 | 8,908 | 390,521 |
| jscpd default | 225,864 | 451,728 | 35,943 | 2,682,956 |
| jscpd type-2/3 | 590,166 | 1,180,332 | 36,528 | 3,845,155 |

#### Elasticsearch

| Repo state | Config | Runs | Median wall | Range | CPU s | Cores used | Peak RAM MB | Peak commit MB | Read MB | Written MB | Page faults |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| unplanted | jscpd default | 15 | 220.25 s | 195.83-238.49 | 331.1 | 1.5 | 15115 | 16912 | 296 | 321 | 6195917 |
| unplanted | jscpd type-2/3 | 15 | 396.95 s | 373.56-412.44 | 514.7 | 1.3 | 18362 | 21028 | 318 | 957 | 8485719 |
| unplanted | dupehound | 15 | 13.88 s | 13.36-14.77 | 144.5 | 10.4 | 216 | 213 | 311 | 14 | 118868 |
| unplanted | doppelscan exact | 15 | 12.02 s | 11.78-12.26 | 126.5 | 10.5 | 2066 | 2776 | 306 | 86 | 721783 |
| unplanted | doppelscan near | 15 | 19.37 s | 19.31-19.82 | 261.7 | 13.5 | 1851 | 2514 | 306 | 8 | 664136 |
| unplanted | doppelscan yaml | 15 | 0.75 s | 0.72-0.80 | 5.1 | 6.8 | 137 | 431 | 14 | 10 | 35008 |
| unplanted | doppelscan baseline | 15 | 11.56 s | 11.48-12.12 | 125.9 | 10.9 | 2067 | 2791 | 306 | 36 | 765459 |
| planted | jscpd default | 9 | 228.37 s | 195.13-247.98 | 337.1 | 1.5 | 15110 | 16908 | 297 | 321 | 6198359 |
| planted | jscpd type-2/3 | 9 | 393.54 s | 375.26-411.40 | 510.5 | 1.3 | 18351 | 21013 | 319 | 957 | 8488006 |
| planted | dupehound | 9 | 13.71 s | 13.13-14.18 | 143.6 | 10.6 | 217 | 213 | 311 | 15 | 114999 |
| planted | doppelscan exact | 9 | 11.97 s | 11.84-12.03 | 126.6 | 10.6 | 2068 | 2775 | 306 | 86 | 720029 |
| planted | doppelscan near | 9 | 19.38 s | 19.28-19.50 | 261.9 | 13.5 | 1867 | 2538 | 306 | 8 | 669151 |
| planted | doppelscan yaml | 9 | 0.76 s | 0.73-0.78 | 5.2 | 6.8 | 138 | 432 | 14 | 10 | 35133 |
| planted | doppelscan check | 9 | 11.93 s | 11.89-12.05 | 126.2 | 10.6 | 2113 | 2804 | 379 | 0 | 759264 |
| planted | doppelscan check --base main | 9 | 27.75 s | 27.59-28.79 | 245.6 | 8.9 | 2168 | 2865 | 630 | 324 | 1393891 |

| Config | Groups | Occurrences | Files | Lines covered |
| --- | --- | --- | --- | --- |
| doppelscan exact | 53,603 | 151,653 | 18,478 | 1,371,533 |
| doppelscan near | 6,086 | 21,553 | 8,179 | 406,591 |
| doppelscan yaml | 6,304 | 18,463 | 1,658 | 252,389 |
| dupehound | 11,512 | 36,544 | 10,954 | 721,275 |
| jscpd default | 235,094 | 470,188 | 37,095 | 2,472,511 |
| jscpd type-2/3 | 703,346 | 1,406,692 | 39,741 | 4,698,009 |

### Recall, any overlap

#### Camel

| Kind | n | doppelscan exact | doppelscan near | doppelscan yaml | doppelscan all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| exact | 10 | 10 | 0 | 0 | 10 | 10 | 10 | 10 |
| renamed | 10 | 10 | 0 | 0 | 10 | 10 | 0 | 9 |
| near-add | 10 | 10 | 8 | 0 | 10 | 1 | 10 | 10 |
| near-remove | 10 | 10 | 10 | 0 | 10 | 9 | 10 | 10 |
| near-change | 10 | 10 | 7 | 0 | 10 | 6 | 10 | 10 |
| block | 10 | 10 | 0 | 0 | 10 | 1 | 10 | 9 |
| cross-module | 10 | 10 | 0 | 0 | 10 | 9 | 5 | 9 |
| lexical | 10 | 5 | 0 | 5 | 10 | 0 | 10 | 10 |
| **total** | **80** | **75** | **25** | **5** | **80** | **46** | **65** | **77** |

#### Elasticsearch

| Kind | n | doppelscan exact | doppelscan near | doppelscan yaml | doppelscan all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| exact | 10 | 10 | 0 | 0 | 10 | 10 | 10 | 10 |
| renamed | 10 | 10 | 0 | 0 | 10 | 10 | 0 | 10 |
| near-add | 10 | 10 | 5 | 0 | 10 | 2 | 10 | 10 |
| near-remove | 10 | 10 | 10 | 0 | 10 | 8 | 10 | 10 |
| near-change | 10 | 10 | 5 | 0 | 10 | 6 | 10 | 10 |
| block | 10 | 10 | 0 | 0 | 10 | 0 | 10 | 10 |
| cross-module | 10 | 10 | 0 | 0 | 10 | 10 | 6 | 10 |
| lexical | 10 | 7 | 0 | 2 | 9 | 0 | 9 | 9 |
| **total** | **80** | **77** | **20** | **2** | **79** | **46** | **65** | **79** |

### Recall, 80% coverage

#### Camel

| Kind | n | doppelscan exact | doppelscan near | doppelscan yaml | doppelscan all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| exact | 10 | 10 | 0 | 0 | 10 | 10 | 10 | 10 |
| renamed | 10 | 10 | 0 | 0 | 10 | 10 | 0 | 8 |
| near-add | 10 | 1 | 8 | 0 | 8 | 1 | 4 | 8 |
| near-remove | 10 | 5 | 10 | 0 | 10 | 9 | 5 | 10 |
| near-change | 10 | 4 | 7 | 0 | 8 | 6 | 5 | 7 |
| block | 10 | 10 | 0 | 0 | 10 | 1 | 10 | 9 |
| cross-module | 10 | 10 | 0 | 0 | 10 | 9 | 5 | 9 |
| lexical | 10 | 5 | 0 | 5 | 10 | 0 | 10 | 10 |
| **total** | **80** | **55** | **25** | **5** | **76** | **46** | **49** | **71** |

#### Elasticsearch

| Kind | n | doppelscan exact | doppelscan near | doppelscan yaml | doppelscan all | dupehound | jscpd default | jscpd type-2/3 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| exact | 10 | 10 | 0 | 0 | 10 | 10 | 10 | 10 |
| renamed | 10 | 10 | 0 | 0 | 10 | 10 | 0 | 9 |
| near-add | 10 | 0 | 5 | 0 | 5 | 2 | 3 | 8 |
| near-remove | 10 | 4 | 10 | 0 | 10 | 8 | 5 | 8 |
| near-change | 10 | 4 | 5 | 0 | 6 | 6 | 6 | 10 |
| block | 10 | 10 | 0 | 0 | 10 | 0 | 10 | 10 |
| cross-module | 10 | 10 | 0 | 0 | 10 | 10 | 5 | 10 |
| lexical | 10 | 7 | 0 | 2 | 9 | 0 | 9 | 9 |
| **total** | **80** | **55** | **20** | **2** | **70** | **46** | **48** | **74** |

### Near at similarity 0.7

All of doppelscan's misses at 80% coverage except Elasticsearch's lexical-09
are edited copies whose near score falls between 0.70 and 0.79. The same
recall run with `near --similarity 0.7`:

| | Camel | Elasticsearch |
| --- | --- | --- |
| doppelscan all at 80% coverage, near 0.8 | 76 | 70 |
| doppelscan all at 80% coverage, near 0.7 | **80** | **79** |
| near groups at 0.8 | 3,045 | 6,086 |
| groups at 0.7 sharing no occurrence with a 0.8 group | 2,002 | 4,346 |
| near time at 0.7, one run | 23 s | 22 s |

### Precision

20 random groups per tool and repository, plus 20 of the groups only 0.7
reports, shuffled together under opaque ids and labelled by reviewers who
were not told which tool reported what. E is real duplication worth
extracting, B real but not worth it (boilerplate, tests repetitive by
design, per-type overloads), W not meaningful duplication.

| Source | Camel E / B / W | Elasticsearch E / B / W | Both, E | Both, W |
| --- | --- | --- | --- | --- |
| doppelscan exact | 8 / 10 / 2 | 7 / 13 / 0 | 15 of 40 | 2 of 40 |
| doppelscan near 0.8 | 5 / 13 / 2 | 8 / 12 / 0 | 13 of 40 | 2 of 40 |
| doppelscan near, added by 0.7 | 4 / 14 / 2 | 8 / 11 / 1 | 12 of 40 | 3 of 40 |
| dupehound | 7 / 10 / 3 | 8 / 11 / 1 | 15 of 40 | 4 of 40 |
| jscpd default | 0 / 5 / 15 | 2 / 8 / 10 | 2 of 40 | 25 of 40 |
| jscpd type-2/3 | 0 / 2 / 18 | 0 / 3 / 17 | 0 of 40 | 35 of 40 |

Nearly all of jscpd's wrong groups are license headers, package lines and
import lists; it has no notion of where a member starts. 87 of the 112 B
labels across all tools are tests.

### Verdict

doppelscan catches the most plants on Camel (76 against jscpd type-2/3's 71)
and is 4 behind it on Elasticsearch (70 against 74), in 12 and 32 seconds
against 5 and 6.5 minutes, with a tenth of the memory or less and almost none
of the noise. dupehound stays the lightest and is as precise, but finds 46 plants on
each, as it only compares whole functions. The gap to jscpd is near's 0.8
threshold: at 0.7 doppelscan catches 80 and 79, and the extra groups were as
often worth extracting as those at 0.8 in this small sample, at the cost of
about 70% more near groups to read.

`check` takes about as long as a scan: 5.7 s on Camel and 11.9 s on
Elasticsearch, and 15 s and 28 s with `--base main`.

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

Two repositories, both mostly Java, on one machine; runs 1-4 had other work
running, run 5 did not. The plants and the precision labels come from the
doppelscan side. Each label is one reviewer's call with no second opinion,
and 20 groups per source is a small sample: a difference of a few labels is
noise. The scripts and raw output are not in this repository.
