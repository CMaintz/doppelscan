//! Indexing for languages without a bundled grammar.
//!
//! A tokenizer stands in for the parser: braces become blocks, and a
//! block's statements (split at `;`, at line ends outside brackets, and at
//! `,` between braces) become the sequences searched for repeated runs, as
//! a grammar's blocks are. Names and literals are anonymised as in the
//! syntax path, so renamed copies still match. A braced block whose header
//! names something (`int total(...) {`, `dependencies {`) is a scope, so
//! occurrences are labelled and baselines can follow them. Nothing here
//! knows what a binding is, so these files get no renames and no near-miss
//! units.

use std::borrow::Cow;

use crate::hash::{combine, hash_bytes, seed};
use crate::index::{
    FileIndex, Frag, Leaf, LeafClass, MIN_STORED_TOKENS, NO_SCOPE, Role, Seqs, line_starts,
    placeholder,
};
use crate::lang::{EMBDOC, LANGS, Lexicon};

/// Hashed by text rather than anonymised, so `if (a) return b` and
/// `foo (a) bar b` stay different. One list serves every language: a
/// name that happens to be a keyword elsewhere only matches more strictly.
const KEYWORDS: &[&str] = &[
    "abstract",
    "and",
    "as",
    "async",
    "auto",
    "await",
    "begin",
    "bool",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "def",
    "default",
    "defer",
    "delete",
    "do",
    "done",
    "double",
    "elif",
    "else",
    "elsif",
    "end",
    "enum",
    "esac",
    "except",
    "export",
    "extends",
    "false",
    "fi",
    "final",
    "finally",
    "float",
    "fn",
    "for",
    "foreach",
    "from",
    "fun",
    "func",
    "function",
    "goto",
    "if",
    "impl",
    "implements",
    "import",
    "in",
    "include",
    "instanceof",
    "int",
    "interface",
    "internal",
    "is",
    "lambda",
    "let",
    "local",
    "long",
    "loop",
    "match",
    "module",
    "namespace",
    "new",
    "nil",
    "none",
    "not",
    "null",
    "object",
    "or",
    "override",
    "package",
    "private",
    "protected",
    "pub",
    "public",
    "raise",
    "require",
    "return",
    "self",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "super",
    "switch",
    "then",
    "this",
    "throw",
    "throws",
    "trait",
    "true",
    "try",
    "typeof",
    "union",
    "unless",
    "unsigned",
    "until",
    "use",
    "using",
    "val",
    "var",
    "virtual",
    "void",
    "when",
    "where",
    "while",
    "yield",
];

/// Directives after `#` (C's `#include`) or `@` (SCSS's `@use`) that
/// import; `@include` in SCSS calls a mixin, so it is not one of them.
const DIRECTIVE_NOISE: &[(&str, &str)] = &[
    ("#", "include"),
    ("#", "import"),
    ("@", "import"),
    ("@", "use"),
    ("@", "forward"),
];

/// Stands in for a grammar's node kind in the hash of punctuation and
/// keywords; above any kind id so it cannot collide with the syntax path.
const TOKEN_KIND: u64 = u16::MAX as u64 + 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    OpenBrace,
    CloseBrace,
    Open,
    Close,
    Semi,
    Comma,
    Other,
}

struct Tok {
    start: u32,
    end: u32,
    class: LeafClass,
    number: bool,
    shape: Shape,
    /// The first token on its line.
    line_start: bool,
}

pub fn index_tokens(
    path: String,
    lang: usize,
    source: String,
    strict: bool,
    lexicon: &Lexicon,
) -> FileIndex {
    let lang_seed = hash_bytes(LANGS[lang].name.as_bytes());
    let lines = line_starts(&source);
    let mut builder = Builder::new(&source, strict, lexicon, lang_seed);
    for tok in tokenize(&source, lexicon) {
        builder.push(&tok);
    }
    builder.finish();
    let Builder {
        leaves,
        nodes,
        seqs,
        scopes,
        scope_spans,
        ..
    } = builder;
    let mut index = FileIndex {
        path,
        lang,
        source,
        leaves,
        nodes,
        seqs,
        units: Vec::new(),
        line_starts: lines,
        scopes,
        scope_spans,
        local_scopes: Vec::new(),
    };
    index.drop_ignored();
    index.shrink_to_fit();
    index
}

