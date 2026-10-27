# Contributing to doppelscan

Thanks for helping. The general rules for all of my projects (commits, pull
requests, tests, reviews) are in the
[shared contributing guide](https://github.com/CMaintz/.github/blob/main/CONTRIBUTING.md).
This file covers what is specific to doppelscan.

## Setup

```sh
git clone https://github.com/CMaintz/doppelscan && cd doppelscan
mise install          # pinned Rust toolchain, osv-scanner, gitleaks
mise run gate         # lint, typecheck, test, audit: what CI runs
cargo run -- scan .   # try it on itself
```

Without [mise](https://mise.jdx.dev), stable Rust 1.91 or newer works for
building and testing. The grammars are C code, so the first build takes a
minute and needs a C compiler (the MSVC build tools on Windows).

`mise run fix` applies formatting and clippy's mechanical fixes.

## What makes a good change

- **Matching behaviour needs a test that would catch the bug.** Tests live in
  `tests/detection.rs` and build small fixtures in a temp directory. Add the
  positive case (the clone is found, with the right parameters) and, for
  anything that could over-match, the negative one. False-positive tests are
  the most valuable ones.
- **Check that your test can fail.** Break the code it covers and watch it go
  red before you keep it.
- **Keep it fast.** Scanning is meant to stay well under a second on a
  mid-sized repo. If a change touches the hot path (`index`, `detect`,
  `suffix`), compare `time doppelscan <some repo> --limit 0` before and after
  and put the numbers in the PR.
- **Keep it local and deterministic.** No network calls, no telemetry, the
  same output for the same input.
- **Scripts read `--format json`**, not the text output. The text format may
  change freely; JSON fields are only added, never renamed or removed, without
  a major version bump.

## Adding a language

See [docs/adding-a-language.md](docs/adding-a-language.md).

## Reporting a false positive or a missed clone

Open an issue with a small, self-contained pair of snippets, the command you
ran and what doppelscan printed. These turn directly into regression tests.
