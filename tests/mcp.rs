mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    let _ = std::fs::remove_dir_all(&dir);
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
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 7);
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

/// Feeds `input` to a real server over stdio and returns its replies.
fn serve(root: &Path, input: &[u8]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_doppelscan"))
        .args(["mcp", "--root"])
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// Catches one undecodable line ending the read loop, which drops every
// later request and takes the server down with it.
#[test]
fn invalid_utf8_is_answered_and_the_server_keeps_serving() {
    let root = fixture("utf8");
    let mut input = b"{\"id\":1,\"method\":\"\xff\"}\n".to_vec();
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
    let replies = serve(&root, &input);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["id"], 2);
    assert!(replies[1].get("result").is_some(), "{replies:?}");
}

// Catches valid JSON without an id being mistaken for a notification, which
// leaves the client waiting for a reply forever.
#[test]
fn json_that_is_not_an_object_is_an_invalid_request() {
    let (engine, root) = (Engine::default(), fixture("not-object"));
    for line in ["5", "[]", "\"x\"", "null", "true"] {
        let reply = handle_line(&engine, &root, line).expect(line);
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["error"]["code"], -32600, "{line}");
        assert_eq!(reply["id"], Value::Null, "{line}");
    }
}

fn tool(engine: &Engine, root: &Path, name: &str, arguments: Value) -> Value {
    let params = json!({ "name": name, "arguments": arguments });
    call(engine, root, modern("tools/call", params)).unwrap()
}

// Catches `offset + 1` wrapping to 0: the page then starts at group 0 and
// the id-to-index subtraction panics, which aborts a release server.
#[test]
fn an_offset_past_every_group_is_an_empty_page() {
    let (engine, root) = (Engine::default(), fixture("offset"));
    for name in ["find_duplicates", "find_near_duplicates"] {
        for offset in [u64::MAX, 1000] {
            let args =
                json!({ "path": "src", "min_tokens": 20, "similarity": 0.5, "offset": offset });
            let reply = tool(&engine, &root, name, args);
            assert_eq!(reply["result"]["isError"], false, "{reply}");
            assert!(tool_text(&reply).contains("Showing 0"), "{reply}");
        }
    }
}

// Catches numeric arguments falling back to their default on a wrong type,
// or truncating to a different number that fits: the caller gets an answer
// to a question it did not ask.
#[test]
fn numeric_arguments_that_cannot_be_used_are_tool_errors() {
    let (engine, root) = (Engine::default(), fixture("numbers"));
    let cases = [
        ("duplicates_at", "start_line", json!("2")),
        ("duplicates_at", "start_line", json!(-1)),
        ("duplicates_at", "start_line", json!(4294967297u64)),
        ("duplicates_at", "end_line", json!(4294967297u64)),
        ("duplicates_at", "end_line", json!(1.5)),
        ("find_duplicates", "limit", json!(-1.0)),
        ("find_duplicates", "limit", json!(1e30)),
        ("find_duplicates", "min_tokens", json!(4294967296.0)),
        ("find_duplicates", "min_tokens", json!(4294967296u64)),
        ("find_duplicates", "min_tokens", json!("20")),
        ("find_duplicates", "limit", json!("5")),
        ("find_duplicates", "limit", json!(-1)),
        ("find_duplicates", "offset", json!("1")),
        ("find_duplicates", "max_param_density", json!("8")),
        ("find_near_duplicates", "limit", json!(-1)),
        (
            "check_new_duplicates",
            "check_min_tokens",
            json!(4294967296u64),
        ),
    ];
    for (name, key, value) in cases {
        let mut args = json!({ "path": "src", "file": "src/a.ts" });
        args[key] = value.clone();
        let reply = tool(&engine, &root, name, args);
        assert_eq!(reply["result"]["isError"], true, "{name} {key}={value}");
        assert!(tool_text(&reply).contains(key), "{reply}");
    }
}

/// Runs `during` while reading `path` fails, leaving its mtime and size alone.
#[cfg(windows)]
fn while_unreadable(path: &Path, during: impl FnOnce()) {
    use std::os::windows::fs::OpenOptionsExt as _;
    let mut open = std::fs::OpenOptions::new();
    let _lock = open.read(true).share_mode(0).open(path).unwrap();
    during();
}

#[cfg(unix)]
fn while_unreadable(path: &Path, during: impl FnOnce()) {
    use std::os::unix::fs::PermissionsExt as _;
    let original = std::fs::metadata(path).unwrap().permissions();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    during();
    std::fs::set_permissions(path, original).unwrap();
}

