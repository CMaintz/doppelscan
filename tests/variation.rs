use std::time::Instant;

use doppelscan::engine::{ScanOptions, Source, analyze, index_sources};
use doppelscan::lang::lang_index_for_path;
use doppelscan::report::Group;
use doppelscan::variation::{Kind, Variation};

fn scan(files: &[(String, String)]) -> Vec<Group> {
    let sources = files
        .iter()
        .map(|(path, text)| Source {
            path: path.clone(),
            lang: lang_index_for_path(path.as_ref()).unwrap(),
            text: text.clone(),
        })
        .collect();
    let opts = ScanOptions {
        min_tokens: 20,
        ..ScanOptions::default()
    };
    let analysis = analyze(index_sources(sources, false), &opts, Instant::now());
    (1..=analysis.groups.len())
        .map(|id| analysis.group(id, false))
        .collect()
}

/// `@name` placeholders filled with the first or second spelling.
fn fill(template: &str, names: &[(&str, &str, &str)], second: bool) -> String {
    names
        .iter()
        .fold(template.to_string(), |text, (slot, a, b)| {
            text.replace(&format!("@{slot}"), if second { b } else { a })
        })
}

fn pair(ext: &str, template: &str, names: &[(&str, &str, &str)]) -> Vec<Group> {
    scan(&[
        (format!("a.{ext}"), fill(template, names, false)),
        (format!("b.{ext}"), fill(template, names, true)),
    ])
}

fn has(vars: &[Variation], value: &str) -> bool {
    vars.iter().any(|v| v.values.iter().any(|x| x == value))
}

fn role_of(vars: &[Variation], value: &str) -> Option<Kind> {
    vars.iter()
        .find(|v| v.values.iter().any(|x| x == value))
        .map(|v| v.kind)
}

const NAMES: &[(&str, &str, &str)] = &[
    ("total", "total", "sum"),
    ("item", "item", "el"),
    ("x", "x", "q"),
    ("y", "y", "z"),
    ("err", "err", "e1"),
    ("first", "first", "head"),
    ("rest", "rest", "tail"),
    ("LIMIT", "LIMIT", "MAX"),
    ("log", "log", "warn"),
];

