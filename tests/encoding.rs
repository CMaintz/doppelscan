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

fn utf16(text: &str, unit: fn(u16) -> [u8; 2]) -> Vec<u8> {
    let units = "\u{feff}".encode_utf16().chain(text.encode_utf16());
    units.flat_map(unit).collect()
}

// Catches a non-UTF-8 byte dropping the whole file: its clones then pass
// `check` as "No new duplication".
#[test]
fn a_file_with_a_latin1_byte_is_scanned_with_real_lines() {
    let mut copy = b"// caf\xe9\n// second line\n".to_vec();
    copy.extend_from_slice(CODE.as_bytes());
    let found = scan_bytes(
        "latin1",
        &[("a.ts", CODE.as_bytes().to_vec()), ("b.ts", copy)],
    );
    assert_eq!(lines_in(&found, "b.ts"), [(3, 11)]);
}

// Catches decoding only one byte order, or keeping the BOM as a character.
// UTF-16 is what Windows tools write, so CRLF comes with it.
#[test]
fn utf16_files_with_a_bom_are_scanned_with_real_lines() {
    let crlf = format!("// one\n{CODE}").replace('\n', "\r\n");
    let found = scan_bytes(
        "utf16",
        &[
            ("a.ts", CODE.as_bytes().to_vec()),
            ("le.ts", utf16(&crlf, u16::to_le_bytes)),
            ("be.ts", utf16(&crlf, u16::to_be_bytes)),
        ],
    );
    assert_eq!(lines_in(&found, "le.ts"), [(2, 10)]);
    assert_eq!(lines_in(&found, "be.ts"), [(2, 10)]);
}

// Catches the lossy path swallowing binary: a file that is not UTF-8 and
// holds a NUL is no text in any encoding read here, and that goes for a
// UTF-32 file too, whose BOM starts like UTF-16's.
#[test]
fn binary_stays_skipped() {
    let mut binary = b"\x89PNG\r\n\x1a\n\x00\x00\xff".to_vec();
    binary.extend_from_slice(CODE.as_bytes());
    let utf32: Vec<u8> = "\u{feff}"
        .chars()
        .chain(CODE.chars())
        .flat_map(|c| (c as u32).to_le_bytes())
        .collect();
    let found = scan_bytes(
        "binary",
        &[
            ("a.ts", CODE.as_bytes().to_vec()),
            ("binary.ts", binary),
            ("utf32.ts", utf32),
        ],
    );
    let scanned: Vec<&str> = found.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(scanned, ["a.ts"]);
}

// Catches the read limit still assuming one byte per character: a UTF-16
// CRLF file is four bytes per ASCII line end and two per character, so one
// well under the text cap is over twice it on disk.
#[test]
fn the_read_limit_leaves_room_for_utf16_with_crlf() {
    let text = format!("{CODE}{}", "//\n".repeat(300_000)).replace('\n', "\r\n");
    let found = scan_bytes(
        "utf16-size",
        &[
            ("a.ts", CODE.as_bytes().to_vec()),
            ("b.ts", utf16(&text, u16::to_le_bytes)),
        ],
    );
    assert_eq!(lines_in(&found, "b.ts"), [(1, 9)]);
}