// Catches caching a failed read under the file's stamp: the server would
// keep a file that was only locked for a moment missing until its mtime
// changed.
#[test]
fn a_file_that_could_not_be_read_is_read_again() {
    let (engine, root) = (Engine::default(), fixture("locked"));
    let args = json!({ "path": "src", "min_tokens": 20 });
    while_unreadable(&root.join("src/b.ts"), || {
        tool(&engine, &root, "find_duplicates", args.clone());
    });
    let reply = tool(&engine, &root, "find_duplicates", args);
    assert!(tool_text(&reply).contains("src/b.ts"), "{reply}");
}

// Catches reading integers with `as_u64` alone: JSON Schema counts 5.0 as
// an integer, and clients that hold every number as a double send it so.
#[test]
fn a_number_with_no_fraction_is_an_integer() {
    let (engine, root) = (Engine::default(), fixture("zero-fraction"));
    let cases = [
        ("find_duplicates", "limit", json!(5.0)),
        ("find_duplicates", "offset", json!(0.0)),
        ("find_duplicates", "min_tokens", json!(20.0)),
        ("duplicates_at", "start_line", json!(2.0)),
    ];
    for (name, key, value) in cases {
        let mut args = json!({ "path": "src", "file": "src/a.ts", "min_tokens": 20 });
        args[key] = value.clone();
        let reply = tool(&engine, &root, name, args);
        assert_eq!(reply["result"]["isError"], false, "{name} {key}={value}");
        assert!(tool_text(&reply).contains("src/b.ts"), "{reply}");
    }
}

// Catches get_settings explaining a scan that could not run: an agent would
// read default settings for a path that does not exist.
#[test]
fn get_settings_fails_where_a_scan_would() {
    let (engine, root) = (Engine::default(), fixture("settings-fail"));
    std::fs::create_dir_all(root.join("broken")).unwrap();
    std::fs::write(root.join("broken/.doppelscan.toml"), "nonsense = 1\n").unwrap();
    let cases = [
        json!({ "path": "nonexistent" }),
        json!({ "exclude": ["["] }),
        json!({ "languages": ["bogus"] }),
        json!({}),
    ];
    for args in cases {
        let reply = tool(&engine, &root, "get_settings", args.clone());
        assert_eq!(reply["result"]["isError"], true, "{args}");
    }
    let fine = tool(&engine, &root, "get_settings", json!({ "path": "src" }));
    assert_eq!(fine["result"]["isError"], false, "{fine}");
}

// Catches an explicit null, how some clients spell "unset", being refused
// now that wrong types are.
#[test]
fn a_null_argument_means_unset() {
    let (engine, root) = (Engine::default(), fixture("nulls"));
    let args = json!({ "path": "src", "min_tokens": 20, "limit": null, "offset": null });
    let reply = tool(&engine, &root, "find_duplicates", args);
    assert!(tool_text(&reply).contains("src/a.ts:2-10"), "{reply}");
}

// Catches `end_line` no longer defaulting to `start_line`, which would widen
// a one-line question to the rest of the file.
#[test]
fn a_start_line_alone_asks_about_one_line() {
    let (engine, root) = (Engine::default(), fixture("one-line"));
    let args = json!({ "file": "src/b.ts", "start_line": 1, "min_tokens": 20 });
    let reply = tool(&engine, &root, "duplicates_at", args);
    assert!(tool_text(&reply).starts_with("No clones"), "{reply}");
}

// Catches a config's threshold skipping the range check arguments get: past
// 1 nothing can match, so a typo reads as a clean "0 near-miss groups".
#[test]
fn a_similarity_from_config_outside_the_range_is_a_tool_error() {
    let (engine, root) = (Engine::default(), fixture("config-similarity"));
    std::fs::write(root.join(".doppelscan.toml"), "[near]\nsimilarity = 5.0\n").unwrap();
    let reply = tool(
        &engine,
        &root,
        "find_near_duplicates",
        json!({ "path": "src" }),
    );
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(tool_text(&reply).contains("similarity"), "{reply}");
}

// Catches `base` silently winning over `baseline`, so the caller believes
// the check ran against the file it named.
#[test]
fn check_with_both_base_and_baseline_is_a_tool_error() {
    let (engine, root) = (Engine::default(), fixture("both"));
    let args = json!({ "base": "HEAD", "baseline": "b.json" });
    let reply = tool(&engine, &root, "check_new_duplicates", args);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(tool_text(&reply).contains("baseline"), "{reply}");
}

