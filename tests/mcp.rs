mod common;

use std::path::{Path, PathBuf};

use common::git;
use doppelscan::engine::Engine;
use doppelscan::mcp::handle_line;
use serde_json::{Value, json};

const CLONE: &str = r#"
export function sumTotals(orders: Order[], config: Config): number {
    let total = 0;
    for (const order of orders) {
        if (order.status === "paid") {
            total += order.amount * config.rate;
        }
    }
    return Math.round(total * 100) / 100;
}
"#;

fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("doppelscan-mcp-{name}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.ts"), CLONE).unwrap();
    std::fs::write(
        dir.join("src/b.ts"),
        CLONE
            .replace("sumTotals", "sumRefunds")
            .replace("\"paid\"", "\"approved\""),
    )
    .unwrap();
    dir
}

fn call(engine: &Engine, root: &Path, msg: Value) -> Option<Value> {
    handle_line(engine, root, &msg.to_string()).map(|s| serde_json::from_str(&s).unwrap())
}

fn modern(method: &str, params: Value) -> Value {
    let mut params = params;
    params["_meta"] = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28" });
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
}

fn tool_text(reply: &Value) -> &str {
    reply["result"]["content"][0]["text"].as_str().unwrap()
}

// Catches dropping the version check: modern clients rely on -32022 plus the
// supported list to retry with a version both sides speak.
#[test]
fn unsupported_version_is_rejected_with_the_supported_list() {
    let (engine, root) = (Engine::default(), fixture("version"));
    let mut msg = modern("tools/list", json!({}));
    msg["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    let reply = call(&engine, &root, msg).unwrap();
    assert_eq!(reply["error"]["code"], -32022);
    assert!(
        reply["error"]["data"]["supported"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );
}

// Catches modern results missing `resultType`, and the legacy handshake
// answering with a version the client did not ask for.
#[test]
fn both_protocol_eras_are_served() {
    let (engine, root) = (Engine::default(), fixture("eras"));
    let listed = call(&engine, &root, modern("tools/list", json!({}))).unwrap();
    assert_eq!(listed["result"]["resultType"], "complete");
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 6);
    let init = json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
    let reply = call(&engine, &root, init).unwrap();
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert!(reply["result"].get("resultType").is_none());
}

// Catches replying to notifications, which a client reads as a response to
// a request it never sent.
#[test]
fn notifications_get_no_reply() {
    let (engine, root) = (Engine::default(), fixture("notify"));
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(call(&engine, &root, note).is_none());
}

// Catches the tool wiring: arguments not reaching the engine, or relative
// paths not resolving against the server root.
#[test]
fn find_duplicates_then_drill_into_the_group() {
    let (engine, root) = (Engine::default(), fixture("tools"));
    let reply = call(
        &engine,
        &root,
        modern(
            "tools/call",
            json!({ "name": "find_duplicates", "arguments": { "path": "src", "min_tokens": 20 } }),
        ),
    )
    .unwrap();
    let text = tool_text(&reply);
    assert!(
        text.contains("src/a.ts:2-10") && text.contains("src/b.ts:2-10"),
        "{text}"
    );
    let fingerprint = text.split('[').nth(1).unwrap().split(']').next().unwrap();
    let args = json!({ "id": fingerprint, "path": "src", "min_tokens": 20 });
    let detail = call(
        &engine,
        &root,
        modern(
            "tools/call",
            json!({ "name": "get_duplicate_group", "arguments": args }),
        ),
    )
    .unwrap();
    assert!(
        tool_text(&detail).contains("template:"),
        "{}",
        tool_text(&detail)
    );
}

// Catches location lookup ignoring the requested lines.
#[test]
fn duplicates_at_respects_the_line_range() {
    let (engine, root) = (Engine::default(), fixture("at"));
    let at = |line: u64| {
        let args = json!({ "file": "src/b.ts", "start_line": line, "min_tokens": 20 });
        let reply = call(
            &engine,
            &root,
            modern(
                "tools/call",
                json!({ "name": "duplicates_at", "arguments": args }),
            ),
        )
        .unwrap();
        tool_text(&reply).to_string()
    };
    assert!(at(5).contains("src/a.ts:2-10"));
    assert!(at(40).starts_with("No clones"));
}

// Catches the snippet not being included in the analysis it is matched
// against.
#[test]
fn find_similar_code_locates_an_existing_helper() {
    let (engine, root) = (Engine::default(), fixture("similar"));
    let snippet = "let acc = 0;\nfor (const x of xs) {\n    if (x.status === \"open\") {\n        acc += x.amount * opts.rate;\n    }\n}\n";
    let args = json!({ "code": snippet, "language": "ts" });
    let reply = call(
        &engine,
        &root,
        modern(
            "tools/call",
            json!({ "name": "find_similar_code", "arguments": args }),
        ),
    )
    .unwrap();
    let text = tool_text(&reply);
    assert!(
        text.contains("src/a.ts") && text.contains("<snippet>"),
        "{text}"
    );
}

// Catches tool failures surfacing as protocol errors, which models cannot
// self-correct from.
#[test]
fn bad_arguments_are_tool_errors_and_unknown_tools_are_protocol_errors() {
    let (engine, root) = (Engine::default(), fixture("errors"));
    let bad = call(
        &engine,
        &root,
        modern(
            "tools/call",
            json!({ "name": "find_duplicates", "arguments": { "path": "nope" } }),
        ),
    )
    .unwrap();
    assert_eq!(bad["result"]["isError"], true);
    let unknown = call(
        &engine,
        &root,
        modern("tools/call", json!({ "name": "rm_rf", "arguments": {} })),
    )
    .unwrap();
    assert_eq!(unknown["error"]["code"], -32602);
}

// Catches a bare-function snippet matching only the existing function's
// body, which hides that the whole helper already exists.
#[test]
fn a_bare_function_snippet_matches_the_whole_existing_function() {
    let (engine, root) = (Engine::default(), fixture("similar-fn"));
    let snippet = CLONE
        .replace("export ", "")
        .replace("sumTotals", "computeTotal");
    let args = json!({ "code": snippet, "language": "ts" });
    let reply = call(
        &engine,
        &root,
        modern(
            "tools/call",
            json!({ "name": "find_similar_code", "arguments": args }),
        ),
    )
    .unwrap();
    let text = tool_text(&reply);
    assert!(text.contains("src/a.ts:2-10"), "{text}");
}

fn check_new(engine: &Engine, root: &Path) -> String {
    let args =
        json!({ "name": "check_new_duplicates", "arguments": { "path": "src", "min_tokens": 20 } });
    let reply = call(engine, root, modern("tools/call", args)).unwrap();
    tool_text(&reply).to_string()
}

// Catches revision paths missing the subdirectory prefix the working-tree
// scan shows (a shrunk clone would then look new), and uncommitted copies
// going unreported.
#[test]
fn check_new_duplicates_compares_a_subdirectory_with_head() {
    let (engine, root) = (Engine::default(), fixture("check"));
    let _ = std::fs::remove_dir_all(root.join(".git"));
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "init"]);
    let shrunk = std::fs::read_to_string(root.join("src/b.ts"))
        .unwrap()
        .replace("return Math.round(total * 100) / 100;", "return total;");
    std::fs::write(root.join("src/b.ts"), shrunk).unwrap();
    let text = check_new(&engine, &root);
    assert!(
        text.starts_with("No new duplication against HEAD"),
        "{text}"
    );

    std::fs::write(root.join("src/c.ts"), CLONE.replace("sumTotals", "sumAll")).unwrap();
    let text = check_new(&engine, &root);
    assert!(text.contains("src/c.ts"), "{text}");
}

