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
    let dir = repo_with_config("near", "min-tokens = 20\n[near]\nsimilarity = 0.3\n");
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

fn write(dir: &Path, path: &str, text: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

// Catches excludes matched against the scanned path instead of the config
// file's directory: the same config would skip different files depending
// on which directory is scanned.
#[test]
fn config_excludes_are_relative_to_the_config_file() {
    let dir = repo_with_config("relative", "exclude = [\"src/a.ts\"]\n");
    assert_eq!(group_count(&dir, &["src"]), 0);
    assert_eq!(group_count(&dir, &["."]), 0);
}

// Catches `include` opening the whole skipped directory, or none of it:
// only the named subtree comes back.
#[test]
fn include_scans_a_named_part_of_a_skipped_directory() {
    let dir = repo_with_config("include", "include = [\"vendor/ours\"]\n");
    write(
        &dir,
        "vendor/ours/c.ts",
        &CLONE.replace("sumTotals", "sumTax"),
    );
    write(
        &dir,
        "vendor/theirs/d.ts",
        &CLONE.replace("sumTotals", "sumVat"),
    );
    let (code, out, err) = run(&dir, &[".", "--format", "json"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("vendor/ours/c.ts"), "{out}");
    assert!(!out.contains("vendor/theirs"), "{out}");
}

// Catches an include written with a wildcard, as .gitignore patterns
// usually are, never reaching its directory: `/**` matches only what is
// inside, and a wildcard segment hides the path below it.
#[test]
fn wildcard_includes_reach_into_a_skipped_directory() {
    for pattern in ["vendor/ours/**", "vendor/*/src/"] {
        let config = format!("include = [\"{pattern}\"]\n");
        let dir = repo_with_config("include-glob", &config);
        write(
            &dir,
            "vendor/ours/src/c.ts",
            &CLONE.replace("sumTotals", "sumTax"),
        );
        let (_, out, err) = run(&dir, &[".", "--format", "json"]);
        assert!(
            out.contains("vendor/ours/src/c.ts"),
            "{pattern}: {out}{err}"
        );
    }
}

// Catches the revision side of `check` reading files a config excludes:
// a committed copy there would excuse the same clone added elsewhere.
#[test]
fn check_against_a_revision_honours_config_excludes() {
    let dir = repo_with_config("check", "exclude = [\"src/old/\"]\n");
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    write(&dir, "src/old/b.ts", &CLONE.replace("sumTotals", "sumFees"));
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "excluded copy"]);
    std::fs::remove_file(dir.join("src/old/b.ts")).unwrap();
    write(&dir, "src/c.ts", &CLONE.replace("sumTotals", "sumTax"));
    let (code, out, err) = run(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 1, "{out}{err}");
}

// Catches the repository's min-tokens overriding the snippet search's own
// lower default, which exists because snippets are small.
#[test]
fn snippet_search_keeps_its_own_default_over_the_config() {
    let dir = repo_with_config("similar", "min-tokens = 50\n");
    let snippet = "let total = 0;\nfor (const order of orders) {\n    if (order.status === \"paid\") {\n        total += order.amount * config.rate;\n    }\n}\n";
    let msg = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "find_similar_code", "arguments": { "code": snippet, "language": "ts" } },
    });
    let reply = handle_line(&Engine::default(), &dir, &msg.to_string()).unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("src/a.ts"), "{text}");
}

// Catches a package's config being read only when scanning inside the
// package: its excludes are about its own files, wherever the scan starts.
#[test]
fn a_nested_config_excludes_apply_to_a_scan_from_above() {
    let dir = repo_with_config("below", "");
    write(&dir, "src/.doppelscan.toml", "exclude = [\"a.ts\"]\n");
    assert_eq!(group_count(&dir, &["src"]), 0);
    assert_eq!(group_count(&dir, &["."]), 0);
}

