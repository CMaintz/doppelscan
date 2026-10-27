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