/// One function per language binding a local, a for-each variable, lambda
/// parameters, a caught error and (where the language has it) a
/// destructured name, next to two values from outside.
const BINDINGS: &[(&str, &str, &[&str])] = &[
    (
        "ts",
        "function f(rows: Row[]): number {
  let @total = 0;
  for (const @item of rows) { @total += @item.size; }
  const big = rows.filter((@x) => @x.size > @LIMIT).map(@y => @y.id);
  const { id: @first, ...@rest } = rows[0];
  try { send({ big, @first }, @rest); } catch (@err) { @log(@err); }
  return @total;
}",
        &["total", "item", "x", "y", "first", "rest", "err"],
    ),
    (
        "java",
        "class A { int f(List<Row> rows) {
  int @total = 0;
  for (Row @item : rows) { @total += @item.size; }
  var big = rows.stream().filter(@x -> @x.size > @LIMIT).map((Row @y) -> @y.id).toList();
  try { send(big); } catch (Exception @err) { @log(@err); }
  return @total;
} }",
        &["total", "item", "x", "y", "err"],
    ),
    (
        "cs",
        "class A { int F(List<Row> rows) {
  int @total = 0;
  foreach (var @item in rows) { @total += @item.Size; }
  var big = rows.Where(@x => @x.Size > @LIMIT).Select((@y) => @y.Id).ToList();
  try { Send(big); } catch (Exception @err) { @log(@err); }
  return @total;
} }",
        &["total", "item", "x", "y", "err"],
    ),
    (
        "kt",
        "fun f(rows: List<Row>): Int {
  var @total = 0
  for (@item in rows) { @total += @item.size }
  val big = rows.filter { @x -> @x.size > @LIMIT }.map { @y -> @y.id }
  try { send(big) } catch (@err: Exception) { @log(@err) }
  return @total
}",
        &["total", "item", "x", "y", "err"],
    ),
    (
        "swift",
        "func f(rows: [Row]) -> Int {
  var @total = 0
  for @item in rows { @total += @item.size }
  let big = rows.filter { @x in @x.size > @LIMIT }.map { @y in @y.id }
  do { try send(big) } catch let @err { @log(@err) }
  return @total
}",
        &["total", "item", "x", "y", "err"],
    ),
    (
        "py",
        "def f(rows):
    @total = 0
    for @item in rows:
        @total += @item.size
    big = [@x.id for @x in rows if @x.size > @LIMIT]
    keyed = sorted(big, key=lambda @y: @y.rank)
    try:
        send(keyed)
    except Exception as @err:
        @log(@err)
    return @total
",
        &["total", "item", "x", "y", "err"],
    ),
    (
        "php",
        "<?php
function f($rows) {
  $@total = 0;
  foreach ($rows as $@item) { $@total += $@item->size; }
  $big = array_filter($rows, fn($@x) => $@x->size > @LIMIT);
  try { send($big); } catch (Exception $@err) { @log($@err); }
  return $@total;
}",
        &["total", "item", "x", "err"],
    ),
    (
        "go",
        "package p
func f(rows []Row) int {
	@total := 0
	for _, @item := range rows { @total += @item.Size }
	big := filter(rows, func(@x Row) bool { return @x.Size > @LIMIT })
	if @err := send(big); @err != nil { @log(@err) }
	return @total
}",
        &["total", "item", "x", "err"],
    ),
    (
        "rs",
        "fn f(rows: &[Row]) -> usize {
    let mut @total = 0;
    for @item in rows { @total += @item.size; }
    let big: Vec<_> = rows.iter().filter(|@x| @x.size > @LIMIT).map(|@y| @y.id).collect();
    if let Err(@err) = send(&big) { @log(@err); }
    @total
}",
        &["total", "item", "x", "y", "err"],
    ),
];

// Catches a binding form a grammar spells differently (a lambda parameter,
// a caught error, a for-each variable, a destructured name) being taken
// for a value from outside, which would make it a helper argument.
#[test]
fn bindings_are_renames_in_every_language() {
    for (ext, template, locals) in BINDINGS {
        let groups = pair(ext, template, NAMES);
        assert_eq!(groups.len(), 1, "{ext}: one whole-function clone");
        let g = &groups[0];
        for local in *locals {
            assert!(has(&g.renames, local), "{ext}: {local} should be a rename");
            assert!(
                !has(&g.parameters, local),
                "{ext}: {local} is not a parameter"
            );
        }
        assert!(
            has(&g.parameters, "LIMIT"),
            "{ext}: LIMIT comes from outside"
        );
    }
}

const ROLES: &str = r#"
function @fn(orders: Map<string, @Type>): number {
    let total = 0;
    for (const order of orders.values()) {
        if (order.status === @status && order.@field > @LIMIT) {
            total += order.@field;
            @log(order.id);
        }
    }
    return total;
}
"#;

const ROLE_NAMES: &[(&str, &str, &str)] = &[
    ("fn", "sumOrders", "sumRefunds"),
    ("Type", "Order", "Refund"),
    ("status", "\"paid\"", "\"approved\""),
    ("field", "amount", "total"),
    ("LIMIT", "LIMIT", "MAX"),
    ("log", "audit", "trace"),
];

// Catches roles mapped to the wrong kind of argument: a type needs a
// generic, a called name a function argument, a property a key or an
// accessor, and a value a plain argument.
#[test]
fn parameters_say_what_kind_of_argument_they_are() {
    let groups = pair("ts", ROLES, ROLE_NAMES);
    assert_eq!(groups.len(), 1);
    let params = &groups[0].parameters;
    assert_eq!(role_of(params, "Order"), Some(Kind::Type));
    assert_eq!(role_of(params, "\"paid\""), Some(Kind::Literal));
    assert_eq!(role_of(params, "amount"), Some(Kind::Member));
    assert_eq!(role_of(params, "LIMIT"), Some(Kind::Value));
    assert_eq!(role_of(params, "audit"), Some(Kind::Function));
}

