use std::time::{Duration, Instant};

use doppelscan::baseline::Baseline;
use doppelscan::detect::RawGroup;
use doppelscan::engine::{ScanOptions, Source, analyze, index_sources};
use doppelscan::lang::lang_index_for_path;
use doppelscan::report::{Analysis, Group, name};

fn scan_with(files: &[(&str, &str)], opts: ScanOptions) -> Analysis {
    let sources = files
        .iter()
        .map(|(path, text)| Source {
            path: path.to_string(),
            lang: lang_index_for_path(path.as_ref()).expect("known extension"),
            text: text.to_string(),
        })
        .collect();
    analyze(index_sources(sources, opts.strict), &opts, Instant::now())
}

fn scan(files: &[(&str, &str)]) -> Analysis {
    scan_with(
        files,
        ScanOptions {
            min_tokens: 20,
            ..ScanOptions::default()
        },
    )
}

fn groups(a: &Analysis) -> Vec<Group> {
    (1..=a.groups.len()).map(|id| a.group(id, true)).collect()
}

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

const SUM_REFUNDS: &str = r#"
export function sumRefunds(refunds: Refund[], settings: Config): number {
    let sum = 0;
    for (const refund of refunds) {
        if (refund.status === "approved") {
            sum += refund.amount * settings.rate;
        }
    }
    return Math.round(sum * 100) / 100;
}
"#;

// Catches identifiers or literals being hashed by text in default mode.
#[test]
fn renamed_and_relabelled_function_is_one_clone_group() {
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", SUM_REFUNDS)]);
    let gs = groups(&a);
    assert_eq!(
        gs.len(),
        1,
        "{:#?}",
        gs.iter().map(|g| &g.occurrences).collect::<Vec<_>>()
    );
    assert_eq!(gs[0].occurrences.len(), 2);
    assert_eq!(gs[0].kind, "node");
}

// Catches operators (anonymous tokens) being anonymised along with names.
#[test]
fn different_operator_is_not_a_clone() {
    let changed = SUM_REFUNDS.replace(
        "sum += refund.amount * settings.rate",
        "sum -= refund.amount / settings.rate",
    );
    let a = scan_with(
        &[("a.ts", SUM_TOTALS), ("b.ts", &changed)],
        ScanOptions {
            min_tokens: 60,
            ..ScanOptions::default()
        },
    );
    assert!(
        a.groups.is_empty(),
        "{:#?}",
        groups(&a)
            .iter()
            .map(|g| &g.occurrences)
            .collect::<Vec<_>>()
    );
}

// Catches missing subsumption: the function's block, loop and if-statement
// are clones too, but they sit inside the reported function clone.
#[test]
fn nested_clones_are_reported_once_at_the_outermost_extent() {
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", SUM_REFUNDS)]);
    assert_eq!(a.groups.len(), 1);
    let template = a.group(1, true).template.unwrap();
    assert!(
        template.starts_with("export function sumTotals"),
        "{template}"
    );
}

// Catches over-eager subsumption: the loop also appears in a third file,
// so it is a clone in its own right with three occurrences.
#[test]
fn inner_clone_that_also_appears_elsewhere_survives() {
    let third = r#"
function audit(items: Item[], opts: Opts) {
    log("start");
    for (const item of items) {
        if (item.status === "open") {
            acc += item.amount * opts.rate;
        }
    }
}
"#;
    let lenient = ScanOptions {
        min_tokens: 20,
        max_param_density: 100.0,
        ..ScanOptions::default()
    };
    let a = scan_with(
        &[("a.ts", SUM_TOTALS), ("b.ts", SUM_REFUNDS), ("c.ts", third)],
        lenient,
    );
    let counts: Vec<usize> = groups(&a).iter().map(|g| g.occurrences.len()).collect();
    assert!(counts.contains(&2) && counts.contains(&3), "{counts:?}");
}

// Catches binding detection: `total`/`sum` and `order`/`refund` are declared
// inside the fragment (renames); `config`/`settings` and the status string
// come from outside (parameters).
#[test]
fn locals_are_renames_and_outside_values_are_parameters() {
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", SUM_REFUNDS)]);
    let g = a.group(1, true);
    let params: Vec<&str> = g.parameters.iter().map(|v| v.values[0].as_str()).collect();
    let renames: Vec<&str> = (g.renames.iter().chain(&g.declares))
        .map(|v| v.values[0].as_str())
        .collect();
    for local in ["total", "order", "sumTotals", "orders", "config"] {
        assert!(
            renames.contains(&local),
            "{local} should be a rename: {renames:?} / {params:?}"
        );
    }
    for outside in ["Order", "\"paid\""] {
        assert!(
            params.contains(&outside),
            "{outside} should be a parameter: {params:?}"
        );
    }
    assert!(!params.contains(&"total"));
}

// Catches the template not substituting parameter positions.
#[test]
fn template_marks_parameters_and_keeps_renames_verbatim() {
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", SUM_REFUNDS)]);
    let t = a.group(1, true).template.unwrap();
    assert!(t.contains("status === $"), "{t}");
    assert!(t.contains("let total = 0"), "{t}");
}

// Catches comments taking part in hashing.
#[test]
fn comments_do_not_prevent_a_match() {
    let commented = SUM_REFUNDS.replace(
        "let sum = 0;",
        "let sum = 0; // running sum\n    /* refunds only */",
    );
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", &commented)]);
    assert_eq!(a.groups.len(), 1);
    assert_eq!(
        a.group(1, false).kind,
        "node",
        "whole function should match"
    );
}

// Catches strict mode still anonymising names.
#[test]
fn strict_mode_requires_identical_names() {
    let a = scan_with(
        &[("a.ts", SUM_TOTALS), ("b.ts", SUM_REFUNDS)],
        ScanOptions {
            min_tokens: 20,
            strict: true,
            ..ScanOptions::default()
        },
    );
    assert!(a.groups.iter().all(|g| g.tokens < 20 || g.occs.len() < 2) && a.groups.len() <= 1);
    let same = scan_with(
        &[("a.ts", SUM_TOTALS), ("b.ts", SUM_TOTALS)],
        ScanOptions {
            min_tokens: 20,
            strict: true,
            ..ScanOptions::default()
        },
    );
    assert_eq!(same.groups.len(), 1);
}

const RUN: &str = r#"
    const user = await repo.findUser(id);
    if (!user) { throw new NotFoundError("user", id); }
    const perms = await acl.permissionsFor(user.role);
    audit.record(user.id, "access", perms.length);
    cache.set(`perm:${user.id}`, perms, { ttl: 60 });
"#;

// Catches run detection missing, or reporting a sub-window instead of the
// maximal run: five shared statements inside otherwise different functions.
#[test]
fn shared_statement_run_is_reported_at_full_length() {
    let a_src = format!("async function a(id) {{\n    prepare();\n{RUN}    return 1;\n}}\n");
    let b_src = format!(
        "async function b(id, x) {{\n    other(x, x);\n    twice(x);\n{RUN}    return render(x);\n}}\n"
    );
    let a = scan(&[("a.ts", &a_src), ("b.ts", &b_src)]);
    let gs = groups(&a);
    assert_eq!(
        gs.len(),
        1,
        "{:#?}",
        gs.iter()
            .map(|g| (&g.kind, g.elements, &g.occurrences))
            .collect::<Vec<_>>()
    );
    assert_eq!(gs[0].kind, "run");
    assert_eq!(gs[0].elements, 5);
}

