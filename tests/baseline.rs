mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use common::git;
use doppelscan::baseline::{Baseline, DEFAULT_FILE, Entry};
use doppelscan::engine::{ScanOptions, Source, analyze, index_sources};
use doppelscan::lang::lang_index_for_path;
use doppelscan::report::Analysis;

const SUM_TOTALS: &str = r#"
export function sumTotals(orders: Order[], config: Config): number {
    let total = 0;
    for (const order of orders) {
        if (order.status === "paid") {
            total += order.amount * config.rate;
        }
    }
    return Math.round(total * 100) / 100;
}
"#;

const PARSE_ROWS: &str = r#"
export function parseRows(text: string): Row[] {
    const rows: Row[] = [];
    for (const line of text.split("\n")) {
        const cells = line.split(",").map((c) => c.trim());
        if (cells.length < 3) {
            continue;
        }
        rows.push({ id: cells[0], name: cells[1], value: Number(cells[2]) });
    }
    return rows;
}
"#;

fn renamed(code: &str, n: usize) -> String {
    code.replace("sumTotals", &format!("sumTotals{n}"))
        .replace("parseRows", &format!("parseRows{n}"))
}

fn options() -> ScanOptions {
    ScanOptions {
        min_tokens: 20,
        ..ScanOptions::default()
    }
}

fn scan(files: &[(&str, String)]) -> Analysis {
    let sources = files
        .iter()
        .map(|(path, text)| Source {
            path: path.to_string(),
            lang: lang_index_for_path(path.as_ref()).unwrap(),
            text: text.clone(),
        })
        .collect();
    analyze(index_sources(sources, false), &options(), Instant::now())
}

fn two_copies() -> Vec<(&'static str, String)> {
    vec![
        ("a.ts", renamed(SUM_TOTALS, 1)),
        ("b.ts", renamed(SUM_TOTALS, 2)),
    ]
}

fn new_groups(base: &Analysis, now: &Analysis) -> Vec<usize> {
    Baseline::from_analysis(base, &options()).new_groups(now)
}

// Catches a comparison that flags every group, or fingerprints that drift
// between identical scans.
#[test]
fn unchanged_code_has_no_new_duplication() {
    let base = scan(&two_copies());
    assert_eq!(base.groups.len(), 1);
    assert!(new_groups(&base, &scan(&two_copies())).is_empty());
}

// Catches comparing fingerprints only: a third copy of known duplication
// is new duplication.
#[test]
fn another_copy_of_known_duplication_is_new() {
    let mut now = two_copies();
    now.push(("c.ts", renamed(SUM_TOTALS, 3)));
    assert_eq!(new_groups(&scan(&two_copies()), &scan(&now)), vec![1]);
}

// Catches only counting copies of groups the baseline already knows.
#[test]
fn a_new_duplicate_group_is_new() {
    let mut now = two_copies();
    now.push(("c.ts", renamed(PARSE_ROWS, 1)));
    now.push(("d.ts", renamed(PARSE_ROWS, 2)));
    let now = scan(&now);
    let flagged = new_groups(&scan(&two_copies()), &now);
    assert_eq!(flagged.len(), 1);
    assert!(
        now.group(flagged[0], false).occurrences[0]
            .file
            .starts_with('c')
    );
}

// Catches comparing by location: moved and renamed duplication is not new.
#[test]
fn moving_and_renaming_known_duplication_is_not_new() {
    let now = vec![
        ("lib/x.ts", renamed(SUM_TOTALS, 7)),
        ("lib/y.ts", renamed(SUM_TOTALS, 8)),
    ];
    assert!(new_groups(&scan(&two_copies()), &scan(&now)).is_empty());
}

// Catches fingerprint-only comparison: editing one copy shrinks the clone,
// which changes its fingerprint without adding any duplication.
#[test]
fn shrinking_known_duplication_is_not_new() {
    let edited =
        renamed(SUM_TOTALS, 2).replace("return Math.round(total * 100) / 100;", "return total;");
    let now = scan(&[("a.ts", renamed(SUM_TOTALS, 1)), ("b.ts", edited)]);
    assert_eq!(now.groups.len(), 1, "the shared loop is still a clone");
    let base = scan(&two_copies());
    assert_ne!(
        base.group(1, false).fingerprint,
        now.group(1, false).fingerprint
    );
    assert!(new_groups(&base, &now).is_empty());
}

// Catches a prune that rewrites the baseline from the current scan, which
// would silently accept new duplication.
#[test]
fn pruning_drops_resolved_entries_and_never_adds() {
    let mut base_files = two_copies();
    base_files.push(("c.ts", renamed(PARSE_ROWS, 1)));
    base_files.push(("d.ts", renamed(PARSE_ROWS, 2)));
    let baseline = Baseline::from_analysis(&scan(&base_files), &options());
    assert_eq!(baseline.groups.len(), 2);

    let now = vec![
        ("a.ts", renamed(SUM_TOTALS, 1)),
        ("b.ts", renamed(SUM_TOTALS, 2)),
        ("e.ts", renamed(SUM_TOTALS, 3)),
        ("f.ts", renamed(PARSE_ROWS, 3)),
    ];
    let now = scan(&now);
    let pruned = baseline.pruned(&now);
    assert_eq!(pruned.groups.len(), 1, "the parseRows entry is resolved");
    assert_eq!(pruned.new_groups(&now), vec![1], "the third copy stays new");
}

fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("doppelscan-base-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(&dir, &["init", "-q"]);
    for (path, text) in two_copies() {
        std::fs::write(dir.join("src").join(path), text).unwrap();
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

fn doppelscan(dir: &Path, args: &[&str]) -> (i32, String) {
    run(dir, &[args, &["--min-tokens=20"]].concat())
}

fn run(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_doppelscan"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().unwrap(), text)
}

// Catches reading the working tree instead of the revision (nothing would
// ever be new) and a wrong exit code on findings.
#[test]
fn check_against_a_revision_flags_only_uncommitted_duplication() {
    let dir = repo("rev");
    assert_eq!(doppelscan(&dir, &["check", "--base", "HEAD"]).0, 0);
    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    let (code, out) = doppelscan(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("src/c.ts"), "{out}");
}

// Catches revision paths and working-tree paths disagreeing when only a
// subdirectory is scanned: a shrunk clone is only excused by its places.
#[test]
fn check_against_a_revision_works_on_a_subdirectory() {
    let dir = repo("subdir");
    let shrunk =
        renamed(SUM_TOTALS, 2).replace("return Math.round(total * 100) / 100;", "return total;");
    std::fs::write(dir.join("src/b.ts"), shrunk).unwrap();
    let (code, out) = doppelscan(&dir, &["check", "src", "--base", "HEAD"]);
    assert_eq!(code, 0, "{out}");
}

// Catches the baseline file not round-tripping through disk, and `check`
// not preferring an existing baseline file over HEAD (the new copy is
// committed, so HEAD would accept it).
#[test]
fn a_written_baseline_accepts_existing_and_rejects_new_duplication() {
    let dir = repo("file");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    assert!(dir.join(".doppelscan-baseline.json").exists());
    assert_eq!(doppelscan(&dir, &["check"]).0, 0);
    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "copy"]);
    assert_eq!(doppelscan(&dir, &["check"]).0, 1);
}

fn written_entries(dir: &Path) -> Vec<Entry> {
    let file = dir.join(".doppelscan-baseline.json");
    Baseline::load(&file).unwrap().groups
}