// Catches near's excludes leaking into the exact scan, or being ignored by
// near: tests are near copies by design, but exact copies still matter.
#[test]
fn near_excludes_apply_to_near_only() {
    let config = "min-tokens = 20\n[near]\nsimilarity = 0.3\nexclude = [\"src/b.ts\"]\n";
    let dir = repo_with_config("near-exclude", config);
    let edited = CLONE.replace("sumTotals", "sumTax").replace(
        "    return",
        "    audit(total);\n    log(total);\n    return",
    );
    write(&dir, "src/c.ts", &edited);
    let (_, exact, _) = run(&dir, &[".", "--format", "json"]);
    assert!(exact.contains("src/b.ts"), "{exact}");
    let (_, near, _) = run(&dir, &["near", ".", "--format", "json"]);
    assert!(near.contains("src/c.ts"), "{near}");
    assert!(!near.contains("src/b.ts"), "{near}");
}

// Catches only the nearest config being read: a package's config adds to
// the repository's instead of discarding it, and wins where both set a key.
#[test]
fn nested_configs_merge_with_the_nearest_winning() {
    let dir = repo_with_config("nested", "min-tokens = 500\n");
    write(&dir, "src/.doppelscan.toml", "max-param-density = 9\n");
    assert_eq!(group_count(&dir, &["src"]), 0);
    write(&dir, "src/.doppelscan.toml", "min-tokens = 20\n");
    assert_eq!(group_count(&dir, &["src"]), 1);
}

// Catches `root = true` not stopping the search for parent configs.
#[test]
fn a_root_config_stops_the_merge() {
    let dir = repo_with_config("root", "min-tokens = 500\n");
    write(&dir, "src/.doppelscan.toml", "root = true\n");
    assert_eq!(group_count(&dir, &["src"]), 1);
}

// Catches a config `strict = true` that no flag can turn off.
#[test]
fn no_strict_overrides_a_strict_config() {
    let dir = repo_with_config("strict", "strict = true\n");
    assert_eq!(group_count(&dir, &["."]), 0);
    assert_eq!(group_count(&dir, &[".", "--no-strict"]), 1);
}

// Catches --no-config still reading the file, and --config adding to the
// lookup instead of replacing it.
#[test]
fn config_flags_choose_the_file() {
    let dir = repo_with_config("flags", "min-tokens = 500\n");
    assert_eq!(group_count(&dir, &[".", "--no-config"]), 1);
    let other = dir.join("other.toml");
    std::fs::write(&other, "exclude = [\"nothing\"]\n").unwrap();
    assert_eq!(
        group_count(&dir, &[".", "--config", other.to_str().unwrap()]),
        1
    );
}

fn settings_json(dir: &Path, args: &[&str]) -> Value {
    let (code, out, err) = run(dir, &[&["config"], args, &["--format", "json"]].concat());
    assert_eq!(code, 0, "{err}");
    serde_json::from_str(&out).unwrap()
}

fn setting<'a>(settings: &'a Value, key: &str) -> &'a Value {
    let all = settings["settings"].as_array().unwrap();
    all.iter().find(|s| s["key"] == key).unwrap()
}

// Catches a setting shown with the wrong source, which would send someone
// editing the wrong file (or a flag) to change it.
#[test]
fn config_shows_where_each_setting_comes_from() {
    let dir = repo_with_config("show", "min-tokens = 500\n[near]\nsimilarity = 0.5\n");
    let shown = settings_json(&dir, &[".", "--max-param-density", "9"]);
    let min = setting(&shown, "min-tokens");
    assert_eq!(min["value"], 500);
    assert!(
        min["from"].as_str().unwrap().ends_with(".doppelscan.toml"),
        "{min}"
    );
    assert_eq!(setting(&shown, "max-param-density")["from"], "flag");
    assert_eq!(setting(&shown, "strict")["from"], "default");
    assert_eq!(setting(&shown, "similarity")["value"], 0.5);
    let dirs = shown["built_in_skips"]["dirs"].as_array().unwrap();
    assert!(dirs.iter().any(|d| d == "node_modules"), "{shown}");
}

// Catches the shown skip list drifting from what the walk skips: a
// directory left off it is skipped with nothing to say why.
#[test]
fn config_lists_generated_source_dirs_as_skipped() {
    let dir = repo_with_config("skips", "");
    let shown = settings_json(&dir, &[]);
    let dirs = shown["built_in_skips"]["dirs"].as_array().unwrap();
    assert!(dirs.iter().any(|d| d == "generated-sources"), "{dirs:?}");
}