// Catches overlapping occurrences being reported as clones of each other.
// In "A B A B A" the run "A B A" occurs at 0 and 2, overlapping, and is not
// a tandem repeat (its period 2 is more than half its length).
#[test]
fn occurrences_within_a_group_never_overlap() {
    let a_stmt = "    registry.register(\"alpha\", createHandler(config.alpha, logger));\n";
    let b_stmt = "    registry.validate(\"alpha\", schema.alpha, { strict: true });\n";
    let body = [a_stmt, b_stmt, a_stmt, b_stmt, a_stmt].concat();
    let src =
        format!("function setup() {{\n{body}}}\nfunction other() {{\n{a_stmt}{b_stmt}{a_stmt}}}\n");
    let a = scan(&[("a.ts", &src)]);
    for g in groups(&a) {
        let mut spans: Vec<(u32, u32)> = g
            .occurrences
            .iter()
            .map(|o| (o.start_line, o.end_line))
            .collect();
        spans.sort();
        assert!(
            spans.windows(2).all(|w| w[0].1 < w[1].0),
            "overlap in {spans:?}"
        );
    }
    assert!(!a.groups.is_empty());
}

// Catches the noise filter: identical import blocks are not refactorable.
#[test]
fn identical_imports_are_not_reported() {
    let imports: String = (0..15)
        .map(|i| format!("import {{ thing{i}, other{i} }} from \"./module{i}\";\n"))
        .collect();
    let a = scan(&[
        ("a.ts", &format!("{imports}foo();\n")),
        ("b.ts", &format!("{imports}bar();\n")),
    ]);
    assert!(
        a.groups.is_empty(),
        "{:#?}",
        groups(&a)
            .iter()
            .map(|g| &g.occurrences)
            .collect::<Vec<_>>()
    );
}

/// Groups with exactly these fingerprints, in this id order.
fn with_fingerprints(prints: &[u64]) -> Analysis {
    let group = |&print: &u64| RawGroup {
        hash: print << 16,
        tokens: 20,
        run_len: 1,
        occs: Vec::new(),
    };
    Analysis {
        files: Vec::new(),
        accepted: Vec::new(),
        groups: prints.iter().map(group).collect(),
        elapsed: Duration::ZERO,
    }
}

// Catches a number past the last id falling through to a fingerprint that
// happens to start with it, so `show 54` on 47 groups shows an unrelated one.
#[test]
fn a_number_out_of_id_range_is_no_group_even_if_a_fingerprint_starts_with_it() {
    let a = with_fingerprints(&[0x54ab12345678, 0x41cd12345678, 0xabcdef123456]);
    for key in ["54", "41", "4", "0", "99999999999999999999999"] {
        assert_eq!(a.resolve(key), None, "{key:?}");
    }
    assert_eq!(a.resolve("2"), Some(2));
    assert_eq!(a.resolve(" #3 "), Some(3));
}

// Catches a name lookup that picks a neighbour, such as one comparing only
// the first word.
#[test]
fn a_name_resolves_to_its_own_group() {
    let a = with_fingerprints(&[0x54ab12345678, 0xabcdef123456, 0xabcd00000001]);
    for id in 1..=3 {
        assert_eq!(a.resolve(&name(&a.groups[id - 1])), Some(id));
    }
    assert_eq!(a.resolve(&name(&a.groups[1]).to_uppercase()), Some(2));
}

// Catches the bit layout or the word list changing: names would no longer
// match what people noted down or what an earlier run printed.
#[test]
fn a_fingerprint_always_gets_the_same_name() {
    let a = with_fingerprints(&[0xb4a94232d76e]);
    assert_eq!(name(&a.groups[0]), "rink-point-city-roast");
}

// Catches a shared name silently picking the first or last match. These two
// fingerprints differ only in the bits a name leaves out.
#[test]
fn a_shared_name_is_refused_and_lists_the_candidates() {
    let a = with_fingerprints(&[0xabcdef123400, 0xabcdef1234ff]);
    let shared = name(&a.groups[0]);
    assert_eq!(shared, name(&a.groups[1]));
    assert_eq!(a.resolve(&shared), None);
    let error = a.unresolved(&shared);
    assert!(error.contains("#1 [abcdef123400]"), "{error}");
    assert!(error.contains("#2 [abcdef1234ff]"), "{error}");
}

// Catches matching names by prefix, as fingerprints are: part of a name, or
// a name with a word changed, would land on a group nobody asked for.
#[test]
fn an_unknown_or_partial_name_is_no_group() {
    let a = with_fingerprints(&[0xabcdef123456]);
    let whole = name(&a.groups[0]);
    let (partial, _) = whole.rsplit_once('-').unwrap();
    for key in [partial, "not-a-real-name"] {
        assert_eq!(a.resolve(key), None, "{key:?}");
        assert!(a.unresolved(key).contains("no clone group"), "{key:?}");
    }
}

// Catches an empty key being a prefix of every fingerprint, which resolves
// to the only group when there is one.
#[test]
fn a_key_with_nothing_in_it_is_no_group() {
    let a = with_fingerprints(&[0xabcdef123456]);
    for key in ["", "#", "  ", "# "] {
        assert_eq!(a.resolve(key), None, "{key:?}");
    }
}

// Catches prefixes short enough to pick a group by accident, and a prefix
// shared by two groups picking one of them.
#[test]
fn a_fingerprint_prefix_must_be_long_and_unique() {
    let a = with_fingerprints(&[0x54ab12345678, 0xabcdef123456, 0xabcd00000001]);
    assert_eq!(a.resolve("54"), None);
    assert_eq!(a.resolve("ab"), None);
    assert_eq!(a.resolve("abcd"), None);
    assert_eq!(a.resolve("54ab"), Some(1));
    assert_eq!(a.resolve("ABCDEF"), Some(2));
    assert_eq!(a.resolve("#abcd00000001"), Some(3));
}

// Catches the all-digit rule hiding a whole fingerprint that happens to have
// no letter: results print it, so it has to resolve.
#[test]
fn a_whole_fingerprint_resolves_even_if_it_is_all_digits() {
    let a = with_fingerprints(&[0x123456789012]);
    assert_eq!(a.resolve("123456789012"), Some(1));
    assert_eq!(a.resolve("1234567890"), None);
}

// Catches location lookup missing a clone that covers the asked-for lines.
#[test]
fn overlapping_finds_the_group_covering_a_line() {
    let a = scan(&[("src/a.ts", SUM_TOTALS), ("src/b.ts", SUM_REFUNDS)]);
    assert_eq!(a.overlapping("src/b.ts", 5, 5).unwrap(), vec![1]);
    assert!(a.overlapping("src/b.ts", 40, 50).unwrap().is_empty());
}