const MEMBERS: &str = r#"
class @Cls {
    private @field: number = 0;

    @method(delta: number): number {
        const next = this.@field + delta;
        this.@field = next;
        return next;
    }
}
"#;

// Catches declared members being reported like locals: renaming a local is
// free, but a method or field another file calls cannot just be renamed.
#[test]
fn declared_names_are_told_apart_from_local_renames() {
    let names = [
        ("Cls", "Counter", "Gauge"),
        ("field", "count", "level"),
        ("method", "add", "shift"),
    ];
    let groups = pair("ts", MEMBERS, &names);
    assert_eq!(groups.len(), 1);
    let declares = &groups[0].declares;
    for name in ["Counter", "add", "count"] {
        assert_eq!(role_of(declares, name), Some(Kind::Declared), "{name}");
        assert!(!has(&groups[0].renames, name), "{name} is not a local");
    }
}

const KOTLIN_LOCAL: &str = r#"
fun @fn(rows: List<Row>): Int {
    val @count = rows.size
    var @acc = 0
    for (row in rows) { @acc += row.size * @count }
    return @acc + @count
}
"#;

// Catches Kotlin `val` inside a function counting as a declared property:
// Kotlin spells locals and class properties with the same node.
#[test]
fn locals_declared_like_properties_are_still_local() {
    let names = [
        ("fn", "weigh", "measure"),
        ("count", "count", "n"),
        ("acc", "acc", "sum"),
    ];
    let groups = pair("kt", KOTLIN_LOCAL, &names);
    assert_eq!(groups.len(), 1);
    assert_eq!(role_of(&groups[0].renames, "count"), Some(Kind::Local));
    assert_eq!(role_of(&groups[0].declares, "weigh"), Some(Kind::Declared));
}

const SHADOWS: &str = r#"
function @scale(@x: number): number {
    return @x * 2 + base;
}

function @shift(): number {
    return @x + offset * 3 - base;
}
"#;

// Catches positions joined by spelling alone: `x` in `shift` is not the
// parameter of `scale`, so it is a value the helper must take.
#[test]
fn a_name_outside_its_binding_scope_is_a_parameter() {
    let names = [
        ("scale", "scale", "grow"),
        ("shift", "shift", "move"),
        ("x", "x", "y"),
    ];
    let groups = pair("ts", SHADOWS, &names);
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(role_of(&groups[0].renames, "x"), Some(Kind::Local));
    assert_eq!(role_of(&groups[0].parameters, "x"), Some(Kind::Value));
}

const SETTER: &str = r#"
class Meter {
    @set(@v: number): void {
        this.@v = @v;
        this.total += @v * this.rate;
    }
}
"#;

// Catches a member spelled like a local joining the local's rename: the
// field `this.v` stays `v` whatever the parameter is called.
#[test]
fn a_member_spelled_like_a_local_is_a_parameter() {
    let names = [("set", "setLevel", "setLimit"), ("v", "level", "limit")];
    let groups = pair("ts", SETTER, &names);
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(role_of(&groups[0].renames, "level"), Some(Kind::Local));
    assert_eq!(role_of(&groups[0].parameters, "level"), Some(Kind::Member));
}

const INJECTED: &str = r#"
class @Ctl {
    private readonly @svc: @Service;
    private readonly @clock: Clock;

    constructor(@svc: @Service, @clock: Clock) {
        this.@svc = @svc;
        this.@clock = @clock;
    }
}
"#;

