//! Per-file structural index: one parse, one post-order walk.
//!
//! Every named node gets a hash built bottom-up from its kind and its
//! children's hashes. Identifiers and literals hash to a placeholder by
//! category (unless strict), so two fragments that differ only in names or
//! constants collide, which is what makes them parameterisable clones.

use std::num::NonZeroU16;

use tree_sitter::{Node, Parser, TreeCursor};

use crate::hash::{combine, hash_bytes, seed};
use crate::lang::{
    KindClass, LANGS, Lang, is_binding_site, is_class_like, is_function_like, is_member_position,
};

/// Nodes cheaper than this are never stored; callers clamp `min_tokens` to it.
pub const MIN_STORED_TOKENS: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafClass {
    Identifier,
    Literal,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub start: u32,
    pub end: u32,
    pub class: LeafClass,
    pub role: Role,
    /// For a binding, the leaf index range where its name refers to it: the
    /// enclosing function for a local, the whole file for a declaration.
    pub scope: (u32, u32),
}

const WHOLE_FILE: (u32, u32) = (0, u32::MAX);
/// Keywords that turn a constructor parameter into a field.
const FIELD_MODIFIERS: &[&str] = &[
    "private",
    "protected",
    "public",
    "readonly",
    "internal",
    "val",
    "var",
];
const UNCLAIMED: (u32, u32) = (u32::MAX, u32::MAX);

/// What an identifier does where it stands, which decides what an
/// extracted helper has to do with a name that differs between copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Binds a local or parameter of code inside a function.
    Local,
    /// Binds a name visible outside: a method, field, class or top-level
    /// declaration.
    Declared,
    Type,
    /// The name of a called function.
    Callee,
    /// An object member being read or written.
    Member,
    Value,
}

impl Role {
    pub fn binds(self) -> bool {
        matches!(self, Role::Local | Role::Declared)
    }
}

/// A syntax fragment: a whole node, or one element of a statement sequence.
#[derive(Clone, Copy, Debug)]
pub struct Frag {
    pub hash: u64,
    pub start: u32,
    pub end: u32,
    pub start_line: u32,
    pub end_line: u32,
    pub leaf_start: u32,
    pub leaf_end: u32,
    pub scope: u32,
}

impl Frag {
    pub fn tokens(&self) -> u32 {
        self.leaf_end - self.leaf_start
    }
}

pub const NO_SCOPE: u32 = u32::MAX;

pub struct FileIndex {
    pub path: String,
    pub lang: usize,
    pub source: String,
    pub leaves: Vec<Leaf>,
    /// Candidate whole-node fragments. Wrappers covering the same tokens
    /// (a statement around a call) are all kept; detection picks one.
    pub nodes: Vec<Frag>,
    /// Child sequences of block-like nodes, split at noise such as imports.
    pub seqs: Vec<Vec<Frag>>,
    /// Qualified names ("Class.method") that `Frag::scope` indexes into.
    pub scopes: Vec<String>,
    /// Byte range of each scope's definition, parallel to `scopes`.
    pub scope_spans: Vec<(u32, u32)>,
}

impl FileIndex {
    pub fn text(&self, start: u32, end: u32) -> &str {
        &self.source[start as usize..end as usize]
    }

    pub fn leaf_text(&self, leaf: &Leaf) -> &str {
        self.text(leaf.start, leaf.end)
    }

    pub fn scope_name(&self, scope: u32) -> Option<&str> {
        self.scopes.get(scope as usize).map(String::as_str)
    }

    /// The definition a fragment `[start, end)` consists of, when it wraps
    /// exactly one (an exported function, an annotated method); otherwise
    /// `enclosing`. Names a clone by what it is rather than where it sits.
    pub fn own_scope(&self, start: u32, end: u32, enclosing: u32) -> u32 {
        let inside: Vec<usize> = (0..self.scope_spans.len())
            .filter(|&i| {
                let (s, e) = self.scope_spans[i];
                start <= s && e <= end
            })
            .collect();
        let outermost: Vec<usize> = inside
            .iter()
            .copied()
            .filter(|&i| {
                let (s, e) = self.scope_spans[i];
                !inside.iter().any(|&j| {
                    let (os, oe) = self.scope_spans[j];
                    j != i && os <= s && e <= oe
                })
            })
            .collect();
        match outermost[..] {
            [only] => only as u32,
            _ => enclosing,
        }
    }
}

