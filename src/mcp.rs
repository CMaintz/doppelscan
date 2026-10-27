//! MCP server over stdio (newline-delimited JSON-RPC).
//!
//! Dual-era per the 2026-07-28 spec: requests carrying the modern
//! per-request `_meta` protocol version are served statelessly, and an
//! `initialize` handshake selects legacy semantics for older clients.
//! https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning
//!
//! Hand-rolled instead of an SDK: the protocol surface used here is four
//! methods, and avoiding an async runtime keeps startup instant.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::baseline::Baseline;
use crate::engine::{Against, Engine, ScanOptions};
use crate::lang::{LANGS, lang_index_by_name};
use crate::report::{Analysis, Detail, render_check, render_group, render_summary};

const MODERN: &str = "2026-07-28";
const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";

const INSTRUCTIONS: &str = "doppelscan finds duplicated code by syntax structure (names and literals may differ). \
Use find_duplicates to survey a repo for helper-extraction candidates, get_duplicate_group for one group's \
helper template and parameters, duplicates_at to check a file/line range you are editing, find_similar_code \
before writing a new helper to see whether equivalent code already exists, and check_new_duplicates after \
writing code to see whether it duplicated anything. Prefer fingerprints over numeric ids \
when referring back to a group; ids depend on the scan options.";

pub fn serve(engine: &Engine, root: &Path) -> anyhow::Result<()> {
    let root = root.canonicalize()?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(engine, &root, &line) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// One JSON-RPC message in, at most one out (notifications get none).
pub fn handle_line(engine: &Engine, root: &Path, line: &str) -> Option<String> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(
                error(Value::Null, -32700, &format!("parse error: {e}"), None).to_string(),
            );
        }
    };
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    Some(dispatch(engine, root, id, method, &params).to_string())
}

fn dispatch(engine: &Engine, root: &Path, id: Value, method: &str, params: &Value) -> Value {
    let requested = params
        .pointer(&format!("/_meta/{}", VERSION_KEY.replace('/', "~1")))
        .and_then(Value::as_str);
    if let Some(v) = requested.filter(|v| *v != MODERN && !LEGACY.contains(v)) {
        let data = json!({ "supported": supported(), "requested": v });
        return error(id, -32022, "Unsupported protocol version", Some(data));
    }
    let modern = requested.is_some();
    let result = match method {
        "initialize" => Ok(initialize(params)),
        "server/discover" => Ok(discover()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(engine, root, params),
        _ => return error(id, -32601, &format!("method not found: {method}"), None),
    };
    match result {
        Ok(mut r) => {
            if modern {
                r["resultType"] = json!("complete");
            }
            json!({ "jsonrpc": "2.0", "id": id, "result": r })
        }
        Err((code, message)) => error(id, code, &message, None),
    }
}

fn supported() -> Vec<&'static str> {
    std::iter::once(MODERN)
        .chain(LEGACY.iter().copied())
        .collect()
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

fn server_info() -> Value {
    json!({ "name": "doppelscan", "version": env!("CARGO_PKG_VERSION") })
}

fn initialize(params: &Value) -> Value {
    let asked = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(LEGACY[0]);
    let version = if LEGACY.contains(&asked) {
        asked
    } else {
        LEGACY[0]
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": server_info(),
        "instructions": INSTRUCTIONS,
    })
}

fn discover() -> Value {
    json!({
        "supportedVersions": supported(),
        "capabilities": { "tools": {} },
        "_meta": { "io.modelcontextprotocol/serverInfo": server_info() },
        "instructions": INSTRUCTIONS,
    })
}

type ToolResult = Result<Value, (i64, String)>;

