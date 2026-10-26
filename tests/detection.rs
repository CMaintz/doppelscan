use std::time::Instant;

use doppelscan::engine::{ScanOptions, Source, analyze, index_sources};
use doppelscan::lang::lang_index_for_path;
use doppelscan::report::{Analysis, Group};

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
    assert!(template.starts_with("export function sumTotals"), "{template}");
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
    let renames: Vec<&str> = g.renames.iter().map(|v| v.values[0].as_str()).collect();
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
    let src = format!("function setup() {{\n{body}}}\nfunction other() {{\n{a_stmt}{b_stmt}{a_stmt}}}\n");
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

// Catches location lookup missing a clone that covers the asked-for lines.
#[test]
fn overlapping_finds_the_group_covering_a_line() {
    let a = scan(&[("src/a.ts", SUM_TOTALS), ("src/b.ts", SUM_REFUNDS)]);
    assert_eq!(a.overlapping("src/b.ts", 5, 5), vec![1]);
    assert_eq!(a.overlapping("b.ts", 5, 5), vec![1]);
    assert!(a.overlapping("src/b.ts", 40, 50).is_empty());
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
    assert_eq!(a.group(1, false).occurrences[0].scope.as_deref(), Some("sumTotals"));
}