// Catches prune lowering the count but keeping the removed copy's place:
// the shrink rule would excuse that copy coming back, which is new
// duplication the README says a prune cannot accept.
#[test]
fn a_pruned_copy_that_returns_is_new() {
    let dir = repo("prune-place");
    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    std::fs::remove_file(dir.join("src/c.ts")).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);

    let entries = written_entries(&dir);
    assert_eq!(entries[0].occurrences, 2);
    assert_eq!(entries[0].places.len(), 2, "{:?}", entries[0].places);

    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 1, "{out}");
}

// Catches only the matching-fingerprint path dropping places: an entry kept
// because a smaller clone still sits in its places would go on excusing the
// removed copy, by place and by count.
#[test]
fn a_shrunk_entry_forgets_a_removed_copy() {
    let dir = repo("prune-shrunk");
    let shrunk =
        renamed(SUM_TOTALS, 2).replace("return Math.round(total * 100) / 100;", "return total;");
    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    std::fs::write(dir.join("src/b.ts"), &shrunk).unwrap();
    std::fs::remove_file(dir.join("src/c.ts")).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);

    let entries = written_entries(&dir);
    assert_eq!(entries[0].occurrences, 2);
    assert_eq!(entries[0].places.len(), 2, "{:?}", entries[0].places);
}

// Catches pruning by the same-fingerprint group alone: the smaller clone
// through c.ts is excused only by c.ts's place, so dropping that place turns
// a green check red with no code change.
#[test]
fn pruning_keeps_a_place_only_a_shrunk_group_holds() {
    let dir = repo("prune-union");
    let shrunk =
        renamed(SUM_TOTALS, 3).replace("return Math.round(total * 100) / 100;", "return total;");
    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    std::fs::write(dir.join("src/c.ts"), shrunk).unwrap();
    assert_eq!(doppelscan(&dir, &["check"]).0, 0);

    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);
    let entries = written_entries(&dir);
    assert_eq!(entries[0].occurrences, 3);
    assert_eq!(entries[0].places.len(), 3, "{:?}", entries[0].places);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches keeping a place because any copy matches it: of two copies
// in one function, one is gone, so its place must go too.
#[test]
fn pruning_one_of_two_copies_in_a_function_drops_one_place() {
    let body = r#"
    for (const row of rows) {
        if (row.ok) { total += row.value * rate; }
        log(row.id, row.value, total);
    }
    audit(total);
"#;
    let gap = "    try { next(); } finally { done(); }\n";
    // Unlike `other`, so the clone stops at `body` however many it holds.
    let host = |copies: &[&str]| format!("const f = () => {{{}}};\n", copies.join(gap));
    let other = format!("function g(x: number) {{{body}    return x;\n}}\n");
    let base = [("a.ts", host(&[body, body])), ("b.ts", other.clone())];
    let baseline = Baseline::from_analysis(&scan(&base), &options());
    assert_eq!(baseline.groups[0].occurrences, 3);

    let one = [("a.ts", host(&[body])), ("b.ts", other)];
    let pruned = baseline.pruned(&scan(&one));
    assert_eq!(pruned.groups[0].occurrences, 2);
    assert_eq!(pruned.groups[0].places.len(), 2);
}

// `sumTotals` and `parseRows` as two methods of one class, so the clone
// spanning both sits at `<file>#Cart` until an edit cuts it to one method.
fn cart(parse_rows: &str) -> String {
    let method = |code: &str| code.replace("export function ", "");
    let total = method(&renamed(SUM_TOTALS, 0));
    format!(
        "class Cart {{{total}{}}}
",
        method(parse_rows)
    )
}

fn cart_in_two_files() -> Vec<(&'static str, String)> {
    let parse_rows = renamed(PARSE_ROWS, 0);
    vec![("a.ts", cart(&parse_rows)), ("b.ts", cart(&parse_rows))]
}

fn cart_cut_to_one_method() -> Analysis {
    let edited = cart(
        "flush(): void { queue.clear(); }
",
    );
    scan(&[("a.ts", cart(&renamed(PARSE_ROWS, 0))), ("b.ts", edited)])
}

// Catches an index of exact places: the accepted clone sits at `a.ts#Cart`,
// the shrunk one at `a.ts#Cart.sumTotals0`, and the two share no string.
#[test]
fn a_clone_shrunk_into_a_method_is_excused_by_the_enclosing_class() {
    let baseline = Baseline::from_analysis(&scan(&cart_in_two_files()), &options());
    assert_eq!(baseline.groups[0].places, ["a.ts#Cart", "b.ts#Cart"]);
    let now = cart_cut_to_one_method();
    let current = Baseline::from_analysis(&now, &options());
    assert_eq!(
        current.groups[0].places,
        ["a.ts#Cart.sumTotals0", "b.ts#Cart.sumTotals0"]
    );
    assert!(baseline.new_groups(&now).is_empty());

    let pruned = baseline.pruned(&now);
    assert_eq!(pruned.groups, baseline.groups, "both places are still held");
}

// Catches a prune that finds entries by place alone: moved copies hold no
// place, but their entry still has to excuse them.
#[test]
fn pruning_keeps_the_entry_of_a_moved_clone_without_its_places() {
    let baseline = Baseline::from_analysis(&scan(&two_copies()), &options());
    let moved = scan(&[
        ("lib/x.ts", renamed(SUM_TOTALS, 7)),
        ("lib/y.ts", renamed(SUM_TOTALS, 8)),
    ]);
    let pruned = baseline.pruned(&moved);
    assert_eq!(pruned.groups.len(), 1);
    assert_eq!(pruned.groups[0].occurrences, 2);
    assert!(pruned.groups[0].places.is_empty());
    assert!(pruned.new_groups(&moved).is_empty());
}

fn accepted(fingerprint: &str, occurrences: usize, tokens: u32, places: &[&str]) -> Entry {
    Entry {
        fingerprint: fingerprint.to_string(),
        occurrences,
        tokens,
        places: places.iter().map(|p| p.to_string()).collect(),
        shape: String::new(),
    }
}

fn three_copies() -> Analysis {
    let mut files = two_copies();
    files.push(("c.ts", renamed(SUM_TOTALS, 3)));
    scan(&files)
}

// Catches stopping at the fingerprint: too few copies under one entry says
// nothing about a larger entry whose places cover this group.
#[test]
fn a_group_with_more_copies_than_its_entry_can_still_be_shrunk_from_another() {
    let now = three_copies();
    let mut baseline = Baseline::from_analysis(&now, &options());
    let tokens = baseline.groups[0].tokens;
    let places = baseline.groups[0].places.clone();
    let places: Vec<&str> = places.iter().map(String::as_str).collect();
    let same = accepted(&baseline.groups[0].fingerprint, 2, tokens, &places[..2]);
    baseline.groups = vec![same.clone()];
    assert_eq!(
        baseline.new_groups(&now),
        vec![1],
        "two copies, three found"
    );

    baseline.groups.push(accepted("larger", 3, tokens, &places));
    assert!(baseline.new_groups(&now).is_empty());
}

// Catches one slot per fingerprint: fingerprints are truncated hashes, so two
// entries can share one, and either may be the one with enough copies. Their
// places are stale so that only the count can excuse the group.
#[test]
fn every_entry_sharing_a_fingerprint_counts() {
    let now = three_copies();
    let mut baseline = Baseline::from_analysis(&now, &options());
    let found = baseline.groups[0].clone();
    let enough = accepted(&found.fingerprint, 3, found.tokens, &["y.ts"]);
    let short = accepted(&found.fingerprint, 2, found.tokens, &["z.ts"]);
    baseline.groups = vec![enough.clone(), short.clone()];
    assert!(baseline.new_groups(&now).is_empty());
    assert_eq!(baseline.pruned(&now).groups.len(), 2);

    baseline.groups = vec![short, enough];
    assert!(baseline.new_groups(&now).is_empty());
    assert_eq!(baseline.pruned(&now).groups.len(), 2);
}

