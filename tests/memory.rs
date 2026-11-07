//! Peak heap during a scan, counted by the allocator rather than asked of the
//! OS, so the figures are the same on every CI platform. Each scan holds
//! `SCANS` for its whole run because tests share the allocator's counters.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use doppelscan::engine::{ScanOptions, Source, analyze, index_sources};
use doppelscan::lang::lang_index_for_path;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static SCANS: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Bytes the heap grew by at its highest during a scan of `path` holding
/// `text`, with the number of clone groups found.
fn scan_peak(path: &str, text: String) -> (usize, usize) {
    let _alone = SCANS.lock().unwrap();
    let opts = ScanOptions::default();
    let start = LIVE.load(Relaxed);
    PEAK.store(start, Relaxed);
    let lang = lang_index_for_path(path.as_ref()).expect("known extension");
    let source = Source {
        path: path.to_string(),
        lang,
        text,
    };
    let analysis = analyze(index_sources(vec![source], false), &opts, Instant::now());
    (PEAK.load(Relaxed) - start, analysis.groups.len())
}

fn literal_table(rows: usize) -> String {
    let row = "    0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80,\n".repeat(rows);
    format!("[\n{row}]\n")
}

const MB: usize = 1 << 20;

// Catches each nested interval of a periodic table copying its positions: a
// table of n elements nests n intervals averaging n/2 positions, 200 MB here.
#[test]
fn one_long_table_scans_in_linear_memory() {
    let table = format!("TABLE = {}", literal_table(1250));
    let (peak, found) = scan_peak("table.py", table);
    assert!(peak < 32 * MB, "peak {} MB", peak / MB);
    assert_eq!(found, 0);
}

// Catches each nested group between two tables copying its elements' leaf
// ranges: half the table's lengths each survive detection, one per length,
// until the longest subsumes them.
#[test]
fn two_equal_tables_scan_in_linear_memory() {
    let table = literal_table(800);
    let (peak, found) = scan_peak("tables.py", format!("A = {table}B = {table}"));
    assert!(peak < 32 * MB, "peak {} MB", peak / MB);
    assert_eq!(found, 1);
}
