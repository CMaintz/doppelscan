use std::time::Instant;

use doppelscan::engine::{ScanOptions, Source, index_sources};
use doppelscan::lang::lang_index_for_path;
use doppelscan::near::{NearAnalysis, NearGroup, near_analysis};

fn scan_near(files: &[(&str, String)], similarity: f32) -> NearAnalysis {
    let sources = files
        .iter()
        .map(|(path, text)| Source {
            path: path.to_string(),
            lang: lang_index_for_path(path.as_ref()).unwrap(),
            text: text.clone(),
        })
        .collect();
    let opts = ScanOptions {
        min_tokens: 30,
        ..ScanOptions::default()
    };
    near_analysis(
        index_sources(sources, false),
        &opts,
        similarity,
        Instant::now(),
    )
}

fn groups(a: &NearAnalysis) -> Vec<NearGroup> {
    (1..=a.groups.len()).map(|id| a.group(id)).collect()
}

const REPORT: &str = r#"
export function buildReport(orders: Order[], config: Config): Report {
    const lines: string[] = [];
    let total = 0;
    for (const order of orders) {
        if (order.status !== "paid") {
            continue;
        }
        const amount = order.amount * config.rate;
        total += amount;
        lines.push(`${order.id}: ${amount.toFixed(2)}`);
    }
    lines.push(`total: ${total.toFixed(2)}`);
    return { title: config.title, lines, total };
}
"#;

/// The same function with one statement added in the middle of the loop.
fn with_audit(code: &str) -> String {
    code.replace("buildReport", "buildAuditedReport").replace(
        "        total += amount;\n",
        "        total += amount;\n        audit.record(order.id, amount, config.rate);\n",
    )
}

const PARSE: &str = r#"
export function parseCsv(text: string, separator: string): Row[] {
    const rows: Row[] = [];
    const header = text.split("\n")[0].split(separator);
    for (const line of text.split("\n").slice(1)) {
        const cells = line.split(separator).map((c) => c.trim());
        if (cells.length !== header.length) {
            throw new Error(`bad row: ${line}`);
        }
        rows.push(Object.fromEntries(header.map((h, i) => [h, cells[i]])));
    }
    return rows;
}
"#;

// Catches exact-only matching: a copy with an inserted statement is the
// near-miss clone this mode exists for, and the inserted line is what an
// agent needs pointed out.
#[test]
fn a_copy_with_an_inserted_statement_is_a_near_duplicate() {
    let a = scan_near(
        &[("a.ts", REPORT.to_string()), ("b.ts", with_audit(REPORT))],
        0.7,
    );
    let gs = groups(&a);
    assert_eq!(gs.len(), 1, "{gs:?}");
    let g = &gs[0];
    assert_eq!(g.occurrences.len(), 2);
    assert!(
        g.similarity < 1.0 && g.similarity >= 0.7,
        "{}",
        g.similarity
    );
    let audit_line = with_audit(REPORT)
        .lines()
        .position(|l| l.contains("audit."))
        .unwrap() as u32
        + 1;
    let b = g.occurrences.iter().find(|o| o.file == "b.ts").unwrap();
    let differs = b.differs.as_ref().expect("short enough to diff");
    assert!(
        differs
            .iter()
            .any(|&(from, to)| from <= audit_line && audit_line <= to),
        "line {audit_line} should be marked: {differs:?}"
    );
    let marked: u32 = differs.iter().map(|&(from, to)| to - from + 1).sum();
    assert!(marked <= 2, "only the insertion: {differs:?}");
}

/// The report with one statement changed rather than added.
fn with_refunds(code: &str) -> String {
    code.replace("buildReport", "buildRefunds").replace(
        "        total += amount;\n",
        "        total -= amount * 2;\n",
    )
}