// Catches the MCP settings tool and the CLI drifting apart: an agent and a
// person must see the same settings for the same path.
#[test]
fn mcp_settings_match_the_cli() {
    let dir = repo_with_config("show-mcp", "min-tokens = 500\nexclude = [\"legacy/\"]\n");
    let msg = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "get_settings", "arguments": { "path": "src" } },
    });
    let reply = handle_line(&Engine::default(), &dir, &msg.to_string()).unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    let from_mcp: Value = serde_json::from_str(text).unwrap();
    assert_eq!(from_mcp, settings_json(&dir, &["src"]));
}

// Catches init writing a config the scan then rejects, overwriting an
// existing one, or missing the test files near should leave out.
#[test]
fn init_writes_a_working_config_once() {
    let dir = repo_with_config("init", "");
    std::fs::remove_file(dir.join(".doppelscan.toml")).unwrap();
    write(&dir, "src/a.test.ts", "test('a', () => {});\n");
    let (code, _, err) = run(&dir, &["init"]);
    assert_eq!(code, 0, "{err}");
    let written = std::fs::read_to_string(dir.join(".doppelscan.toml")).unwrap();
    assert!(written.contains("\"**/*.test.ts\""), "{written}");
    assert_eq!(group_count(&dir, &["."]), 1);
    assert_eq!(run(&dir, &["init"]).0, 2);
    assert_eq!(run(&dir, &["init", "--force"]).0, 0);
}

// Catches a key in the generated template that the config would reject,
// which a user only finds when they uncomment it.
#[test]
fn every_key_init_suggests_is_accepted() {
    let dir = repo_with_config("init-keys", "");
    std::fs::remove_file(dir.join(".doppelscan.toml")).unwrap();
    assert_eq!(run(&dir, &["init"]).0, 0);
    let written = std::fs::read_to_string(dir.join(".doppelscan.toml")).unwrap();
    let uncommented: String = written
        .lines()
        .map(|l| {
            l.strip_prefix("# ")
                .filter(|r| r.contains(" = ") || r.starts_with('['))
                .unwrap_or(l)
        })
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(uncommented.contains("min-tokens = "), "{uncommented}");
    std::fs::write(dir.join(".doppelscan.toml"), &uncommented).unwrap();
    let (code, _, err) = run(&dir, &["config", "."]);
    assert_eq!(code, 0, "{err}\n{uncommented}");
}

// Catches the committed schema falling behind the config it describes:
// editors would then flag valid keys or miss new ones.
#[test]
fn the_committed_schema_matches_the_config() {
    let (code, out, err) = run(Path::new("."), &["config", "--schema"]);
    assert_eq!(code, 0, "{err}");
    let committed = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("doppelscan.schema.json"),
    )
    .unwrap();
    assert_eq!(
        out, committed,
        "run `doppelscan config --schema > doppelscan.schema.json`"
    );
}

fn only_fingerprint(dir: &Path) -> String {
    let (_, out, _) = run(dir, &[".", "--format", "json"]);
    let report: Value = serde_json::from_str(&out).unwrap();
    report["groups"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string()
}

// Catches an accepted clone still being reported, or still failing check
// when the same code is copied once more.
#[test]
fn an_accepted_fingerprint_is_not_reported_or_checked() {
    let dir = repo_with_config("accept", "");
    let fingerprint = only_fingerprint(&dir);
    let config =
        format!("[[accept]]\nfingerprint = \"{fingerprint}\"\nreason = \"generated by hand\"\n");
    std::fs::write(dir.join(".doppelscan.toml"), config).unwrap();
    assert_eq!(group_count(&dir, &["."]), 0);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "two copies"]);
    write(&dir, "src/c.ts", &CLONE.replace("sumTotals", "sumTax"));
    assert_eq!(run(&dir, &["check", "--base", "HEAD"]).0, 0);
}

// Catches an accept that keeps no reason, which is the one thing it
// records that a baseline entry does not.
#[test]
fn an_accept_needs_a_reason() {
    let dir = repo_with_config("accept-reason", "[[accept]]\nfingerprint = \"abc\"\n");
    let (code, _, err) = run(&dir, &["."]);
    assert_eq!(code, 2);
    assert!(err.contains("reason"), "{err}");
}

