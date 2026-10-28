# doppelscan

[![ci](https://github.com/CMaintz/doppelscan/actions/workflows/ci.yml/badge.svg)](https://github.com/CMaintz/doppelscan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Fast, syntax-aware duplicate code finder built for coding agents.

doppelscan parses your code with tree-sitter (or tokenizes it, for languages
without a bundled grammar) and finds fragments that have the same structure, even when names and literals differ, in the spirit of
IntelliJ's duplicate detection. Each clone group comes with what an agent needs
to extract a shared helper:

- **parameters**: values that differ between occurrences and would become
  helper arguments, labelled by what the helper takes: `type` (a generic),
  `function` (a callback), `member` (a key or accessor), `name` (in a
  token-level language, a name that may be a parameter or a rename), or
  unlabelled for a plain value or literal (`kind` in the JSON output),
- **renames**: locals the fragment binds itself (variables, its own
  parameters, loop and lambda variables), which differ but need no argument,
- **declares**: names the fragment declares for other code (methods, fields,
  classes), so callers have to switch to the one name the helper keeps,
- **template**: the first occurrence with each parameter replaced by `$1`,
  `$2`, ..., i.e. the helper body.

It runs as a CLI and as an MCP server, so any Claude or other LLM session can
call it as a tool.

```
#1 [b4a94232d76e] x9 | 71 tokens, 3 lines, run of 3 | score 568
  src/app/resume-state.service.ts:303-305  (ResumeStateService.addExperience)
  src/app/resume-state.service.ts:317-319  (ResumeStateService.addEducation)
  ...
  params: $1:type=ResumeExperience | ResumeEducation | ...;  $2='experience' | 'education' | ...
  declares: addExperience | addEducation | ...;  updateExperience | updateEducation | ...
  template:
    addExperience(item: Omit<$1, 'id'>): void { this.addTo($2, item); }
    updateExperience(id: string, patch: Partial<$1>): void { this.updateIn($2, id, patch); }
    removeExperience(id: string): void { this.removeFrom($2, id); }
```

## Languages

TypeScript, TSX, JavaScript, Java, C#, Kotlin, Swift, PHP, Python, Go, Rust, CSS, HTML
(including Angular and other framework templates, matched as markup).

Without a bundled grammar, these are matched token by token: C, C++,
Objective-C, Scala, Groovy and Gradle, Dart, Zig, Solidity, Protobuf,
SCSS/Less, Vue/Svelte, SQL, Lua, Haskell/Elm, Ruby, shell, PowerShell, Perl,
R, Elixir, Terraform/HCL, and YAML. They find exact and renamed copies, with
three differences: a name that differs is listed as a `name` parameter (the
tokens cannot tell a parameter from a rename, so there are no `renames`),
only names that differ in a single place count toward
`--max-param-density`, and `near` skips them. YAML is mostly data (fixtures,
API specs, manifests), so it is scanned only when asked for: `--only yaml`,
a `.yml` file named directly, or `at` and `similar` on YAML. Other file
types, JSON and lock files among them, are not scanned.

## Install

```sh
cargo install --git https://github.com/CMaintz/doppelscan
```

From a checkout, `cargo install --path . --root ~/.local` puts the binary in
`~/.local/bin` instead of cargo's own `bin` directory. Building needs a C compiler for the tree-sitter grammars.

The repo pins its Rust toolchain in `mise.toml` (`mise install` sets it up).
On Windows the MSVC build tools are required.

## CLI

```sh
doppelscan                         # scan ., top 20 groups
doppelscan src --limit 0           # every group
doppelscan show b4a94232d76e       # one group in full, with template
doppelscan at src/user.ts:40-60    # clones touching these lines
doppelscan similar --lang ts < snippet.ts   # does this code already exist?
doppelscan near --similarity 0.8   # functions that are copies with edits
doppelscan --format json           # machine-readable
```

`near` finds what the exact scan cannot: functions that match except for
statements added, removed or changed. Each copy lists the lines where it
has code another copy lacks, which is where the merged function needs an
option or a callback:

```
#2 x3 near, 81% similar | 128 tokens | score 256
  frontend/src/app/features/documents/documents.component.ts:123-129  (DocumentsComponent.ageLabel)  differs: 125
  frontend/src/app/features/job-feed/job-feed.component.ts:275-282  (JobFeedComponent.ageLabel)  differs: 275-276, 278
  frontend/src/app/features/prompts/prompts-library.component.ts:251-258  (PromptsLibraryComponent.ageLabel)  differs: 251-252
```

`near` is advisory: `check` and `baseline` cover exact clones only. Tests
are often near copies of each other by design, so `--exclude '**/*.test.ts'`
(or your test glob) keeps them out when they are noise.

Useful options: `--min-tokens` (default 50), `--strict` (identical names and
literals only), `--only ts,java`, `--exclude 'generated/**'`,
`--max-param-density` (default 8 per 100 tokens; higher shows data tables and
mapping boilerplate too; in token-level languages a name that differs in
several places is taken for a renamed local and not counted).

`.gitignore` is honoured, and dependency/build directories, minified bundles
and files marked as generated are skipped.

## Gating new duplication

`doppelscan check` fails (exit 1) only on duplication a change adds, so it
can gate CI in a codebase that already has some.

```sh
doppelscan check                  # the working tree vs HEAD
doppelscan check --base main      # a whole branch vs where it left main
doppelscan baseline               # accept today's duplication...
doppelscan check                  # ...then fail only on new duplication
doppelscan baseline --prune       # shrink the baseline after a cleanup
```

With `--base`, `check` compares against the merge-base of that revision and
HEAD, so cleanups that landed on `main` after the branch forked do not count
against it. The revision is read from git's object store (no checkout), with
the same files, filters and options as the working-tree scan; when the local
branch does not exist, `origin/<rev>` is used, as on a CI checkout.

Without `--base`, `check` uses the nearest `.doppelscan-baseline.json` in the
scanned directory or above it (up to the repository root), and HEAD when
there is none. A baseline keeps the settings it was written with
(`--min-tokens`, `--strict`, `--only`, `--exclude`), and `check` uses those
instead of its own flags so the comparison stays like for like; to change
them, write the baseline again.

A baseline stores each accepted group's fingerprint, copy count and the
functions it sits in, not line numbers. Moving or renaming known duplication
passes, and so does shrinking it within the same functions; another copy, a
clone that grew, or a new top-level clone does not. `--prune` only drops
entries and lowers counts, so it cannot accept new duplication.

`check` reads the working tree, staged or not, so as a pre-commit hook it
also sees unstaged edits. Commit the baseline with LF line endings
(`.doppelscan-baseline.json text eol=lf` in `.gitattributes`), or a Windows
checkout with `core.autocrlf` shows it modified after every prune.

Exit codes: 0 clean, 1 new duplication, 2 error. In GitHub Actions, check out
with `fetch-depth: 0` so the merge-base is available.

## MCP server

```sh
claude mcp add doppelscan -- doppelscan mcp
```

Or in any MCP client config: command `doppelscan`, args `["mcp"]`. The server
scans relative to its working directory (override with `--root`). It speaks
the 2026-07-28 protocol and the older `initialize` handshake.

| Tool | Use |
| --- | --- |
| `find_duplicates` | Survey a repo for extraction candidates, best first. Paginated. |
| `get_duplicate_group` | One group's parameters, renames and helper template; optionally every occurrence's code. |
| `duplicates_at` | Is the code at this file/line range duplicated elsewhere? |
| `find_similar_code` | Before writing a helper: does equivalent code already exist? |
| `check_new_duplicates` | After writing code: did this change duplicate anything? |
| `find_near_duplicates` | Functions that are copies with small edits, and where each copy differs. |

The server keeps parsed files in memory and re-parses only files whose size or
mtime changed, so follow-up calls on a large repo take tens of milliseconds.

`skills/doppelscan/SKILL.md` is a Claude Code skill describing the
find-extract-verify loop for agents.

## How it works

1. Every file is parsed; every syntax node gets a hash built bottom-up from its
   kind and its children. Identifiers and literals hash to a placeholder by
   category, operators and keywords by text, comments not at all.
2. Equal hashes across the codebase are whole-node clones.
3. Child sequences of blocks, class bodies, argument lists and literals are
   concatenated and searched with a suffix array for maximal repeated runs, so a
   shared run of statements is reported once at its full length instead of once
   per window.
4. A group is dropped when every occurrence sits inside a larger accepted
   clone, when it is a tandem repeat inside one block (a wall of getters, a
   data table), or when it needs too many parameters to be a useful helper.
5. Occurrences align leaf by leaf, so differing positions become parameters or
   renames. Each binding (declaration, parameter, loop, lambda or catch
   variable, destructured name) gets a scope: its function for a local, the
   file for a declaration. A use is renamed with a binding only inside that
   scope, so a same-spelled name elsewhere is still a parameter, and a member
   access (`this.x`) only follows a field the fragment declares itself.

Files without a grammar go through a tokenizer instead: braces become
blocks, a block's statements (split at `;`, at line ends outside brackets,
and at commas between braces) become the sequences of step 3, and keywords
are hashed by text so only names and literals are anonymised.

Near-miss detection (`near`) compares whole functions instead. Each
function's normalized tokens are cut into 10-token windows and fingerprinted
with winnowing ([Schleimer, Wilkerson and Aiken, SIGMOD
2003](https://theory.stanford.edu/~aiken/publications/papers/sigmod03.pdf)),
which guarantees that any shared run of 17 tokens yields a shared
fingerprint. An inverted index finds the functions that share fingerprints,
fingerprints common to many functions are dropped as idiom, and pairs are
scored by Jaccard similarity of their fingerprint counts (counts, so four
repeated assignments and ten do not look alike). Union-find groups the
pairs, exact clones are left to the exact scan, and the differing lines come
from a token-level diff against another copy.

## Performance

On a 951-file Java/TypeScript repo (64k lines): doppelscan 0.19 s wall time,
jscpd 4.x 15.8 s, both at a 50-token minimum. doppelscan reported 2.6x more
duplicated lines because it also finds renamed clones. jscpd's extra hits were
import blocks and windows that cut across syntax boundaries, which doppelscan
skips because they cannot be extracted as one unit.

## Limitations

- Near-miss groups are whole functions: an edited copy of a block inside two
  otherwise different functions is not found, and a small function copied
  into a much larger one scores low because the larger one dilutes the
  similarity. They come without a template, since the copies do not align.
- Scopes are per function, not per block, and come from each grammar's node
  names rather than a compiler: a name shadowed inside a nested block of the
  same function, or a binding form a grammar spells unusually, can still be
  misclassified.
- Fields that are only assigned, never declared (Python `self.x = x`, JS
  classes without field declarations), are not known to be fields, so
  `self.x` shows as a `member` parameter beside the `x` rename.
- Matches stay within one language.
- Token-level languages split statements by layout, so the same code
  formatted differently (a statement broken over lines outside brackets)
  can fail to match.
- Baselines record the languages they cover. One written by an older
  release (version 1) keeps covering only the grammar languages until it is
  written again with `doppelscan baseline`.

## Contributing

Bug reports with a small pair of snippets are the most useful thing you can
send. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and what a good change
looks like, and [docs/adding-a-language.md](docs/adding-a-language.md) to add a
language.

## License

[MIT](LICENSE)