// Catches a grammar whose identifier or literal kinds are misclassified,
// which would make renamed clones invisible in that language.
#[test]
fn renamed_clones_are_found_in_every_language() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "java",
            "class A { int f(List<Item> items) { int total = 0; for (Item it : items) { if (it.isActive()) { total += it.getPrice() * 2; } } return total; } }",
            "class B { int g(List<Line> lines) { int sum = 0; for (Line ln : lines) { if (ln.isActive()) { sum += ln.getPrice() * 3; } } return sum; } }",
        ),
        (
            "cs",
            "class A { int F(List<Item> items) { var total = 0; foreach (var it in items) { if (it.IsActive) { total += it.Price * 2; } } return total; } }",
            "class B { int G(List<Line> lines) { var sum = 0; foreach (var ln in lines) { if (ln.IsActive) { sum += ln.Price * 3; } } return sum; } }",
        ),
        (
            "py",
            "def f(items):\n    total = 0\n    for it in items:\n        if it.active:\n            total += it.price * 2\n    return total\n",
            "def g(lines):\n    acc = 0\n    for ln in lines:\n        if ln.active:\n            acc += ln.price * 3\n    return acc\n",
        ),
        (
            "kt",
            "fun f(items: List<Item>): Int { var total = 0; for (it in items) { if (it.active) { total += it.price * 2 } }; return total }",
            "fun g(lines: List<Line>): Int { var sum = 0; for (ln in lines) { if (ln.active) { sum += ln.price * 3 } }; return sum }",
        ),
        (
            "swift",
            "func f(items: [Item]) -> Int { var total = 0\n for it in items { if it.active { total += it.price * 2 } }\n return total }",
            "func g(lines: [Line]) -> Int { var sum = 0\n for ln in lines { if ln.active { sum += ln.price * 3 } }\n return sum }",
        ),
        (
            "php",
            "<?php function f($items) { $total = 0; foreach ($items as $it) { if ($it->active) { $total += $it->price * 2; } } return $total; }",
            "<?php function g($lines) { $sum = 0; foreach ($lines as $ln) { if ($ln->active) { $sum += $ln->price * 3; } } return $sum; }",
        ),
        (
            "go",
            "package a\nfunc f(items []Item) int { total := 0; for _, it := range items { if it.Active { total += it.Price * 2 } }; return total }",
            "package b\nfunc g(lines []Line) int { sum := 0; for _, ln := range lines { if ln.Active { sum += ln.Price * 3 } }; return sum }",
        ),
        (
            "rs",
            "fn f(items: &[Item]) -> i64 { let mut total = 0; for it in items { if it.active { total += it.price * 2; } } total }",
            "fn g(lines: &[Line]) -> i64 { let mut sum = 0; for ln in lines { if ln.active { sum += ln.price * 3; } } sum }",
        ),
        (
            "js",
            "function f(items) { let total = 0; for (const it of items) { if (it.active) { total += it.price * 2; } } return total; }",
            "function g(lines) { let sum = 0; for (const ln of lines) { if (ln.active) { sum += ln.price * 3; } } return sum; }",
        ),
    ];
    for (ext, a_src, b_src) in cases {
        let a = scan(&[(&format!("a.{ext}"), a_src), (&format!("b.{ext}"), b_src)]);
        let g = (!a.groups.is_empty()).then(|| a.group(1, false));
        let found = g.as_ref().is_some_and(|g| g.occurrences.len() == 2);
        assert!(found, "{ext}: no clone found");
        let params: Vec<&str> = g
            .as_ref()
            .unwrap()
            .parameters
            .iter()
            .map(|v| v.values[0].as_str())
            .collect();
        assert!(
            params.contains(&"2"),
            "{ext}: literal not a parameter: {params:?}"
        );
    }
}

fn member_wall() -> String {
    [
        "id", "name", "email", "phone", "address", "city", "country", "zip",
    ]
    .iter()
    .map(|f| {
        format!(
            "    public String get{f}() {{ return this.{f}; }}
    public void set{f}(String v) {{ this.{f} = v; }}
"
        )
    })
    .collect()
}

// Catches tandem runs not being collapsed: a wall of getter/setter pairs
// would otherwise be reported once per multiple of the pair.
#[test]
fn a_repetitive_wall_in_one_block_is_one_group() {
    let src = format!(
        "class A {{
{}}}
",
        member_wall()
    );
    let a = scan(&[("A.java", &src)]);
    let gs = groups(&a);
    assert!(
        gs.len() <= 1,
        "{:#?}",
        gs.iter()
            .map(|g| (g.elements, g.occurrences.len()))
            .collect::<Vec<_>>()
    );
}

// Catches tandem collapse going too far: the same wall copied into two
// classes is a clone of the whole wall, not sixteen getter/setter pairs.
#[test]
fn a_repetitive_wall_copied_between_classes_is_reported_whole() {
    let a_src = format!(
        "class A {{
{}}}
",
        member_wall()
    );
    let b_src = format!(
        "class B {{
    private int extra;
{}    void other() {{ run(); }}
}}
",
        member_wall()
    );
    let a = scan(&[("A.java", &a_src), ("B.java", &b_src)]);
    let gs = groups(&a);
    assert_eq!(
        gs.len(),
        1,
        "{:#?}",
        gs.iter()
            .map(|g| (g.elements, g.occurrences.len()))
            .collect::<Vec<_>>()
    );
    assert_eq!((gs[0].elements, gs[0].occurrences.len()), (16, 2));
}

fn parsed_setting(name: &str) -> String {
    format!(
        "    String {name} = parse(context, settings.get{name}());
    if ({name} != null) {{
        strategy.set{name}({name});
    }}
"
    )
}

const SETTING_NAMES: [&str; 7] = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Eta"];

fn settings_wall() -> String {
    let wall: String = SETTING_NAMES.iter().map(|n| parsed_setting(n)).collect();
    format!(
        "class A {{
  void init() {{
{wall}  }}
}}
"
    )
}

/// `class B` holding `settings`, followed by a statement unlike the wall's.
fn settings_copy(settings: &[String]) -> String {
    let settings = settings.concat();
    format!(
        "class B {{
  void make() {{
{settings}    run();
  }}
}}
"
    )
}

/// Wall and copy scanned at a size that leaves out one setting and a half,
/// runs too short to be tandem repeats.
fn scan_wall_and_copy(wall: &str, copy: &str) -> Vec<Group> {
    let opts = ScanOptions::default();
    groups(&scan_with(&[("A.java", wall), ("B.java", copy)], opts))
}

// Catches a part of a wall copied elsewhere being dropped with the wall, or
// reported at wall positions that are not that copy: the rest of the wall
// differs in every name, so it would fail the density limit too.
#[test]
fn a_part_of_a_wall_copied_verbatim_is_a_clone_at_that_part() {
    let copied: Vec<String> = SETTING_NAMES[2..5]
        .iter()
        .map(|n| parsed_setting(n))
        .collect();
    let (wall, copy) = (settings_wall(), settings_copy(&copied));
    let gs = scan_wall_and_copy(&wall, &copy);
    assert_eq!(gs.len(), 1, "{gs:#?}");
    let place = |o: &doppelscan::report::Location| (o.file.clone(), o.start_line);
    let places: Vec<_> = gs[0].occurrences.iter().map(place).collect();
    let gamma = wall
        .lines()
        .position(|l| l.contains("String Gamma"))
        .unwrap() as u32
        + 1;
    assert_eq!(
        places,
        [("A.java".to_string(), gamma), ("B.java".to_string(), 3)]
    );
}