struct Lexer<'a> {
    source: &'a str,
    /// Added to every token: a skipped BOM is still part of the file that
    /// the reported offsets slice.
    offset: u32,
    lexicon: &'a Lexicon,
    /// Past this, a `=begin` has no `=end` to close it.
    last_embdoc_end: usize,
    at: usize,
    line_start: bool,
    toks: Vec<Tok>,
}

fn tokenize(source: &str, lexicon: &Lexicon) -> Vec<Tok> {
    // Cut off, not stepped over: its bytes would read as a word, and as
    // the non-space before a `#` on the first line.
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lexer = Lexer {
        source: text,
        offset: (source.len() - text.len()) as u32,
        lexicon,
        last_embdoc_end: last_embdoc_end(text, lexicon),
        at: 0,
        line_start: true,
        toks: Vec::new(),
    };
    while lexer.at < text.len() {
        lexer.step();
    }
    lexer.toks
}

/// Found once up front: asking at each `=begin` would rescan the file for
/// every one left unclosed.
fn last_embdoc_end(text: &str, lexicon: &Lexicon) -> usize {
    if !lexicon.block_comments.contains(&EMBDOC) {
        return 0;
    }
    text.rmatch_indices(EMBDOC.1)
        .find(|(at, close)| ends_word(text.as_bytes(), at + close.len()))
        .map_or(0, |(at, _)| at)
}

impl Lexer<'_> {
    fn step(&mut self) {
        let b = self.source.as_bytes()[self.at];
        if b == b'\n' {
            self.line_start = true;
            self.at += 1;
        } else if b.is_ascii_whitespace() {
            self.at += 1;
        } else if let Some(end) = self.comment_end() {
            self.line_start |= self.source[self.at..end].contains('\n');
            self.at = end;
        } else {
            self.token();
        }
    }

    fn token(&mut self) {
        let (end, class, shape) = token_at(self.source, self.at, self.lexicon);
        let first = self.source.as_bytes()[self.at];
        self.toks.push(Tok {
            start: self.at as u32 + self.offset,
            end: end as u32 + self.offset,
            class,
            number: first.is_ascii_digit(),
            shape,
            line_start: self.line_start,
        });
        self.line_start = false;
        self.at = end;
    }

    /// Where a comment starting here ends, if one starts here.
    fn comment_end(&self) -> Option<usize> {
        let (source, i, lexicon) = (self.source, self.at, self.lexicon);
        let rest = &source[i..];
        let block = lexicon
            .block_comments
            .iter()
            .find(|(open, _)| rest.starts_with(open) && self.opens_comment(open));
        if let Some((open, close)) = block {
            return Some(block_comment_end(source, i, open, close, lexicon));
        }
        let marker = lexicon
            .line_comments
            .iter()
            .find(|m| rest.starts_with(**m))?;
        self.opens_comment(marker)
            .then(|| rest.find('\n').map_or(source.len(), |e| i + e))
    }

    /// `#` mid-word is shell's `$#` or a YAML `a#b` value, and `//` after a
    /// colon is a URL scheme (an unquoted SCSS `url(http://...)`). Ruby's
    /// `=begin` away from a line start is an assignment (`a =begin_at`), and
    /// one no `=end` follows is text in a heredoc: Ruby rejects it as a
    /// comment. None of them starts one.
    fn opens_comment(&self, marker: &str) -> bool {
        let (bytes, i) = (self.source.as_bytes(), self.at);
        let before = i.checked_sub(1).map(|p| bytes[p]);
        match marker {
            "#" => before.is_none_or(|b| b.is_ascii_whitespace()),
            "//" => before != Some(b':'),
            "--" => !self.lexicon.dash_operators || !in_operator(bytes, i),
            "=begin" => {
                before.is_none_or(|b| b == b'\n')
                    && ends_word(bytes, i + marker.len())
                    && i < self.last_embdoc_end
            }
            _ => true,
        }
    }
}