// Catches treating a shared place as enough: both places are accepted, but
// the second copy in `f` is one more than the entry holds.
#[test]
fn a_shrunk_group_with_more_copies_at_a_place_than_the_entry_is_new() {
    let body = r#"
    for (const row of rows) {
        if (row.ok) { total += row.value * rate; }
        log(row.id, row.value, total);
    }
    audit(total);
"#;
    let gap = "    try { next(); } finally { done(); }
";
    let host = format!(
        "function f() {{{body}{gap}{body}}}
"
    );
    let other = format!(
        "function g(x: number) {{{body}    return x;
}}
"
    );
    let now = scan(&[("a.ts", host), ("b.ts", other)]);
    let mut baseline = Baseline::from_analysis(&now, &options());
    assert_eq!(baseline.groups[0].places, ["a.ts#f", "a.ts#f", "b.ts#g"]);
    let tokens = baseline.groups[0].tokens;
    baseline.groups = vec![accepted("larger", 2, tokens, &["a.ts#f", "b.ts#g"])];
    assert_eq!(baseline.new_groups(&now), vec![1]);
}

fn repo_accepting_its_baseline(name: &str) -> PathBuf {
    let dir = repo(name);
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let fingerprint = &written_entries(&dir)[0].fingerprint;
    let accept = format!("[[accept]]\nfingerprint = \"{fingerprint}\"\nreason = \"vendored\"\n");
    std::fs::write(dir.join(".doppelscan.toml"), accept).unwrap();
    dir
}

// Catches pruning against visible groups only: accepting a group hides it
// from the scan, and dropping its entry would fail `check` on untouched
// code the day the accept line is removed.
#[test]
fn an_accepted_group_is_hidden_not_gone() {
    let dir = repo_accepting_its_baseline("accept-kept");
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("no longer match"), "{out}");

    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);
    assert_eq!(written_entries(&dir).len(), 1);
    std::fs::remove_file(dir.join(".doppelscan.toml")).unwrap();
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches keeping an entry for its accept line alone: once the code is
// deleted the entry has nothing left to excuse and would stay forever.
#[test]
fn an_accepted_group_whose_code_is_gone_is_resolved() {
    let dir = repo_accepting_its_baseline("accept-gone");
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    let (_, out) = doppelscan(&dir, &["check"]);
    assert!(out.contains("no longer match"), "{out}");
    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);
    assert!(written_entries(&dir).is_empty());
}