/// Every setting of a wall a line each, and all but the first in a copy, so
/// the run of the copy's length fits the wall at two overlapping places.
fn one_line_wall_and_copy() -> (String, String) {
    let line = |n: &&str| {
        format!(
            "    config.put(\"{n}\", source.read(\"{n}\", {n}.class).trim());
"
        )
    };
    let rows: Vec<String> = SETTING_NAMES.iter().map(line).collect();
    let wall = format!(
        "class A {{
  void init() {{
{}  }}
}}
",
        rows.concat()
    );
    (wall, settings_copy(&rows[1..]))
}

fn ranges(g: &Group) -> Vec<(&str, u32, u32)> {
    let occurrences = g.occurrences.iter();
    occurrences
        .map(|o| (o.file.as_str(), o.start_line, o.end_line))
        .collect()
}

// Catches positions being taken before asking which of them the copy spells
// like: a wall too short to hold the run twice leaves the first fit standing,
// which here is the wrong setting's and fails the density limit.
#[test]
fn a_copy_of_nearly_a_whole_wall_is_found_at_its_own_part() {
    let (wall, copy) = one_line_wall_and_copy();
    let gs = scan_wall_and_copy(&wall, &copy);
    assert_eq!(gs.len(), 1, "{gs:#?}");
    assert_eq!(ranges(&gs[0]), [("A.java", 4, 9), ("B.java", 3, 8)]);
}

// Catches the same wrong pick where the density limit would not reveal it.
#[test]
fn a_copy_of_nearly_a_whole_wall_keeps_its_part_at_any_density_limit() {
    let (wall, copy) = one_line_wall_and_copy();
    let opts = ScanOptions {
        max_param_density: 1000.0,
        ..Default::default()
    };
    let gs = groups(&scan_with(&[("A.java", &wall), ("B.java", &copy)], opts));
    let verbatim = gs.iter().find(|g| ranges(g).contains(&("B.java", 3, 8)));
    assert_eq!(
        ranges(verbatim.expect("the copy")),
        [("A.java", 4, 9), ("B.java", 3, 8)]
    );
}

// Catches the wall's own period deciding the part: every row recurs in the
// wall, so the first fit has a twin of its own and would win over the part
// the copy spells like.
#[test]
fn a_copy_of_most_of_a_periodic_table_is_found_at_its_part() {
    let rows = ["0x01, 0x02, 0x03, 0x04", "0x05, 0x06, 0x07, 0x08"];
    let row = |i: usize| {
        format!(
            "    {},
",
            rows[i % 2]
        )
    };
    let body = |lines: std::ops::Range<usize>| lines.map(row).collect::<String>();
    let wall = format!(
        "T = [
{}]
",
        body(1..41)
    );
    let copy = format!(
        "C = [
{}]
print(C)
",
        body(2..32)
    );
    let gs = groups_of(&[("t.py", &wall), ("c.py", &copy)]);
    let verbatim = [("t.py", 3, 32), ("c.py", 2, 31)];
    assert!(gs.iter().any(|g| ranges(g) == verbatim), "{gs:#?}");
}

// Catches the wall's other members coming back as noise: renamed fields in a
// second class are what every pair of data classes has in common.
#[test]
fn a_renamed_part_of_a_wall_stays_suppressed() {
    let renamed: Vec<String> = ["One", "Two", "Three"]
        .iter()
        .map(|n| parsed_setting(n))
        .collect();
    let (wall, copy) = (settings_wall(), settings_copy(&renamed));
    let gs = scan_wall_and_copy(&wall, &copy);
    assert!(gs.is_empty(), "{gs:#?}");
}

// Catches an import-bearing file node replacing the function it wraps: the
// files differ only in imports, so the function itself is the clone.
#[test]
fn files_differing_only_in_imports_still_match_their_code() {
    let body = SUM_TOTALS.replace("export ", "");
    let a_src = format!(
        "import {{ a }} from \"./a\";
{body}"
    );
    let b_src = format!(
        "import {{ b }} from \"./b\";
{body}"
    );
    let a = scan(&[("a.ts", &a_src), ("b.ts", &b_src)]);
    assert_eq!(a.groups.len(), 1);
    let template = a.group(1, true).template.unwrap();
    assert!(
        template.starts_with("function sumTotals"),
        "the whole function, not just its body: {template}"
    );
}

// Catches the parameter-density veto: rows of a lookup table share shape
// but every value differs, so there is no helper to extract.
#[test]
fn data_tables_are_not_reported() {
    let rows = |seed: u32| -> String {
        (0..12)
            .map(|i| {
                format!(
                    "    put(\"k{}\", new Point({}.5, {}.25), \"r{}\");\n",
                    i + seed,
                    i * 3 + seed,
                    i * 7 + seed,
                    i + seed * 2
                )
            })
            .collect()
    };
    let a_src = format!("void a() {{\n{}}}\n", rows(1));
    let b_src = format!("void b() {{\n    init();\n{}}}\n", rows(100));
    let wrap = |body: String| format!("class T {{\n{body}}}\n");
    let a = scan(&[("A.java", &wrap(a_src)), ("B.java", &wrap(b_src))]);
    assert!(
        a.groups.is_empty(),
        "{:#?}",
        groups(&a)
            .iter()
            .map(|g| (g.tokens, g.parameters.len()))
            .collect::<Vec<_>>()
    );
}

/// A class copying eight fields under names built from `spelling`, so two of
/// these with different spellings need more parameters than the density limit
/// allows.
fn header_mapper(class: &str, spelling: &str) -> String {
    let fields = [
        "Id", "Source", "Kind", "Version", "Length", "Time", "Trace", "State",
    ];
    let set = |f: &&str| {
        format!(
            "        message.setHeader({spelling}{f}, event.read{spelling}{f}());
"
        )
    };
    let body: String = fields.iter().map(set).collect();
    format!(
        "class {class} {{
    void fill(Message message, Event event) {{
{body}        done();
    }}
}}
"
    )
}

fn group_files(g: &Group) -> Vec<&str> {
    g.occurrences.iter().map(|o| o.file.as_str()).collect()
}

// Catches one different occurrence vetoing the verbatim copies beside it.
#[test]
fn a_different_occurrence_does_not_veto_verbatim_copies() {
    let opts = ScanOptions::default();
    let files = [
        ("A.java", header_mapper("A", "Dapr")),
        ("B.java", header_mapper("B", "Dapr")),
        ("C.java", header_mapper("C", "Azure")),
    ];
    let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let gs = groups(&scan_with(&files, opts));
    assert_eq!(gs.len(), 1, "{gs:#?}");
    assert_eq!(group_files(&gs[0]), ["A.java", "B.java"]);
    assert!(gs[0].parameters.is_empty(), "{:?}", gs[0].parameters);
}

