//! Source files as they were at a git revision, read straight from the
//! object database so nothing is checked out.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use rustc_hash::FxHashSet;

use crate::engine::{ScanOptions, Source};
use crate::walk::{MAX_FILE_BYTES, accept_listed, discover, exclude_overrides, looks_generated};

pub struct Revision {
    pub sources: Vec<Source>,
    /// Submodule paths, relative to the scanned directory. Their files are
    /// not in this repository's history, so a comparison has to leave them
    /// out of the working tree too.
    pub submodules: Vec<String>,
}

struct Listed {
    oid: String,
    path: String,
    submodule: bool,
}

/// Files under `dir` at `rev`, with paths relative to `dir` (prefixed with
/// `display_prefix`). A file that still exists is read only when a
/// working-tree scan would read it, so hidden, ignored and excluded files
/// drop out on both sides alike; a deleted file is judged by its path.
pub fn read_revision(
    dir: &Path,
    rev: &str,
    opts: &ScanOptions,
    display_prefix: &str,
) -> anyhow::Result<Revision> {
    anyhow::ensure!(
        dir.is_dir(),
        "comparing against a revision needs a directory, not {}",
        dir.display()
    );
    let scanned: FxHashSet<String> = discover(dir, &opts.exclude, &opts.languages)?
        .into_iter()
        .filter_map(|(path, _)| relative(dir, &path))
        .collect();
    let overrides = exclude_overrides(dir, &opts.exclude)?;
    let listed = list_tree(dir, rev)?;
    let submodules = listed
        .iter()
        .filter(|l| l.submodule)
        .map(|l| l.path.clone())
        .collect();
    let blobs: Vec<(Listed, usize)> = listed
        .into_iter()
        .filter(|l| !l.submodule)
        .filter_map(|l| {
            let lang = accept_listed(Path::new(&l.path), &overrides, &opts.languages)?;
            let wanted = !dir.join(&l.path).exists() || scanned.contains(&l.path);
            wanted.then_some((l, lang))
        })
        .collect();
    let contents = read_blobs(dir, blobs.iter().map(|(l, _)| l.oid.as_str()))?;
    let sources = blobs
        .into_iter()
        .zip(contents)
        .filter_map(|((l, lang), bytes)| {
            let text = String::from_utf8(bytes?).ok()?;
            (!looks_generated(&text)).then(|| Source {
                path: format!("{display_prefix}{}", l.path),
                lang,
                text,
            })
        })
        .collect();
    Ok(Revision {
        sources,
        submodules,
    })
}

fn relative(dir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(dir).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// The commit a change to `rev` started from: the merge-base with HEAD, so
/// work that landed on `rev` after the branch forked does not count as the
/// branch's own. A missing local branch falls back to `origin/<rev>`, as on
/// a CI checkout.
pub fn fork_point(dir: &Path, rev: &str) -> anyhow::Result<String> {
    let commit = [rev.to_string(), format!("origin/{rev}")]
        .into_iter()
        .find_map(|r| {
            git_line(
                dir,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{r}^{{commit}}"),
                ],
            )
        })
        .with_context(|| format!("unknown git revision {rev:?}"))?;
    Ok(git_line(dir, &["merge-base", &commit, "HEAD"]).unwrap_or(commit))
}

fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    let line = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !line.is_empty()).then_some(line)
}

/// Regular files and submodules at `rev`, relative to `dir`.
fn list_tree(dir: &Path, rev: &str) -> anyhow::Result<Vec<Listed>> {
    let out = Command::new("git")
        .args(["ls-tree", "-r", "-z", rev, "--", "."])
        .current_dir(dir)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git ls-tree {rev}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let listing = String::from_utf8(out.stdout).context("git ls-tree output")?;
    Ok(listing.split('\0').filter_map(parse_entry).collect())
}

fn parse_entry(line: &str) -> Option<Listed> {
    let (meta, path) = line.split_once('\t')?;
    let mut parts = meta.split(' ');
    let (mode, kind, oid) = (parts.next()?, parts.next()?, parts.next()?);
    // Symlinks (120000) point at text that is not code.
    let keep = kind == "commit" || (kind == "blob" && mode != "120000");
    keep.then(|| Listed {
        oid: oid.to_string(),
        path: path.to_string(),
        submodule: kind == "commit",
    })
}

/// Contents in request order; `None` for blobs too large to scan.
fn read_blobs<'a>(
    dir: &Path,
    oids: impl Iterator<Item = &'a str>,
) -> anyhow::Result<Vec<Option<Vec<u8>>>> {
    let request: String = oids.map(|oid| format!("{oid}\n")).collect();
    let count = request.lines().count();
    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("running git cat-file")?;
    let mut stdin = child.stdin.take().context("git cat-file stdin")?;
    // Fed from another thread: git stops reading requests while its output
    // pipe is full, so writing everything first would deadlock.
    let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
    let stdout = child.stdout.take().context("git cat-file stdout")?;
    let mut reader = BufReader::new(stdout);
    let blobs = (0..count)
        .map(|_| read_one(&mut reader))
        .collect::<anyhow::Result<Vec<_>>>();
    // Closing the pipe first lets git exit even when reading failed midway,
    // so the writer and the child are always reaped.
    drop(reader);
    let written = writer.join();
    child.wait()?;
    let blobs = blobs?;
    written.map_err(|_| anyhow::anyhow!("git cat-file writer panicked"))??;
    Ok(blobs)
}

fn read_one(reader: &mut impl BufRead) -> anyhow::Result<Option<Vec<u8>>> {
    let mut header = String::new();
    reader.read_line(&mut header)?;
    let size: u64 = header
        .trim_end()
        .rsplit(' ')
        .next()
        .and_then(|s| s.parse().ok())
        .with_context(|| format!("unexpected git cat-file header {header:?}"))?;
    let mut body = vec![0; size as usize + 1];
    reader.read_exact(&mut body)?;
    body.pop();
    Ok((size <= MAX_FILE_BYTES).then_some(body))
}