fn write_all(dir: &Path, files: &[(&str, String)]) {
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// The `repo` pair in `src`, and a separate clone in `src-old`: a sibling
/// sharing the name's start, so a bare prefix test reads it as inside `src`.
fn repo_with_sibling(name: &str) -> PathBuf {
    let dir = repo(name);
    let sibling = [
        ("src-old/x.ts", renamed(PARSE_ROWS, 1)),
        ("src-old/y.ts", renamed(PARSE_ROWS, 2)),
    ];
    write_all(&dir, &sibling);
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    assert_eq!(written_entries(&dir).len(), 2);
    dir
}

fn repo_with_spanning_clone(name: &str) -> PathBuf {
    let dir = repo(name);
    write_all(&dir, &[("other/c.ts", renamed(SUM_TOTALS, 3))]);
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    assert_eq!(written_entries(&dir)[0].occurrences, 3);
    dir
}

// Catches comparing part of the tree with the whole baseline: entries for
// code the scan never looked at would read as resolved, and pruning would
// fail the next root check on untouched code.
#[test]
fn entries_outside_a_scanned_directory_are_not_resolved() {
    let dir = repo_with_sibling("scope-outside");
    let (code, out) = doppelscan(&dir, &["check", "src"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("no longer match"), "{out}");

    assert_eq!(doppelscan(&dir, &["baseline", "--prune", "src"]).0, 0);
    assert_eq!(written_entries(&dir).len(), 2);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches judging an entry from part of the tree by its recorded places, stale once
// a copy moves: the copy looks deleted from inside, so dropping the entry
// fails the next root check on code that still exists.
#[test]
fn a_copy_moved_out_of_a_scanned_directory_keeps_its_entry() {
    let dir = repo("scope-moved-out");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    write_all(&dir, &[("lib/b.ts", renamed(SUM_TOTALS, 2))]);
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    let (_, out) = doppelscan(&dir, &["check", "src"]);
    assert!(!out.contains("no longer match"), "{out}");

    assert_eq!(doppelscan(&dir, &["baseline", "--prune", "src"]).0, 0);
    assert_eq!(written_entries(&dir).len(), 1);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches a prune judging only the path it was given: run from a package,
// it would keep an entry for code that is gone from the rest of the tree.
#[test]
fn a_prune_from_a_subdirectory_covers_the_whole_baseline() {
    let dir = repo_with_sibling("scope-prune-all");
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline", "--prune", "src"]).0, 0);
    assert_eq!(written_entries(&dir).len(), 1);
}

// Catches a prune from a subdirectory lowering counts: two copies in view of
// three reads as one gone when it moved outside, and the root check then counts
// three against the two left.
#[test]
fn a_copy_moved_out_of_a_scanned_directory_keeps_its_count() {
    let dir = repo("scope-moved-count");
    write_all(&dir, &[("src/c.ts", renamed(SUM_TOTALS, 3))]);
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    write_all(&dir, &[("lib/c.ts", renamed(SUM_TOTALS, 3))]);
    std::fs::remove_file(dir.join("src/c.ts")).unwrap();

    assert_eq!(doppelscan(&dir, &["baseline", "--prune", "src"]).0, 0);
    assert_eq!(written_entries(&dir)[0].occurrences, 3);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches the rule stopping pruning altogether: code that really is gone
// still goes, once a scan of the whole directory can say so.
#[test]
fn a_check_from_anywhere_sees_what_is_resolved() {
    let dir = repo_with_sibling("scope-root");
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    for args in [&["check", "src"][..], &["check"][..]] {
        let (_, out) = doppelscan(&dir, args);
        assert!(out.contains("1 baseline entry no longer match"), "{out}");
    }

    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);
    let entries = written_entries(&dir);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].places[0].starts_with("src-old/"), "{entries:?}");
}

// Catches lowering a count to the copies under the given path: the copy
// outside is still there, so a root check would fail on untouched code.
#[test]
fn a_clone_spanning_the_scanned_directory_keeps_its_count() {
    let dir = repo_with_spanning_clone("scope-span");
    let (_, out) = doppelscan(&dir, &["check", "src"]);
    assert!(!out.contains("no longer match"), "{out}");

    assert_eq!(doppelscan(&dir, &["baseline", "--prune", "src"]).0, 0);
    assert_eq!(written_entries(&dir)[0].occurrences, 3);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches pruning a spanning clone by its copies inside: one copy left in
// that directory is no group there, so it looks deleted though the clone
// with the copy outside is alive.
#[test]
fn a_lone_copy_of_a_spanning_clone_is_not_gone() {
    let dir = repo_with_spanning_clone("scope-lone");
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline", "--prune", "src"]).0, 0);
    let entries = written_entries(&dir);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].places.len(), 2, "{entries:?}");
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches trusting places a root prune has already cut: the moved copy is
// recorded nowhere, so the entry only looks wholly inside `src`.
#[test]
fn an_entry_with_an_unrecorded_copy_survives_a_prune_from_a_subdirectory() {
    let dir = repo("scope-moved");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    write_all(&dir, &[("other/b.ts", renamed(SUM_TOTALS, 2))]);
    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);
    assert_eq!(written_entries(&dir)[0].places.len(), 1);

    assert_eq!(doppelscan(&dir, &["baseline", "--prune", "src"]).0, 0);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches errors exiting 1, which a CI gate would read as "new
// duplication" instead of a broken setup.
#[test]
fn check_errors_exit_with_two() {
    let dir = repo("error");
    assert_eq!(doppelscan(&dir, &["check", "--base", "no-such-rev"]).0, 2);
}

// Catches the places rule excusing anything between two files that share
// one top-level clone, whatever its size or position.
#[test]
fn a_new_top_level_clone_between_files_that_share_one_is_new() {
    let shared = "for (const row of rows) {\n  if (row.ok) { total += row.value * rate; }\n}\n";
    let added = "while (queue.length > 0) { handle(queue.pop(), limit); }\n";
    let base = scan(&[
        ("a.ts", format!("{shared}const z = 1;\n")),
        ("b.ts", format!("{shared}call();\n")),
    ]);
    let now = scan(&[
        ("a.ts", format!("{shared}const z = 1;\n{added}")),
        ("b.ts", format!("{shared}call();\n{added}")),
    ]);
    assert_eq!(
        now.groups.len(),
        2,
        "the loop and the new while are separate clones"
    );
    assert_eq!(new_groups(&base, &now).len(), 1);
}

// Catches the places rule excusing a clone that grew inside the same
// functions: more duplicated code, not less.
#[test]
fn growing_known_duplication_in_place_is_new() {
    let grown = |n| {
        renamed(SUM_TOTALS, n).replace(
            "    return Math.round",
            "    for (const order of orders) { audit(order.id, order.amount, config.rate); }\n    return Math.round",
        )
    };
    let now = scan(&[("a.ts", grown(1)), ("b.ts", grown(2))]);
    assert_eq!(new_groups(&scan(&two_copies()), &now), vec![1]);
}

// Catches comparing against the tip of the base branch: duplication the
// base removed after the branch forked is not the branch's doing.
#[test]
fn check_against_a_branch_uses_the_fork_point() {
    let dir = repo("fork");
    git(&dir, &["branch", "-m", "trunk"]);
    git(&dir, &["branch", "feature"]);
    git(&dir, &["rm", "-q", "src/b.ts"]);
    git(&dir, &["commit", "-qm", "dedupe"]);
    git(&dir, &["checkout", "-q", "feature"]);
    let (code, out) = doppelscan(&dir, &["check", "--base", "trunk"]);
    assert_eq!(code, 0, "{out}");
}

// Catches `--base main` failing on CI checkouts, which only have the
// remote-tracking branch.
#[test]
fn a_base_branch_falls_back_to_its_remote_tracking_branch() {
    let dir = repo("remote");
    git(&dir, &["update-ref", "refs/remotes/origin/release", "HEAD"]);
    let (code, out) = doppelscan(&dir, &["check", "--base", "release"]);
    assert_eq!(code, 0, "{out}");
}

fn repo_with_fixture_copies(name: &str) -> PathBuf {
    let dir = repo(name);
    std::fs::create_dir_all(dir.join("fixtures")).unwrap();
    for n in [5, 6] {
        let path = dir.join(format!("fixtures/f{n}.ts"));
        std::fs::write(path, renamed(SUM_TOTALS, n)).unwrap();
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "fixtures"]);
    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    dir
}

// Catches the revision side ignoring a directory `--exclude` on files
// deleted since: their copies would raise the known count and excuse the
// new copy.
#[test]
fn an_excluded_directory_is_left_out_of_the_revision_too() {
    let dir = repo_with_fixture_copies("exclude");
    std::fs::remove_dir_all(dir.join("fixtures")).unwrap();
    let (code, out) = doppelscan(&dir, &["check", "--base", "HEAD", "--exclude", "fixtures"]);
    assert_eq!(code, 1, "{out}");
}

// Catches the revision side reading tracked files the working-tree scan
// skips (here gitignored ones), with the same effect.
#[test]
fn files_the_scan_skips_are_left_out_of_the_revision_too() {
    let dir = repo_with_fixture_copies("ignored");
    std::fs::write(dir.join(".gitignore"), "fixtures/\n").unwrap();
    let (code, out) = doppelscan(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 1, "{out}");
}

const CLAMP: &str = r#"
export function clamp(v: number): number {
    if (v < 0) { return 0; }
    if (v > LIMIT) { return LIMIT; }
    return v;
}
"#;

// Catches `check` quietly comparing scans made with different settings
// (or quietly switching to the baseline's): a token minimum changed in a
// flag or config since the baseline was written must stop the check and
// say what differs.
#[test]
fn check_refuses_a_baseline_written_with_other_settings() {
    let dir = repo("settings");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let (code, out) = run(&dir, &["check", "--min-tokens", "30"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("min-tokens") && out.contains("20") && out.contains("30"),
        "{out}"
    );
    std::fs::write(dir.join(".doppelscan.toml"), "min-tokens = 30\n").unwrap();
    assert_eq!(run(&dir, &["check"]).0, 2);
    std::fs::write(dir.join(".doppelscan.toml"), "min-tokens = 20\n").unwrap();
    assert_eq!(run(&dir, &["check"]).0, 0);
}

// Catches lists compared in the order they were given: the same languages
// named in another order are the same scan.
#[test]
fn check_compares_language_lists_as_sets() {
    let dir = repo("set-settings");
    assert_eq!(doppelscan(&dir, &["baseline", "--only", "ts,js"]).0, 0);
    let (code, out) = doppelscan(&dir, &["check", "--only", "js,ts,ts"]);
    assert_eq!(code, 0, "{out}");
}

// Catches an old file being compared at all: fingerprints changed since it
// was written, so its entries match nothing and unchanged code fails the
// check as new instead of saying what to do.
#[test]
fn a_baseline_from_an_older_format_asks_to_be_written_again() {
    let dir = repo("old-format");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let file = dir.join(".doppelscan-baseline.json");
    let current = std::fs::read_to_string(&file).unwrap();
    let mut old: serde_json::Value = serde_json::from_str(&current).unwrap();
    for version in [1, 2] {
        old["version"] = version.into();
        std::fs::write(&file, old.to_string()).unwrap();
        let (code, out) = doppelscan(&dir, &["check"]);
        assert_eq!(code, 2, "{out}");
        assert!(out.contains("doppelscan baseline"), "{out}");
    }
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches prune accepting a scan under other settings: it would drop
// entries that only stopped matching because the settings changed.
#[test]
fn prune_refuses_other_settings() {
    let dir = repo("prune-settings");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let (code, out) = run(&dir, &["baseline", "--prune", "--min-tokens", "30"]);
    assert_eq!(code, 2, "{out}");
}

// Catches the check threshold being ignored, or applied to the report so
// small clones vanish from view instead of only not failing the check.
#[test]
fn small_new_clones_under_the_check_threshold_do_not_fail_it() {
    let dir = repo("threshold");
    std::fs::write(dir.join("src/c.ts"), CLAMP.replace("clamp", "clampA")).unwrap();
    std::fs::write(dir.join("src/d.ts"), CLAMP.replace("clamp", "clampB")).unwrap();
    assert_eq!(doppelscan(&dir, &["check", "--base", "HEAD"]).0, 1);
    let args = ["check", "--base", "HEAD", "--check-min-tokens", "500"];
    let (code, out) = doppelscan(&dir, &args);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("1 smaller new"), "{out}");
    assert!(out.contains("src/c.ts"), "{out}");
    std::fs::write(dir.join(".doppelscan.toml"), "[check]\nmin-tokens = 500\n").unwrap();
    assert_eq!(doppelscan(&dir, &["check", "--base", "HEAD"]).0, 0);
}

// Catches a scoped check missing the repository's baseline (falling back
// to HEAD, which accepts committed copies) and places recorded relative to
// a different directory than the scan shows (a shrunk clone looks new).
#[test]
fn checking_a_subdirectory_uses_the_baseline_above_it() {
    let dir = repo("above");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let shrunk =
        renamed(SUM_TOTALS, 2).replace("return Math.round(total * 100) / 100;", "return total;");
    std::fs::write(dir.join("src/b.ts"), shrunk).unwrap();
    let (code, out) = doppelscan(&dir, &["check", "src"]);
    assert_eq!(code, 0, "{out}");

    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "copy"]);
    assert_eq!(doppelscan(&dir, &["check", "src"]).0, 1);
}

