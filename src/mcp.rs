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
use crate::config::{Config, Requested, known_langs};
use crate::engine::{Against, Engine, ScanOptions};
use crate::lang::{lang_index_by_name, lang_index_for_path};
use crate::near::{render_near, render_near_summary};
use crate::report::{
    Analysis, Detail, render_accepted, render_check, render_group, render_summary,
};
use crate::settings::{check_scannable, explain};

const MODERN: &str = "2026-07-28";
const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";

const INSTRUCTIONS: &str = "doppelscan finds duplicated code by syntax structure (names and literals may differ). \
Use find_duplicates to survey a repo for helper-extraction candidates, get_duplicate_group for one group's \
helper template and parameters, duplicates_at to check a file/line range you are editing, find_similar_code \
before writing a new helper to see whether equivalent code already exists, check_new_duplicates after \
writing code to see whether it duplicated anything, and find_near_duplicates for functions that are copies \
with small edits. Prefer fingerprints over numeric ids \
when referring back to a group; ids depend on the scan options.";

pub fn serve(engine: &Engine, root: &Path) -> anyhow::Result<()> {
    let root = root.canonicalize()?;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        if stdin.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        if let Some(reply) = handle_bytes(engine, &root, &line) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
}

/// Decoded here rather than by the reader, whose UTF-8 error would end the
/// session over one bad line.
fn handle_bytes(engine: &Engine, root: &Path, line: &[u8]) -> Option<String> {
    match std::str::from_utf8(line) {
        Ok(text) if text.trim().is_empty() => None,
        Ok(text) => handle_line(engine, root, text),
        Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"), None).to_string()),
    }
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
    if !msg.is_object() {
        let message = "invalid request: not a JSON object";
        return Some(error(Value::Null, -32600, message, None).to_string());
    }
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
        "find_near_duplicates" => find_near(engine, root, &args),
        "get_settings" => get_settings(root, &args),
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

/// An explicit null is how some clients spell "unset".
fn present<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| !v.is_null())
}

/// A value that does not fit is an error rather than a default or a
/// truncation, which would answer a different question than the one asked.
fn uint_arg<T: TryFrom<u64>>(args: &Value, key: &str) -> anyhow::Result<Option<T>> {
    let Some(v) = present(args, key) else {
        return Ok(None);
    };
    let n = whole_number(v).and_then(|n| T::try_from(n).ok());
    n.map(Some)
        .ok_or_else(|| anyhow::anyhow!("{key} must be a non-negative integer in range, not {v}"))
}

/// JSON Schema counts a number with a zero fraction as an integer, and
/// clients that hold every number as a double send 5 as 5.0.
fn whole_number(v: &Value) -> Option<u64> {
    const PAST_U64: f64 = 18446744073709551616.0;
    let whole = |n: &f64| n.fract() == 0.0 && (0.0..PAST_U64).contains(n);
    v.as_u64()
        .or_else(|| v.as_f64().filter(whole).map(|n| n as u64))
}