// Catches subsets of one vetoed group keeping its fingerprint: `show` could
// not tell them apart and accepting one would hide the other.
#[test]
fn each_verbatim_subset_of_a_vetoed_group_has_its_own_fingerprint() {
    let opts = ScanOptions::default();
    let files = [
        ("A.java", header_mapper("A", "Dapr")),
        ("B.java", header_mapper("B", "Dapr")),
        ("C.java", header_mapper("C", "Azure")),
        ("D.java", header_mapper("D", "Azure")),
    ];
    let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let gs = groups(&scan_with(&files, opts));
    assert_eq!(gs.len(), 2, "{gs:#?}");
    assert_ne!(gs[0].fingerprint, gs[1].fingerprint);
}

/// A scan of one mapper per class and spelling, each in `<class>.java`.
fn scan_mappers(mappers: &[(&str, &str)]) -> Analysis {
    let file = |&(class, spelling): &(&str, &str)| {
        (format!("{class}.java"), header_mapper(class, spelling))
    };
    let files: Vec<(String, String)> = mappers.iter().map(file).collect();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    scan_with(&files, ScanOptions::default())
}

// Catches a subset's fingerprint hanging on its neighbours, such as a number
// in file order: the new pair would take the accepted pair's number, and the
// pair it displaced would pass as shrunk, so `check` would see nothing new.
#[test]
fn a_new_verbatim_pair_sorting_first_is_new() {
    let accepted = [("M1", "Dapr"), ("M2", "Dapr"), ("N", "Azure")];
    let opts = ScanOptions::default();
    let baseline = Baseline::from_analysis(&scan_mappers(&accepted), &opts);
    let added = [("A1", "Kafka"), ("A2", "Kafka")];
    let now = scan_mappers(&[added.as_slice(), &accepted].concat());
    let new = baseline.new_groups(&now);
    assert_eq!(new.len(), 1, "{:#?}", groups(&now));
    assert_eq!(
        group_files(&now.group(new[0], true)),
        ["A1.java", "A2.java"]
    );
}

// Catches the same dependence the other way: a move changing which subset
// sorts first would hand a larger subset the smaller one's fingerprint.
#[test]
fn moving_a_larger_verbatim_subset_to_sort_first_is_not_new() {
    let dapr = [("M1", "Dapr"), ("M2", "Dapr")];
    let last = [("Z1", "Azure"), ("Z2", "Azure"), ("Z3", "Azure")];
    let first = [("A1", "Azure"), ("A2", "Azure"), ("A3", "Azure")];
    let opts = ScanOptions::default();
    let baseline =
        Baseline::from_analysis(&scan_mappers(&[dapr.as_slice(), &last].concat()), &opts);
    let now = scan_mappers(&[first.as_slice(), &dapr].concat());
    assert_eq!(baseline.new_groups(&now), Vec::<usize>::new());
}

// Catches runs being limited to statement blocks: two constructor calls
// sharing a long argument prefix are a clone even though the calls differ.
#[test]
fn a_shared_argument_prefix_is_a_clone() {
    let args = "a.id(), a.jobId(), a.status().name(), a.appliedAt(), a.recruiterName(), a.recruiterEmail(), a.coverLetter(), a.notes(), a.createdAt(), a.updatedAt()";
    let src = format!(
        "class R {{\n  static R from(A a) {{\n    return new R({args},\n        null, null);\n  }}\n  static R from(A a, Job job) {{\n    return new R({args},\n        job != null ? job.title() : null, job.url());\n  }}\n}}\n"
    );
    let a = scan(&[("R.java", &src)]);
    let gs = groups(&a);
    assert_eq!(
        gs.len(),
        1,
        "{:#?}",
        gs.iter()
            .map(|g| (&g.kind, g.elements, &g.occurrences))
            .collect::<Vec<_>>()
    );
    assert_eq!((gs[0].kind, gs[0].elements), ("run", 10));
}

// Catches a file node standing in for the only function it contains: one
// side bare, the other with an import, must still match as functions.
#[test]
fn a_bare_function_file_matches_the_same_function_after_imports() {
    let body = SUM_TOTALS.replace("export ", "");
    let with_import = format!("import {{ a }} from \"./a\";\n{body}");
    let a = scan(&[("a.ts", &body), ("b.ts", &with_import)]);
    assert_eq!(a.groups.len(), 1);
    let template = a.group(1, true).template.unwrap();
    assert!(template.starts_with("function sumTotals"), "{template}");
}

// Catches the file node winning over the function it holds: equal tokens,
// but the function is the extractable unit and carries the scope name.
#[test]
fn between_a_file_and_its_only_function_the_function_is_reported() {
    let body = SUM_TOTALS.replace("export ", "");
    let a = scan(&[("a.ts", &body), ("b.ts", &body)]);
    assert_eq!(a.groups.len(), 1);
    assert_eq!(
        a.group(1, false).occurrences[0].scope.as_deref(),
        Some("sumTotals")
    );
}