pub fn index_source(
    parser: &mut Parser,
    path: String,
    lang: usize,
    source: String,
    strict: bool,
) -> Option<FileIndex> {
    parser.set_language(&LANGS[lang].language).ok()?;
    let tree = parser.parse(&source, None)?;
    let mut builder = Builder::new(&LANGS[lang], &source, strict);
    builder.walk(&mut tree.walk());
    builder.claim_top_level();
    Some(FileIndex {
        path,
        lang,
        leaves: builder.leaves,
        nodes: builder.nodes,
        seqs: builder.seqs,
        scopes: builder.scopes,
        scope_spans: builder.scope_spans,
        source,
    })
}

struct Frame {
    kind: u16,
    field: Option<NonZeroU16>,
    hash: u64,
    leaf_start: u32,
    block: Option<Vec<Frag>>,
    scope_pushed: bool,
}

/// A finished child handed up to its parent frame.
struct Done {
    frag: Frag,
    class: KindClass,
    named: bool,
}

struct Builder<'a> {
    lang: &'a Lang,
    source: &'a str,
    strict: bool,
    lang_seed: u64,
    leaves: Vec<Leaf>,
    /// Locals whose function has not closed yet, in leaf order.
    pending_locals: Vec<u32>,
    nodes: Vec<Frag>,
    seqs: Vec<Vec<Frag>>,
    scopes: Vec<String>,
    scope_spans: Vec<(u32, u32)>,
    scope_stack: Vec<u32>,
    frames: Vec<Frame>,
}

impl<'a> Builder<'a> {
    fn new(lang: &'a Lang, source: &'a str, strict: bool) -> Self {
        Self {
            lang,
            source,
            strict,
            lang_seed: hash_bytes(lang.name.as_bytes()),
            leaves: Vec::new(),
            pending_locals: Vec::new(),
            nodes: Vec::new(),
            seqs: Vec::new(),
            scopes: Vec::new(),
            scope_spans: Vec::new(),
            scope_stack: Vec::new(),
            frames: Vec::new(),
        }
    }