// Catches accepting by prefix or substring: another clone whose
// fingerprint merely starts the same must still be reported.
#[test]
fn only_the_exact_fingerprint_is_accepted() {
    let dir = repo_with_config("accept-exact", "");
    let fingerprint = only_fingerprint(&dir);
    let config = format!(
        "[[accept]]\nfingerprint = \"{}\"\nreason = \"x\"\n",
        &fingerprint[..6]
    );
    std::fs::write(dir.join(".doppelscan.toml"), config).unwrap();
    assert_eq!(group_count(&dir, &["."]), 1);
}

// Catches --no-config and --config stopping at the lookup: a config inside
// the scanned tree is still a config, on either side of a comparison.
#[test]
fn config_flags_also_cover_nested_configs() {
    let dir = repo_with_config("flags-nested", "");
    write(&dir, "src/.doppelscan.toml", "exclude = [\"*.ts\"]\n");
    assert_eq!(group_count(&dir, &["."]), 0);
    assert_eq!(group_count(&dir, &[".", "--no-config"]), 1);
    let other = dir.join("other.toml");
    std::fs::write(&other, "min-tokens = 20\n").unwrap();
    assert_eq!(
        group_count(&dir, &[".", "--config", other.to_str().unwrap()]),
        1
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "pair"]);
    let (code, out, err) = run(&dir, &["check", "--base", "HEAD", "--no-config"]);
    assert_eq!(code, 0, "{out}{err}");
}

// Catches --no-config still parsing nested configs: it exists to get past
// a config that does not load.
#[test]
fn no_config_survives_a_broken_nested_config() {
    let dir = repo_with_config("flags-broken", "");
    write(&dir, "src/.doppelscan.toml", "bogus = 1\n");
    assert_eq!(run(&dir, &["."]).0, 2);
    assert_eq!(group_count(&dir, &[".", "--no-config"]), 1);
}

// Catches near keeping its own route to nested configs.
#[test]
fn near_no_config_covers_nested_configs() {
    let dir = repo_with_config("flags-near", "");
    let edited = CLONE.replace("sumTotals", "sumFees").replace(
        "    return",
        "    audit(total);\n    log(total);\n    return",
    );
    write(&dir, "src/b.ts", &edited);
    write(&dir, "src/.doppelscan.toml", "exclude = [\"*.ts\"]\n");
    let args = ["near", ".", "--no-config", "--min-tokens", "20"];
    let (code, out, err) = run(&dir, &[&args[..], &["--similarity", "0.3"]].concat());
    assert_eq!(code, 0, "{err}");
    assert!(!out.starts_with("0 near-miss groups"), "{out}");
}

const MARKED: &str = "// @generated by a tool\n";

/// A repo whose only clone pair sits in `generated/`, marked as generated.
fn repo_with_marked_pair(name: &str, config: &str) -> PathBuf {
    let dir = repo_with_config(name, config);
    std::fs::remove_dir_all(dir.join("src")).unwrap();
    let marked = format!("{MARKED}{CLONE}");
    write(&dir, "generated/a.ts", &marked);
    write(&dir, "generated/b.ts", &marked.replace("sumTotals", "sumFees"));
    dir
}

// Catches `include` bringing a generated directory back only to lose its
// files one by one to their marker: the group silently disappears.
#[test]
fn include_brings_back_files_marked_as_generated() {
    let dir = repo_with_marked_pair("include-marked", "include = [\"generated/\"]\n");
    assert_eq!(group_count(&dir, &["."]), 1);
}

