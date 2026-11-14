# Using doppelscan

The [README](../README.md) has the quick start. This is the full reference.

## Install

Each release attaches prebuilt binaries for Linux (x86_64, aarch64), macOS
(Apple silicon, Intel) and Windows (x86_64), with a `SHA256SUMS` file and
build provenance. Download one from the
[releases page](https://github.com/CMaintz/doppelscan/releases) and check it:

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify doppelscan-<tag>-<target>.tar.gz --repo CMaintz/doppelscan
```

Or build from source:

```sh
cargo install --git https://github.com/CMaintz/doppelscan
```

From a checkout, `cargo install --path . --root ~/.local` puts the binary in
`~/.local/bin` instead of cargo's own `bin` directory. Building needs a C compiler for the tree-sitter grammars.

The repo pins its Rust toolchain in `mise.toml` (`mise install` sets it up).
On Windows the MSVC build tools are required.

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

## CLI

```sh
doppelscan                         # scan ., top 20 groups
doppelscan src --limit 0           # every group
doppelscan show rink-point-city-roast   # one group in full, with template
doppelscan at src/user.ts:40-60    # clones touching these lines
doppelscan similar --lang ts < snippet.ts   # does this code already exist?
doppelscan near --similarity 0.8   # functions that are copies with edits
doppelscan --format json           # machine-readable
doppelscan init                    # write a commented .doppelscan.toml
doppelscan config                  # the settings in use, and where each comes from
```

Listings label a group with a four-word name derived from its fingerprint,
the content hash that identifies it. The name needs no state and stays the
same as long as the fingerprint does. `show` takes the name, the fingerprint
or a unique prefix of it, or the number from a listing, and prints the
fingerprint too. JSON has both, as `name` and `fingerprint`; baselines and
`accept` use the fingerprint alone. Two groups can share a name, about 0.005
pairs expected in a scan of 100,000 groups. `show` then exits with an error
listing their fingerprints rather than picking one. `near` groups have only a
number.

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

Useful options: `--min-tokens` (default 50; 20 for `similar`, since snippets
are small), `--strict` (identical names and
literals only), `--only ts,java`, `--exclude 'generated/**'`,
`--max-param-density` (default 8 per 100 tokens; higher shows data tables and
mapping boilerplate too; a group over the limit still keeps its copies that
are spelled exactly alike; in token-level languages a name that differs in
several places is taken for a renamed local and not counted).

`.gitignore` is honoured, and dependency/build directories, tool caches,
minified bundles, files marked as generated and generated-source
directories (`generated`, `generated-src`, `__generated__` and the like) are
skipped. Other
dot-directories, such as `.github`, are scanned.

Files need not be UTF-8. UTF-16 with a BOM is decoded, and any other non-UTF-8
text is read with its non-ASCII bytes replaced, which keeps structure and line
numbers. A non-UTF-8 file holding a NUL byte counts as binary and is skipped,
and so is UTF-16 without a BOM.

## Configuration

A `.doppelscan.toml` sets a project's defaults for the CLI and the MCP
server alike. `doppelscan init` writes one with every setting commented
out at its default, the languages it found, and the test files it found
already listed for `near` to skip. `doppelscan config` (or the MCP
`get_settings` tool) shows the settings a scan uses, where each comes
from, and the built-in skip list; it fails where a scan would, and configs
nested inside the tree apply but are not listed. Editors that read `#:schema` (Taplo,
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
`--config FILE` reads that file instead and `--no-config` reads none;
neither reads configs inside the scanned tree. An unknown key is an error.

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
clone can be accepted by its fingerprint, the hex id `doppelscan show` prints,
in `.doppelscan.toml`. The readable name in brackets does not work here:

```toml
[[accept]]
fingerprint = "b4a94232d76e"
reason = "Generated from the OpenAPI spec; regenerated, never edited."
```

An accepted group is left out of every report and never fails `check`,
however many copies it has. A change to the code's structure changes its
fingerprint, so the clone comes back for another look; renaming things or
changing literals does not, unless the scan is `strict`, where they do and
fingerprints differ from a non-strict scan's. Hex case does not matter. Its
baseline entry, if it has one, stays until the code is gone. `near` groups
have no fingerprint and are not affected, and `similar` still reports a match
on an accepted group, under `accepted` in its JSON.

Copies spelled alike that stand on their own because their group went over the
density limit are the exception. Their fingerprint holds their spelling, so
renaming their code changes it, and an `accept` of them or a baseline entry for
them has to be redone. Moving them does not.

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

A baseline stores each accepted group's fingerprint, copy count, normalized
tokens and the functions it sits in, not line numbers. Moving or renaming
known duplication passes, and so does shrinking it within the same functions,
which means a clone of code the entry held, sitting where the entry sat, with
up to two stretches deleted from it. A deletion in more spots than that reads
as a new clone until the baseline is written again. Such a deletion counts
only once the whole clone has gone, since new code can open and close like an
accepted clone; a new function added in the same change that edits the clone
can still slip through that way. A clone over several top-level definitions
sits in its files, so what an edit leaves of it, even inside one definition,
counts as shrunk. Another copy, a clone that grew, or new code beside an
accepted clone does not, even in the same class or function. A baseline
written before tokens were recorded goes by
size and functions alone until it is written again, so there new code smaller
than an accepted clone in the same functions passes. `--prune` only drops
entries and forgets copies that are gone, so it cannot accept new duplication.
Only a scan of the whole directory the baseline describes can tell that code
is gone: from a subdirectory, a copy that moved out looks deleted. So `check`
on a subdirectory scans all of that directory and reports the new groups with
a copy under the path, using the settings from there, and `--prune` always
covers the whole baseline, wherever it is run from. A baseline of one folder
kept above it (`baseline src -o baseline.json`) records that folder, and
describes nothing outside it.

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
| `get_duplicate_group` | One group by name, fingerprint or id: parameters, renames and helper template; optionally every occurrence's code. |
| `duplicates_at` | Is the code at this file/line range duplicated elsewhere? |
| `find_similar_code` | Before writing a helper: does equivalent code already exist? |
| `check_new_duplicates` | After writing code: did this change duplicate anything? |
| `find_near_duplicates` | Functions that are copies with small edits, and where each copy differs. |
| `get_settings` | The settings a scan uses and where each comes from, as `doppelscan config` shows them. |

The server keeps parsed files in memory and re-parses only files whose size or
mtime changed, so follow-up calls on a large repo take tens of milliseconds.

`skills/doppelscan/SKILL.md` is a Claude Code skill describing the
find-extract-verify loop for agents.