// Catches the near-duplicate tool not reaching the near-miss detector, or
// reporting copies without saying where they differ.
#[test]
fn find_near_duplicates_points_at_the_differing_lines() {
    let (engine, root) = (Engine::default(), fixture("near"));
    let edited = CLONE.replace("sumTotals", "sumWithFees").replace(
        "            total += order.amount * config.rate;\n",
        "            total += order.amount * config.rate;\n            fees += order.fee;\n",
    );
    std::fs::write(root.join("src/b.ts"), edited).unwrap();
    let args = json!({ "name": "find_near_duplicates", "arguments": { "path": "src", "min_tokens": 20, "similarity": 0.6 } });
    let reply = call(&engine, &root, modern("tools/call", args)).unwrap();
    let text = tool_text(&reply);
    assert!(
        text.contains("src/a.ts:2-10") && text.contains("src/b.ts:2-11"),
        "{text}"
    );
    assert!(text.contains("differs: 7"), "{text}");
}

// Catches `limit: 0` returning nothing with a hint to repeat the same call.
#[test]
fn a_limit_of_zero_returns_every_group() {
    let (engine, root) = (Engine::default(), fixture("limit"));
    let args = json!({ "name": "find_duplicates", "arguments": { "path": "src", "min_tokens": 20, "limit": 0 } });
    let reply = call(&engine, &root, modern("tools/call", args)).unwrap();
    let text = tool_text(&reply);
    assert!(text.contains("src/a.ts:2-10"), "{text}");
    assert!(!text.contains("More groups"), "{text}");
}
