use std::io::Read as _;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use doppelscan::engine::{Engine, ScanOptions};
use doppelscan::lang::{LANGS, lang_index_by_name, lang_index_for_path};
use doppelscan::report::{Analysis, render_group, render_summary};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Find duplicated code by syntax structure, with the parameters an
/// extracted helper would need.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    scan: ScanArgs,
}

#[derive(Subcommand)]
enum Command {
    /// List clone groups, best extraction candidates first (the default).
    Scan(ScanArgs),
    /// Show one group in full: locations, parameters, helper template.
    Show {
        /// Group id from `scan`, or its fingerprint.
        id: String,
        #[command(flatten)]
        common: Common,
    },
    /// Clone groups touching FILE[:LINE[-LINE]].
    At {
        location: String,
        #[command(flatten)]
        common: Common,
    },
    /// Clone groups containing a snippet read from --file or stdin.
    Similar {
        #[arg(long)]
        file: Option<PathBuf>,
        /// Snippet language (name or extension); defaults from --file.
        #[arg(long)]
        lang: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Run the MCP server on stdio.
    Mcp {
        /// Default root for tool calls that do not pass one.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
}

#[derive(Args, Clone)]
struct Common {
    /// Directory or file to scan.
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Smallest clone reported, in syntax tokens.
    #[arg(long, default_value_t = 50)]
    min_tokens: u32,
    /// Match identifiers and literals exactly (copy-paste clones only).
    #[arg(long)]
    strict: bool,
    /// Only these languages (comma-separated names or extensions).
    #[arg(long = "only", value_delimiter = ',')]
    only: Vec<String>,
    /// Glob to skip; repeatable.
    #[arg(long)]
    exclude: Vec<String>,
    /// Drop groups needing more parameters than this per 100 tokens.
    #[arg(long, default_value_t = 8.0)]
    max_param_density: f32,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

#[derive(Args, Clone)]
struct ScanArgs {
    #[command(flatten)]
    common: Common,
    /// Groups to show; 0 for all.
    #[arg(long, default_value_t = 20)]
    limit: usize,
    /// Include each group's helper template.
    #[arg(long)]
    template: bool,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum Format {
    Text,
    Json,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let engine = Engine::default();
    match cli.command.unwrap_or(Command::Scan(cli.scan)) {
        Command::Scan(args) => scan(&engine, &args),
        Command::Show { id, common } => show(&engine, &id, &common),
        Command::At { location, common } => at(&engine, &location, &common),
        Command::Similar { file, lang, common } => similar(&engine, file, lang, &common),
        Command::Mcp { root } => doppelscan::mcp::serve(&engine, &root),
    }
}

fn options(c: &Common) -> anyhow::Result<ScanOptions> {
    let languages = c
        .only
        .iter()
        .map(|l| {
            lang_index_by_name(l)
                .with_context(|| format!("unknown language {l:?}; known: {}", known_langs()))
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(ScanOptions {
        min_tokens: c.min_tokens,
        strict: c.strict,
        languages,
        exclude: c.exclude.clone(),
        max_param_density: c.max_param_density,
    })
}

fn known_langs() -> String {
    LANGS.iter().map(|l| l.name).collect::<Vec<_>>().join(", ")
}

fn scan(engine: &Engine, args: &ScanArgs) -> anyhow::Result<()> {
    let analysis = engine.scan(&args.common.path, &options(&args.common)?)?;
    let limit = if args.limit == 0 {
        analysis.groups.len()
    } else {
        args.limit.min(analysis.groups.len())
    };
    print_groups(
        &analysis,
        &(1..=limit).collect::<Vec<_>>(),
        args.template,
        args.common.format,
    )
}

fn print_groups(
    analysis: &Analysis,
    ids: &[usize],
    template: bool,
    format: Format,
) -> anyhow::Result<()> {
    let groups: Vec<_> = ids.iter().map(|&id| analysis.group(id, template)).collect();
    if format == Format::Json {
        let body = serde_json::json!({ "summary": analysis.summary(), "groups": groups });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print!("{}", render_summary(&analysis.summary(), groups.len()));
    for g in &groups {
        print!("\n{}", render_group(g));
    }
    Ok(())
}

fn show(engine: &Engine, id: &str, common: &Common) -> anyhow::Result<()> {
    let analysis = engine.scan(&common.path, &options(common)?)?;
    let Some(id) = analysis.resolve(id) else {
        bail!(
            "no clone group {id:?} (ids run 1..={})",
            analysis.groups.len()
        )
    };
    print_groups(&analysis, &[id], true, common.format)
}

fn at(engine: &Engine, location: &str, common: &Common) -> anyhow::Result<()> {
    let (file, from, to) = parse_location(location)?;
    let analysis = engine.scan(&common.path, &options(common)?)?;
    print_groups(
        &analysis,
        &analysis.overlapping(&file, from, to),
        false,
        common.format,
    )
}

/// "path", "path:12" or "path:12-30". A Windows drive colon is not a line
/// separator because what follows it is not a digit.
fn parse_location(s: &str) -> anyhow::Result<(String, u32, u32)> {
    let Some((file, lines)) = s
        .rsplit_once(':')
        .filter(|(_, l)| l.starts_with(|c: char| c.is_ascii_digit()))
    else {
        return Ok((s.to_string(), 1, u32::MAX));
    };
    let (from, to) = lines.split_once('-').unwrap_or((lines, lines));
    Ok((file.to_string(), from.parse()?, to.parse()?))
}

fn similar(
    engine: &Engine,
    file: Option<PathBuf>,
    lang: Option<String>,
    common: &Common,
) -> anyhow::Result<()> {
    let code = match &file {
        Some(f) => std::fs::read_to_string(f)?,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
    };
    let lang = match (&lang, &file) {
        (Some(l), _) => lang_index_by_name(l),
        (None, Some(f)) => lang_index_for_path(f),
        (None, None) => None,
    }
    .with_context(|| format!("pass --lang (one of: {})", known_langs()))?;
    let (analysis, hits) = engine.similar(&common.path, &code, lang, &options(common)?)?;
    print_groups(&analysis, &hits, false, common.format)
}