fn call_tool(engine: &Engine, root: &Path, params: &Value) -> ToolResult {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let outcome = match name {
        "find_duplicates" => find_duplicates(engine, root, &args),
        "get_duplicate_group" => get_group(engine, root, &args),
        "duplicates_at" => duplicates_at(engine, root, &args),
        "find_similar_code" => find_similar(engine, root, &args),
        "check_new_duplicates" => check_new(engine, root, &args),
        _ => return Err((-32602, format!("Unknown tool: {name}"))),
    };
    Ok(match outcome {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
        Err(e) => {
            json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true })
        }
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn uint_arg(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn str_list(args: &Value, key: &str) -> Vec<String> {
    let items = args.get(key).and_then(Value::as_array);
    items
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Relative paths resolve against the server root, so an agent can pass
/// paths exactly as they appear in the results.
fn scan_root(root: &Path, args: &Value) -> anyhow::Result<PathBuf> {
    let path = str_arg(args, "path")
        .map(|p| root.join(p))
        .unwrap_or_else(|| root.to_path_buf());
    anyhow::ensure!(path.exists(), "path does not exist: {}", path.display());
    Ok(path)
}

fn options(root: &Path, args: &Value, default_min: u64) -> anyhow::Result<ScanOptions> {
    let languages = str_list(args, "languages")
        .iter()
        .map(|l| {
            lang_index_by_name(l)
                .ok_or_else(|| anyhow::anyhow!("unknown language {l:?}; known: {}", known_langs()))
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(ScanOptions {
        min_tokens: uint_arg(args, "min_tokens", default_min) as u32,
        strict: args.get("strict").and_then(Value::as_bool).unwrap_or(false),
        languages,
        exclude: str_list(args, "exclude"),
        display_base: Some(root.to_path_buf()),
        ..ScanOptions::default()
    })
}

fn known_langs() -> String {
    LANGS.iter().map(|l| l.name).collect::<Vec<_>>().join(", ")
}

fn render(analysis: &Analysis, ids: &[usize], template: bool) -> String {
    let mut out = render_summary(&analysis.summary(), ids.len());
    for &id in ids {
        out.push('\n');
        let detail = if template {
            Detail::Full
        } else {
            Detail::Brief
        };
        out.push_str(&render_group(&analysis.group(id, template), detail));
    }
    out
}

fn find_duplicates(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let analysis = engine.scan(&scan_root(root, args)?, &options(root, args, 50)?)?;
    let offset = uint_arg(args, "offset", 0) as usize;
    let limit = uint_arg(args, "limit", 15) as usize;
    let ids: Vec<usize> = (offset + 1..=analysis.groups.len()).take(limit).collect();
    let templates = args
        .get("include_templates")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut out = render(&analysis, &ids, templates);
    if offset + ids.len() < analysis.groups.len() {
        out.push_str(&format!(
            "\nMore groups: call again with offset={}.\n",
            offset + ids.len()
        ));
    }
    Ok(out)
}

fn get_group(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let key =
        str_arg(args, "id").ok_or_else(|| anyhow::anyhow!("missing required argument: id"))?;
    let analysis = engine.scan(&scan_root(root, args)?, &options(root, args, 50)?)?;
    let id = analysis
        .resolve(key)
        .ok_or_else(|| anyhow::anyhow!("no clone group {key:?}; run find_duplicates again"))?;
    let mut out = render_group(&analysis.group(id, true), Detail::Full);
    if args
        .get("include_code")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        out.push_str(&occurrence_code(&analysis, id));
    }
    Ok(out)
}

fn occurrence_code(analysis: &Analysis, id: usize) -> String {
    analysis.groups[id - 1]
        .occs
        .iter()
        .map(|o| {
            let file = &analysis.files[o.file as usize];
            format!(
                "\n--- {}:{}-{}\n{}\n",
                file.path,
                o.start_line,
                o.end_line,
                file.text(o.start, o.end)
            )
        })
        .collect()
}

fn duplicates_at(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let file =
        str_arg(args, "file").ok_or_else(|| anyhow::anyhow!("missing required argument: file"))?;
    let from = uint_arg(args, "start_line", 1) as u32;
    let to = uint_arg(
        args,
        "end_line",
        if args.get("start_line").is_some() {
            from as u64
        } else {
            u32::MAX as u64
        },
    ) as u32;
    let analysis = engine.scan(&scan_root(root, args)?, &options(root, args, 50)?)?;
    let ids = analysis.overlapping(file, from, to);
    if ids.is_empty() {
        return Ok(format!(
            "No clones overlap {file}:{from}-{to} (min_tokens {}).\n",
            uint_arg(args, "min_tokens", 50)
        ));
    }
    Ok(render(&analysis, &ids, true))
}

fn find_similar(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let code =
        str_arg(args, "code").ok_or_else(|| anyhow::anyhow!("missing required argument: code"))?;
    let lang_name = str_arg(args, "language")
        .ok_or_else(|| anyhow::anyhow!("missing required argument: language"))?;
    let lang = lang_index_by_name(lang_name).ok_or_else(|| {
        anyhow::anyhow!("unknown language {lang_name:?}; known: {}", known_langs())
    })?;
    let (analysis, ids) = engine.similar(
        &scan_root(root, args)?,
        code,
        lang,
        &options(root, args, 20)?,
    )?;
    if ids.is_empty() {
        return Ok("No existing code matches this snippet's structure.\n".into());
    }
    Ok(render(&analysis, &ids, false))
}

fn check_new(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let path = scan_root(root, args)?;
    let file = str_arg(args, "baseline").map(|f| root.join(f));
    let against = match (
        str_arg(args, "base"),
        file.or_else(|| Baseline::find(&path)),
    ) {
        (Some(rev), _) => Against::Rev(rev.to_string()),
        (None, Some(file)) => Against::File(file),
        (None, None) => Against::Rev("HEAD".into()),
    };
    let check = engine.check(&path, &against, &options(root, args, 50)?)?;
    Ok(render_check(&check, &against))
}

fn tool_definitions() -> Value {
    let common = json!({
        "path": { "type": "string", "description": "Directory or file to scan, relative to the server root. Default: the root." },
        "min_tokens": { "type": "integer", "minimum": 8, "description": "Smallest clone reported, in syntax tokens. Default 50." },
        "languages": { "type": "array", "items": { "type": "string" }, "description": format!("Restrict to these languages: {}.", known_langs()) },
        "exclude": { "type": "array", "items": { "type": "string" }, "description": "Globs to skip, e.g. \"**/*.test.ts\"." },
        "strict": { "type": "boolean", "description": "Require identical names and literals (copy-paste clones only)." },
    });
    let with = |extra: Value| {
        let mut props = common.clone();
        props
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        props
    };
    json!([
        {
            "name": "find_duplicates",
            "title": "Find duplicated code",
            "description": "List clone groups in a codebase, ranked by how much code extracting a shared helper would remove. Each group lists its locations, the values that differ per occurrence (helper parameters) and names that are merely renamed locals.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "limit": { "type": "integer", "description": "Groups to return. Default 15." },
                "offset": { "type": "integer", "description": "Skip this many groups (pagination)." },
                "include_templates": { "type": "boolean", "description": "Include each group's helper template. Default false." },
            })) },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "get_duplicate_group",
            "title": "Show one clone group",
            "description": "Full detail for one clone group: locations, parameters, and a helper template (the first occurrence with each parameter replaced by $1, $2, ...). Pass the same scan options used to find it.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "id": { "type": "string", "description": "Fingerprint (preferred) or numeric id from find_duplicates." },
                "include_code": { "type": "boolean", "description": "Also return the source of every occurrence." },
            })), "required": ["id"] },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "duplicates_at",
            "title": "Clones at a location",
            "description": "Clone groups with an occurrence overlapping a file or line range: check whether code you are about to change or extract is duplicated elsewhere.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "file": { "type": "string", "description": "File path as shown in results, or relative to the root." },
                "start_line": { "type": "integer" },
                "end_line": { "type": "integer" },
            })), "required": ["file"] },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "find_similar_code",
            "title": "Find code like a snippet",
            "description": "Search the codebase for code structurally equivalent to a snippet (names and literals may differ). Use before writing a helper to find one that already exists. Only fragments of at least min_tokens (default 20 here) are matched, so pass whole statements or functions.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "code": { "type": "string", "description": "The snippet: whole functions or statements." },
                "language": { "type": "string", "description": "Language name or file extension of the snippet." },
            })), "required": ["code", "language"] },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "check_new_duplicates",
            "title": "Check a change for new duplication",
            "description": "Clone groups a change added: the working tree against a git revision (default HEAD, i.e. uncommitted work) or against the repo's committed baseline file when it has one. Call after writing code and before committing, then reuse or extract the existing code it points to.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "base": { "type": "string", "description": "Git revision to compare against, e.g. \"main\" for a whole branch." },
                "baseline": { "type": "string", "description": "Baseline file relative to the root. Default: the nearest .doppelscan-baseline.json in path or a parent, up to the repository root." },
            })) },
            "annotations": { "readOnlyHint": true },
        },
    ])
}