// Catches a path outside the server root showing absolute working-tree paths
// beside relative revision paths, so a clone that only shrank reads as new,
// and the `//?/` prefix of canonical Windows paths reaching the results.
#[test]
fn a_path_outside_the_server_root_reads_like_any_other() {
    let (engine, repo) = (Engine::default(), fixture("outside"));
    let server = fixture("outside-root");
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "init"]);
    let shrunk = std::fs::read_to_string(repo.join("src/b.ts"))
        .unwrap()
        .replace("return Math.round(total * 100) / 100;", "return total;");
    std::fs::write(repo.join("src/b.ts"), shrunk).unwrap();
    let dir = repo.file_name().unwrap().to_str().unwrap();
    for path in [repo.to_str().unwrap(), &format!("../{dir}")] {
        let args = json!({ "path": path, "min_tokens": 20 });
        let found = tool(&engine, &server, "find_duplicates", args.clone());
        assert!(tool_text(&found).contains("\n  src/a.ts:"), "{found}");
        let check = tool(&engine, &server, "check_new_duplicates", args);
        assert!(
            tool_text(&check).starts_with("No new duplication"),
            "{check}"
        );
    }
}

// Catches a lookup failure sending the agent back to find_duplicates, which
// never lists a group that only exists under other options or with a
// snippet in the scan, and hiding why the id or prefix was refused.
#[test]
fn an_unknown_group_says_what_would_find_it() {
    let (engine, root) = (Engine::default(), fixture("unknown"));
    let args = json!({ "id": "abcdef123456", "path": "src", "min_tokens": 20 });
    let reply = tool(&engine, &root, "get_duplicate_group", args);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = tool_text(&reply);
    assert!(!text.contains("run find_duplicates again"), "{text}");
    for wanted in [
        "ids run 1..=1",
        "at least 4",
        "languages",
        "find_similar_code",
    ] {
        assert!(text.contains(wanted), "{wanted}: {text}");
    }
}

fn at(engine: &Engine, root: &Path, file: &str) -> Value {
    let args = json!({ "file": file, "start_line": 5, "min_tokens": 20 });
    tool(engine, root, "duplicates_at", args)
}

// Catches suffix matching: `a.ts` answering for `src/a.ts`, or the reverse,
// reports another file's clones for the file being edited.
#[test]
fn duplicates_at_means_exactly_the_file_named() {
    let (engine, root) = (Engine::default(), fixture("exact-file"));
    std::fs::write(root.join("a.ts"), "export const only = 1;\n").unwrap();
    let root_file = at(&engine, &root, "a.ts");
    assert_eq!(root_file["result"]["isError"], false, "{root_file}");
    assert!(
        tool_text(&root_file).starts_with("No clones"),
        "{root_file}"
    );
    let longer = at(&engine, &root, "packages/src/a.ts");
    assert_eq!(longer["result"]["isError"], true, "{longer}");
}

// Catches normalising too little once matching is exact: a shown path has
// forward slashes and no `./`, but a caller's need not.
#[test]
fn duplicates_at_accepts_other_spellings_of_the_path() {
    let (engine, root) = (Engine::default(), fixture("spelling"));
    for file in ["src/a.ts", "./src/a.ts", "src\\a.ts", ".\\src\\a.ts"] {
        let reply = at(&engine, &root, file);
        assert!(tool_text(&reply).contains("src/b.ts:2-10"), "{reply}");
    }
}

// Catches a typo in the path reading as "this code is not duplicated".
#[test]
fn duplicates_at_a_file_outside_the_scan_is_a_tool_error() {
    let (engine, root) = (Engine::default(), fixture("no-file"));
    let reply = at(&engine, &root, "src/missing.ts");
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(tool_text(&reply).contains("src/missing.ts"), "{reply}");
}

// Catches the overlap test accepting a range that ends before it starts,
// which reads as a match when both ends fall inside an occurrence.
#[test]
fn duplicates_at_an_inverted_range_is_a_tool_error() {
    let (engine, root) = (Engine::default(), fixture("inverted"));
    let args = json!({ "file": "src/a.ts", "start_line": 8, "end_line": 3, "min_tokens": 20 });
    let reply = tool(&engine, &root, "duplicates_at", args);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(tool_text(&reply).contains("8-3"), "{reply}");
}

