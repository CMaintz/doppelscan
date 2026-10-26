//! What differs between the occurrences of a clone, and the helper body
//! that would replace them.
//!
//! Occurrences are structurally identical, so their leaves align 1:1. A
//! position whose text differs is either a *rename* (a name the fragment
//! binds itself, so the extracted helper just picks one) or a *parameter*
//! (a name or constant from outside that the helper must take as input).

use rustc_hash::FxHashMap;
use serde::Serialize;

use crate::detect::{Occ, RawGroup};
use crate::index::{FileIndex, LeafClass};

#[derive(Clone, Debug, Serialize)]
pub struct Variation {
    /// One value per occurrence, in occurrence order.
    pub values: Vec<String>,
    /// How many leaf positions carry this substitution.
    pub uses: usize,
}

#[derive(Default)]
pub struct Variations {
    pub parameters: Vec<Variation>,
    pub renames: Vec<Variation>,
    /// Leaf offsets (within an occurrence) of each parameter, by parameter.
    param_offsets: Vec<Vec<usize>>,
}

struct Slot {
    first: usize,
    offsets: Vec<usize>,
    binds: bool,
    literal: bool,
}

pub fn analyze(group: &RawGroup, files: &[impl AsRef<FileIndex>]) -> Variations {
    let columns: Vec<Vec<u32>> = group
        .occs
        .iter()
        .map(|o| o.leaf_indices().collect())
        .collect();
    let mut slots: FxHashMap<Vec<&str>, Slot> = FxHashMap::default();
    for offset in 0..columns[0].len() {
        let texts: Vec<&str> = group
            .occs
            .iter()
            .zip(&columns)
            .map(|(o, c)| leaf_text(files, o, c[offset]))
            .collect();
        if texts.iter().all(|t| *t == texts[0]) {
            continue;
        }
        let leaf = files[group.occs[0].file as usize].as_ref().leaves[columns[0][offset] as usize];
        let slot = slots.entry(texts).or_insert(Slot {
            first: offset,
            offsets: Vec::new(),
            binds: false,
            literal: false,
        });
        slot.offsets.push(offset);
        slot.binds |= leaf.binds;
        slot.literal |= leaf.class == LeafClass::Literal;
    }
    split(slots)
}

fn leaf_text<'f>(files: &'f [impl AsRef<FileIndex>], occ: &Occ, leaf: u32) -> &'f str {
    let file = files[occ.file as usize].as_ref();
    file.leaf_text(&file.leaves[leaf as usize])
}

fn split(slots: FxHashMap<Vec<&str>, Slot>) -> Variations {
    let mut ordered: Vec<_> = slots.into_iter().collect();
    ordered.sort_by_key(|(_, s)| s.first);
    let mut out = Variations::default();
    for (texts, slot) in ordered {
        let variation = Variation {
            values: texts.iter().map(|t| t.to_string()).collect(),
            uses: slot.offsets.len(),
        };
        if slot.binds && !slot.literal {
            out.renames.push(variation);
        } else {
            out.parameters.push(variation);
            out.param_offsets.push(slot.offsets);
        }
    }
    out
}

/// The first occurrence's source with each parameter replaced by `$1`,
/// `$2`, ...: the body of the helper an agent would extract.
pub fn template(group: &RawGroup, files: &[impl AsRef<FileIndex>], vars: &Variations) -> String {
    let occ = &group.occs[0];
    let file = files[occ.file as usize].as_ref();
    let columns: Vec<u32> = occ.leaf_indices().collect();
    let mut cuts: Vec<(u32, u32, usize)> = vars
        .param_offsets
        .iter()
        .enumerate()
        .flat_map(|(p, offs)| offs.iter().map(move |&o| (p, o)))
        .map(|(p, o)| {
            let leaf = file.leaves[columns[o] as usize];
            (leaf.start, leaf.end, p + 1)
        })
        .collect();
    cuts.sort_unstable();
    let mut out = String::new();
    let mut at = occ.start;
    for (start, end, p) in cuts {
        out.push_str(file.text(at, start));
        out.push_str(&format!("${p}"));
        at = end;
    }
    out.push_str(file.text(at, occ.end));
    out
}