/// Whether the dashes at `i` belong to a longer symbol (`-->`, `|--`). A dash
/// before them counts, so a long run is walked once, from its start.
fn in_operator(bytes: &[u8], i: usize) -> bool {
    const SYMBOLS: &[u8] = b"!#$%&*+./<=>?@\\^|~:";
    let before = i.checked_sub(1).map(|p| bytes[p]);
    let after = bytes[i..].iter().find(|&&b| b != b'-');
    before.is_some_and(|b| b == b'-' || SYMBOLS.contains(&b))
        || after.is_some_and(|b| SYMBOLS.contains(b))
}

/// Ruby's `=begin` and `=end` count only as whole words, unlike a line that
/// merely starts with them (`=ending`).
fn ends_word(bytes: &[u8], i: usize) -> bool {
    bytes.get(i).is_none_or(u8::is_ascii_whitespace)
}

/// `=end` closes at a whole word and takes the rest of its line with it.
fn embdoc_end(source: &str, body: usize) -> usize {
    let bytes = source.as_bytes();
    let close = source[body..]
        .match_indices(EMBDOC.1)
        .map(|(at, close)| body + at + close.len())
        .find(|&after| ends_word(bytes, after));
    close.map_or(source.len(), |after| {
        source[after..]
            .find('\n')
            .map_or(source.len(), |e| after + e)
    })
}

fn block_comment_end(source: &str, i: usize, open: &str, close: &str, lexicon: &Lexicon) -> usize {
    let body = i + open.len();
    if open == EMBDOC.0 {
        return embdoc_end(source, body);
    }
    if lexicon.nested_comments {
        return nested_comment_end(source.as_bytes(), body, open, close);
    }
    source[body..]
        .find(close)
        .map_or(source.len(), |e| body + e + close.len())
}

/// One forward pass with a depth count, so nesting costs no more than the
/// comment's length. An unclosed comment runs to the end of the file.
fn nested_comment_end(bytes: &[u8], body: usize, open: &str, close: &str) -> usize {
    let (mut at, mut depth) = (body, 1);
    while at < bytes.len() {
        if bytes[at..].starts_with(close.as_bytes()) {
            at += close.len();
            depth -= 1;
            if depth == 0 {
                return at;
            }
        } else if bytes[at..].starts_with(open.as_bytes()) {
            at += open.len();
            depth += 1;
        } else {
            at += 1;
        }
    }
    bytes.len()
}

fn token_at(source: &str, i: usize, lexicon: &Lexicon) -> (usize, LeafClass, Shape) {
    let bytes = source.as_bytes();
    if let Some(shape) = punctuation(bytes[i]) {
        return (i + 1, LeafClass::Other, shape);
    }
    let (end, class) = match bytes[i] {
        b'"' | b'\'' | b'`' => (string_end(bytes, i, lexicon), LeafClass::Literal),
        b'0'..=b'9' => (word_end(bytes, i, true, lexicon.primes), LeafClass::Literal),
        b if is_word_byte(b) => {
            let end = word_end(bytes, i, false, lexicon.primes);
            (end, word_class(&source[i..end], lexicon))
        }
        _ => (char_end(source, i), LeafClass::Other),
    };
    (end, class, Shape::Other)
}

fn punctuation(b: u8) -> Option<Shape> {
    match b {
        b'{' => Some(Shape::OpenBrace),
        b'}' => Some(Shape::CloseBrace),
        b'(' | b'[' => Some(Shape::Open),
        b')' | b']' => Some(Shape::Close),
        b';' => Some(Shape::Semi),
        b',' => Some(Shape::Comma),
        _ => None,
    }
}

