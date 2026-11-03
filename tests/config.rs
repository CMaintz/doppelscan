mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// A repo with one clone pair in `src` and `config` as its config file.
fn repo_with_config(name: &str, config: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("doppelscan-config-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join("src/a.ts"), CLONE).unwrap();
    std::fs::write(dir.join("src/b.ts"), CLONE.replace("sumTotals", "sumFees")).unwrap();
    std::fs::write(dir.join(".doppelscan.toml"), config).unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_doppelscan"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).to_string();
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

/// Groups found by a JSON scan with `args`.
fn group_count(dir: &Path, args: &[&str]) -> u64 {
    let (code, out, err) = run(dir, &[args, &["--format", "json"]].concat());
    assert_eq!(code, 0, "{err}");
    let report: Value = serde_json::from_str(&out).unwrap();
    report["summary"]["groups"].as_u64().unwrap()
}

// Catches the config file not being read at all.
#[test]
fn config_settings_apply_without_flags() {
    let dir = repo_with_config("applies", "min-tokens = 500\n");
    assert_eq!(group_count(&dir, &["."]), 0);
}

// Catches the config winning over what was typed: a flag is the more
// specific request.
#[test]
fn a_flag_overrides_the_config() {
    let dir = repo_with_config("override", "min-tokens = 500\n");
    assert_eq!(group_count(&dir, &[".", "--min-tokens", "20"]), 1);
}

// Catches excludes from the config replacing, or being replaced by, those
// on the command line instead of adding to them.
#[test]
fn config_and_flag_excludes_add_up() {
    let dir = repo_with_config("exclude", "exclude = [\"src/a.ts\"]\n");
    std::fs::write(dir.join("src/c.ts"), CLONE.replace("sumTotals", "sumTax")).unwrap();
    assert_eq!(group_count(&dir, &["."]), 1);
    assert_eq!(group_count(&dir, &[".", "--exclude", "src/b.ts"]), 0);
}

// Catches only looking in the scanned directory: a config at the repo
// root covers a scan of any directory under it.
#[test]
fn a_config_in_a_parent_directory_applies() {
    let dir = repo_with_config("parent", "min-tokens = 500\n");
    assert_eq!(group_count(&dir, &["src"]), 0);
}

// Catches silently ignoring a misspelled key, which would leave a team
// believing a setting is on.
#[test]
fn an_unknown_key_is_an_error() {
    let dir = repo_with_config("unknown", "min_tokens = 500\n");
    let (code, _, err) = run(&dir, &["."]);
    assert_eq!(code, 2);
    assert!(err.contains("min_tokens"), "{err}");
}

// Catches the near command taking its threshold only from the flag.
#[test]
fn near_reads_its_similarity_from_the_config() {
    let edited = CLONE.replace("sumTotals", "sumFees").replace(
        "    return",
        "    audit(total);\n    log(total);\n    return",
    );
    let dir = repo_with_config("near", "similarity = 0.3\nmin-tokens = 20\n");
    std::fs::write(dir.join("src/b.ts"), edited).unwrap();
    let (code, out, err) = run(&dir, &["near", "."]);
    assert_eq!(code, 0, "{err}");
    assert!(!out.starts_with("0 near-miss groups"), "{out}");
    let (_, out, _) = run(&dir, &["near", ".", "--similarity", "1.0"]);
    assert!(out.starts_with("0 near-miss groups"), "{out}");
}

// Catches the MCP server reading settings only from tool arguments: an
// agent should see what the team configured.
#[test]
fn mcp_tools_use_the_config() {
    let dir = repo_with_config("mcp", "min-tokens = 500\n");
    let msg = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "find_duplicates", "arguments": { "path": "src" } },
    });
    let reply = handle_line(&Engine::default(), &dir, &msg.to_string()).unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("0 clone groups"), "{text}");
}
