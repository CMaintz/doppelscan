//! Files as they sit on disk: line endings and encodings only reach the
//! scan through the file reader, so these write bytes rather than `Source`s.

use std::path::PathBuf;

use doppelscan::engine::{Engine, ScanOptions};
use doppelscan::report::Analysis;

const CODE: &str = r#"export function sumTotals(orders: Order[], config: Config): number {
    let total = 0;
    for (const order of orders) {
        if (order.status === "paid") {
            total += order.amount * config.rate;
        }
    }
    return Math.round(total * 100) / 100;
}
"#;

fn scan_bytes(name: &str, files: &[(&str, Vec<u8>)]) -> Analysis {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("doppelscan-enc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (path, bytes) in files {
        std::fs::write(dir.join(path), bytes).unwrap();
    }
    let opts = ScanOptions {
        min_tokens: 20,
        ..ScanOptions::default()
    };
    Engine::default().scan(&dir, &opts).unwrap()
}

fn lines_in(found: &Analysis, file: &str) -> Vec<(u32, u32)> {
    (1..=found.groups.len())
        .flat_map(|id| found.group(id, false).occurrences)
        .filter(|o| o.file == file)
        .map(|o| (o.start_line, o.end_line))
        .collect()
}

// Catches a lone CR staying whitespace: every line of the file collapses
// into line 1, so `check` and `duplicates_at` point nowhere useful.
#[test]
fn cr_only_files_report_real_lines() {
    let found = scan_bytes(
        "cr",
        &[
            ("a.ts", CODE.as_bytes().to_vec()),
            ("b.ts", format!("// x\n{CODE}").replace('\n', "\r").into()),
        ],
    );
    assert_eq!(lines_in(&found, "b.ts"), [(2, 10)]);
}

// Catches a lone CR reaching `looks_generated` as one long line: an
// ordinary file over 4 KB reads as a minified bundle and vanishes.
#[test]
fn a_large_cr_only_file_is_not_taken_for_minified() {
    let padding = "// padding line, 24 bytes\n".repeat(300);
    let found = scan_bytes(
        "cr-large",
        &[
            ("a.ts", CODE.as_bytes().to_vec()),
            (
                "b.ts",
                format!("{padding}{CODE}").replace('\n', "\r").into(),
            ),
        ],
    );
    assert_eq!(lines_in(&found, "b.ts"), [(301, 309)]);
}

// Catches turning every CR into a line end: the stray CR of a mixed file is
// no line end to git, grep or a blame, so counting it would shift each
// reported line after it.
#[test]
fn a_stray_cr_in_an_lf_file_does_not_shift_lines() {
    let found = scan_bytes(
        "cr-mixed",
        &[
            ("a.ts", CODE.as_bytes().to_vec()),
            ("b.ts", format!("// a\rb\n{CODE}").into_bytes()),
        ],
    );
    assert_eq!(lines_in(&found, "b.ts"), [(2, 10)]);
}
