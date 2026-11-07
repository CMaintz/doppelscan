use std::time::{Duration, Instant};

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