fn similar(engine: &Engine, root: &Path, code: &str, language: &str) -> String {
    let args = json!({ "code": code, "language": language });
    let reply = tool(engine, root, "find_similar_code", args);
    tool_text(&reply).to_string()
}

// Catches a snippet that repeats itself counting as a match: the only
// copies are its own, so the agent is told code exists that does not.
#[test]
fn a_snippet_repeating_itself_is_not_a_match() {
    let (engine, root) = (Engine::default(), fixture("self-match"));
    let line = "registry.publish(order.customer.id, order.total * config.rate, \"paid\", { retry: true, delay: 100 });\n";
    let text = similar(&engine, &root, &format!("{line}\n{line}"), "ts");
    assert!(text.starts_with("No existing code"), "{text}");
}

// Catches snippet search applying the density limit meant for ranking
// helpers: with every name and literal changed a match is all parameters,
// which is what the tool promises to find.
#[test]
fn a_snippet_differing_in_every_name_and_literal_still_matches() {
    let (engine, root) = (Engine::default(), fixture("density"));
    let existing = "export function loadUser(cache: Store): Result {
    audit.record(\"loading\", 1);
    metrics.bump(\"users\", 2);
    tracer.start(\"span\", 3);
    queue.push(\"job\", 4);
    return registry.fetch(\"key\", 5);
}
";
    std::fs::write(root.join("src/c.ts"), existing).unwrap();
    let snippet = "function dropOrder(ledger: Vault): Outcome {
    journal.write(\"closing\", 10);
    gauges.raise(\"orders\", 20);
    probes.begin(\"trace\", 30);
    inbox.send(\"task\", 40);
    return catalog.lookup(\"slot\", 50);
}
";
    let text = similar(&engine, &root, snippet, "ts");
    assert!(text.contains("src/c.ts:1-7"), "{text}");
}

// Catches the opening tag a tagless PHP snippet is given shifting every
// line, so results point past the code the caller sent.
#[test]
fn a_php_snippet_without_a_tag_keeps_its_line_numbers() {
    let (engine, root) = (Engine::default(), fixture("php"));
    let function = "function sumTotals(array $orders, float $rate): float {
    $total = 0;
    foreach ($orders as $order) {
        $total += $order->amount * $rate;
    }
    return round($total, 2);
}
";
    std::fs::write(root.join("src/c.php"), format!("<?php\n{function}")).unwrap();
    let text = similar(&engine, &root, function, "php");
    assert!(text.contains("<snippet>:1-7"), "{text}");
}

// Catches an accepted clone reading as "nothing like this exists", which
// sends the agent off to write a further copy of it.
#[test]
fn a_match_on_an_accepted_clone_says_so() {
    let (engine, root) = (Engine::default(), fixture("accepted"));
    let args = json!({ "path": "src", "min_tokens": 20 });
    let found = tool(&engine, &root, "find_duplicates", args);
    let text = tool_text(&found);
    let fingerprint = text.split('[').nth(1).unwrap().split(']').next().unwrap();
    let config = format!("[[accept]]\nfingerprint = \"{fingerprint}\"\nreason = \"on purpose\"\n");
    std::fs::write(root.join(".doppelscan.toml"), config).unwrap();
    let text = similar(&engine, &root, CLONE, "ts");
    assert!(!text.starts_with("No existing code"), "{text}");
    assert!(text.contains("accepted"), "{text}");
    assert!(text.contains("src/a.ts:2-10"), "{text}");
}

// Catches the listing truncating at the brief limit: the tool promises every
// copy, and offers no other way to reach the ones it cut.
#[test]
fn find_near_duplicates_lists_every_copy() {
    let (engine, root) = (Engine::default(), fixture("near-all"));
    for copy in 0..7 {
        let edited = CLONE.replace(
            "            total += order.amount * config.rate;\n",
            &format!(
                "            total += order.amount * config.rate;\n{}",
                "            fees += order.fee;\n".repeat(copy + 1)
            ),
        );
        std::fs::write(root.join(format!("src/copy{copy}.ts")), edited).unwrap();
    }
    let args = json!({ "path": "src", "min_tokens": 20, "similarity": 0.5 });
    let reply = tool(&engine, &root, "find_near_duplicates", args);
    let text = tool_text(&reply);
    assert!(!text.contains("more"), "{text}");
    for copy in 0..7 {
        assert!(text.contains(&format!("src/copy{copy}.ts:")), "{text}");
    }
}