// Catches listings growing with occurrence count: a brief group must cap
// locations and print each distinct parameter value once, while the full
// form keeps one value per occurrence for rewriting call sites.
#[test]
fn brief_rendering_stays_small_for_widely_repeated_code() {
    let files: Vec<(String, String)> = (0..7)
        .map(|i| {
            (
                format!("f{i}.ts"),
                SUM_TOTALS.replace("\"paid\"", if i % 2 == 0 { "\"even\"" } else { "\"odd\"" }),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let a = scan(&refs);
    let g = a.group(1, false);
    let brief = doppelscan::report::render_group(&g, doppelscan::report::Detail::Brief);
    assert!(brief.contains("... and 2 more"), "{brief}");
    assert!(brief.contains("$1=\"even\" | \"odd\"\n"), "{brief}");
    let full = doppelscan::report::render_group(&g, doppelscan::report::Detail::Full);
    assert_eq!(full.matches("\"even\"").count(), 4, "{full}");
}

const CARD: &str = r#"<section class="card">
  <header class="card-header">
    <h2 class="card-title">@title</h2>
    <button type="button" class="close" (click)="@action()">Close</button>
  </header>
  <div class="card-body">
    <p class="lead">@text</p>
    <a class="more" href="@href">Read more</a>
  </div>
</section>
"#;

fn card(title: &str, action: &str, text: &str, href: &str) -> String {
    CARD.replace("@title", title)
        .replace("@action", action)
        .replace("@text", text)
        .replace("@href", href)
}

// Catches HTML text and attribute values hashed verbatim, which would hide
// every template copy whose labels differ.
#[test]
fn html_copies_differing_in_text_and_attributes_are_one_clone() {
    let a = scan(&[
        (
            "a.html",
            &card("Orders", "closeOrders", "Your orders", "/orders"),
        ),
        (
            "b.html",
            &card("Refunds", "closeRefunds", "Your refunds", "/refunds"),
        ),
    ]);
    assert_eq!(a.groups.len(), 1);
    let g = a.group(1, false);
    let params: Vec<&str> = g.parameters.iter().map(|v| v.values[0].as_str()).collect();
    assert!(params.contains(&"Orders"), "{params:?}");
    assert!(params.contains(&"\"/orders\""), "{params:?}");
}

// Catches tag names becoming placeholders: the same shape built from
// different elements is different markup.
#[test]
fn html_with_different_tags_is_not_a_clone() {
    let a = "<p class=\"note\"><b>One</b> and <i>two</i> and <u>three</u> and <s>four</s> and <q>five</q></p>
";
    let b = "<div class=\"note\"><em>One</em> and <strong>two</strong> and <mark>three</mark> and <small>four</small> and <cite>five</cite></div>
";
    assert!(scan(&[("a.html", a), ("b.html", b)]).groups.is_empty());
}

const FIELDS: &str = r#"
    <label for="name">Name</label>
    <input id="name" type="text" [(ngModel)]="model.name" required>
    <label for="email">Email</label>
    <input id="email" type="email" [(ngModel)]="model.email" required>
    <label for="phone">Phone</label>
    <input id="phone" type="tel" [(ngModel)]="model.phone">
"#;

// Catches element children not being searched for shared runs: the same
// fields pasted into two different forms are one clone.
#[test]
fn a_run_of_elements_shared_by_two_templates_is_a_clone() {
    let signup = format!(
        "<form class=\"signup\">\n  <h1>Join</h1>{FIELDS}  <button>Sign up</button>\n</form>\n"
    );
    let profile = format!(
        "<div class=\"profile\">\n  <img src=\"me.png\" alt=\"\">\n  <form>{FIELDS}  </form>\n  <p>Saved automatically.</p>\n</div>\n"
    );
    let a = scan(&[("a.html", &signup), ("b.html", &profile)]);
    let g = groups(&a);
    assert!(
        g.iter().any(|g| g.occurrences.len() == 2 && g.lines >= 6),
        "{g:?}"
    );
}

fn ignored(code: &str) -> String {
    format!("// doppelscan-ignore-start\n{code}// doppelscan-ignore-end\n")
}

// Catches markers being read but not applied: the marked copy must not
// pair with its twin.
#[test]
fn code_between_ignore_markers_is_not_matched() {
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", &ignored(SUM_REFUNDS))]);
    assert_eq!(groups(&a).len(), 0);
}

// Catches dropping the whole group when one copy is ignored: the other
// two copies are still duplication.
#[test]
fn an_ignored_copy_leaves_the_other_copies_grouped() {
    let a = scan(&[
        ("a.ts", SUM_TOTALS),
        ("b.ts", SUM_REFUNDS),
        (
            "c.ts",
            &ignored(&SUM_TOTALS.replace("sumTotals", "sumFees")),
        ),
    ]);
    let gs = groups(&a);
    assert_eq!(gs.len(), 1);
    let files: Vec<&str> = gs[0].occurrences.iter().map(|o| o.file.as_str()).collect();
    assert_eq!(files, ["a.ts", "b.ts"]);
}

// Catches an end marker that is ignored, so everything after a start
// marker vanishes: code after the region still matches.
#[test]
fn code_after_an_ignore_end_is_matched() {
    let b = format!("{}{SUM_REFUNDS}", ignored("const skipped = 1;\n"));
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", &b)]);
    assert_eq!(groups(&a).len(), 1);
}

// Catches a start marker without an end being dropped instead of covering
// the rest of the file, which would let a forgotten end un-ignore code.
#[test]
fn an_unclosed_ignore_start_covers_the_rest_of_the_file() {
    let b = format!("// doppelscan-ignore-start\n{SUM_REFUNDS}");
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", &b)]);
    assert_eq!(groups(&a).len(), 0);
}

// Catches the file marker being treated as a region start at its line:
// it ignores code above it too.
#[test]
fn an_ignore_file_marker_covers_the_whole_file() {
    let b = format!("{SUM_REFUNDS}// doppelscan-ignore-file\n");
    let a = scan(&[("a.ts", SUM_TOTALS), ("b.ts", &b)]);
    assert_eq!(groups(&a).len(), 0);
}

// Catches markers only working where comments parse with a grammar: files
// read token by token honour them too.
#[test]
fn ignore_markers_work_in_token_level_languages() {
    let code = "void sum_totals(struct order *orders, int n, double rate) {\n    double total = 0;\n    for (int i = 0; i < n; i++) {\n        if (orders[i].paid) {\n            total += orders[i].amount * rate;\n        }\n    }\n    printf(\"%f\n\", total);\n}\n";
    let a = scan(&[("a.c", code), ("b.c", &ignored(code))]);
    assert_eq!(groups(&a).len(), 0);
}

const STATS: &str = "    protected void masterOperation(Task task, Request request, ActionListener<Response> listener) {
        StatsRequest statsRequest = new StatsRequest();
        statsRequest.setParentTask(clusterService.localNode().getId(), task.getId());
        if (request.getTimeout() != null) {
            statsRequest.setTimeout(request.getTimeout());
        }
        client.execute(StatsAction.INSTANCE, statsRequest, listener);
    }
";

/// A class around `member` whose other code differs per class, so the
/// member and not the class is the clone.
fn java_class(name: &str, member: &str) -> String {
    let field = match name {
        "A" => "private int padding;",
        _ => "private final Map<String, Integer> counts = new HashMap<>();",
    };
    format!("class {name} {{\n    {field}\n\n{member}}}\n")
}

/// Where each occurrence of the best group starts, by file.
fn best_group_starts(a: &Analysis) -> Vec<(String, u32)> {
    let g = a.group(1, false);
    g.occurrences
        .iter()
        .map(|o| (o.file.clone(), o.start_line))
        .collect()
}

// Catches a marker annotation splitting a method from its copy: a copy
// moved to a class where nothing is overridden loses `@Override`, and only
// the body matched, so the signature was left out of the extraction.
#[test]
fn a_marker_annotation_does_not_split_a_method_from_its_copy() {
    let annotated = format!("    @Override\n{STATS}");
    let a = scan(&[
        ("A.java", &java_class("A", &annotated)),
        ("B.java", &java_class("B", STATS)),
    ]);
    assert_eq!(
        best_group_starts(&a),
        [("A.java".to_string(), 4), ("B.java".to_string(), 4)]
    );
}

// Catches skipping the annotation but keeping the modifier list it leaves
// empty: `@Override void x()` has a modifiers node that `void x()` lacks.
#[test]
fn a_method_whose_only_modifier_is_a_marker_matches_its_copy() {
    let bare = STATS.replace("protected ", "");
    let annotated = format!("    @Override\n{bare}");
    let a = scan(&[
        ("A.java", &java_class("A", &annotated)),
        ("B.java", &java_class("B", &bare)),
    ]);
    assert_eq!(
        best_group_starts(&a),
        [("A.java".to_string(), 4), ("B.java".to_string(), 4)]
    );
}

// Catches ignoring every annotation: one with arguments configures the
// method (`@Cacheable("stats")`, `@Transactional(readOnly = true)`), so a
// copy without it is not the same method.
#[test]
fn an_annotation_with_arguments_still_tells_methods_apart() {
    let annotated = format!("    @Cacheable(\"stats\")\n{STATS}");
    let a = scan(&[
        ("A.java", &java_class("A", &annotated)),
        ("B.java", &java_class("B", STATS)),
    ]);
    assert_eq!(
        best_group_starts(&a),
        [("A.java".to_string(), 5), ("B.java".to_string(), 4)]
    );
}

// Catches Go's `statement_list` missing from the block kinds: a Go block has
// that one child, so no run of statements in a function was ever found.
#[test]
fn a_run_of_statements_is_found_in_go() {
    let shared = "\ta := x + 1\n\tb := a * 2\n\tc := b - 3\n\td := c + 4\n\te := d * 5\n\tf := e - 6\n\tg := f + 7\n";
    let body = |head: &str, tail: &str| {
        format!(
            "package p\n\nfunc {head}(x int) int {{\n\tprintln(\"{head}\")\n{shared}\treturn {tail}\n}}\n"
        )
    };
    let (one, two) = (body("first", "g"), body("second", "g * x + a"));
    let a = scan(&[("a.go", &one), ("b.go", &two)]);
    let best = groups(&a).remove(0);
    let starts: Vec<u32> = best.occurrences.iter().map(|o| o.start_line).collect();
    assert_eq!(starts, [4, 4]);
}

const MULTI_LINE_IMPORT_TS: &str = r#"export function describe(items: string[], config: Config): string {
    const header = `first line
second line`;
    let out = header;
    for (const item of items) {
        if (item.length > config.min) {
            out += alpha(item) + beta(item);
        }
    }
    return out;
}

import {
    alpha,
    beta,
} from "./letters";

export function count(items: string[], config: Config): number {
    let total = 0;
    for (const item of items) {
        if (item.length > config.max) {
            total += item.length * config.rate;
        }
    }
    return total;
}
"#;

const MULTI_LINE_STRING_SQL: &str = "SELECT id, name, 'first line\nsecond line' AS label, total\nFROM orders\nWHERE status = 'paid' AND total > 100 AND region = 'north'\nORDER BY total DESC, name ASC, id ASC;\n";

fn fingerprints(a: &Analysis) -> Vec<String> {
    let mut prints: Vec<String> = groups(a).into_iter().map(|g| g.fingerprint).collect();
    prints.sort();
    prints
}

// Catches raw bytes reaching the hashes: a mid-file multi-line import is
// hashed as text, so an LF and a CRLF copy lose their whole-file group;
// strict multi-line literals differ the same way; and normalising past the
// early return for grammar-less languages would miss the SQL case.
#[test]
fn line_endings_do_not_change_fingerprints() {
    let cases = [
        ("a.ts", "b.ts", MULTI_LINE_IMPORT_TS),
        ("a.sql", "b.sql", MULTI_LINE_STRING_SQL),
    ];
    for (a, b, lf) in cases {
        let crlf = lf.replace('\n', "\r\n");
        for strict in [false, true] {
            let opts = || ScanOptions {
                min_tokens: 20,
                strict,
                ..ScanOptions::default()
            };
            let both_lf = fingerprints(&scan_with(&[(a, lf), (b, lf)], opts()));
            assert!(!both_lf.is_empty(), "{a} strict={strict}");
            let mixed = scan_with(&[(a, lf), (b, &crlf)], opts());
            let both_crlf = scan_with(&[(a, &crlf), (b, &crlf)], opts());
            assert_eq!(fingerprints(&mixed), both_lf, "{a} strict={strict}");
            assert_eq!(fingerprints(&both_crlf), both_lf, "{a} strict={strict}");
        }
    }
}

fn line_spans(a: &Analysis) -> Vec<(u32, u32)> {
    let mut spans: Vec<(u32, u32)> = groups(a)
        .iter()
        .flat_map(|g| &g.occurrences)
        .map(|o| (o.start_line, o.end_line))
        .collect();
    spans.sort();
    spans.dedup();
    spans
}

const RS_WITH_MID_FILE_USE: &str = "fn first(a: u32, b: u32) -> u32 { a + b * 2 - a / 3 }\nuse std::fmt;\nfn second(a: u32, b: u32) -> u32 { a * b + 4 - b / 5 }\n";

const GO_SWITCH: &str = "package main\n\nfunc pick(n int) int {\n\tswitch n {\n\tcase 1:\n\t\tvalue := compute(n, 1)\n\t\tlog(value, n)\n\t\treturn value + offset(n)\n\tcase 2:\n\t\tvalue := compute(n, 2)\n\t\tlog(value, n)\n\t\treturn value + offset(n)\n\t}\n\treturn 0\n}\n";

// Catches an end line taken from an end offset just past a newline, which
// reports the line after the code: the next line of the file, or a line
// that does not exist.
#[test]
fn a_clone_ends_on_its_last_line_of_code() {
    let a = scan(&[
        ("a.rs", RS_WITH_MID_FILE_USE),
        ("b.rs", RS_WITH_MID_FILE_USE),
    ]);
    assert_eq!(line_spans(&a), [(1, 3)]);
}

// Catches trimming only one newline: trailing blank lines inside a node's
// range are not code either.
#[test]
fn a_clone_end_skips_trailing_blank_lines() {
    let padded = format!("{RS_WITH_MID_FILE_USE}\n\n\n");
    let a = scan(&[("a.rs", &padded), ("b.rs", &padded)]);
    assert_eq!(line_spans(&a), [(1, 3)]);
}

// Catches a Go case's range, which runs to the next line after its last
// statement, reaching the switch's closing brace.
#[test]
fn a_last_case_ends_before_the_closing_brace() {
    let a = scan(&[("a.go", GO_SWITCH)]);
    assert_eq!(line_spans(&a), [(5, 8), (9, 12)]);
}

// Catches a run dropped because it repeats inside one file when another file
// holds a verbatim copy: 4 statements in A and 2 in B are 34 tokens each, so
// only the run of two reaches the default size.
#[test]
fn a_tandem_run_copied_to_another_file_is_a_clone() {
    let statement = "    r.add(p.parse(c, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11));
";
    let method = |count: usize| {
        format!(
            "void m() {{
{}}}
",
            statement.repeat(count)
        )
    };
    let (a, b) = (method(4), method(2));
    let gs = groups(&scan_with(
        &[("A.java", &a), ("B.java", &b)],
        ScanOptions::default(),
    ));
    assert_eq!(gs.len(), 1, "{gs:#?}");
    let places: Vec<_> = gs[0]
        .occurrences
        .iter()
        .map(|o| (o.file.as_str(), o.start_line))
        .collect();
    assert_eq!(places, [("A.java", 2), ("A.java", 4), ("B.java", 2)]);
}

