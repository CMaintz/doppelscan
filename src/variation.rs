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
use crate::index::{FileIndex, Leaf, LeafClass, Role};

#[derive(Clone, Debug, Serialize)]
pub struct Variation {
    /// One value per occurrence, in occurrence order.
    pub values: Vec<String>,
    /// How many leaf positions carry this substitution.
    pub uses: usize,
    pub kind: Kind,
}

/// What a differing name means for the extracted helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A rename the helper absorbs by picking one name.
    Local,
    /// A rename other code depends on: callers switch to the kept name.
    Declared,
    /// A parameter that becomes a generic.
    Type,
    /// A parameter that is called: a callback.
    Function,
    /// A parameter naming an object member: a key or an accessor.
    Member,
    Literal,
    Value,
    /// A name in a file read without a grammar: a parameter or a rename,
    /// which the tokens cannot tell apart.
    Name,
}

impl Kind {
    pub fn is_rename(self) -> bool {
        matches!(self, Kind::Local | Kind::Declared)
    }
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
    roles: Vec<Role>,
    literal: bool,
}

struct Binding<'a> {
    offset: usize,
    texts: &'a [&'a str],
    role: Role,
    scope: (u32, u32),
}

type Differing<'f> = Vec<(usize, Vec<&'f str>)>;

pub fn analyze(group: &RawGroup, files: &[impl AsRef<FileIndex>]) -> Variations {
    let columns: Vec<Vec<u32>> = group
        .occs
        .iter()
        .map(|o| o.leaf_indices().collect())
        .collect();
    let first = files[group.occs[0].file as usize].as_ref();
    let leaf_at = |offset: usize| first.leaves[columns[0][offset] as usize];
    let differing = differing(group, files, &columns);
    let bindings = bindings(&differing, leaf_at);
    let mut slots: FxHashMap<(Vec<&str>, Option<usize>), Slot> = FxHashMap::default();
    for (offset, texts) in &differing {
        let leaf = leaf_at(*offset);
        let owner = owner(&bindings, texts, columns[0][*offset], leaf.role);
        let slot = slots.entry((texts.clone(), owner)).or_insert(Slot {
            first: *offset,
            offsets: Vec::new(),
            roles: Vec::new(),
            literal: false,
        });
        slot.offsets.push(*offset);
        slot.roles.push(leaf.role);
        slot.literal |= leaf.class == LeafClass::Literal;
    }
    split(slots)
}

/// Offsets whose text is not the same in every occurrence, with the texts.
fn differing<'f>(
    group: &RawGroup,
    files: &'f [impl AsRef<FileIndex>],
    columns: &[Vec<u32>],
) -> Differing<'f> {
    (0..columns[0].len())
        .filter_map(|offset| {
            let texts: Vec<&str> = group
                .occs
                .iter()
                .zip(columns)
                .map(|(o, c)| leaf_text(files, o, c[offset]))
                .collect();
            (texts.iter().any(|t| *t != texts[0])).then_some((offset, texts))
        })
        .collect()
}

fn bindings<'a>(differing: &'a Differing, leaf_at: impl Fn(usize) -> Leaf) -> Vec<Binding<'a>> {
    differing
        .iter()
        .filter(|(offset, _)| leaf_at(*offset).role.binds())
        .map(|(offset, texts)| Binding {
            offset: *offset,
            texts,
            role: leaf_at(*offset).role,
            scope: leaf_at(*offset).scope,
        })
        .collect()
}

/// The binding a position refers to: the innermost one with the same
/// spellings whose scope covers it. A member access can refer to a field
/// the fragment declares, never to a local, however it is spelled.
fn owner(bindings: &[Binding], texts: &[&str], leaf: u32, role: Role) -> Option<usize> {
    bindings
        .iter()
        .filter(|b| role != Role::Member || b.role == Role::Declared)
        .filter(|b| b.texts == texts && b.scope.0 <= leaf && leaf < b.scope.1)
        .min_by_key(|b| b.scope.1 - b.scope.0)
        .map(|b| b.offset)
}

fn leaf_text<'f>(files: &'f [impl AsRef<FileIndex>], occ: &Occ, leaf: u32) -> &'f str {
    let file = files[occ.file as usize].as_ref();
    file.leaf_text(&file.leaves[leaf as usize])
}

fn split(slots: FxHashMap<(Vec<&str>, Option<usize>), Slot>) -> Variations {
    let mut ordered: Vec<_> = slots.into_iter().collect();
    ordered.sort_by_key(|(_, s)| s.first);
    let mut out = Variations::default();
    for ((texts, _), slot) in ordered {
        let variation = Variation {
            values: texts.iter().map(|t| t.to_string()).collect(),
            uses: slot.offsets.len(),
            kind: kind_of(&slot),
        };
        if variation.kind.is_rename() {
            merge_rename(&mut out.renames, variation);
        } else {
            out.parameters.push(variation);
            out.param_offsets.push(slot.offsets);
        }
    }
    out
}

/// Sibling scopes renaming alike (`id` to `key` in two methods) are one
/// rename to the reader, as long as both are the same kind of rename.
fn merge_rename(renames: &mut Vec<Variation>, variation: Variation) {
    let same = renames
        .iter_mut()
        .find(|r| r.values == variation.values && r.kind == variation.kind);
    match same {
        Some(same) => same.uses += variation.uses,
        None => renames.push(variation),
    }
}

/// A name used both as a type and as a value is most constrained as a
/// type, so the strongest role wins.
fn kind_of(slot: &Slot) -> Kind {
    let any = |r: Role| slot.roles.contains(&r);
    if !slot.literal && slot.roles.iter().any(|r| r.binds()) {
        return if any(Role::Declared) {
            Kind::Declared
        } else {
            Kind::Local
        };
    }
    if slot.literal {
        Kind::Literal
    } else if any(Role::Unresolved) {
        Kind::Name
    } else if any(Role::Type) {
        Kind::Type
    } else if any(Role::Callee) {
        Kind::Function
    } else if any(Role::Member) {
        Kind::Member
    } else {
        Kind::Value
    }
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
