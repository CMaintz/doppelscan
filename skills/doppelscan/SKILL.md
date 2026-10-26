---
name: doppelscan
description: Find duplicated code and extract shared helpers with the doppelscan tool. Use when asked to find duplication, DRY up code, or extract a reusable helper; before writing a new helper or utility (to check one already exists); and after copy-pasting or writing code that resembles existing code.
---

# doppelscan

doppelscan finds code with the same syntax structure even when names and
literals differ, and tells you what a shared helper would take as arguments.

Use the MCP tools when available (`find_duplicates`, `get_duplicate_group`,
`duplicates_at`, `find_similar_code`). Otherwise use the CLI: `doppelscan`,
`doppelscan show <fingerprint>`, `doppelscan at <file>:<from>-<to>`,
`doppelscan similar --lang <ext> < snippet`.

## Reading a group

- `x9`: nine occurrences. `score` is tokens x (occurrences - 1), the code an
  extraction removes. Work on high scores first.
- `params`: values that differ per occurrence, one column per occurrence.
  Each becomes a helper argument. An identifier parameter that is called is a
  function to pass in; a type parameter means a generic.
- `renames`: names the fragment declares itself. Pick one name in the helper;
  they need no argument.
- `template`: the helper body, with parameters as `$1`, `$2`, ...

## Extracting a helper

1. `find_duplicates` (or `duplicates_at` for the code you are touching). Refer
   back to a group by its fingerprint, with the same `path` and `min_tokens`.
2. `get_duplicate_group` for the template. Add `include_code` if occurrences
   sit in different contexts (return values, surrounding variables) you need
   to see.
3. Decide whether to extract. Skip it when the occurrences only look alike and
   will evolve separately (test arrange blocks, independent API adapters), or
   when the helper would need more parameters than the code it replaces.
4. Write the helper from the template in the module the occurrences share (or
   their nearest common one), name it for what it does, and replace every
   occurrence. Values in `params` become arguments at each call site, in
   column order.
5. Run the tests, then `duplicates_at` on a former occurrence: the group should
   be gone.

## Before writing new code

Call `find_similar_code` with the function or statements you are about to
write. If it matches, reuse or generalise the existing code instead.

## Tuning

- `min_tokens` 50 is the default; 25-30 finds smaller helpers, 100+ only large
  copy-paste.
- `strict: true` reports only exact copy-paste (names and literals identical).
- `exclude` globs (e.g. `**/*.test.ts`) when test duplication is noise for the
  task.
