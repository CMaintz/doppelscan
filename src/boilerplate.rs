//! Code every class writes for itself, which matches across unrelated
//! classes once names are normalised but holds nothing to extract: runs of
//! trivial getters and setters, and `equals`/`hashCode` bodies.

use std::sync::Arc;

use tree_sitter::Node;

use crate::detect::RawGroup;
use crate::index::{FileIndex, Frag};
use crate::lang::LANGS;
use crate::near::RawNear;

/// A method whose whole body is `return field;` or `this.field = value;`,
/// read by kind name so it holds in every grammar that spells these alike.
pub fn is_trivial_accessor(node: Node) -> bool {
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let mut statements = code_children(body);
    match (statements.next(), statements.next()) {
        (Some(only), None) => is_getter_body(only) || is_setter_body(only),
        _ => false,
    }
}

fn code_children(node: Node) -> impl Iterator<Item = Node> {
    let children: Vec<Node> = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i as u32))
        .filter(|c| !c.kind().contains("comment"))
        .collect();
    children.into_iter()
}

fn is_getter_body(statement: Node) -> bool {
    let mut values = code_children(statement);
    statement.kind().starts_with("return")
        && matches!((values.next(), values.next()), (Some(v), None) if is_reference(v))
}

/// `this.x = v;`, but not `this.x += v;`: that computes something.
fn is_setter_body(statement: Node) -> bool {
    let Some(assignment) = only_child(statement) else {
        return false;
    };
    let plain = assignment
        .child_by_field_name("operator")
        .is_none_or(|op| op.kind() == "=");
    let side = |field| {
        assignment
            .child_by_field_name(field)
            .is_some_and(is_reference)
    };
    statement.kind() == "expression_statement"
        && assignment.kind().starts_with("assignment")
        && plain
        && side("left")
        && side("right")
}

fn only_child(node: Node) -> Option<Node> {
    let mut children = code_children(node);
    match (children.next(), children.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// A bare name or a chain of member reads (`x`, `this.x`, `$this->x`, Go's
/// `return s.x` list of one): nothing called, nothing computed.
fn is_reference(node: Node) -> bool {
    let kind = node.kind();
    let name = kind.contains("identifier") || matches!(kind, "this" | "self" | "name");
    if node.named_child_count() == 0 {
        return name;
    }
    let access = ["field_access", "member", "selector", "variable_name"]
        .iter()
        .any(|w| kind.contains(w))
        || kind == "expression_list";
    access && code_children(node).all(is_reference)
}

/// Every copy of the group is nothing but trivial accessors. One element of
/// real code among them makes the run worth reporting.
pub fn accessors_only(group: &RawGroup, files: &[Arc<FileIndex>]) -> bool {
    group.occs.iter().all(|occ| {
        let file = &files[occ.file as usize];
        occ.frags().iter().all(|f| file.is_accessor(f))
    })
}

/// Every member is a method its class must write for itself, such as
/// `equals`: copies of the idiom across value classes, not a helper to share.
pub fn identity_methods_only(group: &RawNear, files: &[Arc<FileIndex>]) -> bool {
    group.members.iter().all(|m| {
        let file = &files[m.file as usize];
        is_identity_method(file, &m.frag)
    })
}

fn is_identity_method(file: &FileIndex, unit: &Frag) -> bool {
    let names = LANGS[file.lang].identity_methods;
    let own = file.scope_spans.get(unit.scope as usize) == Some(&(unit.start, unit.end));
    let name = file
        .scope_name(unit.scope)
        .and_then(|s| s.rsplit('.').next());
    own && name.is_some_and(|n| names.contains(&n))
}
