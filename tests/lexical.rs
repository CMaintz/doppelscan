use std::path::PathBuf;
use std::time::Instant;

use doppelscan::baseline::Baseline;
use doppelscan::engine::{Engine, ScanOptions, Source, analyze, index_sources};
use doppelscan::lang::{LANGS, Syntax, lang_index_by_name, lang_index_for_path};
use doppelscan::report::{Analysis, Group};
use doppelscan::variation::Kind;

fn scan_with(files: &[(&str, &str)], strict: bool) -> Analysis {
    let sources = files
        .iter()
        .map(|(path, text)| Source {
            path: path.to_string(),
            lang: lang_index_for_path(path.as_ref()).expect("known extension"),
            text: text.to_string(),
        })
        .collect();
    let opts = ScanOptions {
        min_tokens: 20,
        strict,
        ..ScanOptions::default()
    };
    analyze(index_sources(sources, strict), &opts, Instant::now())
}

fn scan(files: &[(&str, &str)]) -> Analysis {
    scan_with(files, false)
}

fn groups(a: &Analysis) -> Vec<Group> {
    (1..=a.groups.len()).map(|id| a.group(id, false)).collect()
}

fn start_lines(a: &Analysis) -> Vec<u32> {
    groups(a)
        .iter()
        .flat_map(|g| g.occurrences.iter().map(|o| o.start_line))
        .collect()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("doppelscan-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const SUM_PAID: &str = r#"
/* Totals for the invoice screen. */
static long sum_paid(const struct order *orders, int count) {
    long total = 0;
    for (int i = 0; i < count; i++) {
        if (orders[i].status == PAID) {
            total += orders[i].amount * 3;
        }
    }
    return total / 100;
}
"#;

const SUM_REFUNDED: &str = r#"
// Refund totals, kept apart from payments.
static long sum_refunded(const struct order *refunds, int n) {
    long sum = 0;
    for (int j = 0; j < n; j++) {
        if (refunds[j].status == REFUNDED) {
            sum += refunds[j].amount * 7;
        }
    }
    return sum / 100;
}
"#;

const STEPS: &str = "      - uses: actions/checkout@v4\n        with:\n          fetch-depth: 0\n      - uses: jdx/mise-action@v2\n        with:\n          version: 2026.9.18\n      - run: mise run gate\n";

// Catches a language without a grammar being skipped, its names being
// hashed by text, or its renamed locals counting against the parameter
// density: a renamed copy in C is one clone, with its names reported as
// `name` since the tokens cannot say what they are.
#[test]
fn renamed_c_copies_are_one_clone() {
    let a = scan(&[("a.c", SUM_PAID), ("b.c", SUM_REFUNDED)]);
    assert_eq!(a.groups.len(), 1, "{:?}", a.groups);
    let g = a.group(1, false);
    let paid = g.parameters.iter().find(|v| v.values[0] == "PAID").unwrap();
    assert_eq!(paid.kind, Kind::Name);
    assert!(g.parameters.iter().any(|v| v.values[0] == "3"));
}

// Catches the default anonymising carrying over to --strict.
#[test]
fn strict_does_not_match_renamed_c_copies() {
    let a = scan_with(&[("a.c", SUM_PAID), ("b.c", SUM_REFUNDED)], true);
    assert!(a.groups.is_empty(), "{:?}", a.groups);
}

// Catches a C header and source being separate languages: an inline
// function copied from a .h into a .cpp is a clone.
#[test]
fn a_header_matches_a_cpp_source() {
    let a = scan(&[("util.h", SUM_PAID), ("impl.cpp", SUM_REFUNDED)]);
    assert_eq!(a.groups.len(), 1);
}

// Catches keywords being anonymised like names: `if` and `return` are
// structure, so a different statement shape is not a clone.
#[test]
fn keywords_are_not_parameters() {
    let a = "void f(int a, int b) {\n    if (check(a, b, 1, 2)) return compute(a, b, 3, 4);\n    while (check(a, b, 5, 6)) return compute(b, a, 7, 8);\n}\n";
    let b = "void g(int a, int b) {\n    foo (check(a, b, 1, 2)) bar compute(a, b, 3, 4);\n    baz (check(a, b, 5, 6)) qux compute(b, a, 7, 8);\n}\n";
    assert!(scan(&[("a.c", a), ("b.c", b)]).groups.is_empty());
}

// Catches strings and numbers sharing a placeholder: `f("x")` is not a
// copy of `f(1)`.
#[test]
fn a_string_is_not_a_number() {
    let a = "void f(void) {\n    put(alpha, beta, \"a\");\n    send(gamma, delta, \"b\");\n    keep(alpha, gamma, \"c\");\n}\n";
    let b = "void f(void) {\n    put(alpha, beta, 1);\n    send(gamma, delta, 2);\n    keep(alpha, gamma, 3);\n}\n";
    assert!(scan(&[("a.c", a), ("b.c", b)]).groups.is_empty());
}

// Catches a comment marker inside a string starting a comment, which
// would hide the code after it and pair statements that differ.
#[test]
fn comment_marker_in_a_string_is_not_a_comment() {
    let a = "void f(void) {\n    log(\"http://x\"); alpha(one, two, three, four);\n    log(\"http://y\"); beta(five, six, seven);\n    log(\"http://z\"); gamma(eight, nine);\n}\n";
    let b = "void f(void) {\n    log(\"http://x\");\n    log(\"http://y\");\n    log(\"http://z\");\n}\n";
    assert!(scan(&[("a.c", a), ("b.c", b)]).groups.is_empty());
}

// Catches a line break inside parentheses ending the statement: the same
// call wrapped differently is still the same code.
#[test]
fn wrapped_arguments_still_match() {
    let a = "int f(void) {\n    int r = combine(alpha, beta, gamma, delta);\n    return finish(r, alpha, beta, gamma, delta);\n}\n";
    let b = "int g(void) {\n    int r = combine(alpha,\n        beta, gamma, delta);\n    return finish(r, alpha, beta,\n        gamma, delta);\n}\n";
    assert_eq!(scan(&[("a.c", a), ("b.c", b)]).groups.len(), 1);
}

// Catches a brace on its own line (Allman style) ending the header: the
// same function in either brace style is one clone.
#[test]
fn allman_and_kr_braces_match() {
    let allman = SUM_PAID.replace(") {\n", ")\n{\n");
    assert_eq!(
        scan(&[("a.c", &allman), ("b.c", SUM_REFUNDED)])
            .groups
            .len(),
        1
    );
}

// Catches statements in a brace block not being labelled with the
// function they are in, which baselines need to follow a clone.
#[test]
fn a_run_inside_a_function_is_labelled_with_it() {
    let run = "    open_log(path, mode, flags);\n    write_header(log, version, stamp);\n    flush_all(log, buffers, count);\n";
    let a = format!("void first(void) {{\n    int x = 1;\n{run}}}\n");
    let b = format!("void second(void) {{\n    char *s = name(3, 4);\n{run}}}\n");
    let found = scan(&[("a.c", &a), ("b.c", &b)]);
    let scopes: Vec<_> = groups(&found)[0]
        .occurrences
        .iter()
        .map(|o| o.scope.clone())
        .collect();
    assert_eq!(scopes, [Some("first".into()), Some("second".into())]);
}

// Catches import lines taking part in clones: a shared import block is
// not duplication anyone can extract.
#[test]
fn shared_imports_are_not_a_clone() {
    let imports = "import scala.collection.mutable.ListBuffer\nimport scala.concurrent.duration.Duration\nimport scala.concurrent.ExecutionContext.Implicits.global\nimport scala.util.control.NonFatal\nimport java.util.concurrent.TimeUnit\n";
    let a = format!("{imports}\nobject A {{ def run(): Int = 1 }}\n");
    let b = format!("{imports}\nobject B {{ val label = \"b\" }}\n");
    assert!(scan(&[("a.scala", &a), ("b.scala", &b)]).groups.is_empty());
}

// Catches an import inside a block dropping the statements before it from
// the block's hash, which pairs blocks whose code differs.
#[test]
fn an_import_inside_a_block_does_not_hide_its_code() {
    let a = "struct Alpha {\n    int first(int x) { return x + 1 + 2 + 3 + 4 + 5 + 6; }\n    using Base::Base;\n    int tail(int y) { return y + 1; }\n};\n";
    let b = "struct Beta {\n    int first(int x) { return x - 1 + 2 + 3 + 4 + 5 + 6; }\n    using Base::Base;\n    int tail(int y) { return y + 1; }\n};\n";
    assert!(scan(&[("a.cpp", a), ("b.cpp", b)]).groups.is_empty());
}

// Catches a line opening with `.` or `&` continuing a statement whose
// block already closed: each SCSS rule is a statement of its own.
#[test]
fn scss_rules_are_separate_statements() {
    let header = ".header {\n  display: flex;\n  gap: 8px;\n}\n";
    let shared = ".button {\n  color: red;\n  padding: 4px 8px;\n  border: 1px solid black;\n}\n.card {\n  margin: 0 auto;\n  box-shadow: 0 1px 2px gray;\n}\n";
    let a = format!("{header}{shared}");
    let b = format!(".nav {{\n  float: left;\n}}\n{shared}");
    assert_eq!(
        start_lines(&scan(&[("a.scss", &a), ("b.scss", &b)])),
        [5, 4]
    );
}

// Catches `//` after a colon starting a comment: an unquoted URL would eat
// the rest of the line, its closing brace included, and swallow the next
// rule.
#[test]
fn an_unquoted_url_is_not_a_comment() {
    let tail = ".tail {\n  color: red;\n  padding: 4px 8px;\n  margin: 0 auto;\n  border: 1px solid black;\n}\n";
    let a = format!(".footer {{ margin: 1px; background: url(http://cdn/x.png); }}\n{tail}");
    let b = format!(".footer {{ margin: 1px; }}\n{tail}");
    assert_eq!(
        start_lines(&scan(&[("a.scss", &a), ("b.scss", &b)])),
        [2, 2]
    );
}

// Catches `#` mid-word starting a comment: shell's `$#` would eat the
// closing bracket and fold the rest of the file into one statement.
#[test]
fn shell_dollar_hash_is_not_a_comment() {
    let shared = "mkdir -p \"$out/bin\" \"$out/lib\"\ncp build/app \"$out/bin/app\"\ncp -r build/lib/* \"$out/lib/\"\nchmod 755 \"$out/bin/app\"\n";
    let a = format!("if [ $# -eq 0 ]; then\n  exit 1\nfi\n{shared}");
    let b = format!("out=/opt/app\n{shared}");
    assert_eq!(scan(&[("a.sh", &a), ("b.sh", &b)]).groups.len(), 1);
}

// Catches a block the file never closes keeping only its header in the
// opening statement's hash, which pairs whatever follows it.
#[test]
fn an_unclosed_block_still_counts_its_content() {
    let head = "count() {\n  echo start alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi\n";
    let a = format!("{head}  if ready then go fi\n");
    let b = format!("{head}  while ready do go done\n");
    assert!(scan(&[("a.sh", &a), ("b.sh", &b)]).groups.is_empty());
}

// Catches renamed names that differ only once each escaping the density
// veto: lines of the same shape with different words are not a clone.
#[test]
fn same_shaped_lines_with_different_words_are_not_a_clone() {
    let a = "mkdir build output cache\ndocker pull redis\ngit fetch origin main now\nls tmp\nkubectl get pods all\necho one two three four five\n";
    let b = "touch notes todo list\nbrew install jq\ncargo build release fast please\ncd src\nhelm list charts now\nprintf a b c d e\n";
    assert!(scan(&[("a.sh", a), ("b.sh", b)]).groups.is_empty());
}

// Catches SQL's FROM being taken for an import: it is part of the query,
// so the copied query is reported from its first line.
#[test]
fn a_copied_query_includes_its_from_line() {
    let query = "select o.id, o.total, c.name\nfrom orders o\njoin customers c on c.id = o.customer_id\nwhere o.status = 'paid'\norder by o.created_at desc;\n";
    let a = format!("delete from audit where age > 30;\n{query}");
    let b = format!("update stats set n = 0;\n{query}");
    assert_eq!(start_lines(&scan(&[("a.sql", &a), ("b.sql", &b)])), [2, 2]);
}

// Catches a triple-quoted string ending at its third quote, which reads
// its content as code: queries in two copies may differ like any literal.
#[test]
fn triple_quoted_strings_are_one_literal() {
    let wrap = |query: &str| {
        format!(
            "object Repo {{\n  def find(db: Db, id: Long): Row = {{\n    val q = \"\"\"\n{query}\n    \"\"\"\n    db.run(q, id).head\n  }}\n}}\n"
        )
    };
    let a = wrap("select * from rows where id = ? and live");
    let b = wrap("update rows set seen = now() where id = ?");
    assert_eq!(scan(&[("a.scala", &a), ("b.scala", &b)]).groups.len(), 1);
}

// Catches an unclosed string taking its newline with it, which merges
// the next line into the same statement.
#[test]
fn an_unclosed_string_ends_at_its_line() {
    let a = format!("one: 'unclosed value\nsteps:\n{STEPS}");
    let b = format!("name: other\nsteps:\n{STEPS}");
    assert_eq!(start_lines(&scan(&[("a.yml", &a), ("b.yml", &b)])), [2, 2]);
}

// Catches a prime opening a string: Haskell's `go'` is a name, so the
// different expressions after it are not one literal.
#[test]
fn a_haskell_prime_is_part_of_the_name() {
    let a = "total xs ys zs ws = foldl' (+) 0 xs + 1\ncount a b c d e f = foldl' (+) 0 a + 2\nmerge p q r s t u v = foldl' (+) 0 p + 3\n";
    let b = "total xs ys zs ws = foldl' (*) 1 xs * 1\ncount a b c d e f = foldl' (*) 1 a * 2\nmerge p q r s t u v = foldl' (*) 1 p * 3\n";
    assert!(scan(&[("a.hs", a), ("b.hs", b)]).groups.is_empty());
}

// Catches a wrong comment marker in a language's lexicon: the same code
// with and without a comment in that language's syntax is still a clone.
#[test]
fn every_token_language_skips_its_comments() {
    let cases = [
        ("c", "// note"),
        ("scala", "/* note */"),
        ("groovy", "// note"),
        ("dart", "// note"),
        ("zig", "// note"),
        ("sol", "// note"),
        ("proto", "// note"),
        ("scss", "/* note */"),
        ("vue", "<!-- note -->"),
        ("sql", "-- note"),
        ("lua", "--[[ note ]]"),
        ("hs", "{- note -}"),
        ("rb", "# note"),
        ("sh", "# note"),
        ("ps1", "<# note #>"),
        ("pl", "# note"),
        ("r", "# note"),
        ("ex", "# note"),
        ("yml", "# note"),
        ("tf", "# note"),
    ];
    let body = "call(alpha, beta, gamma, delta)\ncall(alpha, beta, gamma, delta, 1)\ncall(alpha, beta, gamma, delta, 2)\n";
    for (ext, comment) in cases {
        // A block comment goes before code on its line, so a block marker
        // read as a line marker hides that code.
        let with = if comment.ends_with("note") {
            body.replacen('\n', &format!(" {comment}\n"), 1)
        } else {
            format!("{comment} {body}")
        };
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let found = scan(&[(&a, body), (&b, &with)]);
        assert_eq!(start_lines(&found), [1, 1], "{ext}: {comment}");
    }
    let tokens = LANGS
        .iter()
        .filter(|l| matches!(l.syntax, Syntax::Tokens(_)))
        .count();
    assert_eq!(cases.len(), tokens, "every token language has a case");
}

// Catches YAML being scanned by default: data files (test fixtures, API
// specs) would bury the code results.
#[test]
fn yaml_is_scanned_only_when_asked_for() {
    let dir = temp_dir("yaml");
    std::fs::write(dir.join("a.yml"), format!("name: ci\nsteps:\n{STEPS}")).unwrap();
    std::fs::write(dir.join("b.yml"), format!("name: release\nsteps:\n{STEPS}")).unwrap();
    let scan_dir = |languages: Vec<usize>| {
        let opts = ScanOptions {
            min_tokens: 20,
            languages,
            ..ScanOptions::default()
        };
        Engine::default().scan(&dir, &opts).unwrap().groups.len()
    };
    assert_eq!(scan_dir(Vec::new()), 0);
    assert_eq!(scan_dir(vec![lang_index_by_name("yaml").unwrap()]), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// Catches a question about YAML code being answered from a scan that
// skipped YAML: asking about a snippet in an opt-in language scans it.
#[test]
fn similar_code_is_found_in_an_opt_in_language() {
    let dir = temp_dir("yaml-similar");
    std::fs::write(dir.join("a.yml"), format!("name: ci\nsteps:\n{STEPS}")).unwrap();
    let opts = ScanOptions {
        min_tokens: 20,
        ..ScanOptions::default()
    };
    let yaml = lang_index_by_name("yaml").unwrap();
    let (_, hits) = Engine::default().similar(&dir, STEPS, yaml, &opts).unwrap();
    assert_eq!(hits.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// Catches a baseline written without named languages meaning "whatever
// this binary knows": a release adding languages would fail every check
// on their existing duplication.
#[test]
fn a_baseline_records_the_languages_it_covered() {
    let dir = temp_dir("baseline-langs");
    let file = dir.join("old.json");
    let v1 = r#"{"version":1,"settings":{"min_tokens":50,"strict":false,"max_param_density":8.0,"languages":[],"exclude":[]},"groups":[]}"#;
    std::fs::write(&file, v1).unwrap();
    let old = Baseline::load(&file).unwrap().settings.languages;
    assert!(old.contains(&"java".to_string()) && !old.contains(&"c-family".to_string()));
    let empty = Engine::default()
        .scan(&dir, &ScanOptions::default())
        .unwrap();
    let new = Baseline::from_analysis(&empty, &ScanOptions::default());
    let langs = &new.settings.languages;
    assert!(langs.contains(&"c-family".to_string()) && !langs.contains(&"yaml".to_string()));
    let _ = std::fs::remove_dir_all(&dir);
}

// Catches a catch-all fallback: data files are not code.
#[test]
fn data_files_are_not_scanned() {
    assert!(lang_index_for_path("package.json".as_ref()).is_none());
    assert!(lang_index_for_path("Cargo.lock".as_ref()).is_none());
}
