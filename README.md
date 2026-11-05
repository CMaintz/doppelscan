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
doppelscan init                    # write a commented .doppelscan.toml
doppelscan config                  # the settings in use, and where each comes from
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

`.gitignore` is honoured, and dependency/build directories, tool caches,
minified bundles, files marked as generated and generated-source
directories (`generated`, `generated-src`, `__generated__` and the like) are
skipped. Other
dot-directories, such as `.github`, are scanned.

## Configuration

A `.doppelscan.toml` sets a project's defaults for the CLI and the MCP
server alike. `doppelscan init` writes one with every setting commented
out at its default, the languages it found, and the test files it found
already listed for `near` to skip. `doppelscan config` (or the MCP
`get_settings` tool) shows the settings a scan uses, where each comes
from, and the built-in skip list. Editors that read `#:schema` (Taplo,
Even Better TOML) complete and check the keys from
`doppelscan.schema.json`.

```toml
min-tokens = 40
only = ["ts", "java"]
exclude = ["legacy/", "*.generated.ts"]
include = ["vendor/our-fork/"]   # scanned despite the built-in skips
max-param-density = 10
strict = false

[near]
similarity = 0.85
exclude = ["**/*.test.ts"]        # tests are near copies by design
```

Settings come from every `.doppelscan.toml` from the scanned directory up
to the repository root, the nearest winning where two set the same key, so
a package in a monorepo can refine the root's settings; `root = true`
stops the search at that file.

`exclude` and `include` use `.gitignore` syntax, relative to the file that
lists them, and add up across files rather than override. They also apply
from configs inside the scanned tree, so a package's excludes hold whether
the scan starts at the package or at the root; that package's other
settings only apply to a scan that starts inside it. `include` brings back
directories the built-in list skips (dependencies, build output, generated
code), and what it names is scanned even when marked as generated; minified
files stay out. Inside a skipped directory, anchor the path (`vendor/ours/`,
not `ours/`). As in
`.gitignore`, nothing inside an excluded directory can be re-included.

A flag or tool argument overrides a setting, `--exclude` adds to the
config's excludes, `--no-strict` turns off a configured `strict`,
`--config FILE` reads that file instead and `--no-config` reads none, in
both cases including configs inside the scanned tree. An unknown key is an
error.

Patterns in a `--config` file stay relative to that file, so they only
match under its directory. Kept elsewhere, such as a config shared between
repositories, its other settings still apply but its patterns match nothing,
and doppelscan prints a warning to stderr saying so.

## Ignoring code

Duplication you have decided to keep can be marked in a comment, in any
language:

```ts
// doppelscan-ignore-start
...
// doppelscan-ignore-end
```

The region runs from the line with the start marker to the line with the
end marker, or to the end of the file when there is none.
`doppelscan-ignore-file` anywhere in a file ignores all of it. Ignored code
matches nothing, so a clone that touches it is not reported, while its
other copies still are when two or more are left. This applies to `near`,
`check` and the MCP tools too.

Where a comment can't go (vendored or generated code you don't edit), a
clone can be accepted by its fingerprint, the hex id shown in brackets, in
`.doppelscan.toml`:

```toml
[[accept]]
fingerprint = "b4a94232d76e"
reason = "Generated from the OpenAPI spec; regenerated, never edited."
```

An accepted group is left out of every report and never fails `check`,
however many copies it has. A change to the code's structure changes its
fingerprint, so the clone comes back for another look; renaming things or
changing literals does not, unless the scan is `strict`, where any text
change does and fingerprints differ from a non-strict scan's. Hex case does
not matter. `near` groups have no fingerprint and are not affected.

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
there is none. Settings come from flags and `.doppelscan.toml` only; a
baseline records the ones it was written with (`min-tokens`, `strict`,
`only`, `max-param-density`, `--exclude`) just so `check` and `--prune` can
refuse, listing the differences, when they have changed since, rather than
compare unlike scans. To change them, write the baseline again.

`check` fails on any new group the scan reports. To gate on bigger clones
only while still seeing the small ones, set `[check] min-tokens` in the
config (or `--check-min-tokens`): new groups under it are listed after
the others (`smaller` in the JSON) and do not fail the check.

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
| `get_settings` | The settings a scan uses and where each comes from, as `doppelscan config` shows them. |

The server keeps parsed files in memory and re-parses only files whose size or
mtime changed, so follow-up calls on a large repo take tens of milliseconds.

`skills/doppelscan/SKILL.md` is a Claude Code skill describing the
find-extract-verify loop for agents.

## How it works

1. Every file is parsed; every syntax node gets a hash built bottom-up from its
   kind and its children. Identifiers and literals hash to a placeholder by
   category, operators and keywords by text, comments and Java marker
   annotations (`@Override`, no arguments) not at all.
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
skipping fingerprints common to many functions (idiom) so they do not pair
everything, and each pair is scored by Jaccard similarity of all its
fingerprint counts, idiom included (counts, so four repeated assignments and
ten do not look alike). Union-find groups the pairs, exact clones are left to
the exact scan, and the differing lines come from a token-level diff against
another copy.

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
- A baseline written by an older release (version 1) records only the
  grammar languages, so `check` asks for it to be written again with
  `doppelscan baseline` before comparing.

## Contributing

Bug reports with a small pair of snippets are the most useful thing you can
send. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and what a good change
looks like, and [docs/adding-a-language.md](docs/adding-a-language.md) to add a
language.

## License

[MIT](LICENSE)
