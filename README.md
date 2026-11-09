# doppelscan

[![ci](https://github.com/CMaintz/doppelscan/actions/workflows/ci.yml/badge.svg)](https://github.com/CMaintz/doppelscan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Fast, syntax-aware duplicate code finder built for coding agents.

doppelscan finds code with the same structure even when names and literals
differ, in the spirit of IntelliJ's duplicate detection. For each clone it
works out what a shared helper would look like: what becomes a parameter, what
is just a rename, and the helper's body. It runs as a CLI and as an MCP server,
so a Claude or other LLM session can call it as a tool.

## Quick start

Install it (needs Rust and a C compiler for the tree-sitter grammars):

```sh
cargo install --git https://github.com/CMaintz/doppelscan
```

Then, from your repository:

```sh
doppelscan                              # the 20 most worthwhile clone groups
doppelscan show rink-point-city-roast   # one group in full, with its helper template
doppelscan near                         # functions that are copies with small edits
```

To stop duplication growing in a codebase that already has plenty, accept
what is there and fail only on what is new:

```sh
doppelscan baseline              # write .doppelscan-baseline.json, and commit it
doppelscan check                 # exit 1 only on new duplication
```

To give it to Claude Code:

```sh
claude mcp add doppelscan -- doppelscan mcp
```

## What you get

```
#1 [rink-point-city-roast] x9 | 71 tokens, 3 lines, run of 3 | score 568
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

- **params**: values that differ between copies and would become helper
  arguments, labelled `type`, `function`, `member` or `name` where that is
  what the helper takes.
- **renames**: locals the fragment binds itself, which differ but need no
  argument.
- **declares**: names the fragment declares for other code, so callers have
  to switch to the one name the helper keeps.
- **template**: the first copy with each parameter replaced by `$1`, `$2`, and
  so on, which is the helper body.

## What it does

| Command | Use |
| --- | --- |
| `doppelscan [path]` | Find clones, best first. `--limit 0` lists them all. |
| `doppelscan show <id>` | One group in full, by name, fingerprint or number. |
| `doppelscan at <file>:<lines>` | Is the code at these lines duplicated elsewhere? |
| `doppelscan similar --lang ts` | Does the snippet on stdin already exist? |
| `doppelscan near` | Functions that match except for statements added, removed or changed, and the lines that differ. |
| `doppelscan check` | Fail only on duplication a change adds, against a baseline or a git revision. |
| `doppelscan baseline` | Accept today's duplication, so it can be paid down bit by bit. |
| `doppelscan init` | Write a commented `.doppelscan.toml`. |
| `doppelscan config` | Show the settings in use and where each comes from. |
| `doppelscan mcp` | Run as an MCP server. |

Add `--format json` for machine-readable output. `.gitignore` is honoured, and
dependencies, build output, minified bundles and generated code are skipped.
Code you mean to keep duplicated can be marked with a comment or accepted by
fingerprint.

## Languages

Parsed with tree-sitter: TypeScript, TSX, JavaScript, Java, C#, Kotlin, Swift,
PHP, Python, Go, Rust, CSS and HTML, including Angular and other framework
templates.

Matched token by token: C, C++, Objective-C, Scala, Groovy and Gradle, Dart,
Zig, Solidity, Protobuf, SCSS/Less, Vue/Svelte, SQL, Lua, Haskell/Elm, Ruby,
shell, PowerShell, Perl, R, Elixir, Terraform/HCL and YAML. These find exact and
renamed copies, but `near` skips them, and YAML is only scanned when asked for
(`--only yaml`). Matches stay within one language.

## Benchmark

Two large Java repositories, Apache Camel (45,124 files) and Elasticsearch
(51,122 files), each with 80 planted clones, on a Ryzen 7 3800X with 32 GB of
RAM. Times are medians of 15 runs over three passes. "Caught" means one group
covers at least 80% of both the original and the copy.

| Tool | Camel time | Camel memory | Camel caught | Elasticsearch time | Elasticsearch memory | Elasticsearch caught |
| --- | --- | --- | --- | --- | --- | --- |
| **doppelscan**, exact + near + YAML | 12 s | 1.0 GB | **76 of 80** | 32 s | 2.1 GB | 70 of 80 |
| dupehound 0.1.2 | **9 s** | **0.1 GB** | 46 | **14 s** | **0.2 GB** | 46 |
| jscpd 5.3.0, default | 3 min 5 s | 13.6 GB | 49 | 3 min 40 s | 15.1 GB | 48 |
| jscpd 5.3.0, type-2/3 | 5 min 4 s | 15.5 GB | 71 | 6 min 37 s | 18.4 GB | **74** |

In a blinded sample of 20 groups per tool and repository, 25 of 40 jscpd
default groups and 35 of 40 type-2/3 groups were not real duplication
(mostly license headers and import lists), against 2 of 40
each for doppelscan's exact and near scans and 4 for dupehound. All of
doppelscan's misses but one comment-heavy YAML block, which every tool misses,
are edited copies scoring 0.70-0.79 against near's 0.8 threshold; with
`--similarity 0.7` it catches 80 and 79. [Full results](docs/benchmark.md),
with what the benchmark does not show.

## Documentation

- [Usage](docs/usage.md): CLI options, configuration, ignoring code, gating CI
  and the MCP tools.
- [How it works](docs/how-it-works.md): the algorithms, and current limitations.
- [Benchmark](docs/benchmark.md): every run, with recall and precision.
- [Adding a language](docs/adding-a-language.md).

## Contributing

Bug reports with a small pair of snippets are the most useful thing you can
send. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and what a good change
looks like.

## License

[MIT](LICENSE)