// Catches diffing a copy against its own exact twin, which shows nothing,
// and exact twins lost when their class is expanded.
#[test]
fn exact_twins_are_each_diffed_against_a_different_copy() {
    let twin = REPORT.replace("buildReport", "buildSummary");
    let files = [
        ("a.ts", REPORT.to_string()),
        ("b.ts", twin),
        ("c.ts", with_refunds(REPORT)),
    ];
    let gs = groups(&scan_near(&files, 0.7));
    assert_eq!(gs.len(), 1, "{gs:?}");
    assert_eq!(gs[0].occurrences.len(), 3, "{gs:?}");
    for o in &gs[0].occurrences {
        let differs = o.differs.as_ref().unwrap();
        assert!(!differs.is_empty(), "{} shows no difference", o.file);
    }
}

// Catches an expanded exact copy landing inside another member of its own
// group: the same code counted twice.
#[test]
fn no_member_of_a_group_sits_inside_another() {
    let arrow = "(orders: Order[], config: Config): number => {
    let total = 0;
    for (const order of orders) {
        if (order.status === \"paid\") { total += order.amount * config.rate; }
    }
    return Math.round(total * 100) / 100;
}";
    let changed = arrow.replace(
        "total += order.amount",
        "total -= order.fee * 2 + order.amount",
    );
    let files = [
        ("a.ts", format!("export const report = {arrow};\n")),
        (
            "b.ts",
            format!("export function makeReport() {{\n    return {arrow};\n}}\n"),
        ),
        ("c.ts", format!("export const refunds = {changed};\n")),
    ];
    let gs = groups(&scan_near(&files, 0.5));
    for g in &gs {
        for (i, a) in g.occurrences.iter().enumerate() {
            for b in &g.occurrences[i + 1..] {
                let overlap =
                    a.file == b.file && a.start_line <= b.end_line && b.start_line <= a.end_line;
                assert!(!overlap, "{a:?} overlaps {b:?}");
            }
        }
    }
}

const TWO_FORMATTERS: &str = r#"
export function buildReport(orders: Order[], config: Config): Report {
    const fmtA = (value: number, unit: string): string => {
        const fixed = value.toFixed(2);
        const padded = fixed.padStart(12, " ");
        return `${padded} ${unit}`.trimEnd() + (unit === "" ? "" : ".");
    };
    const fmtB = (value: number, unit: string): string => {
        const fixed = value.toFixed(2);
        const padded = fixed.padStart(12, " ");
        trace(fixed, unit);
        return `${padded} ${unit}`.trimEnd() + (unit === "" ? "" : ".");
    };
@body}
"#;

// Catches nested suppression dropping copies that are not carried by an
// enclosing copy: both lambdas sit in one function, so they are a
// duplicate of their own even though that function matches another.
#[test]
fn near_copies_inside_one_function_are_reported() {
    let body = REPORT
        .lines()
        .skip(2)
        .take_while(|l| !l.starts_with('}'))
        .map(|l| format!("{l}\n"))
        .collect::<String>();
    let with_lambdas = TWO_FORMATTERS.replace("@body", &body);
    let other = REPORT.replace("buildReport", "buildOther");
    // 0.4 so the enclosing functions (41% alike) form a group to test against.
    let gs = groups(&scan_near(&[("x.ts", with_lambdas), ("y.ts", other)], 0.4));
    let lambdas = gs
        .iter()
        .any(|g| g.occurrences.iter().all(|o| o.file == "x.ts"));
    assert!(lambdas, "{gs:?}");
}

// Catches a threshold that is not applied: one statement in fourteen is
// well below 95% similarity.
#[test]
fn similarity_below_the_threshold_is_not_reported() {
    let a = scan_near(
        &[("a.ts", REPORT.to_string()), ("b.ts", with_audit(REPORT))],
        0.95,
    );
    assert!(groups(&a).is_empty());
}

// Catches false positives between functions that only share size and
// common idioms (loops, pushes, returns).
#[test]
fn unrelated_functions_are_not_near_duplicates() {
    let a = scan_near(
        &[("a.ts", REPORT.to_string()), ("b.ts", PARSE.to_string())],
        0.5,
    );
    assert!(groups(&a).is_empty(), "{:?}", groups(&a));
}

