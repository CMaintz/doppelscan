use doppelscan::lang::{LANGS, lang_index_for_path};
fn main() {
    let p = std::env::args().nth(1).unwrap();
    let src = std::fs::read_to_string(&p).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&LANGS[lang_index_for_path(p.as_ref()).unwrap()].language)
        .unwrap();
    let t = parser.parse(&src, None).unwrap();
    let mut c = t.walk();
    let mut depth = 0;
    loop {
        let n = c.node();
        println!(
            "{}{}{} [{}]{}",
            "  ".repeat(depth),
            if n.is_named() { "" } else { "'" },
            n.kind(),
            c.field_name().unwrap_or(""),
            if n.child_count() == 0 {
                format!(" = {}", &src[n.byte_range()])
            } else {
                String::new()
            }
        );
        if c.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if c.goto_next_sibling() {
                break;
            }
            if !c.goto_parent() {
                return;
            }
            depth -= 1;
        }
    }
}