    /// Iterative post-order walk; recursion would overflow on deep trees
    /// such as long builder chains or generated code.
    fn walk(&mut self, cursor: &mut TreeCursor) {
        loop {
            let node = cursor.node();
            let class = self.lang.class_of(node.kind_id());
            let field = cursor.field_id();
            if self.descends(node, class) && cursor.goto_first_child() {
                self.open(node, class, field);
                continue;
            }
            let done = self.leaf(node, class, field);
            self.deliver(done);
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return;
                }
                let done = self.close(cursor.node());
                self.deliver(done);
            }
        }
    }

    fn descends(&self, node: Node, class: KindClass) -> bool {
        node.child_count() > 0
            && !matches!(
                class,
                KindClass::Comment | KindClass::Noise | KindClass::StringLit | KindClass::NumberLit
            )
    }

    fn open(&mut self, node: Node, class: KindClass, field: Option<NonZeroU16>) {
        let scope_pushed = class == KindClass::Definition && self.push_scope(node);
        self.frames.push(Frame {
            kind: node.kind_id(),
            field,
            hash: seed(self.lang_seed, node.kind_id() as u64),
            leaf_start: self.leaves.len() as u32,
            block: (class == KindClass::Block).then(Vec::new),
            scope_pushed,
        });
    }

    fn close(&mut self, node: Node) -> Option<Done> {
        let frame = self.frames.pop().expect("frame per opened node");
        let class = self.lang.class_of(frame.kind);
        if let Some(elems) = frame.block {
            self.flush_run(elems);
        }
        let frag = self.frag(node, frame.hash, frame.leaf_start);
        if is_function_like(self.lang.kind_name(frame.kind)) {
            self.claim_locals(frame.leaf_start);
        }
        if frame.scope_pushed {
            self.scope_stack.pop();
        }
        Some(Done {
            frag,
            class,
            named: node.is_named(),
        })
    }

    fn leaf(&mut self, node: Node, class: KindClass, field: Option<NonZeroU16>) -> Option<Done> {
        if class == KindClass::Comment {
            return None;
        }
        let leaf_start = self.leaves.len() as u32;
        let hash = match class {
            KindClass::Noise => hash_bytes(&self.source.as_bytes()[node.byte_range()]),
            _ => self.push_leaf(node, class, field),
        };
        let frag = self.frag(node, hash, leaf_start);
        Some(Done {
            frag,
            class,
            named: node.is_named(),
        })
    }

    fn push_leaf(&mut self, node: Node, class: KindClass, field: Option<NonZeroU16>) -> u64 {
        let text = &self.source.as_bytes()[node.byte_range()];
        let (leaf_class, hash) = match class {
            KindClass::Identifier => (LeafClass::Identifier, self.placeholder(1, text)),
            KindClass::StringLit => (LeafClass::Literal, self.placeholder(2, text)),
            KindClass::NumberLit => (LeafClass::Literal, self.placeholder(3, text)),
            _ => (
                LeafClass::Other,
                combine(node.kind_id() as u64, hash_bytes(text)),
            ),
        };
        let role = match leaf_class {
            LeafClass::Identifier => self.role(node, field),
            _ => Role::Value,
        };
        let scope = self.scope_for(role);
        let range = node.byte_range();
        self.leaves.push(Leaf {
            start: range.start as u32,
            end: range.end as u32,
            class: leaf_class,
            role,
            scope,
        });
        hash
    }

    /// A local's scope is only known when its function closes.
    fn scope_for(&mut self, role: Role) -> (u32, u32) {
        match role {
            Role::Declared => WHOLE_FILE,
            Role::Local => {
                self.pending_locals.push(self.leaves.len() as u32);
                UNCLAIMED
            }
            _ => (0, 0),
        }
    }

    /// Locals outside any function (script code) live for the whole file.
    fn claim_top_level(&mut self) {
        for i in self.pending_locals.drain(..) {
            self.leaves[i as usize].scope = WHOLE_FILE;
        }
    }

    fn placeholder(&self, category: u64, text: &[u8]) -> u64 {
        if self.strict {
            combine(category, hash_bytes(text))
        } else {
            category.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        }
    }

    fn role(&self, node: Node, field: Option<NonZeroU16>) -> Role {
        if self.binds(field) {
            return self.binding_role(field);
        }
        let Some(parent) = self.frames.last() else {
            return Role::Value;
        };
        let (kind, parent_kind) = (node.kind(), self.lang.kind_name(parent.kind));
        let field = self.lang.field_name(field);
        let first_child = parent.leaf_start == self.leaves.len() as u32;
        // `x as u64` and `v.(Foo)` sit under a type-named expression but
        // the operand is a value.
        let type_position = parent_kind.contains("type") && !parent_kind.ends_with("expression");
        if kind.contains("type") || type_position {
            Role::Type
        } else if is_member_position(kind, parent_kind, field, first_child) {
            Role::Member
        } else if is_callee(parent_kind, field, first_child) {
            Role::Callee
        } else {
            Role::Value
        }
    }

    /// Inner functions close first, so the locals still pending inside
    /// this one's leaves are its own.
    fn claim_locals(&mut self, leaf_start: u32) {
        let end = self.leaves.len() as u32;
        while let Some(&i) = self.pending_locals.last()
            && i >= leaf_start
        {
            self.pending_locals.pop();
            self.leaves[i as usize].scope = (leaf_start, end);
        }
    }

    /// Visible to other code: a definition's own name, a class member, or
    /// a constructor parameter promoted to a field (`private x`, `val x`).
    /// Anything else bound inside a function, or in script code, is local.
    fn binding_role(&self, field: Option<NonZeroU16>) -> Role {
        let Some(parent) = self.declaring_frame() else {
            return Role::Local;
        };
        let parent_kind = self.lang.kind_name(parent.kind);
        let names_definition = self.lang.field_name(field) == Some("name")
            && (is_function_like(parent_kind)
                || self.lang.class_of(parent.kind) == KindClass::Definition);
        let promoted =
            parent_kind.contains("parameter") && self.has_modifier_since(parent.leaf_start);
        let in_class = self.innermost_owner().is_some_and(is_class_like);
        if names_definition || promoted || in_class {
            Role::Declared
        } else {
            Role::Local
        }
    }

    /// The nearest enclosing function or class kind.
    fn innermost_owner(&self) -> Option<&str> {
        self.frames
            .iter()
            .rev()
            .map(|f| self.lang.kind_name(f.kind))
            .find(|k| is_function_like(k) || is_class_like(k))
    }

    /// The frame that declares a name, past PHP's `variable_name` wrapper.
    fn declaring_frame(&self) -> Option<&Frame> {
        let mut frames = self.frames.iter().rev();
        let parent = frames.next()?;
        if self.lang.kind_name(parent.kind) == "variable_name" {
            return frames.next();
        }
        Some(parent)
    }

    fn has_modifier_since(&self, leaf_start: u32) -> bool {
        self.leaves[leaf_start as usize..]
            .iter()
            .any(|l| FIELD_MODIFIERS.contains(&&self.source[l.start as usize..l.end as usize]))
    }

    /// Looks through name wrappers (PHP `variable_name`, Swift `pattern`,
    /// Go `expression_list`) so the binding decision sees the real
    /// declaring parent. Swift labels every bound name `bound_identifier`
    /// whatever wraps it (a `catch let`, a `for` pattern), which needs no
    /// parent check.
    fn binds(&self, field: Option<NonZeroU16>) -> bool {
        if self.lang.field_name(field) == Some("bound_identifier") {
            return true;
        }
        let mut frames = self.frames.iter().rev();
        let Some(parent) = frames.next() else {
            return false;
        };
        let parent_kind = self.lang.kind_name(parent.kind);
        if matches!(
            parent_kind,
            "variable_name" | "pattern" | "expression_list" | "pattern_list"
        ) && let Some(grand) = frames.next()
        {
            return is_binding_site(
                self.lang,
                self.lang.kind_name(grand.kind),
                self.lang.field_name(parent.field),
            );
        }
        is_binding_site(self.lang, parent_kind, self.lang.field_name(field))
    }

    fn deliver(&mut self, done: Option<Done>) {
        let Some(done) = done else { return };
        if done.named && done.frag.tokens() >= MIN_STORED_TOKENS && is_candidate(done.class) {
            self.nodes.push(done.frag);
        }
        let Some(parent) = self.frames.last_mut() else {
            return;
        };
        parent.hash = combine(parent.hash, done.frag.hash);
        let Some(elems) = parent.block.as_mut() else {
            return;
        };
        if done.class == KindClass::Noise {
            let run = std::mem::take(elems);
            self.flush_run(run);
        } else if done.named && done.frag.tokens() > 0 {
            elems.push(done.frag);
        }
    }

    fn flush_run(&mut self, elems: Vec<Frag>) {
        if elems.len() >= 2 {
            self.seqs.push(elems);
        }
    }

    fn frag(&self, node: Node, hash: u64, leaf_start: u32) -> Frag {
        let range = node.byte_range();
        Frag {
            hash,
            start: range.start as u32,
            end: range.end as u32,
            start_line: node.start_position().row as u32 + 1,
            end_line: node.end_position().row as u32 + 1,
            leaf_start,
            leaf_end: self.leaves.len() as u32,
            scope: self.scope_stack.last().copied().unwrap_or(NO_SCOPE),
        }
    }

    fn push_scope(&mut self, node: Node) -> bool {
        let Some(name) = definition_name(node, self.source) else {
            return false;
        };
        let qualified = match self.scope_stack.last() {
            Some(&parent) => format!("{}.{}", self.scopes[parent as usize], name),
            None => name.to_string(),
        };
        self.scope_stack.push(self.scopes.len() as u32);
        self.scopes.push(qualified);
        let range = node.byte_range();
        self.scope_spans
            .push((range.start as u32, range.end as u32));
        true
    }
}

fn is_callee(parent_kind: &str, field: Option<&str>, first_child: bool) -> bool {
    let call = parent_kind.contains("call") || parent_kind.contains("invocation");
    let named = matches!(field, Some("function" | "name" | "method"));
    call && (named || (field.is_none() && first_child))
}

fn is_candidate(class: KindClass) -> bool {
    !matches!(
        class,
        KindClass::Noise | KindClass::Comment | KindClass::StringLit
    )
}

fn definition_name<'s>(node: Node, source: &'s str) -> Option<&'s str> {
    let named = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("type"))?;
    let text = named.utf8_text(source.as_bytes()).ok()?;
    Some(text.lines().next().unwrap_or(text).trim())
}