/// One function holding `SUM_TOTALS` with `extra` in front of it, so two
/// functions built with different `extra` share only the summing run.
fn sum_totals_after(name: &str, extra: &str) -> String {
    let body = SUM_TOTALS.split_once('{').unwrap().1;
    format!(
        "export function {name}(orders: Order[], config: Config): number {{
{extra}{body}"
    )
}

const AUDIT: &str = "    audit.record(orders.length, config.rate, Date.now());
    metrics.increment(\"totals\", { region: config.region, tier: config.tier });
    logger.info(\"totals\", { count: orders.length, rate: config.rate });
";

const TRACE: &str = "    tracer.begin(\"sum\", orders.length, config.region, config.tier);
    cache.touch(config.region, config.tier, orders.length, Date.now());
    limiter.take(config.tier, orders.length, config.rate);
";

/// `src/one.ts` and `other/one.ts` share a large function, and so do the
/// two `two.ts`, while both functions hold the same smaller run. From the
/// root that run sits inside clones already reported; from `src` alone it
/// is a clone of `one.ts` and `two.ts`.
fn tree_hiding_a_run_inside_larger_clones(name: &str) -> PathBuf {
    let dir = repo(name);
    std::fs::remove_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    for folder in ["src", "other"] {
        let one = sum_totals_after("one", AUDIT);
        let two = sum_totals_after("two", TRACE);
        std::fs::write(dir.join(folder).join("one.ts"), one).unwrap();
        std::fs::write(dir.join(folder).join("two.ts"), two).unwrap();
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "tree"]);
    dir
}

// Catches a subdirectory being judged on its own: what counts as one clone
// changes with what else is in view, so an untouched tree failed its gate.
#[test]
fn an_unchanged_subdirectory_passes_against_the_baseline_above_it() {
    let dir = tree_hiding_a_run_inside_larger_clones("hidden-run");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    assert_eq!(doppelscan(&dir, &["check"]).0, 0);
    let (code, out) = doppelscan(&dir, &["check", "src"]);
    assert_eq!(code, 0, "{out}");
}

// Catches a scoped check only seeing copies inside the scope: pasting
// existing code into `src` from elsewhere is the duplication it is there for.
#[test]
fn a_subdirectory_check_finds_a_new_copy_of_code_outside_it() {
    let dir = repo("copied-in");
    std::fs::create_dir_all(dir.join("other")).unwrap();
    std::fs::write(dir.join("other/rows.ts"), PARSE_ROWS).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    std::fs::write(dir.join("src/rows.ts"), renamed(PARSE_ROWS, 2)).unwrap();
    let (code, out) = doppelscan(&dir, &["check", "src"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("other/rows.ts"), "{out}");
}

// Catches a scoped check failing on duplication the scope has no part in.
#[test]
fn a_subdirectory_check_ignores_new_copies_elsewhere() {
    let dir = repo("elsewhere");
    std::fs::create_dir_all(dir.join("other")).unwrap();
    std::fs::write(dir.join("other/rows.ts"), PARSE_ROWS).unwrap();
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    std::fs::write(dir.join("other/more.ts"), renamed(PARSE_ROWS, 2)).unwrap();
    assert_eq!(doppelscan(&dir, &["check"]).0, 1);
    let (code, out) = doppelscan(&dir, &["check", "src"]);
    assert_eq!(code, 0, "{out}");
}

// Catches a check taking its config from the path it was given while
// scanning from the baseline: a `root = true` there cuts off the root config
// the baseline was written with, and what it excludes reads as new.
#[test]
fn a_subdirectory_check_uses_the_config_the_baseline_was_written_with() {
    let dir = repo("config-above");
    write_all(
        &dir,
        &[
            (".doppelscan.toml", "exclude = [\"legacy/\"]\n".to_string()),
            ("src/.doppelscan.toml", "root = true\n".to_string()),
            ("legacy/a.ts", renamed(SUM_TOTALS, 3)),
        ],
    );
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let (code, out) = doppelscan(&dir, &["check", "src"]);
    assert_eq!(code, 0, "{out}");
}

// Catches a pass that looked at nothing: the path holds a built-in skip, so
// the scan never reaches it, and "no new duplication" would read as a verdict.
#[test]
fn a_check_of_a_path_no_scan_reaches_says_so() {
    let dir = repo("skipped-path");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    write_all(
        &dir,
        &[
            ("src/bin/a.ts", renamed(PARSE_ROWS, 1)),
            ("src/bin/b.ts", renamed(PARSE_ROWS, 2)),
        ],
    );
    let (code, out) = doppelscan(&dir, &["check", "src/bin"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("not checked"), "{out}");
}

// Catches a baseline of one folder kept above it being compared with
// everything around that folder: code it never covered would read as new.
#[test]
fn a_baseline_of_a_subdirectory_stays_about_that_subdirectory() {
    let dir = repo("baseline-of-src");
    write_all(
        &dir,
        &[
            ("other/x.ts", renamed(PARSE_ROWS, 1)),
            ("other/y.ts", renamed(PARSE_ROWS, 2)),
        ],
    );
    let file = DEFAULT_FILE;
    assert_eq!(doppelscan(&dir, &["baseline", "src", "-o", file]).0, 0);
    let (code, out) = doppelscan(&dir, &["check", "src"]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 2, "{out}");
}

// Catches a streamed JSON report drifting from what a serde_json Value
// writes (keys sorted at every level, pretty-printed, one trailing
// newline), which scripts reading it have always seen.
#[test]
fn json_reports_are_written_as_a_sorted_value() {
    let dir = repo("json");
    std::fs::write(dir.join("src/c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    for (args, list) in [
        (&["--format", "json"][..], "groups"),
        (&["check", "--format", "json"][..], "new"),
    ] {
        let (_, out) = doppelscan(&dir, args);
        let report: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(!report[list].as_array().unwrap().is_empty(), "{out}");
        assert_eq!(serde_json::to_string_pretty(&report).unwrap() + "\n", out);
    }
}

fn repo_with_copy_at(name: &str, path: &str) -> PathBuf {
    let dir = repo(name);
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, renamed(SUM_TOTALS, 3)).unwrap();
    dir
}

// Catches skipping every dot-directory: CI scripts under `.github` or
// `.buildkite` are source like any other and get copied like any other.
#[test]
fn copies_in_a_dot_directory_are_found() {
    let dir = repo_with_copy_at("dotdir", ".github/scripts/c.ts");
    let (code, out) = doppelscan(&dir, &["."]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(".github/scripts/c.ts"), "{out}");
}

// Catches the revision side still skipping dot-directories the working
// tree now scans: a committed copy there would read as new on every check.
#[test]
fn a_committed_copy_in_a_dot_directory_is_not_new() {
    let dir = repo_with_copy_at("dotrev", ".github/scripts/c.ts");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "ci copy"]);
    let (code, out) = doppelscan(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 0, "{out}");
}

// Catches dropping tool output from the skip list: editor local history
// keeps a full copy of every saved file and is rarely gitignored.
#[test]
fn copies_in_local_history_are_skipped() {
    let dir = repo_with_copy_at("history", ".history/src/a_20261028.ts");
    let (_, out) = doppelscan(&dir, &["."]);
    assert!(!out.contains(".history"), "{out}");
}

// Catches dropping the generated-source names from the skip list: build
// tools write these without a "generated" header (Elasticsearch's
// `src/main/generated` has none), so the path is the only sign.
#[test]
fn copies_in_generated_source_dirs_are_skipped() {
    let dir = repo_with_copy_at("gendir", "src/main/generated/c.ts");
    let (_, out) = doppelscan(&dir, &["."]);
    assert!(!out.contains("generated"), "{out}");
}

// Catches matching generated directories by prefix or by a short name:
// `gen` often holds a hand-written code generator.
#[test]
fn copies_in_a_gen_dir_are_found() {
    let dir = repo_with_copy_at("gen", "tools/gen/c.ts");
    let (code, out) = doppelscan(&dir, &["."]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("tools/gen/c.ts"), "{out}");
}

const CARD_TSX: &str = r#"export function Card({ title, items }: Props) {
    return (
        <section className="card">
            <h2 className="title">
                {title}
            </h2>
            <ul>
                {items.map((item) => (
                    <li key={item.id}>
                        {item.label}
                    </li>
                ))}
            </ul>
        </section>
    );
}
"#;

