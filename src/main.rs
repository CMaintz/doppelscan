use std::io::{BufWriter, ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use doppelscan::baseline::{Baseline, DEFAULT_FILE};
use doppelscan::config::{Config, Requested, Source, known_langs};
use doppelscan::engine::{Against, Check, Engine, ScanOptions, anchor};
use doppelscan::lang::{lang_index_by_name, lang_index_for_path};
use doppelscan::near::{render_near, render_near_summary};
use doppelscan::report::{Analysis, Detail, render_check, render_group, render_summary};
use doppelscan::settings::{explain, init_template, render as render_settings};
use serde::Serialize;
use serde::ser::{Error as _, SerializeSeq as _, Serializer};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Find duplicated code by syntax structure (or tokens, for languages
/// without a bundled grammar), with the parameters an extracted helper
/// would need.
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
        /// Only new groups of at least this many tokens fail the check;
        /// smaller ones are still reported [default: --min-tokens].
        #[arg(long)]
        check_min_tokens: Option<u32>,
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
    /// Near-miss clones: functions alike except for added, removed or
    /// changed statements, which the exact scan cannot align. Languages
    /// read without a grammar are skipped.
    Near {
        /// Smallest similarity reported, 0 to 1 [default: 0.8].
        #[arg(long)]
        similarity: Option<f32>,
        /// Groups to show; 0 for all.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[command(flatten)]
        common: Common,
    },
    /// The settings a scan of PATH uses and where each comes from: a flag,
    /// a config file or the default.
    Config {
        /// Print the config file's JSON schema instead.
        #[arg(long)]
        schema: bool,
        /// Threshold for `near`, to see it resolved like the others.
        #[arg(long)]
        similarity: Option<f32>,
        #[command(flatten)]
        common: Common,
    },
    /// Write a commented .doppelscan.toml for PATH: every setting with its
    /// default, the languages found and the test files `near` should skip.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Replace an existing config.
        #[arg(long)]
        force: bool,
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
    /// Smallest clone reported, in syntax tokens [default: 50].
    #[arg(long)]
    min_tokens: Option<u32>,
    /// Match identifiers and literals exactly (copy-paste clones only).
    #[arg(long)]
    strict: bool,
    /// Match names and literals loosely even where a config sets strict.
    #[arg(long, conflicts_with = "strict")]
    no_strict: bool,
    /// Read settings from this file instead of looking for .doppelscan.toml.
    #[arg(long, conflicts_with = "no_config")]
    config: Option<PathBuf>,
    /// Ignore any .doppelscan.toml.
    #[arg(long)]
    no_config: bool,
    /// Only these languages (comma-separated names or extensions); yaml is
    /// scanned only when named here.
    #[arg(long = "only", value_delimiter = ',')]
    only: Vec<String>,
    /// Glob to skip; repeatable.
    #[arg(long)]
    exclude: Vec<String>,
    /// Drop groups needing more parameters than this per 100 tokens (in
    /// token-level languages, names differing in several places are not
    /// counted) [default: 8].
    #[arg(long)]
    max_param_density: Option<f32>,
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
            check_min_tokens,
            common,
        } => return check(&engine, base, baseline, check_min_tokens, &common),
        Command::Baseline {
            prune,
            output,
            common,
        } => write_baseline(&engine, prune, output, &common),
        Command::Near {
            similarity,
            limit,
            common,
        } => near(&engine, similarity, limit, &common),
        Command::Config {
            schema,
            similarity,
            common,
        } => show_config(schema, similarity, &common),
        Command::Init { path, force } => init(&path, force),
        Command::Mcp { root } => doppelscan::mcp::serve(&engine, &root),
    };
    done.map(|()| ExitCode::SUCCESS)
}

fn options(c: &Common) -> anyhow::Result<ScanOptions> {
    config(c)?.options(requested(c))
}

fn show_config(schema: bool, similarity: Option<f32>, c: &Common) -> anyhow::Result<()> {
    if schema {
        return emit(&doppelscan::config::schema());
    }
    let effective = explain(&config(c)?, &requested(c), similarity);
    match c.format {
        Format::Json => emit_json(&effective),
        Format::Text => emit(&render_settings(&effective)),
    }
}

fn init(path: &Path, force: bool) -> anyhow::Result<()> {
    let file = path.join(doppelscan::config::FILE);
    anyhow::ensure!(
        force || !file.exists(),
        "{} exists; pass --force to replace it",
        file.display()
    );
    std::fs::write(&file, init_template(path)?)
        .with_context(|| format!("writing {}", file.display()))?;
    let _ = writeln!(std::io::stderr(), "Wrote {}.", file.display());
    Ok(())
}

fn config(c: &Common) -> anyhow::Result<Config> {
    Config::read(match (&c.config, c.no_config) {
        (Some(file), _) => Source::File(file),
        (None, true) => Source::None,
        (None, false) => Source::Lookup(&c.path),
    })
}

fn requested(c: &Common) -> Requested {
    Requested {
        min_tokens: c.min_tokens,
        strict: (c.strict || c.no_strict).then_some(c.strict),
        only: c.only.clone(),
        exclude: c.exclude.clone(),
        max_param_density: c.max_param_density,
        check_min_tokens: None,
    }
}

fn scan(engine: &Engine, args: &ScanArgs) -> anyhow::Result<()> {
    let analysis = engine.scan(&args.common.path, &options(&args.common)?)?;
    let ids = first_ids(args.limit, analysis.groups.len());
    print_groups(&analysis, &ids, args.template, args.common.format)
}