fn number_arg(args: &Value, key: &str) -> anyhow::Result<Option<f32>> {
    let Some(v) = present(args, key) else {
        return Ok(None);
    };
    let n = v.as_f64().map(|n| Some(n as f32));
    n.ok_or_else(|| anyhow::anyhow!("{key} must be a number, not {v}"))
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

fn options(root: &Path, args: &Value) -> anyhow::Result<ScanOptions> {
    with_root(root, args, config(root, args)?.options(requested(args)?)?)
}

fn snippet_options(root: &Path, args: &Value) -> anyhow::Result<ScanOptions> {
    let asked = requested(args)?.for_snippet();
    with_root(root, args, config(root, args)?.options(asked)?)
}

fn with_root(root: &Path, args: &Value, opts: ScanOptions) -> anyhow::Result<ScanOptions> {
    Ok(ScanOptions {
        display_base: Some(display_base(root, &scan_root(root, args)?)?),
        ..opts
    })
}

/// Results read relative to the server root. A scan outside it shares no
/// base with the root, so it reads relative to itself; the working tree and
/// a git revision then show the same paths, and neither shows the verbatim
/// prefix of a canonical Windows path.
fn display_base(root: &Path, scan: &Path) -> anyhow::Result<PathBuf> {
    let (root, scan) = (root.canonicalize()?, scan.canonicalize()?);
    if scan.starts_with(&root) {
        return Ok(root);
    }
    let dir = scan.parent().filter(|_| scan.is_file());
    Ok(dir.unwrap_or(&scan).to_path_buf())
}

fn similarity_arg(args: &Value) -> anyhow::Result<Option<f32>> {
    let similarity = number_arg(args, "similarity")?;
    anyhow::ensure!(
        similarity.is_none_or(|s| (0.0..=1.0).contains(&s)),
        "similarity is a fraction between 0 and 1"
    );
    Ok(similarity)
}

fn requested(args: &Value) -> anyhow::Result<Requested> {
    Ok(Requested {
        min_tokens: uint_arg(args, "min_tokens")?,
        strict: args.get("strict").and_then(Value::as_bool),
        only: str_list(args, "languages"),
        exclude: str_list(args, "exclude"),
        max_param_density: number_arg(args, "max_param_density")?,
        check_min_tokens: uint_arg(args, "check_min_tokens")?,
    })
}

/// The config for the scanned path; a path that does not exist is
/// reported by the scan itself.
fn config(root: &Path, args: &Value) -> anyhow::Result<Config> {
    Config::for_scan(&scan_root(root, args).unwrap_or_else(|_| root.to_path_buf()))
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
    let page = Page::of(args)?;
    let analysis = engine.scan(&scan_root(root, args)?, &options(root, args)?)?;
    let ids = page.ids(analysis.groups.len());
    let templates = args
        .get("include_templates")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let out = render(&analysis, &ids, templates);
    Ok(out + &more_hint(&ids, analysis.groups.len()))
}

struct Page {
    offset: usize,
    limit: usize,
}

impl Page {
    /// Read before scanning, so a bad argument does not cost a scan. A limit
    /// of 0 means the rest.
    fn of(args: &Value) -> anyhow::Result<Self> {
        Ok(Self {
            offset: uint_arg(args, "offset")?.unwrap_or(0),
            limit: match uint_arg(args, "limit")?.unwrap_or(15) {
                0 => usize::MAX,
                n => n,
            },
        })
    }

    /// Ids start at 1, so wrapping here would ask for a group 0 that the
    /// id-to-index subtraction cannot survive.
    fn ids(&self, total: usize) -> Vec<usize> {
        (self.offset.saturating_add(1)..=total)
            .take(self.limit)
            .collect()
    }
}

fn more_hint(ids: &[usize], total: usize) -> String {
    match ids.last() {
        Some(&last) if last < total => format!("\nMore groups: call again with offset={last}.\n"),
        _ => String::new(),
    }
}

fn get_group(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let key =
        str_arg(args, "id").ok_or_else(|| anyhow::anyhow!("missing required argument: id"))?;
    let analysis = engine.scan(&scan_root(root, args)?, &options(root, args)?)?;
    let id = analysis
        .resolve(key)
        .ok_or_else(|| anyhow::anyhow!("{}. {SCAN_HINT}", analysis.unresolved(key)))?;
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

const SCAN_HINT: &str = "Groups depend on the scan: pass the path, min_tokens and languages it was \
found with (YAML needs languages: [\"yaml\"]). A group find_similar_code listed with <snippet> \
in it exists only in that call: read its locations instead.";

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
    let (from, to) = line_range(args)?;
    let opts = options(root, args)?.narrowed_to(lang_index_for_path(file.as_ref()));
    let analysis = engine.scan(&scan_root(root, args)?, &opts)?;
    let ids = analysis.overlapping(file, from, to)?;
    if ids.is_empty() {
        return Ok(format!(
            "No clones overlap {file}:{from}-{to} (min_tokens {}).\n",
            opts.min_tokens
        ));
    }
    Ok(render(&analysis, &ids, true))
}

/// The whole file by default; a start line alone is that one line.
fn line_range(args: &Value) -> anyhow::Result<(u32, u32)> {
    let from = uint_arg(args, "start_line")?;
    let to = uint_arg(args, "end_line")?;
    Ok((from.unwrap_or(1), to.or(from).unwrap_or(u32::MAX)))
}

fn find_similar(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let code =
        str_arg(args, "code").ok_or_else(|| anyhow::anyhow!("missing required argument: code"))?;
    let (lang, opts) = (snippet_lang(args)?, snippet_options(root, args)?);
    let found = engine.similar(&scan_root(root, args)?, code, lang, &opts)?;
    if found.hits.is_empty() && found.accepted.is_empty() {
        return Ok("No existing code matches this snippet's structure.\n".into());
    }
    let out = render(&found.analysis, &found.hits, false);
    Ok(out + &render_accepted(&found.analysis, &found.accepted))
}

fn snippet_lang(args: &Value) -> anyhow::Result<usize> {
    let name = str_arg(args, "language")
        .ok_or_else(|| anyhow::anyhow!("missing required argument: language"))?;
    lang_index_by_name(name)
        .ok_or_else(|| anyhow::anyhow!("unknown language {name:?}; known: {}", known_langs()))
}

/// The same as `doppelscan config --format json`, so an agent sees what a
/// person does.
fn get_settings(root: &Path, args: &Value) -> anyhow::Result<String> {
    let config = config(root, args)?;
    let opts = config.options(requested(args)?)?;
    check_scannable(&scan_root(root, args)?, &opts)?;
    let effective = explain(&config, &requested(args)?, similarity_arg(args)?);
    Ok(serde_json::to_string_pretty(&effective)?)
}

fn find_near(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let page = Page::of(args)?;
    let config = config(root, args)?;
    let similarity = config.similarity(similarity_arg(args)?)?;
    let analysis = engine.near(
        &scan_root(root, args)?,
        &with_root(root, args, config.near_options(requested(args)?)?)?,
        similarity,
    )?;
    let ids = page.ids(analysis.groups.len());
    let groups = ids.iter().map(|&id| analysis.group(id));
    let shown = groups.map(|g| format!("\n{}", render_near(&g, Detail::Full)));
    let mut out = render_near_summary(&analysis, ids.len());
    out.extend(shown);
    Ok(out + &more_hint(&ids, analysis.groups.len()))
}

fn check_new(engine: &Engine, root: &Path, args: &Value) -> anyhow::Result<String> {
    let path = scan_root(root, args)?;
    let against = against(root, &path, args)?;
    let config = Config::for_scan(&against.config_root(&path)?)?;
    let asked = requested(args)?;
    let min_new = config.check_min_tokens(asked.check_min_tokens);
    let opts = with_root(root, args, config.options(asked)?)?;
    let check = engine.check(&path, &against, &opts, min_new)?;
    Ok(render_check(&check, &against))
}

fn against(root: &Path, path: &Path, args: &Value) -> anyhow::Result<Against> {
    let base = str_arg(args, "base");
    let file = str_arg(args, "baseline").map(|f| root.join(f));
    anyhow::ensure!(
        base.is_none() || file.is_none(),
        "pass base or baseline, not both"
    );
    Ok(match (base, file.or_else(|| Baseline::find(path))) {
        (Some(rev), _) => Against::Rev(rev.to_string()),
        (None, Some(file)) => Against::File(file),
        (None, None) => Against::Rev("HEAD".into()),
    })
}

fn tool_definitions() -> Value {
    let common = json!({
        "path": { "type": "string", "description": "Directory or file to scan, relative to the server root or absolute. Results show paths relative to the root, or to this path when it lies outside the root. Default: the root." },
        "min_tokens": { "type": "integer", "minimum": 8, "description": "Smallest clone reported, in syntax tokens. Default 50, or the repo's .doppelscan.toml." },
        "languages": { "type": "array", "items": { "type": "string" }, "description": format!("Restrict to these languages: {}. yaml is scanned only when listed here.", known_langs()) },
        "exclude": { "type": "array", "items": { "type": "string" }, "description": "Globs to skip, e.g. \"**/*.test.ts\"." },
        "strict": { "type": "boolean", "description": "Require identical names and literals (copy-paste clones only)." },
        "max_param_density": { "type": "number", "description": "Drop groups needing more parameters than this per 100 tokens. Default 8, or the repo's .doppelscan.toml." },
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
            "description": "List clone groups in a codebase, ranked by how much code extracting a shared helper would remove. Each group lists its locations, the values that differ per occurrence (helper parameters) and names that are merely renamed locals (in languages read without a grammar, differing names are all listed as `name` parameters).",
            "inputSchema": { "type": "object", "properties": with(json!({
                "limit": { "type": "integer", "description": "Groups to return; 0 for all. Default 15." },
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
            "description": "Search the codebase for code structurally equivalent to a snippet (names and literals may differ). Use before writing a helper to find one that already exists. Only fragments of at least min_tokens (default 20 here) are matched, so pass whole statements or functions. max_param_density does not apply, and a match the repo has accepted is listed as such.",
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
                "check_min_tokens": { "type": "integer", "description": "Only new groups of at least this many tokens fail the check; smaller ones are listed after them. Default: the repo's [check] min-tokens, else min_tokens." },
            })) },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "find_near_duplicates",
            "title": "Find near-duplicate functions",
            "description": "Functions that are mostly the same but not exact clones: a copy with statements added, removed or changed. Each group lists every copy and the line ranges where it differs from the others; read those lines before merging the copies into one function with an option or callback for the difference. Languages read without a grammar (C, shell, SQL, ...) are skipped.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "similarity": { "type": "number", "minimum": 0, "maximum": 1, "description": "Smallest similarity reported. Default 0.8, or the repo's .doppelscan.toml." },
                "limit": { "type": "integer", "description": "Groups to return; 0 for all. Default 15." },
                "offset": { "type": "integer", "description": "Skip this many groups (pagination)." },
            })) },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "get_settings",
            "title": "Show the scan settings",
            "description": "The settings a scan of a path uses (from tool arguments, the repository's .doppelscan.toml files or the defaults) and where each comes from, as JSON, plus the excludes, includes and built-in skip list. Use when results look wrong, before suggesting a config change.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "similarity": { "type": "number", "minimum": 0, "maximum": 1, "description": "A threshold for near, to see it resolved like the others." },
            })) },
            "annotations": { "readOnlyHint": true },
        },
    ])
}