// Catches reporting exact clones a second time: the exact scan already
// covers them, with a template.
#[test]
fn exact_clones_are_left_to_the_exact_scan() {
    let renamed = REPORT.replace("buildReport", "buildSummary");
    let a = scan_near(&[("a.ts", REPORT.to_string()), ("b.ts", renamed)], 0.7);
    assert!(groups(&a).is_empty(), "{:?}", groups(&a));
}

// Catches a nested function being reported on its own when the function
// around it is already a near duplicate.
#[test]
fn a_nested_function_inside_a_near_duplicate_is_not_reported_again() {
    // The nested copies differ too, so they are a near pair of their own.
    let nest = |code: String, extra: &str| {
        let format = format!(
            "    const format = (value: number, unit: string): string => {{\n        const fixed = value.toFixed(2);\n        const padded = fixed.padStart(12, \" \");\n{extra}        return `${{padded}} ${{unit}}`.trimEnd() + (unit === \"\" ? \"\" : \".\");\n    }};\n    return {{ title"
        );
        code.replace("    return { title", &format)
    };
    let a = scan_near(
        &[
            ("a.ts", nest(REPORT.to_string(), "")),
            (
                "b.ts",
                nest(with_audit(REPORT), "        trace(fixed, unit);\n"),
            ),
        ],
        0.5,
    );
    let gs = groups(&a);
    assert_eq!(gs.len(), 1, "{gs:?}");
}

// Catches clusters built from pairs alone: three copies, each with its own
// small change, are one group of three.
#[test]
fn several_near_copies_form_one_group() {
    let c = REPORT.replace("buildReport", "buildRefundReport").replace(
        "    lines.push(`total",
        "    lines.sort();\n    lines.push(`total",
    );
    let a = scan_near(
        &[
            ("a.ts", REPORT.to_string()),
            ("b.ts", with_audit(REPORT)),
            ("c.ts", c),
        ],
        0.7,
    );
    let gs = groups(&a);
    assert_eq!(gs.len(), 1, "{gs:?}");
    assert_eq!(gs[0].occurrences.len(), 3);
}

const INJECT: &str = r#"
export function wire(target: Target, deps: Deps): void {
@lines    target.ready = true;
    target.started = Date.now();
}
"#;

// Catches comparing fingerprints as sets: a function repeating one line
// three times and one repeating it nine times would look identical.
#[test]
fn repetition_counts_toward_similarity() {
    let lines = |n: usize| {
        (0..n)
            .map(|i| format!("    target.slot{i} = deps.slot{i};\n"))
            .collect::<String>()
    };
    let short = INJECT.replace("@lines", &lines(3));
    let long = INJECT
        .replace("@lines", &lines(9))
        .replace("wire", "wireAll");
    let a = scan_near(&[("a.ts", short), ("b.ts", long)], 0.8);
    assert!(groups(&a).is_empty(), "{:?}", groups(&a));
}

// Catches a constructor's body counting as a function of its own, which
// put every constructor in near groups twice.
#[test]
fn a_constructor_is_one_unit() {
    let java = "class Orders {
    private final Repo repo;
    private final Clock clock;
    Orders(Repo repo, Clock clock, Config config) {
        this.repo = repo;
        this.clock = clock;
        this.limit = config.limit() * 2 + config.offset();
    }
}";
    let mut parser = tree_sitter::Parser::new();
    let lang = lang_index_for_path("A.java".as_ref()).unwrap();
    let file =
        doppelscan::index::index_source(&mut parser, "A.java".into(), lang, java.into(), false)
            .unwrap();
    assert_eq!(file.units.len(), 1, "{:?}", file.units);
}

const GUARD: &str = "    if (input === undefined || input === null) {\n        throw new Error(\"missing input\");\n    }\n";