/// Triple-quoted strings run to their closing triple across lines. Others
/// end at the line end when unclosed, so a stray apostrophe (an unquoted
/// "don't" in YAML) costs one line, not the rest of the file; the newline
/// stays out so the next line still starts a statement.
fn string_end(bytes: &[u8], i: usize, lexicon: &Lexicon) -> usize {
    let quote = bytes[i];
    if lexicon.triple_quotes.contains(&quote) && bytes[i..].starts_with(&[quote; 3]) {
        let raw = lexicon
            .raw_triples
            .iter()
            .any(|prefix| bytes[..i].ends_with(prefix.as_bytes()));
        return triple_end(bytes, i, raw);
    }
    let mut j = i + 1;
    while j < bytes.len() && bytes[j] != quote && bytes[j] != b'\n' {
        j += if bytes[j] == b'\\' { 2 } else { 1 };
    }
    match bytes.get(j) {
        Some(&b) if b == quote => j + 1,
        _ => j.min(bytes.len()),
    }
}

fn triple_end(bytes: &[u8], i: usize, raw: bool) -> usize {
    let close = [bytes[i]; 3];
    let mut j = i + 3;
    while j < bytes.len() {
        if bytes[j..].starts_with(&close) {
            return j + 3;
        }
        j += if bytes[j] == b'\\' && !raw { 2 } else { 1 };
    }
    bytes.len()
}

fn word_end(bytes: &[u8], i: usize, number: bool, primes: bool) -> usize {
    let mut j = i + 1;
    while j < bytes.len() && in_word(bytes, j, number, primes) {
        j += 1;
    }
    j
}