// Catches member accesses never joining a binding: `this.svc` is the
// field this fragment declares, so it is renamed along with it rather
// than passed in.
#[test]
fn a_member_of_a_declared_field_follows_the_field() {
    let names = [
        ("Ctl", "OrderController", "UserController"),
        ("svc", "orders", "users"),
        ("Service", "OrderService", "UserService"),
        ("clock", "clock", "time"),
    ];
    let groups = pair("ts", INJECTED, &names);
    assert_eq!(groups.len(), 1, "{groups:?}");
    let g = &groups[0];
    assert!(has(&g.renames, "orders"));
    assert!(!has(&g.parameters, "orders"), "{:?}", g.parameters);
    assert_eq!(role_of(&g.parameters, "OrderService"), Some(Kind::Type));
}

/// Asserts one name pair is a local rename and not a parameter.
fn assert_local(ext: &str, template: &str, names: &[(&str, &str, &str)], name: &str) {
    let groups = pair(ext, template, names);
    assert_eq!(groups.len(), 1, "{ext}: {groups:?}");
    let g = &groups[0];
    assert_eq!(role_of(&g.renames, name), Some(Kind::Local), "{ext}: {g:?}");
    assert!(
        !has(&g.parameters, name),
        "{ext}: {name} in {:?}",
        g.parameters
    );
}

// Catches PHP's `static $x` statement being taken for a function scope,
// which would cut the local off from its own uses.
#[test]
fn a_php_static_local_is_one_rename() {
    let code = "<?php
function @f($rows) {
    static $@c = 0;
    foreach ($rows as $r) { $@c += $r->size * 2 + 1; }
    echo $@c;
    return $@c + 1;
}";
    let names = [("f", "count", "tally"), ("c", "count", "n")];
    assert_local("php", code, &names, "count");
}

// Catches script code (no function around it) reporting its variables as
// declarations other code depends on, or leaving them without a scope, in
// which case `report.total` would join the local `total`.
#[test]
fn script_variables_are_local() {
    let code = "rows = load_rows(source)
@total = 0
for @item in rows:
    @total += @item.size * 2 + 1
print(@total, report.@total, len(rows))
";
    let names = [("total", "total", "acc"), ("item", "item", "el")];
    assert_local("py", code, &names, "item");
    let groups = pair("py", code, &names);
    assert_eq!(role_of(&groups[0].renames, "total"), Some(Kind::Local));
    assert_eq!(role_of(&groups[0].parameters, "total"), Some(Kind::Member));
}

// Catches Swift initialisers not counting as functions, which made their
// parameters file-wide declarations that `self.x` members then joined.
#[test]
fn swift_init_parameters_are_local() {
    let code = "class Meter {
    init(@l: Int, total: Int) {
        let scaled = @l * 2 + total
        self.@l = scaled
        self.sum = scaled + @l
    }
}";
    let names = [("l", "level", "limit")];
    let groups = pair("swift", code, &names);
    assert_eq!(role_of(&groups[0].renames, "level"), Some(Kind::Local));
    assert_eq!(role_of(&groups[0].parameters, "level"), Some(Kind::Member));
}

// Catches a value cast to a type (`x as u64`) being labelled a type.
#[test]
fn a_cast_operand_is_a_value() {
    let code = "fn f(rows: &[u32]) -> u64 {
    let mut total = 0;
    for r in rows { total += (*r + @L) as u64; }
    total + @L as u64
}";
    let groups = pair("rs", code, &[("L", "LIMIT", "MAX")]);
    assert_eq!(role_of(&groups[0].parameters, "LIMIT"), Some(Kind::Value));
}

// Catches Kotlin member accesses (a field-less `navigation_expression`)
// joining a same-spelled local instead of staying a member.
#[test]
fn a_kotlin_member_spelled_like_a_local_is_a_parameter() {
    let code = "class Meter {
    fun @set(@v: Int) {
        this.@v = @v
        total += @v * rate
    }
}";
    let names = [("set", "setLevel", "setLimit"), ("v", "level", "limit")];
    let groups = pair("kt", code, &names);
    assert_eq!(role_of(&groups[0].renames, "level"), Some(Kind::Local));
    assert_eq!(role_of(&groups[0].parameters, "level"), Some(Kind::Member));
}