/// A function opening with `guards` copies of a guard clause, as an idiom
/// that most functions in a codebase share.
fn guarded(name: &str, guards: usize, body: &str) -> String {
    format!(
        "export function {name}(input: Input): number {{\n{}{body}}}\n",
        GUARD.repeat(guards)
    )
}

/// Groups found among `pair` plus enough functions sharing the guard idiom
/// to make it common: more than the cull floor of 100 functions, below which
/// no fingerprint counts as idiom and these tests would test nothing.
fn near_with_idiom(pair: [(&str, String); 2]) -> Vec<NearGroup> {
    let noise = (0..120).map(|i| {
        let tail = "    log(input);\n".repeat(i + 1);
        let name = format!("noise{i}");
        (format!("{name}.ts"), guarded(&name, 2, &tail))
    });
    let files: Vec<(String, String)> = noise.chain(pair.map(|(p, t)| (p.to_string(), t))).collect();
    let refs: Vec<(&str, String)> = files.iter().map(|(p, t)| (p.as_str(), t.clone())).collect();
    groups(&scan_near(&refs, 0.8))
}

/// The similarity of the group holding both a.ts and b.ts, if any.
fn pair_similarity(found: &[NearGroup]) -> Option<f64> {
    let has = |g: &NearGroup, f: &str| g.occurrences.iter().any(|o| o.file == f);
    found
        .iter()
        .find(|g| has(g, "a.ts") && has(g, "b.ts"))
        .map(|g| g.similarity)
}

// Catches idiom fingerprints being dropped from what two functions share
// but kept in their sizes: a pair alike apart from one statement, but
// heavy with an idiom hundreds of other functions also use, would score
// far below its real similarity and go unreported.
#[test]
fn a_shared_idiom_does_not_dilute_similarity() {
    let body = "    const rows = input.rows.filter((r) => r.active);\n    const total = rows.reduce((s, r) => s + r.amount * input.rate, 0);\n    const label = `${input.name}: ${total.toFixed(2)}`;\n    report.write(label, rows.length, input.currency);\n";
    let found = near_with_idiom([
        (
            "a.ts",
            guarded("summarize", 6, &format!("{body}    return total;\n")),
        ),
        (
            "b.ts",
            guarded(
                "summarizeAll",
                6,
                &format!("{body}    return rows.length;\n"),
            ),
        ),
    ]);
    let similarities: Vec<f64> = found.iter().map(|g| g.similarity).collect();
    assert!(pair_similarity(&found).is_some(), "{similarities:?}");
}

// Catches idiom fingerprints being left out of the score altogether: two
// functions that differ only in how much of the idiom they carry (2 guard
// clauses against 12) are not 100% alike.
#[test]
fn a_different_amount_of_idiom_still_counts() {
    let body = "    const rows = input.rows.filter((r) => r.active);\n    const total = rows.reduce((s, r) => s + r.amount * input.rate, 0);\n    return total;\n";
    let found = near_with_idiom([
        ("a.ts", guarded("summarize", 2, body)),
        ("b.ts", guarded("summarizeAll", 12, body)),
    ]);
    let similarity = pair_similarity(&found);
    assert!(similarity.is_none(), "reported as {similarity:?} alike");
}

const ROUNDING_SHORT: &str = r#"
function f() {
    const r = a ? b : c;
    x.y.z(1, "s");
    delete p.q;
    if (a > 1) { b(); }
    await w();
    return;
    for (const x of y) { z.push(x); }
    let u = [1, 2, 3];
    delete p.q;
    try { f(); } catch (e) { g(e); }
    const r = a ? b : c;
    const r = a ? b : c;
    for (const x of y) { z.push(x); }
    while (i < 3) { i += 2; }
    await w();
}
"#;

const ROUNDING_ADDED: &str = r#"    try { f(); } catch (e) { g(e); }
    n = -m * (k + 1) / 2;
    delete p.q;
    n = -m * (k + 1) / 2;
    while (i < 3) { i += 2; }
    while (i < 3) { i += 2; }
    await w();
    const r = a ? b : c;
    try { f(); } catch (e) { g(e); }
