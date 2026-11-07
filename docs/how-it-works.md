# How doppelscan works

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
   data table; only a verbatim copy of part of the wall elsewhere is reported),
   or when it needs too many parameters to be a useful helper.
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
- A part of a repetitive wall copied elsewhere is matched by its spelling, so
  when two walls and a copy share such a run, the part in the second wall can
  drop out of the report. A copy of most of a table whose rows themselves
  recur can also come with a second, overlapping group that has parameters.
- Matches stay within one language.
- Token-level languages split statements by layout, so the same code
  formatted differently (a statement broken over lines outside brackets)
  can fail to match.
- Fingerprints can change between releases, so a baseline written by an
  older release (version 2 or below) is refused, and `check` asks for it to
  be written again with `doppelscan baseline` before comparing.
