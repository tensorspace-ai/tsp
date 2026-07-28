//! `ds` — data version control backed by Git LFS.

mod repo;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use repo::{Repo, State, pointers_of, short};

#[derive(Parser)]
#[command(
    name = "ds",
    version,
    about = "Version datasets in git, with Git LFS as the object store"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare the repository for `ds`
    Init,
    /// Start tracking files or directories as data
    Track {
        /// Files or directories to track
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Show what is tracked and whether it is present
    Status,
    /// Switch branches, moving tracked data out of git's way
    Checkout {
        /// Branch, tag or commit to switch to
        rev: String,
    },
    /// Upload tracked objects to the remote's LFS store
    Push {
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// Download tracked objects and restore them into the working tree
    Pull {
        #[arg(long, default_value = "origin")]
        remote: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;

    match cli.command {
        Command::Init => init(&cwd),
        Command::Track { paths } => track(&cwd, &paths),
        Command::Status => status(&cwd),
        Command::Checkout { rev } => checkout(&cwd, &rev),
        Command::Push { remote } => block_on(push(cwd, remote)),
        Command::Pull { remote } => block_on(pull(cwd, remote)),
    }
}

/// Transfers are the only async work, so the runtime is built on demand.
fn block_on<F: std::future::Future<Output = Result<()>>>(fut: F) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(fut)
}

fn init(cwd: &std::path::Path) -> Result<()> {
    let repo = Repo::open(cwd)?;
    std::fs::create_dir_all(repo.cache().root())?;
    install_hook(&repo, "pre-commit", "ds-guard", GUARD_HOOK)?;
    install_hook(&repo, "pre-push", "ds-push", PUSH_HOOK)?;

    println!("Initialized ds in {}", repo.git().work_tree().display());
    println!("  cache: {}", repo.cache().root().display());
    println!("\nTrack data with `ds track <path>`, then `git commit` and `git push`.");
    Ok(())
}

/// Installs one of our hooks, refusing to clobber a hook someone else owns.
///
/// `marker` is the string our own script carries, so re-running `ds init` over
/// a hook we wrote is silent while a hook the user wrote is left alone.
fn install_hook(repo: &Repo, name: &str, marker: &str, body: &str) -> Result<()> {
    let hooks = repo.git().git_dir().join("hooks");
    std::fs::create_dir_all(&hooks)?;
    let path = hooks.join(name);

    if path.exists() {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.contains(marker) {
            eprintln!(
                "warning: {} already exists and was left alone; \
                 add `{marker}` to it by hand to keep ds working",
                path.display()
            );
        }
        return Ok(());
    }

    std::fs::write(&path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

const GUARD_HOOK: &str = r#"#!/bin/sh
# ds-guard: refuse to commit a large blob that is not an LFS pointer.
# ds tracks data by writing pointer blobs directly, with no clean filter, so
# nothing else would stop an accidental `git add` of a multi-gigabyte file.
limit=1048576
fail=0
git diff --cached --name-only --diff-filter=ACM | while IFS= read -r path; do
    sha=$(git ls-files --stage -- "$path" | awk '{print $2}')
    [ -n "$sha" ] || continue
    size=$(git cat-file -s "$sha" 2>/dev/null) || continue
    [ "$size" -le "$limit" ] && continue
    if ! git cat-file -p "$sha" 2>/dev/null | head -n 1 |
        grep -q '^version https://git-lfs.github.com/spec/v1$'; then
        echo "ds: refusing to commit $path ($size bytes, not an LFS pointer)" >&2
        echo "ds: track it with 'ds track $path', or bypass with --no-verify" >&2
        exit 1
    fi
done || fail=1
exit $fail
"#;

/// Uploads objects as part of `git push`, which is how git-lfs behaves.
///
/// git-lfs is transparent through two independent mechanisms: clean/smudge
/// filters, which `ds` deliberately does without, and this hook, which it has
/// no reason to. Pushing the pointers without the bytes they name is what
/// leaves a remote holding references to data nobody can fetch.
///
/// git passes the remote name as $1, and `ds push` reports its own errors, so a
/// failure here fails the push — the same contract git-lfs's hook has.
const PUSH_HOOK: &str = r#"#!/bin/sh
# ds-push: upload tracked data before the pointers referring to it are pushed.
# Bypass with `git push --no-verify` if you mean to push pointers alone.
command -v ds >/dev/null 2>&1 || {
    echo "ds: not on PATH; skipping data upload" >&2
    exit 0
}
exec ds push --remote "${1:-origin}"
"#;

fn track(cwd: &std::path::Path, paths: &[PathBuf]) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let files = repo.expand(paths)?;

    let mut tracked = 0usize;
    let mut bytes = 0u64;
    let mut skipped = 0usize;

    for file in &files {
        match repo.track_file(file)? {
            Some(p) => {
                tracked += 1;
                bytes += p.size;
                println!("  {} {}", short(&p.oid), file.display());
            }
            None => skipped += 1,
        }
    }

    println!("Tracked {tracked} file(s), {}", human_bytes(bytes));
    if skipped > 0 {
        println!("{skipped} empty file(s) staged as-is (git-lfs does not point at empty files)");
    }
    println!("Next: `git commit` to record the pointers; `git push` uploads the data.");
    Ok(())
}

fn status(cwd: &std::path::Path) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let tracked = repo.tracked()?;

    if tracked.is_empty() {
        println!("No tracked data. Use `ds track <path>` to start.");
        return Ok(());
    }

    let (mut current, mut modified, mut cached, mut missing, mut not_local) = (0, 0, 0, 0, 0);
    for t in &tracked {
        let state = repo.state_of(t);
        match state {
            State::Current => current += 1,
            State::NotMaterialized => not_local += 1,
            State::Modified => modified += 1,
            State::Cached => cached += 1,
            State::Missing => missing += 1,
        }
        if state != State::Current {
            println!("  {:<9} {}", label(state), t.path.display());
        }
    }

    let total: u64 = tracked.iter().map(|t| t.pointer.size).sum();
    println!(
        "{} tracked file(s), {} — {current} current, {not_local} not-local, \
         {modified} modified, {cached} cached, {missing} missing",
        tracked.len(),
        human_bytes(total)
    );
    if modified > 0 {
        println!("Re-track modified files with `ds track <path>`.");
    }
    if not_local + cached + missing > 0 {
        println!("Restore absent files with `ds pull`.");
    }
    Ok(())
}

/// Switches branches, which plain `git checkout` cannot do here.
///
/// A tracked path is `skip-worktree`, which git reads as "this file is not in
/// the working tree" — the sparse-checkout contract. `ds` breaks that half of
/// the deal on purpose: the file *is* there, holding the data rather than the
/// pointer. So the moment a tracked file differs between two commits, git
/// refuses to switch and `-f` does not help either:
///
/// ```text
/// error: Entry 'models/model.joblib' not uptodate. Cannot merge.
/// ```
///
/// The way out is to take the data out of git's way, let git do the switch
/// against what it thinks is an empty slot, and put the data back afterwards.
/// Nothing is downloaded: the objects are content-addressed in the cache.
fn checkout(cwd: &std::path::Path, rev: &str) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let before = repo.tracked()?;

    let mut moved_aside = Vec::new();
    for t in &before {
        match repo.state_of(t) {
            // Removing this would destroy the only copy of the data.
            State::Modified => anyhow::bail!(
                "{} has changes that are not tracked yet; \
                 run `ds track {}` or restore it before switching",
                t.path.display(),
                t.path.display()
            ),
            State::Current => {
                // Current means the bytes hash to the pointer, so caching them
                // is a verified no-op when the object is already there — and
                // the difference between safe and lossy when it is not.
                if !repo.cache().contains(&t.pointer.oid) {
                    repo.cache()
                        .insert_file(repo.git().work_tree().join(&t.path))
                        .with_context(|| format!("caching {}", t.path.display()))?;
                }
                moved_aside.push(t.clone());
            }
            // Pointer text, which git can write again from the index.
            State::NotMaterialized => moved_aside.push(t.clone()),
            // Already absent from the working tree; nothing is in git's way.
            State::Cached | State::Missing => {}
        }
    }

    for t in &moved_aside {
        let abs = repo.git().work_tree().join(&t.path);
        std::fs::remove_file(&abs)
            .with_context(|| format!("clearing {} before checkout", t.path.display()))?;
    }

    if let Err(err) = repo.git().checkout(rev) {
        // Leave the working tree as it was found rather than stripped of data.
        for t in &moved_aside {
            let _ = repo.materialize(t);
        }
        return Err(err).with_context(|| format!("switching to {rev}"));
    }

    let (mut restored, mut absent) = (0usize, 0usize);
    for t in &repo.tracked()? {
        if !repo.cache().contains(&t.pointer.oid) {
            absent += 1;
            continue;
        }
        if repo.state_of(t) != State::Current {
            repo.materialize(t)?;
            restored += 1;
        }
    }

    println!("Switched to {rev}; restored {restored} file(s) from the cache.");
    if absent > 0 {
        println!("{absent} file(s) are not cached locally — run `ds pull` to fetch them.");
    }
    Ok(())
}

