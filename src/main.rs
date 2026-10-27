use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use doppelscan::baseline::{Baseline, DEFAULT_FILE};
use doppelscan::engine::{Against, Check, Engine, ScanOptions, anchor};
use doppelscan::lang::{LANGS, lang_index_by_name, lang_index_for_path};
use doppelscan::report::{Analysis, Detail, render_check, render_group, render_summary};

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
    /// Fail (exit 1) when there is duplication a baseline does not cover:
    /// the baseline file if one exists, otherwise the code at HEAD.
    Check {
        /// Compare against this git revision instead of a baseline file.
        #[arg(long, conflicts_with = "baseline")]
        base: Option<String>,
        /// Baseline file [default: PATH/.doppelscan-baseline.json].
        #[arg(long)]
        baseline: Option<PathBuf>,
        #[command(flatten)]
        common: Common,
    },
    /// Write a baseline accepting today's duplication, for `check`.
    Baseline {
        /// Only drop entries that no longer match; never accept new
        /// duplication. Uses the settings stored in the baseline.
        #[arg(long)]
        prune: bool,
        /// Baseline file [default: PATH/.doppelscan-baseline.json].
        #[arg(long, short)]
        output: Option<PathBuf>,
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

/// Exit codes: 0 success, 1 new duplication found by `check`, 2 error.
fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let engine = Engine::default();
    let done = match cli.command.unwrap_or(Command::Scan(cli.scan)) {
        Command::Scan(args) => scan(&engine, &args),
        Command::Show { id, common } => show(&engine, &id, &common),
        Command::At { location, common } => at(&engine, &location, &common),
        Command::Similar { file, lang, common } => similar(&engine, file, lang, &common),
        Command::Check {
            base,
            baseline,
            common,
        } => return check(&engine, base, baseline, &common),
        Command::Baseline {
            prune,
            output,
            common,
        } => write_baseline(&engine, prune, output, &common),
        Command::Mcp { root } => doppelscan::mcp::serve(&engine, &root),
    };
    done.map(|()| ExitCode::SUCCESS)
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
        display_base: None,
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
    // Asking for templates means drilling in, so show every occurrence too.
    let detail = if template {
        Detail::Full
    } else {
        Detail::Brief
    };
    let out = if format == Format::Json {
        let body = serde_json::json!({ "summary": analysis.summary(), "groups": groups });
        serde_json::to_string_pretty(&body)? + "\n"
    } else {
        let mut out = render_summary(&analysis.summary(), groups.len());
        groups
            .iter()
            .for_each(|g| out.push_str(&format!("\n{}", render_group(g, detail))));
        out
    };
    emit(&out)
}

/// A closed pipe (`doppelscan | head`) is a normal way to stop reading, not
/// an error worth a panic.
fn emit(text: &str) -> anyhow::Result<()> {
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
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
        true,
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

fn check(
    engine: &Engine,
    base: Option<String>,
    baseline: Option<PathBuf>,
    common: &Common,
) -> anyhow::Result<ExitCode> {
    let against = match (base, baseline.or_else(|| Baseline::find(&common.path))) {
        (Some(rev), _) => Against::Rev(rev),
        (None, Some(file)) => Against::File(file),
        (None, None) => Against::Rev("HEAD".into()),
    };
    let result = engine.check(&common.path, &against, &options(common)?)?;
    print_check(&result, &against, common.format)?;
    Ok(if result.new.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn print_check(result: &Check, against: &Against, format: Format) -> anyhow::Result<()> {
    let groups: Vec<_> = result
        .new
        .iter()
        .map(|&id| result.analysis.group(id, false))
        .collect();
    if format == Format::Json {
        let body = serde_json::json!({
            "against": against.label(),
            "new": groups,
            "resolved": result.resolved,
            "note": result.note,
            "summary": result.analysis.summary(),
        });
        return emit(&(serde_json::to_string_pretty(&body)? + "\n"));
    }
    emit(&render_check(result, against))
}

fn write_baseline(
    engine: &Engine,
    prune: bool,
    output: Option<PathBuf>,
    common: &Common,
) -> anyhow::Result<()> {
    let path = match (output, prune) {
        (Some(path), _) => path,
        (None, true) => Baseline::find(&common.path).context("no baseline to prune")?,
        (None, false) => common.path.join(DEFAULT_FILE),
    };
    let requested = options(common)?;
    let baseline = if prune {
        let old = Baseline::load(&path)?;
        let opts = anchored(old.settings.apply(&requested)?, common, &path)?;
        old.pruned(&engine.scan(&common.path, &opts)?)
    } else {
        let opts = anchored(requested, common, &path)?;
        Baseline::from_analysis(&engine.scan(&common.path, &opts)?, &opts)
    };
    baseline.save(&path)?;
    let _ = writeln!(
        std::io::stderr(),
        "Wrote {} with {} accepted clone groups.",
        path.display(),
        baseline.groups.len()
    );
    Ok(())
}

/// Baseline places are relative to the baseline file's directory.
fn anchored(opts: ScanOptions, common: &Common, file: &Path) -> anyhow::Result<ScanOptions> {
    Ok(ScanOptions {
        display_base: Some(anchor(&common.path, file)?),
        ..opts
    })
}