/// `'` ends a word so that `N'text'` and `L'x'` open their literal, unless
/// the language has primes (`foldl'`) or it separates digits (`1'000`).
fn in_word(bytes: &[u8], j: usize, number: bool, primes: bool) -> bool {
    match bytes[j] {
        b'\'' => primes || (number && bytes.get(j + 1).is_some_and(u8::is_ascii_alphanumeric)),
        b'.' => number,
        b => is_word_byte(b),
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

fn char_end(source: &str, i: usize) -> usize {
    i + source[i..].chars().next().map_or(1, char::len_utf8)
}

fn word_class(word: &str, lexicon: &Lexicon) -> LeafClass {
    let ignore_case = lexicon.ignore_case;
    if find_word(KEYWORDS, word, ignore_case).is_some()
        || find_word(lexicon.keywords, word, ignore_case).is_some()
    {
        LeafClass::Other
    } else {
        LeafClass::Identifier
    }
}

/// The entry of `list` that `word` spells, as `list` spells it.
fn find_word(list: &[&'static str], word: &str, ignore_case: bool) -> Option<&'static str> {
    list.iter().copied().find(|entry| {
        if ignore_case {
            entry.eq_ignore_ascii_case(word)
        } else {
            *entry == word
        }
    })
}

/// A statement being read.
struct Stmt {
    hash: u64,
    start: u32,
    leaf_start: u32,
    /// A braced block closed inside it: the next line starts a new
    /// statement whatever it opens with (`}` then `.rule {` in SCSS).
    closed_block: bool,
}

/// A brace block, or the file: its finished statements and the one being
/// read, with bracket depth so line ends inside `( )` do not split it.
struct Block {
    elems: Vec<Frag>,
    /// Folds every statement, imports included, so two blocks hash alike
    /// only when all their statements do.
    hash: u64,
    stmt: Option<Stmt>,
    depth: u32,
    braced: bool,
    scope: Option<u32>,
}

struct Builder<'a> {
    source: &'a str,
    strict: bool,
    lexicon: &'a Lexicon,
    lang_seed: u64,
    leaves: Vec<Leaf>,
    nodes: Vec<Frag>,
    seqs: Seqs,
    scopes: Vec<String>,
    scope_spans: Vec<(u32, u32)>,
    blocks: Vec<Block>,
}

impl<'a> Builder<'a> {
    fn new(source: &'a str, strict: bool, lexicon: &'a Lexicon, lang_seed: u64) -> Self {
        Self {
            source,
            strict,
            lexicon,
            lang_seed,
            leaves: Vec::new(),
            nodes: Vec::new(),
            seqs: Seqs::default(),
            scopes: Vec::new(),
            scope_spans: Vec::new(),
            blocks: vec![Block::new(false, None, lang_seed)],
        }
    }

    fn push(&mut self, tok: &Tok) {
        if self.splits_before(tok) {
            self.end_stmt();
        }
        let depth0 = self.top().depth == 0;
        match tok.shape {
            Shape::OpenBrace => self.open_block(tok),
            Shape::CloseBrace if self.blocks.len() > 1 => self.close_block(Some(tok)),
            Shape::Semi if depth0 => self.push_terminator(tok),
            Shape::Comma if depth0 && self.top().braced => self.push_terminator(tok),
            Shape::Open => {
                self.push_leaf(tok);
                self.top().depth += 1;
            }
            Shape::Close => {
                self.push_leaf(tok);
                self.top().depth = self.top().depth.saturating_sub(1);
            }
            _ => self.push_leaf(tok),
        }
    }

    fn splits_before(&mut self, tok: &Tok) -> bool {
        let continues = self.continues(tok);
        let top = self.top();
        let closed_block = top.stmt.as_ref().is_some_and(|s| s.closed_block);
        tok.line_start && top.depth == 0 && (closed_block || !continues)
    }

    /// A line opening with `{` (Allman braces), `)`, `.` or an operator
    /// continues the statement above it (a header, a wrapped method chain
    /// or condition).
    fn continues(&self, tok: &Tok) -> bool {
        let text = &self.source[tok.start as usize..tok.end as usize];
        matches!(tok.shape, Shape::OpenBrace | Shape::Close)
            || (tok.class == LeafClass::Other
                && tok.shape == Shape::Other
                && text.starts_with(['.', '&', '|', '?', ':', '+', '*']))
    }

    fn top(&mut self) -> &mut Block {
        self.blocks
            .last_mut()
            .expect("the file block is never popped")
    }

    fn push_terminator(&mut self, tok: &Tok) {
        self.push_leaf(tok);
        self.end_stmt();
    }

    fn push_leaf(&mut self, tok: &Tok) {
        let hash = self.leaf_hash(tok);
        let index = self.leaves.len() as u32;
        self.leaves.push(Leaf {
            start: tok.start,
            end: tok.end,
            class: tok.class,
            role: match tok.class {
                LeafClass::Identifier => Role::Unresolved,
                _ => Role::Value,
            },
            token: (hash ^ (hash >> 32)) as u32,
        });
        self.fold_into_stmt(tok.start, index, hash);
    }

    fn fold_into_stmt(&mut self, start: u32, leaf: u32, hash: u64) {
        let lang_seed = self.lang_seed;
        let stmt = self.top().stmt.get_or_insert(Stmt {
            hash: seed(lang_seed, 0),
            start,
            leaf_start: leaf,
            closed_block: false,
        });
        stmt.hash = combine(stmt.hash, hash);
    }

    fn leaf_hash(&self, tok: &Tok) -> u64 {
        let text = &self.source.as_bytes()[tok.start as usize..tok.end as usize];
        match tok.class {
            // Only --strict reads the text, so only it pays to fold it.
            LeafClass::Identifier if self.strict => placeholder(true, 1, &self.folded(text)),
            LeafClass::Identifier => placeholder(false, 1, text),
            LeafClass::Literal if tok.number => placeholder(self.strict, 3, text),
            LeafClass::Literal => placeholder(self.strict, 2, text),
            LeafClass::Other => combine(TOKEN_KIND, hash_bytes(&self.folded(text))),
        }
    }

    fn folded<'t>(&self, text: &'t [u8]) -> Cow<'t, [u8]> {
        if self.lexicon.ignore_case && text.iter().any(u8::is_ascii_uppercase) {
            Cow::Owned(text.to_ascii_lowercase())
        } else {
            Cow::Borrowed(text)
        }
    }

    fn open_block(&mut self, tok: &Tok) {
        let scope = self.open_scope();
        self.push_leaf(tok);
        let block = Block::new(true, scope, self.lang_seed);
        self.blocks.push(block);
    }

    /// The block's hash joins the statement that opened it, so a function
    /// is one fragment from its header to its closing brace. `tok` is
    /// `None` for a block the file never closes.
    fn close_block(&mut self, tok: Option<&Tok>) {
        self.end_stmt();
        let block = self
            .blocks
            .pop()
            .expect("checked: more than the file block");
        self.flush_run(block.elems);
        if let Some(scope) = block.scope {
            let end = tok.map_or(self.source.len() as u32, |t| t.end);
            self.scope_spans[scope as usize].1 = end;
        }
        if let Some(stmt) = self.top().stmt.as_mut() {
            stmt.hash = combine(stmt.hash, block.hash);
            stmt.closed_block = true;
        }
        if let Some(tok) = tok {
            self.push_leaf(tok);
        }
    }

    fn end_stmt(&mut self) {
        let Some(stmt) = self.top().stmt.take() else {
            return;
        };
        let frag = self.frag(&stmt);
        let top = self.top();
        top.hash = combine(top.hash, frag.hash);
        if self.is_noise(&stmt) {
            let run = std::mem::take(&mut self.top().elems);
            self.flush_run(run);
            return;
        }
        if frag.tokens() >= MIN_STORED_TOKENS {
            self.nodes.push(frag);
        }
        self.top().elems.push(frag);
    }

    fn is_noise(&self, stmt: &Stmt) -> bool {
        let leaves = &self.leaves[stmt.leaf_start as usize..];
        let [first, second, ..] = leaves else {
            return false;
        };
        let (word, next) = (self.leaf_text(first), self.leaf_text(second));
        DIRECTIVE_NOISE.contains(&(word, next))
            || find_word(
                self.lexicon.import_heads,
                self.hyphenated(leaves, 0),
                self.lexicon.ignore_case,
            )
            .is_some_and(|head| head.contains('-') || self.imports(stmt, head, leaves))
    }

    /// Whether a statement opening with a head word is an import: what it
    /// brings in has to come next. `include:`, `package =`, `part->a` and
    /// `require(a > 0, "x")` use the word as a key, a name or a function, and
    /// SQL's `USE AS c` is a column: `use` never brings in a keyword.
    fn imports(&self, stmt: &Stmt, head: &str, leaves: &[Leaf]) -> bool {
        let next = &leaves[1];
        let named = next.class == LeafClass::Literal
            || is_word_byte(self.source.as_bytes()[next.start as usize]);
        if named {
            // `library L { ... }` and `package p { ... }` hold code; every
            // other head with braces lists names (`import a.{b, c}`).
            let definition = stmt.closed_block && matches!(head, "library" | "package");
            return !definition && !(head == "use" && next.class == LeafClass::Other);
        }
        self.lexicon
            .import_openers
            .contains(&(head, self.leaf_text(next)))
            || self.is_load_call(head, leaves)
    }

    /// `require("socket")` imports through a call taking only what it loads.
    /// Heads the language lists import whatever their arguments.
    fn is_load_call(&self, head: &str, leaves: &[Leaf]) -> bool {
        if self.lexicon.load_calls.contains(&head) {
            return self.is_whole_call(leaves);
        }
        let [_, open, arg, close, rest @ ..] = leaves else {
            return false;
        };
        self.leaf_text(open) == "("
            && self.leaf_text(close) == ")"
            && rest.len() <= 1
            && arg.class == LeafClass::Literal
    }

    /// Whether the parenthesis after the head closes at the statement's end,
    /// so `library(a)$b` stays an expression.
    fn is_whole_call(&self, leaves: &[Leaf]) -> bool {
        let mut depth = 0;
        for (i, leaf) in leaves.iter().enumerate().skip(1) {
            match self.leaf_text(leaf) {
                "(" => depth += 1,
                ")" => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                let rest = &leaves[i + 1..];
                return rest.is_empty() || matches!(rest, [l] if self.leaf_text(l) == ";");
            }
        }
        false
    }

    fn leaf_text(&self, leaf: &Leaf) -> &'a str {
        &self.source[leaf.start as usize..leaf.end as usize]
    }

    /// Names the block about to open after the statement read so far: the
    /// name called just before its first `(` (`int total(...)`, `foo() {`),
    /// or its first name when it has no parentheses (`class Cart {`,
    /// `dependencies {`). Control flow (`if (x) {`) names nothing.
    fn header_name(&mut self) -> Option<&'a str> {
        let leaf_start = self.top().stmt.as_ref()?.leaf_start as usize;
        let header = &self.leaves[leaf_start..];
        let at = match header.iter().position(|l| self.leaf_text(l) == "(") {
            Some(0) => return None,
            Some(p) => p - 1,
            None => first_name(header)?,
        };
        (header[at].class == LeafClass::Identifier).then(|| self.hyphenated(header, at))
    }

    /// The whole of a name around `header[at]`: shell's `my-task` and
    /// PowerShell's `Get-Total` are three tokens each.
    fn hyphenated(&self, header: &[Leaf], at: usize) -> &'a str {
        let (mut first, mut last) = (at, at);
        while first >= 2 && self.hyphen_joins(&header[first - 2..]) {
            first -= 2;
        }
        while self.hyphen_joins(&header[last..]) {
            last += 2;
        }
        &self.source[header[first].start as usize..header[last].end as usize]
    }

    /// `a-b` with no spaces, so `a - b` stays arithmetic and `a->b` an access.
    fn hyphen_joins(&self, leaves: &[Leaf]) -> bool {
        let [left, dash, right, ..] = leaves else {
            return false;
        };
        let word = |l: &Leaf| is_word_byte(self.source.as_bytes()[l.start as usize]);
        self.leaf_text(dash) == "-"
            && left.end == dash.start
            && dash.end == right.start
            && word(left)
            && word(right)
    }

    fn open_scope(&mut self) -> Option<u32> {
        let name = self.header_name()?;
        let start = self.top().stmt.as_ref()?.start;
        let qualified = match self.blocks.iter().rev().find_map(|b| b.scope) {
            Some(parent) => format!("{}.{name}", self.scopes[parent as usize]),
            None => name.to_string(),
        };
        self.scopes.push(qualified);
        self.scope_spans.push((start, self.source.len() as u32));
        Some(self.scopes.len() as u32 - 1)
    }

    fn frag(&self, stmt: &Stmt) -> Frag {
        let end = self.leaves.last().map_or(stmt.start, |l| l.end);
        Frag {
            hash: stmt.hash,
            start: stmt.start,
            end,
            leaf_start: stmt.leaf_start,
            leaf_end: self.leaves.len() as u32,
            scope: self
                .blocks
                .iter()
                .rev()
                .find_map(|b| b.scope)
                .unwrap_or(NO_SCOPE),
        }
    }

    fn flush_run(&mut self, elems: Vec<Frag>) {
        if elems.len() >= 2 {
            self.seqs.push(&elems);
        }
    }

    /// Unclosed braces (a truncated file, a brace inside an unlexed
    /// construct) are closed at the end so their content still counts in
    /// the statement that opened them.
    fn finish(&mut self) {
        while self.blocks.len() > 1 {
            self.close_block(None);
        }
        self.end_stmt();
        let block = self.blocks.pop().expect("the file block");
        self.flush_run(block.elems);
    }
}

/// Two names in a row are a kind word no keyword list covers and the name
/// it introduces: `contract Cart`, `message Order`, `task build`.
fn first_name(header: &[Leaf]) -> Option<usize> {
    let is_name = |l: &Leaf| l.class == LeafClass::Identifier;
    let at = header.iter().position(is_name)?;
    Some(at + usize::from(header.get(at + 1).is_some_and(is_name)))
}

impl Block {
    fn new(braced: bool, scope: Option<u32>, lang_seed: u64) -> Self {
        Self {
            elems: Vec::new(),
            hash: seed(lang_seed, 1),
            stmt: None,
            depth: 0,
            braced,
            scope,
        }
    }
}
