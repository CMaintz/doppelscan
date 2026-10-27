//! Language registry and node-kind classification.
//!
//! Classification is by kind *name* so one table of heuristics covers every
//! grammar; grammars name their nodes consistently enough ("identifier",
//! "*_literal", "*_body") that per-language tables are only needed for the
//! few semantic quirks recorded in [`Lang`].

use std::path::Path;
use std::sync::LazyLock;

use tree_sitter::Language;

/// How a node kind takes part in hashing and candidate selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindClass {
    /// Skipped entirely: no tokens, no hash contribution.
    Comment,
    /// Imports, package and using clauses: hashed by text, never a candidate,
    /// and they break statement runs so import blocks are not reported.
    Noise,
    Identifier,
    StringLit,
    NumberLit,
    /// A container whose children form a sequence: statements, members,
    /// arguments or literal elements.
    Block,
    /// A function/class/etc. whose name labels the occurrences inside it.
    Definition,
    Plain,
}

pub struct Lang {
    pub name: &'static str,
    pub language: Language,
    pub extensions: &'static [&'static str],
    /// Plain assignment introduces a local (Python, PHP), so an assigned
    /// name that differs between clones is a rename, not a parameter.
    pub assign_declares: bool,
    classes: Vec<KindClass>,
}

impl Lang {
    fn new(
        name: &'static str,
        language: Language,
        extensions: &'static [&'static str],
        assign_declares: bool,
    ) -> Self {
        let classes = (0..language.node_kind_count() as u16)
            .map(|id| classify_kind(&language, id))
            .collect();
        Self {
            name,
            language,
            extensions,
            assign_declares,
            classes,
        }
    }

    pub fn class_of(&self, kind_id: u16) -> KindClass {
        self.classes
            .get(kind_id as usize)
            .copied()
            .unwrap_or(KindClass::Plain)
    }

    pub fn kind_name(&self, kind_id: u16) -> &str {
        self.language.node_kind_for_id(kind_id).unwrap_or("")
    }

    pub fn field_name(&self, field_id: Option<std::num::NonZeroU16>) -> Option<&str> {
        self.language.field_name_for_id(field_id?.get())
    }
}

pub static LANGS: LazyLock<Vec<Lang>> = LazyLock::new(|| {
    vec![
        Lang::new(
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            &["ts", "mts", "cts"],
            false,
        ),
        Lang::new(
            "tsx",
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            &["tsx"],
            false,
        ),
        Lang::new(
            "javascript",
            tree_sitter_javascript::LANGUAGE.into(),
            &["js", "jsx", "mjs", "cjs"],
            false,
        ),
        Lang::new("java", tree_sitter_java::LANGUAGE.into(), &["java"], false),
        Lang::new(
            "csharp",
            tree_sitter_c_sharp::LANGUAGE.into(),
            &["cs"],
            false,
        ),
        Lang::new(
            "kotlin",
            tree_sitter_kotlin_ng::LANGUAGE.into(),
            &["kt", "kts"],
            false,
        ),
        Lang::new(
            "swift",
            tree_sitter_swift::LANGUAGE.into(),
            &["swift"],
            false,
        ),
        Lang::new("php", tree_sitter_php::LANGUAGE_PHP.into(), &["php"], true),
        Lang::new(
            "python",
            tree_sitter_python::LANGUAGE.into(),
            &["py", "pyi"],
            true,
        ),
        Lang::new("go", tree_sitter_go::LANGUAGE.into(), &["go"], false),
        Lang::new("rust", tree_sitter_rust::LANGUAGE.into(), &["rs"], false),
        Lang::new("css", tree_sitter_css::LANGUAGE.into(), &["css"], false),
    ]
});

pub fn lang_index_for_path(path: &Path) -> Option<usize> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    LANGS
        .iter()
        .position(|l| l.extensions.contains(&ext.as_str()))
}

pub fn lang_index_by_name(name: &str) -> Option<usize> {
    let name = name.to_ascii_lowercase();
    LANGS
        .iter()
        .position(|l| l.name == name || l.extensions.contains(&name.as_str()))
}

fn classify_kind(language: &Language, id: u16) -> KindClass {
    let Some(kind) = language.node_kind_for_id(id) else {
        return KindClass::Plain;
    };
    if !language.node_kind_is_named(id) {
        return KindClass::Plain;
    }
    if kind.contains("comment") {
        KindClass::Comment
    } else if is_noise(kind) {
        KindClass::Noise
    } else if kind.contains("identifier") || kind == "name" || kind == "implicit_parameter" {
        KindClass::Identifier
    } else if is_string(kind) {
        KindClass::StringLit
    } else if is_number(kind) {
        KindClass::NumberLit
    } else if is_block(kind) {
        KindClass::Block
    } else if is_definition(kind) {
        KindClass::Definition
    } else {
        KindClass::Plain
    }
}

fn is_noise(kind: &str) -> bool {
    kind.contains("import")
        || kind.starts_with("package")
        || kind.starts_with("namespace_use")
        || kind.contains("extern_crate")
        || matches!(kind, "using_directive" | "use_declaration")
}

fn is_string(kind: &str) -> bool {
    let stringy = kind.contains("string") || (kind.contains("char") && kind.contains("literal"));
    // Interpolated strings are walked so the embedded expressions still count.
    stringy && !kind.contains("interpolat") && !kind.contains("template")
}