fn commit_lf_then_check_crlf(name: &str, files: &[(&str, String)]) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("doppelscan-crlf-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    for (path, text) in files {
        std::fs::write(dir.join(path), text).unwrap();
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    // Written directly rather than through core.autocrlf, so the working
    // tree is CRLF on every platform.
    for (path, text) in files {
        std::fs::write(dir.join(path), text.replace('\n', "\r\n")).unwrap();
    }
    doppelscan(&dir, &["check", "--base", "HEAD"])
}

// Catches JSX text parsing to a different token count under CRLF, which
// fails `check` on a clean Windows checkout with core.autocrlf=true.
#[test]
fn check_is_clean_when_only_line_endings_differ() {
    let files = [
        ("a.tsx", CARD_TSX.to_string()),
        ("b.tsx", CARD_TSX.replace("Card", "Tile")),
    ];
    let (code, out) = commit_lf_then_check_crlf("tsx", &files);
    assert_eq!(code, 0, "{out}");
}

// Catches the generated-file check reading raw bytes: the CRs push the
// marker out of the head it searches, so generated files committed with LF
// turn into new clones in a CRLF working tree.
#[test]
fn check_skips_a_generated_file_under_crlf_too() {
    let padding = "// padding line, 24 bytes\n".repeat(38);
    let generated = format!("{padding}// @generated\n{SUM_TOTALS}");
    let files = [
        ("a.ts", generated.clone()),
        ("b.ts", generated.replace("sumTotals", "sumAll")),
    ];
    let (code, out) = commit_lf_then_check_crlf("generated", &files);
    assert_eq!(code, 0, "{out}");
}

const SIZE_CAP: usize = 1 << 20;

/// A third copy padded to `size` bytes with lines CRLF grows by a third.
fn padded_copy(size: usize) -> String {
    let code = renamed(SUM_TOTALS, 3);
    let padding = size.saturating_sub(code.len()) / 3;
    code + &"//\n".repeat(padding)
}

fn with_crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

// Catches the revision side capping raw blob bytes: a file committed with
// CRLF drops out there but not from its LF checkout, so its unchanged clone
// reads as new on a clean tree.
#[test]
fn the_size_cap_judges_a_blob_by_its_normalised_size() {
    let dir = repo("cap-blob");
    let copy = padded_copy(SIZE_CAP * 9 / 10);
    write_all(&dir, &[("src/big.ts", with_crlf(&copy))]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "crlf copy"]);
    write_all(&dir, &[("src/big.ts", copy)]);
    let (code, out) = doppelscan(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 0, "{out}");
}

// Catches the working tree capping raw checkout bytes: the same file would
// be scanned or not depending on the platform's line endings.
#[test]
fn the_size_cap_judges_a_checkout_by_its_normalised_size() {
    let dir = repo("cap-file");
    let copy = with_crlf(&padded_copy(SIZE_CAP * 9 / 10));
    write_all(&dir, &[("src/big.ts", copy)]);
    let (code, out) = doppelscan(&dir, &["."]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("src/big.ts"), "{out}");
}

// Catches loosening the cap to the raw limit: a file past it after
// normalising must stay out on both sides, or the revision would excuse a
// copy by one no scan of the tree reads.
#[test]
fn a_file_over_the_cap_once_normalised_stays_out_on_both_sides() {
    let dir = repo("cap-over");
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    write_all(&dir, &[("src/big.ts", padded_copy(SIZE_CAP * 3 / 2))]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "big copy"]);
    let (_, out) = doppelscan(&dir, &["."]);
    assert!(!out.contains("src/big.ts"), "{out}");

    write_all(&dir, &[("src/b.ts", renamed(SUM_TOTALS, 2))]);
    let (code, out) = doppelscan(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 1, "{out}");
}

// Catches `--exclude` being judged on the revision side with the scan root's
// name stripped from a path that never carried it: a directory named like
// the root loses its first component there, so an unchanged tree looks new.
#[test]
fn a_directory_named_like_the_relative_root_is_excluded_alike() {
    let dir = repo("same-name");
    std::fs::remove_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("a/a/x")).unwrap();
    std::fs::create_dir_all(dir.join("a/y")).unwrap();
    std::fs::write(dir.join("a/a/x/one.ts"), renamed(SUM_TOTALS, 1)).unwrap();
    std::fs::write(dir.join("a/a/x/two.ts"), renamed(SUM_TOTALS, 2)).unwrap();
    std::fs::write(dir.join("a/y/one.ts"), CLAMP).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "nested"]);
    let (code, out) = doppelscan(&dir, &["check", "a", "--exclude", "x/*.ts"]);
    assert_eq!(code, 0, "{out}");
}