fn py_table(name: &str, row: &str, rows: usize) -> String {
    format!("{name} = [\n{}]\n", format!("    {row}\n").repeat(rows))
}

fn groups_of(files: &[(&str, &str)]) -> Vec<Group> {
    groups(&scan_with(files, ScanOptions::default()))
}

const ZERO_ROW: &str = "0, 0, 0, 0, 0, 0, 0, 0,";

// Catches every length that fits the shorter table once counting as a copy
// of the longer: tilings at different lengths do not subsume one another, so
// the pair floods the report with one group per length.
#[test]
fn a_zero_table_and_a_shorter_one_are_one_group() {
    let (long, short) = (py_table("A", ZERO_ROW, 100), py_table("B", ZERO_ROW, 40));
    let gs = groups_of(&[("a.py", &long), ("b.py", &short)]);
    assert_eq!(gs.len(), 1, "{} groups", gs.len());
    let in_short: Vec<_> = gs[0]
        .occurrences
        .iter()
        .filter(|o| o.file == "b.py")
        .map(|o| (o.start_line, o.end_line))
        .collect();
    assert_eq!(in_short, [(2, 41)]);
}

// The same flood with both tables in one file, where only blocks tell them
// apart.
#[test]
fn two_zero_arrays_in_one_file_are_one_group() {
    let row = "    0, 0, 0, 0, 0, 0, 0, 0,\n";
    let text = format!(
        "const a = [\n{}];\nconst b = [\n{}];\n",
        row.repeat(60),
        row.repeat(25)
    );
    let gs = groups_of(&[("t.ts", &text)]);
    assert_eq!(gs.len(), 1, "{} groups", gs.len());
}