/// Ids `1..=limit`, with 0 meaning all `total`.
fn first_ids(limit: usize, total: usize) -> Vec<usize> {
    let shown = if limit == 0 { total } else { limit.min(total) };
    (1..=shown).collect()
}

fn print_groups(
    analysis: &Analysis,
    ids: &[usize],
    template: bool,
    format: Format,
) -> anyhow::Result<()> {
    if format == Format::Json {
        return emit_json(&Report {
            groups: Each(|| ids.iter().map(|&id| analysis.group(id, template))),
            summary: Sorted(analysis.summary()),
        });
    }
    let groups: Vec<_> = ids.iter().map(|&id| analysis.group(id, template)).collect();
    // Asking for templates means drilling in, so show every occurrence too.
    let detail = if template {
        Detail::Full
    } else {
        Detail::Brief
    };
    let mut out = render_summary(&analysis.summary(), groups.len());
    groups
        .iter()
        .for_each(|g| out.push_str(&format!("\n{}", render_group(g, detail))));
    emit(&out)
}

/// A report with sorted keys, built one group at a time: a `Value` tree of
/// every group would take several times the size of the output on a large
/// repo.
#[derive(Serialize)]
struct Report<G, S> {
    groups: G,
    summary: Sorted<S>,
}

/// Serialized through a `Value`, whose maps sort their keys.
struct Sorted<T>(T);

impl<T: Serialize> Serialize for Sorted<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serde_json::to_value(&self.0)
            .map_err(S::Error::custom)?
            .serialize(s)
    }
}

/// The items a closure yields, each made only when it is written.
struct Each<F>(F);

impl<F, I> Serialize for Each<F>
where
    F: Fn() -> I,
    I: Iterator<Item: Serialize>,
{
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(None)?;
        for item in (self.0)() {
            seq.serialize_element(&Sorted(item))?;
        }
        seq.end()
    }
}

fn emit_json(body: &impl Serialize) -> anyhow::Result<()> {
    let mut out = BufWriter::new(std::io::stdout().lock());
    let written = serde_json::to_writer_pretty(&mut out, body)
        .map_err(std::io::Error::from)
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush());
    ignore_closed_pipe(written)
}

fn emit(text: &str) -> anyhow::Result<()> {
    ignore_closed_pipe(std::io::stdout().lock().write_all(text.as_bytes()))
}

/// A closed pipe (`doppelscan | head`) is a normal way to stop reading, not
/// an error worth a panic.
fn ignore_closed_pipe(written: std::io::Result<()>) -> anyhow::Result<()> {
    match written {
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
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
    let opts = options(common)?.narrowed_to(lang_index_for_path(file.as_ref()));
    let analysis = engine.scan(&common.path, &opts)?;
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
    check_min_tokens: Option<u32>,
    common: &Common,
) -> anyhow::Result<ExitCode> {
    let against = match (base, baseline.or_else(|| Baseline::find(&common.path))) {
        (Some(rev), _) => Against::Rev(rev),
        (None, Some(file)) => Against::File(file),
        (None, None) => Against::Rev("HEAD".into()),
    };
    let min_new = config(common)?.check_min_tokens(check_min_tokens);
    let result = engine.check(&common.path, &against, &options(common)?, min_new)?;
    print_check(&result, &against, common.format)?;
    Ok(if result.new.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn print_check(result: &Check, against: &Against, format: Format) -> anyhow::Result<()> {
    if format == Format::Json {
        return emit_json(&CheckReport {
            against: against.label(),
            new: Each(|| {
                result
                    .new
                    .iter()
                    .map(|&id| result.analysis.group(id, false))
            }),
            note: &result.note,
            resolved: result.resolved,
            smaller: Each(|| {
                result
                    .smaller
                    .iter()
                    .map(|&id| result.analysis.group(id, false))
            }),
            summary: Sorted(result.analysis.summary()),
        });
    }
    emit(&render_check(result, against))
}

/// `check`'s report, keys in sorted order like `Report`: against a large
/// diff, its new groups can be as many as a full scan's.
#[derive(Serialize)]
struct CheckReport<'a, G, S> {
    against: String,
    new: G,
    note: &'a Option<String>,
    resolved: usize,
    /// New groups under the check's own minimum, which do not fail it.
    smaller: S,
    summary: Sorted<doppelscan::report::Summary>,
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
        old.settings.require_same(&requested, &path)?;
        let opts = anchored(requested, common, &path)?;
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

fn near(
    engine: &Engine,
    similarity: Option<f32>,
    limit: usize,
    common: &Common,
) -> anyhow::Result<()> {
    let config = config(common)?;
    let similarity = config.similarity(similarity)?;
    let opts = config.near_options(requested(common))?;
    let analysis = engine.near(&common.path, &opts, similarity)?;
    let ids = first_ids(limit, analysis.groups.len());
    if common.format == Format::Json {
        return emit_json(&Report {
            groups: Each(|| ids.iter().map(|&id| analysis.group(id))),
            summary: Sorted(analysis.summary()),
        });
    }
    let groups: Vec<_> = ids.into_iter().map(|id| analysis.group(id)).collect();
    let mut out = render_near_summary(&analysis, groups.len());
    for g in &groups {
        out.push_str(&format!("\n{}", render_near(g, Detail::Brief)));
    }
    emit(&out)
}
