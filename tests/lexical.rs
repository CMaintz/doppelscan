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
    let found = Engine::default().similar(&dir, STEPS, yaml, &opts).unwrap();
    assert_eq!(found.hits.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// Catches a baseline written without named languages meaning "whatever
// this binary knows": a release adding languages would fail every check
// on their existing duplication.
#[test]
fn a_baseline_records_the_languages_it_covered() {
    let dir = temp_dir("baseline-langs");
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

const CALLS: &str = "call(alpha, beta, gamma, delta)\ncall(alpha, beta, gamma, delta, 1)\ncall(alpha, beta, gamma, delta, 2)\n";

// Catches `-->` hiding the code after it: dashes in a longer symbol are a
// Haskell operator, not a comment.
#[test]
fn a_haskell_operator_with_dashes_is_code() {
    let code = "go1 a b = a --> call(alpha, beta, gamma, delta, epsilon, zeta, eta, theta)\ngo2 a b = a |-- call(alpha, beta, gamma, delta, epsilon, zeta, eta, theta)\n";
    let found = scan(&[("a.hs", code), ("b.hs", code)]);
    assert_eq!(found.groups.len(), 1);
}

// Catches the operator rule eating real comments, in Haskell or in a
// language where any `--` opens one: the bracket on these lines would
// fold the code below into one statement.
#[test]
fn dashes_still_open_comments() {
    let cases = [
        ("hs", "--- heading (\n"),
        ("hs", "--note (\n"),
        ("sql", "--> note (\n"),
        ("lua", "--> note (\n"),
    ];
    for (ext, comment) in cases {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let with = format!("{comment}{CALLS}");
        assert_eq!(
            start_lines(&scan(&[(&a, &with), (&b, CALLS)])),
            [2, 1],
            "{comment}"
        );
    }
}

// Catches an escaped quote closing a triple-quoted string: the leftover
// `"""` would open another one and swallow the rest of the file.
#[test]
fn an_escaped_quote_does_not_close_a_triple_quoted_string() {
    for ext in ["dart", "groovy", "ex", "elm", "hs"] {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let with = format!("s = \"\"\"a\\\"\"\"b\"\"\"\n{CALLS}");
        assert_eq!(
            start_lines(&scan(&[(&a, &with), (&b, CALLS)])),
            [2, 1],
            "{ext}"
        );
    }
}

// Catches escapes being honoured where a string has none: Scala's triple
// quotes, Dart's `r` and Elixir's `~S` keep a backslash as text, so the
// closing quotes after one really close it.
#[test]
fn a_raw_triple_quoted_string_closes_after_a_backslash() {
    let cases = [
        ("scala", "\"\"\"a\\\"\"\""),
        ("dart", "r\"\"\"a\\\"\"\""),
        ("dart", "r'''a\\'''"),
        ("ex", "~S\"\"\"a\\\"\"\""),
    ];
    for (ext, string) in cases {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let with = format!("s = {string}\n{CALLS}");
        assert_eq!(
            start_lines(&scan(&[(&a, &with), (&b, CALLS)])),
            [2, 1],
            "{string}"
        );
    }
}

// Catches `''''` opening a triple-quoted string where the language has
// none: a quoted quote would swallow the rest of the file as one literal.
#[test]
fn a_doubled_quote_does_not_open_a_triple_quoted_string() {
    let cases = [
        ("sql", "SELECT REPLACE(name, '''', '') FROM t;"),
        ("sql", "SELECT 'a''''b' FROM t;"),
        ("ps1", "$s = ''''"),
        ("sh", "echo ''''"),
    ];
    for (ext, line) in cases {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let with = format!("{line}\n{CALLS}");
        let found = scan(&[(&a, &with), (&b, CALLS)]);
        assert_eq!(start_lines(&found), [2, 1], "{line}");
    }
}

// Catches a language with triple quotes reading them as three strings:
// the text of a heredoc would count as code, so copies with different
// text would not match.
#[test]
fn triple_quotes_span_lines_where_the_language_has_them() {
    let cases = [
        ("scala", "\"\"\""),
        ("groovy", "\"\"\""),
        ("groovy", "'''"),
        ("dart", "\"\"\""),
        ("dart", "'''"),
        ("ex", "\"\"\""),
        ("ex", "'''"),
        ("hs", "\"\"\""),
    ];
    for (ext, quotes) in cases {
        let wrap = |text: &str| format!("doc({quotes}\n{text}\n{quotes})\n{CALLS}");
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let one = wrap("select * from rows where id = ? and live");
        let two = wrap("update rows set seen = now() where id = ?");
        let found = scan(&[(&a, &one), (&b, &two)]);
        assert_eq!(start_lines(&found), [1, 1], "{ext}: {quotes}");
    }
}

// Catches a call or an access on a word that also heads imports counting
// as an import, which drops the statement and breaks the run around it.
#[test]
fn a_head_word_used_as_a_name_is_not_an_import() {
    let cases = [
        (
            "sol",
            "    require(amount > 0, \"zero\");\n    require(amount < limit, \"high\");\n    require(owner != sender, \"self\");\n    require(paused == false, \"off\");\n",
        ),
        (
            "sol",
            "    require(alpha);\n    require(beta);\n    require(gamma);\n    require(delta);\n    require(epsilon);\n",
        ),
        (
            "c",
            "    part->alpha = one;\n    part->beta = two;\n    part->gamma = three;\n    part->delta = four;\n",
        ),
        (
            "c",
            "    part.alpha = one;\n    part.beta = two;\n    part.gamma = three;\n    part.delta = four;\n",
        ),
        (
            "c",
            "    part[0] = one;\n    part[1] = two;\n    part[2] = three;\n    part[3] = four;\n",
        ),
        (
            "c",
            "    library(alpha);\n    library(beta);\n    library(gamma);\n    library(delta);\n    library(epsilon);\n",
        ),
        (
            "c",
            "    use[0] = one;\n    use[1] = two;\n    use[2] = three;\n    use[3] = four;\n",
        ),
    ];
    for (ext, run) in cases {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let one = format!("void first(void) {{\n    int x = 1;\n{run}}}\n");
        let two = format!("void second(void) {{\n    char *s = name(3, 4);\n{run}}}\n");
        let found = scan(&[(&a, &one), (&b, &two)]);
        assert_eq!(start_lines(&found), [3, 3], "{run}");
    }
}

// Catches the rule for calls and accesses also dropping a real import:
// a repeated import block would be reported as a clone.
#[test]
fn every_import_form_stays_noise() {
    let cases = [
        ("pl", "use strict;"),
        ("rb", "require \"json\""),
        ("rb", "require 'json'"),
        ("rb", "require_relative 'helpers'"),
        ("scala", "import scala.util.Try"),
        ("scala", "import scala.util.{Failure, Success}"),
        ("vue", "import { ref } from 'vue'"),
        ("vue", "import type { Ref } from 'vue'"),
        ("vue", "import Store, { use } from './store'"),
        ("vue", "import * as api from './api'"),
        ("dart", "part 'cart.g.dart';"),
        ("dart", "part of 'cart.dart';"),
        ("dart", "library cart;"),
        ("cpp", "using namespace std;"),
        ("lua", "require(\"socket\")"),
        ("r", "library(dplyr)"),
        ("r", "require(dplyr)"),
        ("r", "library(purrr, quietly = TRUE)"),
        ("gradle", "include(\":a\", \":b\")"),
        ("sql", "use [shop]"),
        ("ps1", "Import-Module Baz"),
        ("ps1", "import-module foo"),
    ];
    for (ext, line) in cases {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let imports = format!("{line}\n").repeat(12);
        let found = scan(&[(&a, &imports), (&b, &imports)]);
        assert!(found.groups.is_empty(), "{line}: {:?}", found.groups);
    }
}

// Catches a head that opens a body being read as an import: a Solidity
// library or a Scala package block is a definition, so it is a clone from
// its header, not just from the statements inside it.
#[test]
fn a_head_with_a_body_is_a_definition() {
    let body = "    function add(uint a, uint b) internal returns (uint) {\n        return a + b;\n    }\n    function sub(uint a, uint b) internal returns (uint) {\n        return a - b;\n    }\n";
    let a = format!("event Paid(address payer);\nlibrary Math {{\n{body}}}\n");
    let b = format!("error Empty();\nerror Full(uint size);\nlibrary Calc {{\n{body}}}\n");
    assert_eq!(start_lines(&scan(&[("a.sol", &a), ("b.sol", &b)])), [2, 3]);
}

const BOM: &str = "\u{feff}";

const PS_FUNCTION: &str = "function Get-Total($items) {\n    $sum = Measure-Items $items -Property Amount\n    Write-Output $sum.Sum\n    return $sum.Sum * 2\n}\n";

// Catches a BOM being glued to the first token: `function` becomes a name,
// so a file saved with a BOM no longer matches its copy from line 1.
#[test]
fn a_bom_does_not_change_the_first_token() {
    let with = format!("{BOM}{PS_FUNCTION}");
    let found = scan(&[("a.ps1", PS_FUNCTION), ("b.ps1", &with)]);
    let group = &groups(&found)[0];
    assert_eq!(group.kind, "node");
    let spans: Vec<_> = group
        .occurrences
        .iter()
        .map(|o| (o.start_line, o.end_line))
        .collect();
    assert_eq!(spans, [(1, 5), (1, 5)]);
}

// Catches `#` after a BOM not opening a comment because the byte before
// it is not whitespace: the unclosed bracket would fold the file into one
// statement.
#[test]
fn a_comment_can_follow_a_bom() {
    let with = format!("{BOM}# note (\n{CALLS}");
    let found = scan(&[("a.ps1", CALLS), ("b.ps1", &with)]);
    assert_eq!(start_lines(&found), [1, 2]);
}

// Catches BOM skipping shifting token offsets: a clone's text is cut from
// the file by those offsets, so a shift would cut into the BOM or lose
// the last byte.
#[test]
fn offsets_after_a_bom_still_slice_the_source() {
    let with = format!("{BOM}{CALLS}");
    let found = scan(&[("a.ps1", &with), ("b.ps1", &with)]);
    let template = found.group(1, true).template.unwrap();
    assert_eq!(template, CALLS.trim_end());
}

// Catches `'` continuing a word in a language without primes: `N'New York
// City'` splits into `N'New`, `York` and `City'`, so the literal counts as
// three tokens and its copy with one word in it no longer matches.
#[test]
fn a_prefixed_string_is_one_literal() {
    let cases = [
        ("sql", "city = N'New York City'\n", "city = N'Oslo'\n"),
        ("c", "int s = L' ';\n", "int s = L'x';\n"),
    ];
    for (ext, one, two) in cases {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let found = scan(&[
            (&a, &format!("{one}{CALLS}")),
            (&b, &format!("{two}{CALLS}")),
        ]);
        assert_eq!(start_lines(&found), [1, 1], "{one}");
    }
}

// Catches `'` ending every number: a C++ digit separator would open a
// string, so the same constant written with one would stop matching.
#[test]
fn a_digit_separator_stays_in_the_number() {
    let a = format!("long limit = 1'000'000;\n{CALLS}");
    let b = format!("long limit = 1000000;\n{CALLS}");
    assert_eq!(start_lines(&scan(&[("a.cpp", &a), ("b.cpp", &b)])), [1, 1]);
}

const RUBY_METHOD: &str = "def total(items)\n  sum = items.map(&:price).sum\n  tax = sum * rate(0.25)\n  sum + tax\nend\n";

// Catches `=begin` matching mid-line: an assignment with no space after
// `=` would turn the rest of the file into a comment.
#[test]
fn ruby_begin_mid_line_is_not_a_comment() {
    let a = format!("start =begin_index\n{RUBY_METHOD}");
    let b = format!("reset(1)\n{RUBY_METHOD}");
    assert_eq!(start_lines(&scan(&[("a.rb", &a), ("b.rb", &b)])), [2, 2]);
}

// Catches the line-start rule breaking real block comments, and `=end`
// closing one mid-line: the unclosed bracket on the line after it would
// then be code and fold the rest of the file into one statement.
#[test]
fn ruby_block_comments_run_from_line_start_to_line_start() {
    let a = format!("=begin\nsee x =end_value\nstill a comment (\n=end\n{RUBY_METHOD}");
    let found = scan(&[("a.rb", &a), ("b.rb", RUBY_METHOD)]);
    assert_eq!(start_lines(&found), [5, 1]);
}

// Catches `=end` closing on a prefix: `=ending` is text inside the comment,
// so the bracket after it would become code and fold the file together.
#[test]
fn ruby_end_needs_a_word_boundary() {
    let a = format!("=begin\ntext\n=ending is not end\nmore (\n=end\n{RUBY_METHOD}");
    let found = scan(&[("a.rb", &a), ("b.rb", RUBY_METHOD)]);
    assert_eq!(start_lines(&found), [6, 1]);
}

// Catches the rest of the `=end` line being read as code: Ruby ignores it,
// and the bracket there would swallow the method below.
#[test]
fn ruby_ignores_the_rest_of_the_end_line() {
    let a = format!("=begin\ndocs\n=end of docs (\n{RUBY_METHOD}");
    let found = scan(&[("a.rb", &a), ("b.rb", RUBY_METHOD)]);
    assert_eq!(start_lines(&found), [4, 1]);
}

// Catches `=begin` opening on a prefix: `=beginning` at a line start is an
// assignment's value, and treating it as a comment hid the code up to the
// next `=end`.
#[test]
fn ruby_begin_needs_a_word_boundary() {
    let a = format!("value\n=beginning\n{RUBY_METHOD}=end\n");
    let found = scan(&[("a.rb", &a), ("b.rb", RUBY_METHOD)]);
    assert_eq!(start_lines(&found), [3, 1]);
}

// Catches an unclosed `=begin` running to the end of the file: Ruby rejects
// one, so it can only be text, such as a line of a heredoc.
#[test]
fn an_unclosed_ruby_begin_is_not_a_comment() {
    let a = format!("def one(x)\n  doc = <<~TXT\n=begin\n  TXT\n  x + 1\nend\n{RUBY_METHOD}");
    let found = scan(&[("a.rb", &a), ("b.rb", RUBY_METHOD)]);
    assert_eq!(start_lines(&found), [7, 1]);
}

const ZIG_FUNCTION: &str = "pub fn two(a: u32, b: u32) u32 {\n    const v = a + b;\n    const w = v * 2;\n    const z = w + a + b;\n    return z + v + w;\n}\n";

// Catches Zig getting a block comment it does not have: `/*` in a multiline
// string would swallow the rest of the file.
#[test]
fn zig_has_no_block_comments() {
    let a = format!("const q =\n    \\\\/* a header\n;\n{ZIG_FUNCTION}");
    let found = scan(&[("a.zig", &a), ("b.zig", ZIG_FUNCTION)]);
    assert_eq!(start_lines(&found), [4, 1]);
}

const CASED_QUERY: &str = "SELECT Id, Total, Status\nFROM Orders\nWHERE Total > 10\nORDER BY Id DESC;\nDELETE FROM Audit WHERE Age > 30;\n";

// Catches keywords being hashed with their case: SQL's `SELECT` and
// `select` are one keyword, so a query and its lowercased copy are a clone.
#[test]
fn sql_keywords_match_in_any_case() {
    let lower = CASED_QUERY.to_lowercase();
    assert_eq!(
        scan(&[("a.sql", CASED_QUERY), ("b.sql", &lower)])
            .groups
            .len(),
        1
    );
}

// Catches names differing only in case being hashed apart under --strict:
// `Orders` and `orders` are the same table, as PowerShell's `$Sum` and
// `$sum` are the same variable.
#[test]
fn strict_folds_names_where_the_language_does() {
    let lower = CASED_QUERY.to_lowercase();
    let found = scan_with(&[("a.sql", CASED_QUERY), ("b.sql", &lower)], true);
    assert_eq!(found.groups.len(), 1);
    let found = scan_with(
        &[("a.c", SUM_PAID), ("b.c", &SUM_PAID.to_lowercase())],
        true,
    );
    assert!(found.groups.is_empty(), "{:?}", found.groups);
}

// Catches a keyword or name differing in case counting as a parameter:
// there is nothing for a helper to take, and enough of them trip the
// density veto and drop the group.
#[test]
fn case_alone_is_not_a_parameter() {
    let lower = CASED_QUERY.to_lowercase();
    let found = scan(&[("a.sql", CASED_QUERY), ("b.sql", &lower)]);
    assert!(groups(&found)[0].parameters.is_empty());
}

// Catches parameters being keyed by exact spelling: `Total`, `TOTAL` and
// `total` are one name to SQL, so a helper takes one parameter, not three.
#[test]
fn spellings_of_one_name_are_one_parameter() {
    let a = "SELECT Total, price, qty, region, name FROM Orders WHERE TOTAL > 1 AND price > 2 AND qty > 3 AND total < 100;\n";
    let b = "select Amount, price, qty, region, name from Orders where Amount > 1 and price > 2 and qty > 3 and Amount < 100;\n";
    let g = &groups(&scan(&[("a.sql", a), ("b.sql", b)]))[0];
    assert_eq!(g.parameters.len(), 1, "{:?}", g.parameters);
    assert_eq!(g.parameters[0].uses, 3);
    assert_eq!(g.parameters[0].values, ["Total", "Amount"]);
}

// Catches folding reaching a language where case tells names apart: in C
// `Total` and `TOTAL` can be different variables, so they stay two.
#[test]
fn case_separates_parameters_where_the_language_does() {
    let a = "void f(void) {\n    use(Total, price, qty, region, name, 1);\n    use(TOTAL, price, qty, region, name, 2);\n}\n";
    let b = a.replace("Total", "Amount").replace("TOTAL", "Amount");
    let g = &groups(&scan(&[("a.c", a), ("b.c", &b)]))[0];
    assert_eq!(g.parameters.len(), 2, "{:?}", g.parameters);
}

// Catches case folding reaching literals: `'Paid'` and `'paid'` are
// different values even in SQL, so they stay a parameter.
#[test]
fn a_string_keeps_its_case() {
    let a = format!("UPDATE Orders SET Status = 'Paid' WHERE Id = 7;\n{CASED_QUERY}");
    let b = a.replace("'Paid'", "'paid'");
    let g = &groups(&scan(&[("a.sql", &a), ("b.sql", &b)]))[0];
    assert_eq!(g.parameters.len(), 1);
    assert_eq!(g.parameters[0].values, ["'Paid'", "'paid'"]);
}

// Catches an uppercase import escaping the noise list: `USE shop` is `use
// shop`, and a repeated block of it is not a clone.
#[test]
fn imports_match_in_any_case_where_keywords_do() {
    for line in ["USE shop;", "USE [shop]"] {
        let imports = format!("{line}\n").repeat(12);
        let found = scan(&[("a.sql", &imports), ("b.sql", &imports)]);
        assert!(found.groups.is_empty(), "{line}: {:?}", found.groups);
    }
}

// Catches the shared head list folding with the language's case: `PART`
// and `LIBRARY` are columns to SQL, which has no such statements, and
// dropping them hid a repeated SELECT list. Lowercase spellings are no
// different, and `use` followed by a keyword is not `USE shop`.
#[test]
fn sql_columns_named_like_import_heads_are_code() {
    let upper = "SELECT\n  PART AS P1,\n  LIBRARY AS L1,\n  a AS b,\n  USE AS c,\n  d AS e,\n  f AS g\nFROM\n  t;\n";
    for list in [upper.to_string(), upper.to_lowercase()] {
        let found = scan(&[("a.sql", &list), ("b.sql", &list)]);
        assert_eq!(found.groups.len(), 1, "{list}");
    }
}

// Catches `Import-Module` being matched on its first word: `Import-Csv`
// and its kin load data, so a repeated block of them is code.
#[test]
fn only_import_module_is_a_powershell_import() {
    let block = "Import-Csv $path\n".repeat(12);
    let found = scan(&[("a.ps1", &block), ("b.ps1", &block)]);
    assert_eq!(found.groups.len(), 1);
}

// Catches a nested block comment closing at its first closer: what is
// left of it reads as code, and the bracket in it folds the rest of the
// file into one statement.
#[test]
fn nested_block_comments_close_at_their_own_closer() {
    let cases = [
        ("hs", "{- outer {- inner -} tail (\n-}\n"),
        ("scala", "/* outer /* inner */ tail (\n*/\n"),
        ("dart", "/* outer /* inner */ tail (\n*/\n"),
    ];
    for (ext, comment) in cases {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let with = format!("{comment}{CALLS}");
        assert_eq!(
            start_lines(&scan(&[(&a, &with), (&b, CALLS)])),
            [3, 1],
            "{ext}"
        );
    }
}

// Catches nesting reaching a language without it: C's `/* /* */` is a
// whole comment, and counting the inner opener would eat the file.
#[test]
fn block_comments_do_not_nest_in_c() {
    let with = format!("/* one /* two */\n{CALLS}");
    assert_eq!(
        start_lines(&scan(&[("a.c", &with), ("b.c", CALLS)])),
        [2, 1]
    );
}

// Catches a block labelled with its kind or half its name: a word that is
// no keyword in the shared list (`contract`) took the label from the name
// after it, and a hyphenated name kept only one part.
#[test]
fn a_block_is_labelled_with_its_whole_name() {
    let cases = [
        ("sol", "contract Cart", "Cart"),
        ("sol", "abstract contract Cart is Base", "Cart"),
        ("sol", "library Cart", "Cart"),
        ("ps1", "function Get-Total($items)", "Get-Total"),
        ("ps1", "function Get-Total", "Get-Total"),
        ("sh", "my-task()", "my-task"),
        ("scala", "class Cart", "Cart"),
        ("gradle", "dependencies", "dependencies"),
        ("c", "long total(int a, int b)", "total"),
    ];
    let run = "    open_log(path, mode, flags);\n    write_header(log, version, stamp);\n    flush_all(log, buffers, count);\n";
    for (ext, header, name) in cases {
        let (a, b) = (format!("a.{ext}"), format!("b.{ext}"));
        let one = format!("{header} {{\n    int x = 1;\n{run}}}\n");
        let two = format!("other() {{\n    char *s = name(3, 4);\n{run}}}\n");
        let found = scan(&[(&a, &one), (&b, &two)]);
        let scope = groups(&found)[0].occurrences[0].scope.clone();
        assert_eq!(scope.as_deref(), Some(name), "{header}");
    }
}

// Catches a block losing the scope around it: a nested name would forget
// its parent, and an unnamed block such as an `if` would label nothing.
#[test]
fn a_nested_block_keeps_the_scope_around_it() {
    let run = "open_log(path, mode, flags);\nwrite_header(log, version, stamp);\nflush_all(log, buffers, count);\n";
    let cases = [
        (
            "class Outer {\nvoid inner() {\nint x = 1;\n@\n}\n}\n",
            "Outer.inner",
        ),
        (
            "void total() {\nif (ready) {\nint x = 1;\n@\n}\n}\n",
            "total",
        ),
        (
            "class Outer {\nif (ready) {\nvoid inner() {\nint x = 1;\n@\n}\n}\n}\n",
            "Outer.inner",
        ),
    ];
    for (shell, name) in cases {
        let one = shell.replace('@', run);
        let two = format!("other() {{\nchar *s = name(3, 4);\n{run}}}\n");
        let found = scan(&[("a.c", &one), ("b.c", &two)]);
        let scope = groups(&found)[0].occurrences[0].scope.clone();
        assert_eq!(scope.as_deref(), Some(name), "{shell}");
    }
}

// Catches labels growing with nesting: each holds its parents' names, so
// labels for a file of `f {` lines take memory quadratic in its size, and
// a megabyte of them would need tens of gigabytes.
#[test]
fn a_deeply_nested_label_stays_short() {
    let depth = 3000;
    let open = "f {\n".repeat(depth);
    let close = "}\n".repeat(depth);
    let a = format!("{open}int x = 1;\n{CALLS}{close}");
    let b = format!("other() {{\nchar *s = name(3, 4);\n{CALLS}}}\n");
    let found = scan(&[("a.c", &a), ("b.c", &b)]);
    let scope = groups(&found)[0].occurrences[0].scope.clone().unwrap();
    assert!(scope.len() < 1100, "{} bytes", scope.len());
}

// Catches hyphen joining reaching past a name: `a->b` is an access, so the
// block belongs to `b`, and subtraction is not a hyphen.
#[test]
fn only_an_unspaced_hyphen_joins_a_name() {
    let run = "    open_log(path, mode, flags);\n    write_header(log, version, stamp);\n    flush_all(log, buffers, count);\n";
    for (header, name) in [("run->go()", "go"), ("run - go()", "go")] {
        let one = format!("{header} {{\n    int x = 1;\n{run}}}\n");
        let two = format!("other() {{\n    char *s = name(3, 4);\n{run}}}\n");
        let found = scan(&[("a.c", &one), ("b.c", &two)]);
        let scope = groups(&found)[0].occurrences[0].scope.clone();
        assert_eq!(scope.as_deref(), Some(name), "{header}");
    }
}

// Catches a hyphen chain being walked whole: every block of a statement
// repeats the walk and stores the result as a label, so a long chain before
// many blocks costs time and memory quadratic in the file.
#[test]
fn a_hyphen_chain_label_stays_short() {
    let chain = vec!["a"; 3000].join("-");
    let a = format!("{chain} {{\nint x = 1;\n{CALLS}}}\n");
    let b = format!("other() {{\nchar *s = name(3, 4);\n{CALLS}}}\n");
    let found = scan(&[("a.c", &a), ("b.c", &b)]);
    let scope = groups(&found)[0].occurrences[0].scope.clone().unwrap();
    assert!(scope.len() < 1100, "{} bytes", scope.len());
}