// Catches the revision side still dropping marked files: the working tree
// would then show a committed pair as new on every check.
#[test]
fn check_keeps_included_marked_files_on_both_sides() {
    let dir = repo_with_marked_pair("include-check", "include = [\"generated/\"]\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "generated pair"]);
    let (code, out, err) = run(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 0, "{out}{err}");
}

// Catches the marker check being dropped for everything, not just for what
// a config names.
#[test]
fn a_marker_still_skips_files_no_config_includes() {
    let dir = repo_with_marked_pair("include-other", "");
    write(&dir, "src/a.ts", &format!("{MARKED}{CLONE}"));
    write(
        &dir,
        "src/b.ts",
        &format!("{MARKED}{}", CLONE.replace("sumTotals", "sumTax")),
    );
    assert_eq!(group_count(&dir, &["."]), 0);
}

// Catches the MCP server's file cache keeping a generated file's empty
// result after `include` starts to name it: the cache stamps the source
// file, which did not change.
#[test]
fn a_long_lived_engine_sees_a_new_include() {
    let dir = repo_with_marked_pair("include-cache", "");
    let engine = Engine::default();
    let msg = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "find_duplicates", "arguments": {} },
    })
    .to_string();
    let first = handle_line(&engine, &dir, &msg).unwrap();
    assert!(first.contains("0 clone groups"), "{first}");
    let config = "include = [\"generated/\"]\n";
    std::fs::write(dir.join(".doppelscan.toml"), config).unwrap();
    let second = handle_line(&engine, &dir, &msg).unwrap();
    assert!(second.contains("1 clone groups"), "{second}");
}

// Catches a directory named like a source file counting as one: `init`
// reports files the scan cannot read.
#[test]
fn init_counts_files_not_directories_named_like_them() {
    let dir = repo_with_config("init-dir", "");
    std::fs::remove_file(dir.join(".doppelscan.toml")).unwrap();
    std::fs::create_dir_all(dir.join("foo.ts")).unwrap();
    assert_eq!(run(&dir, &["init"]).0, 0);
    let written = std::fs::read_to_string(dir.join(".doppelscan.toml")).unwrap();
    assert!(written.contains("(2 files)"), "{written}");
}

// Catches init counting a file every scan then drops as generated, which
// would suggest a language the repository has no code in.
#[test]
fn init_does_not_count_generated_files() {
    let dir = repo_with_config("init-generated", "");
    std::fs::remove_file(dir.join(".doppelscan.toml")).unwrap();
    write(&dir, "src/c.ts", &format!("{MARKED}{CLONE}"));
    assert_eq!(run(&dir, &["init"]).0, 0);
    let written = std::fs::read_to_string(dir.join(".doppelscan.toml")).unwrap();
    assert!(written.contains("(2 files)"), "{written}");
}

// Catches init reading configs below it: it describes the whole tree, and
// a broken package config should not stop it writing the root one.
#[test]
fn init_does_not_read_nested_configs() {
    let dir = repo_with_config("init-nested", "");
    std::fs::remove_file(dir.join(".doppelscan.toml")).unwrap();
    write(&dir, "src/.doppelscan.toml", "bogus = 1\n");
    let (code, _, err) = run(&dir, &["init"]);
    assert_eq!(code, 0, "{err}");
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

// Creating a link can need a privilege on Windows; without it the test has
// nothing to show there, and CI on Unix still runs it.
#[cfg(windows)]
fn symlink(target: &str, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(target, link).is_ok()
}

// Catches the walk following a tracked symlink the revision side drops
// (mode 120000): the same text would read as new on every check.
#[test]
fn a_tracked_symlink_is_left_out_of_both_sides() {
    let dir = repo_with_config("symlink", "");
    std::fs::remove_file(dir.join("src/b.ts")).unwrap();
    if !symlink("a.ts", &dir.join("src/b.ts")) {
        eprintln!("no symlink privilege here; skipping");
        return;
    }
    git(&dir, &["-c", "core.symlinks=true", "add", "."]);
    git(&dir, &["commit", "-qm", "linked copy"]);
    assert_eq!(group_count(&dir, &["."]), 0);
    let (code, out, err) = run(&dir, &["check", "--base", "HEAD"]);
    assert_eq!(code, 0, "{out}{err}");
}

// Catches dropping symlinks from what was typed: a link named directly is
// a request to scan what it points at, as `.` through a linked checkout is.
#[test]
fn a_symlink_named_directly_is_still_scanned() {
    let dir = repo_with_config("symlink-root", "");
    if !symlink("a.ts", &dir.join("src/link.ts")) {
        eprintln!("no symlink privilege here; skipping");
        return;
    }
    let (code, out, err) = run(&dir, &["src/link.ts", "--format", "json"]);
    assert_eq!(code, 0, "{err}");
    let report: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["summary"]["files"], 1, "{out}");
}