// Catches constructor parameters promoted to fields (`private readonly`,
// Kotlin `val`) being scoped to the constructor, so their uses elsewhere
// in the class became parameters.
#[test]
fn promoted_constructor_parameters_are_declared_fields() {
    let ts = "class @S {
    constructor(private readonly @dep: Service) {}
    find(id: string): Item {
        return this.@dep.find(id) ?? this.@dep.fallback(id, 1);
    }
}";
    let kt = "class @S(val @dep: Int) {
    fun scaled(f: Int): Int {
        return @dep * f + @dep / 2 - 1
    }
}";
    let names = [("S", "Orders", "Users"), ("dep", "orders", "users")];
    for (ext, code) in [("ts", ts), ("kt", kt)] {
        let groups = pair(ext, code, &names);
        assert_eq!(groups.len(), 1, "{ext}: {groups:?}");
        let g = &groups[0];
        assert_eq!(
            role_of(&g.declares, "orders"),
            Some(Kind::Declared),
            "{ext}: {g:?}"
        );
        assert!(!has(&g.parameters, "orders"), "{ext}: {:?}", g.parameters);
    }
}

const SAME_NAMES: &str = r#"
class Meter {
    reset(@level: number): void {
        this.base = @level * 2;
    }
    private @level: number = 0;
    bump(): number {
        return this.@level + this.base * 3;
    }
}
"#;

// Catches renames merged by spelling alone: the local `level` and the
// field `level` are different names to update.
#[test]
fn a_local_and_a_field_with_one_spelling_stay_apart() {
    let groups = pair("ts", SAME_NAMES, &[("level", "level", "limit")]);
    assert_eq!(groups.len(), 1);
    assert!(has(&groups[0].renames, "level"));
    assert!(has(&groups[0].declares, "level"));
}

const TWIN_METHODS: &str = r#"
class Store {
    get(@id: string): Item { return this.items.get(@id) ?? this.load(@id, 1); }
    has(@id: string): boolean { return this.items.has(@id) || this.known(@id, 2); }
}
"#;

// Catches sibling scopes renaming alike being listed once per scope.
#[test]
fn sibling_scopes_renaming_alike_are_one_rename() {
    let groups = pair("ts", TWIN_METHODS, &[("id", "id", "key")]);
    assert_eq!(groups.len(), 1);
    let ids = groups[0].renames.iter().filter(|r| r.values[0] == "id");
    assert_eq!(ids.count(), 1, "{:?}", groups[0].renames);
}

// Catches the text rendering mislabelling what the JSON reports: declared
// names under `declares:`, and parameter kinds on their numbers.
#[test]
fn rendering_labels_declared_names_and_parameter_kinds() {
    let groups = pair("ts", ROLES, ROLE_NAMES);
    let text = doppelscan::report::render_group(&groups[0], doppelscan::report::Detail::Brief);
    assert!(text.contains("declares: sumOrders | sumRefunds"), "{text}");
    assert!(text.contains(":type=Order | Refund"), "{text}");
    assert!(text.contains(":function=audit | trace"), "{text}");
    assert!(
        !text.contains(":literal") && !text.contains(":value"),
        "{text}"
    );
}

// Catches Go function literals not counting as functions: their
// parameters would then reach past the literal to a package-level `x`.
#[test]
fn go_function_literal_parameters_are_local() {
    let code = "package p

var handler = func(@x int, scale int) int {
	return @x*scale + @x/3 - offset
}

var fallback = @x*2 + offset
";
    let groups = pair("go", code, &[("x", "x", "n")]);
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(role_of(&groups[0].renames, "x"), Some(Kind::Local));
    assert_eq!(
        role_of(&groups[0].parameters, "x"),
        Some(Kind::Value),
        "the x outside the literal"
    );
}