"#;

// Catches a size prefilter stricter than the score: these fingerprint
// counts are 15 inside 25, exactly 0.6 by Jaccard, but 0.6f32 * 25 rounds
// up to 15.000001 and the pair was dropped before it was scored. If token
// hashing changes, this stops being exactly 15/25 and fails on the
// similarity rather than passing without testing anything.
#[test]
fn a_pair_scoring_exactly_the_threshold_is_reported() {
    let tail = "    await w();\n}\n";
    let long = ROUNDING_SHORT
        .replace("function f", "function g")
        .replace(tail, &format!("{ROUNDING_ADDED}{tail}"));
    let gs = groups(&scan_near(
        &[("a.ts", ROUNDING_SHORT.to_string()), ("b.ts", long)],
        0.6,
    ));
    assert_eq!(gs.len(), 1, "{gs:?}");
    assert_eq!(gs[0].similarity, 0.6);
}

const KOTLIN_BODY: &str = "    val first = it.items.filter { item -> item.active }
    val total = first.sumOf { item -> item.amount * rate }
    val label = \"${it.name}: ${total}\"
    report.write(label, first.size, it.currency)
";

/// The body as a block passed to `with`, where the grammar wraps the lambda
/// in an `annotated_lambda` covering exactly the same text.
fn kotlin_with(extra: &str) -> String {
    format!("val q = with(x) {{\n{KOTLIN_BODY}{extra}    label\n}}\n")
}

/// The body as a plain function, with no wrapper around it.
fn kotlin_fun(name: &str, extra: &str) -> String {
    format!("fun {name}(it: Order) {{\n{KOTLIN_BODY}{extra}    return label\n}}\n")
}

// Catches a grammar wrapper with the same span as its lambda counting as a
// second unit: one function listed twice, with x and score inflated.
#[test]
fn a_function_is_listed_once_whatever_wraps_it() {
    let files = [
        ("a.kt", kotlin_fun("one", "    log(total)\n")),
        ("b.kt", kotlin_fun("two", "    trace(label)\n")),
        ("c.kt", kotlin_with("")),
    ];
    let gs = groups(&scan_near(&files, 0.5));
    assert_eq!(gs.len(), 1, "{gs:?}");
    let g = &gs[0];
    let at: Vec<_> = g
        .occurrences
        .iter()
        .map(|o| (&o.file, o.start_line))
        .collect();
    assert_eq!(g.occurrences.len(), 3, "{at:?}");
    assert_eq!(g.score, g.tokens as u64 * 2);
}

// Catches a wrapper being told apart from the lambda it wraps: a block
// passed to `with` and its exact copy passed as a plain argument are the
// same code, so which file sorts first must not turn them into a 100%
// near pair.
#[test]
fn a_wrapped_lambda_and_its_bare_copy_are_left_to_the_exact_scan() {
    let block = format!("{{\n{KOTLIN_BODY}    label\n}}\n");
    let wrapped = format!("val q = with(x) {block}");
    let bare = format!("val r = with(x, {block})");
    for (first, second) in [(&wrapped, &bare), (&bare, &wrapped)] {
        let files = [("a.kt", first.clone()), ("b.kt", second.clone())];
        let gs = groups(&scan_near(&files, 0.8));
        assert!(gs.is_empty(), "{gs:?}");
    }
}