fn is_number(kind: &str) -> bool {
    let literal = kind.ends_with("_literal") || matches!(kind, "number" | "integer" | "float");
    let numeric = [
        "num",
        "int",
        "float",
        "decimal",
        "real",
        "hex",
        "octal",
        "bin",
        "imaginary",
    ]
    .iter()
    .any(|w| kind.contains(w));
    literal && numeric
}

fn is_block(kind: &str) -> bool {
    const EXACT: &[&str] = &[
        "program",
        "source_file",
        "module",
        "compilation_unit",
        "translation_unit",
        "stylesheet",
        "switch_section",
        "switch_case",
        "case_clause",
        "expression_case",
        "type_case",
        "default_case",
        "communication_case",
        "chunk",
    ];
    [
        "block",
        "body",
        "statements",
        "declaration_list",
        "compound_statement",
    ]
    .iter()
    .any(|w| kind.contains(w))
        || EXACT.contains(&kind)
        || is_list(kind)
}

/// Argument lists, array and object literals: a shared prefix of a long
/// argument list is as extractable as a shared run of statements. Binary
/// expressions and the like are deliberately absent, since a run of their
/// operands would ignore the operator between them.
fn is_list(kind: &str) -> bool {
    const EXACT: &[&str] = &[
        "object",
        "array",
        "list",
        "dictionary",
        "literal_value",
        "arguments",
    ];
    kind.ends_with("argument_list")
        || kind.ends_with("_arguments")
        || kind.contains("array_initializer")
        || kind.contains("array_expression")
        || kind.contains("initializer_expression")
        || kind == "initializer_list"
        || EXACT.contains(&kind)
}

fn is_definition(kind: &str) -> bool {
    const SUBJECTS: &[&str] = &[
        "function",
        "method",
        "class",
        "constructor",
        "interface",
        "struct",
        "impl",
        "enum",
        "trait",
        "object",
        "protocol",
        "record",
    ];
    let shaped = ["declaration", "definition", "_item"]
        .iter()
        .any(|s| kind.ends_with(s));
    shaped && SUBJECTS.iter().any(|s| kind.contains(s))
}

/// Whether an identifier in this position binds a new name, given its parent
/// kind and the field it sits in. Heuristic: grammars mark binding sites with
/// a `name`/`pattern` field or an unlabelled child of a declaration node.
pub fn is_binding_site(lang: &Lang, parent_kind: &str, field: Option<&str>) -> bool {
    if lang.assign_declares && parent_kind.starts_with("assignment") {
        return field == Some("left");
    }
    let declish = [
        "declarator",
        "declaration",
        "parameter",
        "pattern",
        "binding",
        "definition",
        "catch",
        "for",
        "item",
        "range_clause",
        "lambda",
        "closure",
        "arrow_function",
    ]
    .iter()
    .any(|w| parent_kind.contains(w));
    let binding_field = matches!(
        field,
        None | Some("name" | "pattern" | "left" | "parameter" | "parameters" | "item")
    );
    // `{ id: first }` binds `first`, but `x = first` only reads it.
    let pattern_value = field == Some("value") && parent_kind.contains("pattern");
    declish && (binding_field || pattern_value)
}

/// Kinds that open a scope for their parameters and locals.
pub fn is_function_like(kind: &str) -> bool {
    // Swift initialisers and Go function literals use none of the words.
    let named = matches!(
        kind,
        "init_declaration" | "deinit_declaration" | "subscript_declaration" | "func_literal"
    );
    let callable = [
        "function",
        "method",
        "lambda",
        "closure",
        "constructor",
        "arrow",
        "accessor",
    ]
    .iter()
    .any(|w| kind.contains(w));
    // `lambda_parameter(s)` sits inside the lambda, not around it, and
    // PHP's `function_static_declaration` is a statement.
    let reference = [
        "invocation",
        "call",
        "type",
        "reference",
        "modifier",
        "parameter",
        "static",
    ]
    .iter()
    .any(|w| kind.contains(w));
    named || (callable && !reference)
}

/// Kinds whose direct bindings are members other code can reach.
pub fn is_class_like(kind: &str) -> bool {
    let owner = [
        "class",
        "interface",
        "struct",
        "object",
        "enum",
        "trait",
        "protocol",
        "record",
    ]
    .iter()
    .any(|w| kind.contains(w));
    // Rust's `impl` blocks, matched exactly: `simple_parameter` contains it.
    let owner = owner || kind == "impl_item";
    let value = ["pattern", "literal", "expression", "type", "creation"]
        .iter()
        .any(|w| kind.contains(w));
    owner && !value
}

/// A name read where an object's member goes: `order.amount`, `self.x`.
pub fn is_member_position(
    kind: &str,
    parent_kind: &str,
    field: Option<&str>,
    first_child: bool,
) -> bool {
    // Kotlin's `navigation_expression` has no fields: the receiver comes
    // first and the member after it.
    if parent_kind == "navigation_expression" && field.is_none() {
        return !first_child;
    }
    // `{ name }` is shorthand for `{ name: name }`: it reads the local.
    if kind.contains("shorthand") {
        return false;
    }
    if kind.contains("property_identifier") || kind.contains("field_identifier") {
        return true;
    }
    let access = [
        "member",
        "field_access",
        "attribute",
        "selector",
        "navigation",
        "field_expression",
    ]
    .iter()
    .any(|w| parent_kind.contains(w));
    access
        && matches!(
            field,
            Some("property" | "field" | "attribute" | "name" | "suffix")
        )
}
