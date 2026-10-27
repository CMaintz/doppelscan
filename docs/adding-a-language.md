# Adding a language

doppelscan needs no per-language queries. It classifies a grammar's node kinds
by name (identifier, string, number, comment, block, import...) when the
grammar loads, so most languages work once their grammar is registered.

1. Add the grammar crate to `Cargo.toml`. It must target the same tree-sitter
   ABI as the `tree-sitter` dependency: if `cargo test` fails with a language
   version error, pick an older or newer grammar release.
2. Add a `Lang::new(...)` entry to `LANGS` in `src/lang.rs` with the
   language name, the grammar, its file extensions and `assign_declares`
   (true when a plain assignment introduces a local, as in Python and PHP).
3. Add a renamed-clone fixture to `renamed_clones_are_found_in_every_language`
   in `tests/detection.rs`: two functions with the same structure and
   different names and literals. Make it big enough to pass the 50-token
   minimum.
4. Add the language to the list in `README.md`.

## When it does not match

Dump the syntax tree of a fixture to see what the grammar calls things:

```sh
cargo run --example tree -- path/to/file.ext
```

Each line shows the node kind, its field name in brackets and, for leaves,
the text. The usual problems:

- **Identifiers do not count as renames.** `is_binding_site` in
  `src/lang.rs` decides which identifier positions declare a name. Check
  which parent kind and field the grammar uses for declarations and
  parameters.
- **Imports are reported as clones.** Add the grammar's import kind to
  `is_noise`.
- **Statement runs are missed.** The grammar's block or body kind is not
  matched by `is_block`; add it.
- **Literals stay distinct.** The literal kind is not matched by `is_string`
  or `is_number`.

Change a classifier only together with a test that shows the effect, and run
the whole suite: these functions are shared by every language.