// Catches git being run with a file as its working directory, which
// surfaces as an unknown HEAD and hides the real reason.
#[test]
fn check_on_a_file_says_it_needs_a_directory() {
    let dir = repo("file-root");
    let (code, out) = doppelscan(&dir, &["check", "src/a.ts"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("needs a directory"), "{out}");
}

/// Commits a tree holding `tracked`'s blob again under a name that is not
/// UTF-8, which git allows and which no platform's checkout has to show.
fn commit_non_utf8_path(dir: &Path, tracked: &str) {
    let oid = git_output(dir, &["rev-parse", &format!("HEAD:{tracked}")]);
    let mut entry = format!("100644 {} 0\t", oid.trim()).into_bytes();
    entry.extend_from_slice(b"bad\xff.ts\0");
    let mut add = Command::new("git")
        .args(["update-index", "-z", "--index-info"])
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    add.stdin.take().unwrap().write_all(&entry).unwrap();
    assert!(add.wait().unwrap().success());
    git(dir, &["commit", "-qm", "odd name"]);
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

// Catches one odd path failing the whole listing, and with it every check
// in the repository until someone renames a file they may not own.
#[test]
fn a_non_utf8_path_at_the_revision_is_skipped() {
    let dir = repo("non-utf8");
    commit_non_utf8_path(&dir, "src/a.ts");
    let (code, out) = doppelscan(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 0, "{out}");
}

// Catches the walk scanning a file inside a directory the revision side
// must skip: an unchanged clone there would read as new on every check.
// Linux only: APFS and NTFS refuse a path that is not valid UTF-8.
#[cfg(target_os = "linux")]
#[test]
fn a_non_utf8_directory_is_left_out_of_the_walk() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = repo("non-utf8-dir");
    let odd = dir.join(std::ffi::OsStr::from_bytes(b"bad\xff"));
    std::fs::create_dir_all(&odd).unwrap();
    std::fs::write(odd.join("c.ts"), renamed(SUM_TOTALS, 3)).unwrap();
    let (code, out) = doppelscan(&dir, &["--format", "json"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("c.ts"), "{out}");
}

// Catches a case-only rename reading as new where the filesystem folds
// case: the old name still "exists", but no scanned path spells it that way,
// so the revision side dropped it. Trivially green on a case-sensitive
// filesystem, where the old name is simply gone.
#[test]
fn an_uncommitted_case_only_rename_is_not_new() {
    let dir = repo("case-rename");
    git(&dir, &["mv", "-f", "src/a.ts", "src/A.ts"]);
    let (code, out) = doppelscan(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 0, "{out}");
}

// Fields of unlike shapes: more tokens than `SUM_TOTALS` as a method, and no
// shorter run inside that recurs and surfaces as a group of its own.
const FIELDS: &str = r#"
  id: string = "x";
  count: number = 1;
  readonly flag: boolean = true;
  private rows?: Order[] = [];
  static limit = 10 * 2;
  lookup: Map<string, number> = new Map();
  pending = new Set<Order>();
  handler = (x: number) => x + 1;
  range: [number, number] = [0, 1];
  label: string | null = null;
  meta = { a: 1, b: 2 };
  retries: number = Math.max(1, 3);
"#;

fn method(code: &str) -> String {
    code.replace("export function ", "")
}

// `C` and `D` share the fields, and a member that differs stops anything
// added after it from extending that run.
fn class_c(added: &str) -> String {
    format!("class C {{{FIELDS}  size(): number {{ return 1; }}\n{added}}}\n")
}

fn class_d(added: &str) -> String {
    format!("class D {{{FIELDS}  label(x: boolean) {{ if (x) {{ log(x); }} }}\n{added}}}\n")
}

// Catches excusing a group for sitting inside an accepted place: the class
// holds an accepted clone, but the method is not part of it.
#[test]
fn a_new_method_in_classes_sharing_an_accepted_clone_is_new() {
    let dir = repo("new-method");
    let foo = method(SUM_TOTALS);
    write_all(
        &dir,
        &[("src/a.ts", class_c("")), ("src/b.ts", class_d(""))],
    );
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    assert_eq!(
        written_entries(&dir)[0].places,
        ["src/a.ts#C", "src/b.ts#D"]
    );
    assert!(written_entries(&dir)[0].tokens > 90);

    write_all(
        &dir,
        &[("src/a.ts", class_c(&foo)), ("src/b.ts", class_d(&foo))],
    );
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("sumTotals"), "{out}");
}

const AUDIT_LOOP: &str = r#"
    for (const entry of ledger.entries()) {
        audit.record(entry.id, entry.amount, entry.currency);
    }
"#;

fn body_with(name: &str, differs: &str, added: &str) -> String {
    let statements = SUM_TOTALS.lines().skip(2);
    let statements: Vec<&str> = statements.take_while(|l| !l.starts_with('}')).collect();
    let body = statements.join("\n");
    format!("function {name}(orders: Order[], config: Config) {{\n{body}\n{differs}\n{added}}}\n")
}

// Catches trusting an exact place: the statements are new, though the entry
// sits at the very same functions and is larger than they are.
#[test]
fn new_statements_in_functions_sharing_an_accepted_clone_are_new() {
    let side = |added| {
        vec![
            ("a.ts", body_with("f", "first();", added)),
            ("b.ts", body_with("g", "if (ready) { second(1); }", added)),
        ]
    };
    let base = scan(&side(""));
    let baseline = Baseline::from_analysis(&base, &options());
    assert_eq!(baseline.groups[0].places, ["a.ts#f", "b.ts#g"]);
    let now = scan(&side(AUDIT_LOOP));
    assert_eq!(now.groups.len(), 2, "the loop is a clone of its own");
    assert!(now.groups[1].tokens <= baseline.groups[0].tokens);
    assert_eq!(baseline.new_groups(&now).len(), 1);
}

// Catches comparing token streams with their separators: the accepted
// whole-class clone holds the `;` between fields, the run cut from it does
// not, and nothing was added.
#[test]
fn a_whole_class_shrunk_to_a_run_of_its_fields_is_not_new() {
    let class = |name: &str, fields: &str| format!("class {name} {{{fields}}}\n");
    let edited = FIELDS.replace("retries: number", "declare retries");
    let base = scan(&[("a.ts", class("C", FIELDS)), ("b.ts", class("D", FIELDS))]);
    let now = scan(&[("a.ts", class("C", FIELDS)), ("b.ts", class("D", &edited))]);
    assert_eq!(base.group(1, false).kind, "node");
    assert_eq!(now.groups.len(), 1);
    assert_eq!(now.group(1, false).kind, "run");
    assert!(new_groups(&base, &now).is_empty());
}

fn drop_shapes(dir: &Path) {
    let file = dir.join(".doppelscan-baseline.json");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut baseline: serde_json::Value = serde_json::from_str(&text).unwrap();
    for entry in baseline["groups"].as_array_mut().unwrap() {
        entry.as_object_mut().unwrap().remove("shape");
    }
    std::fs::write(&file, baseline.to_string()).unwrap();
}

// Catches treating a missing shape as holding nothing: a baseline written
// before shapes were recorded would fail on every clone that shrank.
#[test]
fn a_baseline_without_shapes_still_excuses_a_shrunk_clone() {
    let dir = repo("no-shape");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    drop_shapes(&dir);
    let shrunk =
        renamed(SUM_TOTALS, 2).replace("return Math.round(total * 100) / 100;", "return total;");
    std::fs::write(dir.join("src/b.ts"), shrunk).unwrap();
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

const FORMAT_ROW: &str = r#"
export function formatRow(row: Row, width: number): string {
    const padded = row.name.padEnd(width, " ");
    const value = row.value.toFixed(2).padStart(10, " ");
    return `${row.id} ${padded} ${value}`;
}
"#;

// Three definitions: no one of them names the clone, so its place is the
// bare file.
fn three_definitions() -> String {
    [SUM_TOTALS, PARSE_ROWS, FORMAT_ROW].concat()
}

fn with_edited_total() -> String {
    three_definitions().replace("return Math.round(total * 100) / 100;", "return total;")
}

fn repo_sharing_definitions(name: &str) -> PathBuf {
    let dir = repo(name);
    write_all(
        &dir,
        &[
            ("src/a.ts", three_definitions()),
            ("src/b.ts", three_definitions()),
        ],
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "definitions"]);
    dir
}

// Catches a bare file enclosing nothing: an edit splits the top-level clone
// into a run still at the file and a piece inside `sumTotals`, and both are
// what the entry held. Pruning has to see that too, or the entry goes and
// the same code fails the next check.
#[test]
fn a_top_level_clone_cut_in_two_by_an_edit_is_not_new() {
    let dir = repo_sharing_definitions("top-level-split");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let before = written_entries(&dir);
    assert_eq!(before[0].places, ["src/a.ts", "src/b.ts"]);
    std::fs::write(dir.join("src/b.ts"), with_edited_total()).unwrap();
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");

    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);
    assert_eq!(written_entries(&dir), before);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Catches fixing only the file comparison: without a baseline the accepted
// side comes from scanning HEAD.
#[test]
fn a_top_level_clone_cut_in_two_since_head_is_not_new() {
    let dir = repo_sharing_definitions("top-level-head");
    std::fs::write(dir.join("src/b.ts"), with_edited_total()).unwrap();
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

fn new_function() -> String {
    "\nfunction added(queue: Job[], limit: number) {\n    while (queue.length > limit) {\n        handle(queue.pop(), limit);\n    }\n}\n"
        .to_string()
}

// What differs after the definitions keeps `added` out of the run they form.
fn sharing_definitions_with(added: &str) -> Analysis {
    let shared = three_definitions();
    scan(&[
        (
            "a.ts",
            format!(
                "{shared}const z = 1;
{added}"
            ),
        ),
        (
            "b.ts",
            format!(
                "{shared}call();
{added}"
            ),
        ),
    ])
}

// Catches a bare file excusing whatever is inside it: the function is new,
// and smaller than the top-level clone both files already share.
#[test]
fn a_new_function_in_files_sharing_a_top_level_clone_is_new() {
    let base = sharing_definitions_with("");
    let now = sharing_definitions_with(&new_function());
    assert_eq!(now.groups.len(), 2, "the function is a clone of its own");
    assert_eq!(new_groups(&base, &now).len(), 1);
}

// Catches widening the place rule for entries with no shape to narrow it:
// there a bare file would excuse every later clone between the two files.
#[test]
fn a_bare_file_without_a_shape_excuses_nothing_inside_it() {
    let base = sharing_definitions_with("");
    let mut baseline = Baseline::from_analysis(&base, &options());
    baseline.groups[0].shape.clear();
    let now = sharing_definitions_with(&new_function());
    assert_eq!(baseline.new_groups(&now).len(), 1);

    let tokens = baseline.groups[0].tokens;
    let half_named = accepted("mixed", 2, tokens, &["a.ts#added", "b.ts"]);
    baseline.groups = vec![half_named];
    let flagged = baseline.new_groups(&now);
    assert_eq!(flagged.len(), 2, "both groups: {flagged:?}");
}

// Catches a prune that pairs places less widely than the check: with no
// piece left at the bare file, only the one inside `sumTotals` holds the
// entry, and losing its places fails the next check on the same code.
#[test]
fn pruning_keeps_a_bare_file_only_a_piece_inside_it_holds() {
    let dir = repo_sharing_definitions("top-level-piece");
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    let before = written_entries(&dir);
    let cut = with_edited_total().replace(
        "export function parseRows",
        "export const parseRows = 1;
function unused",
    );
    let cut = cut
        .replace("rows.push", "rows.unshift")
        .replace("padEnd", "trim");
    std::fs::write(dir.join("src/b.ts"), cut).unwrap();
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");

    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);
    assert_eq!(written_entries(&dir), before);
    let (code, out) = doppelscan(&dir, &["check"]);
    assert_eq!(code, 0, "{out}");
}

// Statements of unlike shapes, so the function is one clone with no shorter
// run in it that recurs.
const SETTLE: &str = r#"
export function settle(orders: Order[], config: Config): number {
    const paid = orders.filter((o) => o.status === "paid");
    const gross = paid.reduce((sum, o) => sum + o.amount, 0);
    audit.record(paid.length, gross);
    const fee = Math.ceil(gross * config.rate);
    if (fee > config.cap) { throw new Error("fee"); }
    const net = gross - fee - config.flat;
    return Math.round(net * 100) / 100;
}
"#;

fn settle_without(cut: &[&str]) -> String {
    let drop_cut = |text: String, line: &&str| text.replace(*line, "");
    cut.iter().fold(SETTLE.to_string(), drop_cut)
}

fn repo_of_settle(name: &str) -> PathBuf {
    let dir = repo(name);
    let code = settle_without(&[]);
    write_all(&dir, &[("src/a.ts", code.clone()), ("src/b.ts", code)]);
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    dir
}

const AUDIT_LINE: &str = "    audit.record(paid.length, gross);\n";

// Catches a shrink rule wanting the clone contiguous in its entry: taking a
// statement out of the middle of both copies adds no code.
#[test]
fn deleting_a_middle_statement_from_every_copy_is_not_new() {
    let dir = repo_of_settle("cut-middle");
    let code = settle_without(&[AUDIT_LINE]);
    write_all(&dir, &[("src/a.ts", code.clone()), ("src/b.ts", code)]);
    let (status, out) = doppelscan(&dir, &["check"]);
    assert_eq!(status, 0, "{out}");
}

// Catches a rule that takes one cut only: each cleanup would fail the check
// as soon as it touched a second spot.
#[test]
fn deleting_two_statements_apart_is_not_new() {
    let dir = repo_of_settle("cut-twice");
    let fee_line = "    const fee = Math.ceil(gross * config.rate);\n";
    let code = settle_without(&[AUDIT_LINE, fee_line]).replace("- fee ", "");
    let code = code.replace(
        "    if (fee > config.cap) { throw new Error(\"fee\"); }\n",
        "",
    );
    write_all(&dir, &[("src/a.ts", code.clone()), ("src/b.ts", code)]);
    let (status, out) = doppelscan(&dir, &["check"]);
    assert_eq!(status, 0, "{out}");
}

// Catches a prune that only counts contiguous groups as leaning on an entry:
// it would drop the entry of a clone that is still there, and the next check
// would fail on it.
#[test]
fn pruning_keeps_the_entry_of_a_clone_with_a_statement_deleted() {
    let dir = repo_of_settle("cut-prune");
    let before = written_entries(&dir);
    let code = settle_without(&[AUDIT_LINE]);
    write_all(&dir, &[("src/a.ts", code.clone()), ("src/b.ts", code)]);
    assert_eq!(doppelscan(&dir, &["baseline", "--prune"]).0, 0);
    assert_eq!(written_entries(&dir), before);
    let (status, out) = doppelscan(&dir, &["check"]);
    assert_eq!(status, 0, "{out}");
}

// Every other field of `FIELDS`, with other names. Its tokens all appear in
// `FIELDS` in order, which is what a bare subsequence test would accept, yet
// this is new code spread over the whole accepted clone.
const SCATTERED_FIELDS: &str = r#"
  total: number = 2;
  active: boolean = true;
  maximum = 20 * 4;
  waiting = new Set<Order>();
  window: [number, number] = [2, 9];
  extra = { c: 3, d: 4 };
"#;

// Catches the shrink rule loosening into a subsequence test: over so few
// token kinds, most new code is a subsequence of a long enough accepted clone.
#[test]
fn new_fields_scattered_through_an_accepted_clone_are_new() {
    let dir = repo("scattered");
    write_all(
        &dir,
        &[("src/a.ts", class_c("")), ("src/b.ts", class_d(""))],
    );
    assert_eq!(doppelscan(&dir, &["baseline"]).0, 0);
    write_all(
        &dir,
        &[
            ("src/a.ts", class_c(SCATTERED_FIELDS)),
            ("src/b.ts", class_d(SCATTERED_FIELDS)),
        ],
    );
    let (status, out) = doppelscan(&dir, &["check"]);
    assert_eq!(status, 1, "{out}");
}