fn label(state: State) -> &'static str {
    match state {
        State::Current => "current",
        State::NotMaterialized => "not-local",
        State::Modified => "modified",
        State::Cached => "cached",
        State::Missing => "missing",
    }
}

async fn push(cwd: PathBuf, remote: String) -> Result<()> {
    let repo = Repo::open(&cwd)?;
    let tracked = repo.tracked()?;
    if tracked.is_empty() {
        println!("Nothing to push.");
        return Ok(());
    }

    // Which objects need bytes is the server's answer, not ours: it is asked in
    // the batch request, and everything it already holds needs nothing local.
    let client = repo.client(&remote)?;
    let summary = client
        .upload(repo.cache(), &pointers_of(&tracked))
        .await
        .context("uploading objects")?;

    println!(
        "Pushed {} object(s); {} already on the server.",
        summary.transferred, summary.already_present
    );
    Ok(())
}

async fn pull(cwd: PathBuf, remote: String) -> Result<()> {
    let repo = Repo::open(&cwd)?;
    let tracked = repo.tracked()?;
    if tracked.is_empty() {
        println!("Nothing to pull.");
        return Ok(());
    }

    let client = repo.client(&remote)?;
    let summary = client
        .download(repo.cache(), &pointers_of(&tracked))
        .await
        .context("downloading objects")?;

    let mut restored = 0usize;
    for t in &tracked {
        if repo.state_of(t) != State::Current {
            repo.materialize(t)?;
            restored += 1;
        }
    }

    println!(
        "Fetched {} object(s); {} already cached. Restored {restored} file(s).",
        summary.transferred, summary.already_present
    );
    Ok(())
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(7_600_000_000), "7.1 GiB");
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
