mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use common::git;
use doppelscan::baseline::Baseline;
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