// Catches nesting judged on each class's first copy: a wrapper around an
// arrow and that arrow standing alone are a real pair, but only if the
// standalone copy happened to sort first.
#[test]
fn a_near_pair_does_not_depend_on_which_file_sorts_first() {
    let arrow = "(orders: Order[], config: Config): number => {
    let total = 0;
    for (const order of orders) {
        if (order.status === \"paid\") { total += order.amount * config.rate; }
    }
    return Math.round(total * 100) / 100;
}";
    let wrapper = format!("export function make() {{\n    return {arrow};\n}}\n");
    let alone = format!("export const solo = {arrow};\n");
    let found = |paths: [&str; 2]| {
        let files = [(paths[0], wrapper.clone()), (paths[1], alone.clone())];
        groups(&scan_near(&files, 0.8))
    };
    let (wrapper_first, wrapper_last) = (
        found(["a_wrapper.ts", "b_alone.ts"]),
        found(["b_wrapper.ts", "a_alone.ts"]),
    );
    assert_eq!(wrapper_first.len(), 1, "{wrapper_first:?}");
    assert_eq!(wrapper_last.len(), 1, "{wrapper_last:?}");
    assert_eq!(wrapper_first[0].similarity, wrapper_last[0].similarity);
    assert_eq!(wrapper_first[0].occurrences.len(), 2);
}

fn statements(range: std::ops::Range<usize>) -> String {
    let shape = |i: usize| match i % 4 {
        0 => format!("    for (const k of list{i}) {{ sum[k] = fn{i}(k, total); }}\n"),
        1 => format!("    while (i{i} < n) {{ i{i} = next(i{i}, step); count++; }}\n"),
        2 => format!("    if (a{i}.x > {i}) {{ total += a{i}.y * {i}; }}\n"),
        _ => format!("    try {{ run{i}(v, w); }} catch (e) {{ log(e, {i}); }}\n"),
    };
    range.map(shape).collect()
}

// Catches a wrapper standing in for what it wraps: each copy of the
// function brings its own lambda, so copies in two files made the two
// classes a pair, the lambda's twin joined them, and the lambdas were then
// dropped as nested. That listed the function beside a twin it does not
// match, at the wrapper's score.
#[test]
fn a_function_does_not_stand_in_for_its_lambda() {
    // Fingerprints count by shape, so the extras need shapes of their own
    // or the function would really match the twin.
    let lambda = format!("  const h = (a, b) => {{\n{}  }};\n", statements(0..40));
    let wrapping = "  const v = obj.get(key).map(q => q + 1).filter(Boolean);\n".repeat(12);
    let outer = |name: &str| format!("function {name}() {{\n{lambda}{wrapping}  return h;\n}}\n");
    let own = "    switch (x) { case 1: y(); break; default: z(); }\n".repeat(4);
    let twin = format!("function twin(a, b) {{\n{}{own}}}\n", statements(0..36));
    for paths in [["a.ts", "b.ts", "c.ts"], ["b.ts", "c.ts", "a.ts"]] {
        let files = [
            (paths[0], outer("one")),
            (paths[1], outer("two")),
            (paths[2], twin.clone()),
        ];
        let gs = groups(&scan_near(&files, 0.7));
        assert_eq!(gs.len(), 1, "{gs:?}");
        let mut starts: Vec<u32> = gs[0].occurrences.iter().map(|o| o.start_line).collect();
        starts.sort_unstable();
        assert_eq!(starts, [1, 2, 2], "{gs:?}");
    }
}

// Catches dropping nesting altogether: a function and the lambda that
// makes up most of it are the same code, not a pair.
#[test]
fn a_function_is_not_a_near_copy_of_its_own_lambda() {
    let code = "export function make() {
    return (orders: Order[], config: Config): number => {
        let total = 0;
        for (const order of orders) {
            if (order.status === \"paid\") { total += order.amount * config.rate; }
        }
        return Math.round(total * 100) / 100;
    };
}
";
    let gs = groups(&scan_near(&[("a.ts", code.to_string())], 0.8));
    assert!(gs.is_empty(), "{gs:?}");
}

// Catches the rounded similarity being widened from f32 on its way to
// JSON, which printed 0.8100000023841858 for 81%.
#[test]
fn similarity_in_json_keeps_its_two_decimals() {
    let gs = groups(&scan_near(
        &[("a.ts", REPORT.to_string()), ("b.ts", with_audit(REPORT))],
        0.7,
    ));
    let json = serde_json::to_value(&gs[0]).unwrap();
    let similarity = json["similarity"].to_string();
    assert!(similarity.len() <= 4, "{similarity}");
}