// Catches a rule that only looks at whether the copy repeats at the next
// offset: with a period of two the run also fits at the odd phase, once.
#[test]
fn a_table_with_a_longer_period_is_one_group_too() {
    let row = "0, \"a\", 0, \"a\", 0, \"a\", 0, \"a\",";
    let (long, short) = (py_table("A", row, 100), py_table("B", row, 40));
    let gs = groups_of(&[("a.py", &long), ("b.py", &short)]);
    assert_eq!(gs.len(), 1, "{} groups", gs.len());
}

// Catches the longest run being the plain pair of the shorter table with the
// start of the longer, which leaves the tiled runs below it nothing to lose
// to: the longer table holds it less than twice.
#[test]
fn a_table_under_twice_as_long_is_one_group() {
    let (long, short) = (py_table("A", ZERO_ROW, 60), py_table("B", ZERO_ROW, 40));
    let gs = groups_of(&[("a.py", &long), ("b.py", &short)]);
    assert_eq!(gs.len(), 1, "{} groups", gs.len());
}

// Catches a copy counting only while the run fits it exactly once: the wall
// breaks between its two stretches and the copy runs one statement past
// either, so its whole length is no repeat and this is the only group there
// is to report.
#[test]
fn a_copy_longer_than_any_stretch_of_the_wall_is_still_found() {
    let statement = "    r.add(p.parse(c, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11));\n";
    let stretch = statement.repeat(4);
    let wall = format!("void m() {{\n{stretch}    other();\n{stretch}}}\n");
    let copy = format!("void n() {{\n{}}}\n", statement.repeat(5));
    let gs = groups_of(&[("A.java", &wall), ("B.java", &copy)]);
    assert_eq!(gs.len(), 1, "{gs:#?}");
    let files: Vec<_> = gs[0].occurrences.iter().map(|o| o.file.as_str()).collect();
    assert_eq!(files, ["A.java", "A.java", "B.java"]);
}

// A wall in `A.java`, a verbatim part of it in `verbatim` and the same part
// renamed in `renamed`, both alone in their blocks. The two copies are clones
// of each other too, so this is the group that reaches the wall. Sorted as
// the walk would.
fn wall_group_with_two_copies(verbatim: &str, renamed: &str) -> Vec<String> {
    let setting =
        |names: &[&str]| -> Vec<String> { names.iter().map(|n| parsed_setting(n)).collect() };
    let kept = settings_copy(&setting(&SETTING_NAMES[2..5]));
    let changed = settings_copy(&setting(&["One", "Two", "Three"]));
    let wall = settings_wall();
    let mut files = [("A.java", &wall), (verbatim, &kept), (renamed, &changed)];
    files.sort_by_key(|(path, _)| *path);
    let gs = groups_of(&files.map(|(p, t)| (p, t.as_str())));
    let in_wall = |g: &&Group| g.occurrences.iter().any(|o| o.file == "A.java");
    let found = gs.iter().find(in_wall).expect("a group in the wall");
    found.occurrences.iter().map(|o| o.file.clone()).collect()
}

// Catches only the first lone block being tried: which copy that is depends
// on how the files sort. Also catches a lone copy with no twin in the wall
// joining the group: a renamed copy is not part of it under either name.
#[test]
fn a_verbatim_copy_is_found_whatever_a_renamed_one_is_called() {
    assert_eq!(
        wall_group_with_two_copies("C.java", "D.java"),
        ["A.java", "C.java"]
    );
    assert_eq!(
        wall_group_with_two_copies("C.java", "B.java"),
        ["A.java", "C.java"]
    );
}

fn put_lines(key: &str, count: usize) -> String {
    (0..count)
        .map(|n| format!("        out.put(\"{key}A{n}\", {key}B{n}.{key}C{n}(row.{key}D{n}));\n"))
        .collect()
}

// Catches a run longer than the copy shadowing it before the density limit
// has judged that run: a vetoed group must not hide the smaller clones inside
// it. The whole method fails the limit, its tails differing in every name.
#[test]
fn a_verbatim_head_survives_a_longer_run_over_the_density_limit() {
    let method = |tail: &str| {
        let lines = put_lines("same", 7) + &put_lines(tail, 6);
        format!(
            "void m() {{
{lines}}}
"
        )
    };
    let (a, b) = (method("left"), method("right"));
    let gs = groups_of(&[("A.java", &a), ("B.java", &b)]);
    assert_eq!(gs.len(), 1, "{gs:#?}");
    let first = &gs[0].occurrences[0];
    assert!(first.start_line == 2 && first.end_line < 14, "{first:?}");
}

fn setter_block(method: &str, names: &[&str]) -> String {
    let set = |n: &&str| format!("        d.set{n}(read(map, \"{n}\"));\n");
    let body: String = names.iter().map(set).collect();
    format!("class {method} {{\n    void m() {{\n{body}    }}\n}}\n")
}

// Catches lone copies being taken before the ones with no twin are ruled out:
// `Y` starts one statement ahead of the verbatim part, so its own lone copy,
// spelled like nothing else, overlaps the part and pushes it out.
#[test]
fn a_lone_copy_with_no_twin_does_not_hide_the_part_beside_it() {
    let part = ["Aa", "Bb", "Cc", "Dd", "Ee", "Ff"];
    let wall: Vec<String> = (0..14).map(|n| format!("W{n}")).collect();
    let wall: Vec<&str> = wall.iter().map(String::as_str).collect();
    let x = [&part[..], &["Gg", "Hh", "Ii", "Jj"]].concat();
    let y = [&["Pp"], &part[..], &["Qq", "Rr", "Ss", "Tt"]].concat();
    let (w, x, y) = (
        setter_block("W", &wall),
        setter_block("X", &x),
        setter_block("Y", &y),
    );
    let gs = groups_of(&[("W.java", &w), ("X.java", &x), ("Y.java", &y)]);
    let is_part = |g: &&Group| {
        let places: Vec<_> = g
            .occurrences
            .iter()
            .map(|o| (o.file.as_str(), o.start_line))
            .collect();
        places == [("X.java", 3), ("Y.java", 4)]
    };
    assert!(gs.iter().any(|g| is_part(&g)), "{gs:#?}");
}
