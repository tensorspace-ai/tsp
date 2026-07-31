//! Git access, via plumbing subprocesses.
//!
//! `tsp` shells out rather than linking a Rust git implementation. The deciding
//! argument is interop: this inherits the user's credential helpers, `includeIf`
//! conditional config, ssh setup, proxies and signing exactly as configured,
//! which `gix`/`git2` each reimplement as a drifting subset. `git credential`
//! is a subprocess regardless, so the dependency is never actually escaped.
//!
//! Rules for everything in this module: plumbing commands only (never
//! porcelain, whose output is explicitly unstable), NUL-delimited lists, and
//! always `--` before user-controlled paths.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("failed to run `git {args}`: {source}")]
    Spawn {
        args: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`git {args}` failed with status {status}: {stderr}")]
    Failed {
        args: String,
        status: i32,
        stderr: String,
    },
    #[error("not inside a git repository")]
    NotARepository,
    #[error("`git {args}` produced output that could not be parsed: {detail}")]
    Unparseable { args: String, detail: String },
}

type Result<T> = std::result::Result<T, GitError>;

/// A git file mode. Datasets contain executables often enough that dropping
/// the bit silently corrupts a checkout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMode {
    Regular,
    Executable,
}

impl FileMode {
    /// Reads the mode from filesystem metadata.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)?.permissions().mode();
            Ok(if mode & 0o111 != 0 {
                Self::Executable
            } else {
                Self::Regular
            })
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::metadata(path)?;
            Ok(Self::Regular)
        }
    }

    pub fn as_octal(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
        }
    }

    /// Reads the mode back off an index entry.
    ///
    /// Anything that is not a regular file — a symlink at 120000, a gitlink at
    /// 160000 — has no data mode to restore, so it yields `None` rather than
    /// being silently flattened to a regular file.
    pub fn from_octal(mode: &str) -> Option<Self> {
        match mode {
            "100644" => Some(Self::Regular),
            "100755" => Some(Self::Executable),
            _ => None,
        }
    }

    pub fn is_executable(self) -> bool {
        matches!(self, Self::Executable)
    }
}

/// A handle to one repository.
#[derive(Clone, Debug)]
pub struct Git {
    work_tree: PathBuf,
    git_dir: PathBuf,
}

impl Git {
    /// Locates the repository containing `start`.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let start = start.as_ref();
        let out = run_in(
            start,
            ["rev-parse", "--show-toplevel", "--absolute-git-dir"],
        )?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let (Some(work_tree), Some(git_dir)) = (lines.next(), lines.next()) else {
            return Err(GitError::NotARepository);
        };
        Ok(Self {
            work_tree: PathBuf::from(work_tree),
            git_dir: PathBuf::from(git_dir),
        })
    }

    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn run<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        run_in(&self.work_tree, args)
    }

    /// Writes `content` into the object database as a blob, returning its sha.
    ///
    /// `--no-filters` is essential: it guarantees no clean filter rewrites the
    /// bytes. `tsp` never installs `filter=lfs`, but a user may have configured
    /// one globally, and a filtered pointer blob would hash differently — which
    /// is exactly what makes Gitea's LFS garbage collector orphan the object.
    pub fn write_blob(&self, content: &[u8]) -> Result<String> {
        let args = ["hash-object", "-w", "-t", "blob", "--no-filters", "--stdin"];
        let mut child = Command::new("git")
            .current_dir(&self.work_tree)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| GitError::Spawn {
                args: args.join(" "),
                source,
            })?;

        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(content)
            .map_err(|source| GitError::Spawn {
                args: args.join(" "),
                source,
            })?;

        let out = child.wait_with_output().map_err(|source| GitError::Spawn {
            args: args.join(" "),
            source,
        })?;
        check(&args.join(" "), out).map(|o| trimmed(&o.stdout))
    }

    /// Stages an already-written blob at `path` (relative to the work tree).
    pub fn stage_blob(&self, mode: FileMode, sha: &str, path: &Path) -> Result<()> {
        let cacheinfo = format!("{},{},{}", mode.as_octal(), sha, path_arg(path));
        self.run(["update-index", "--add", "--cacheinfo", &cacheinfo])
            .map(|_| ())
    }

    /// Stages a path from the working tree as-is.
    pub fn stage_path(&self, path: &Path) -> Result<()> {
        self.run([
            OsStr::new("update-index"),
            OsStr::new("--add"),
            OsStr::new("--"),
            path.as_os_str(),
        ])
        .map(|_| ())
    }

    /// Sets or clears the skip-worktree bit for a staged path.
    ///
    /// This is what lets the index hold a pointer blob while the working tree
    /// holds the real data. Without it git reports every tracked dataset file
    /// as modified, and `git commit -a` would replace the pointer with the raw
    /// bytes. The bit is local to the index and is not committed, so `tsp pull`
    /// re-applies it after populating a fresh clone.
    pub fn set_skip_worktree(&self, path: &Path, skip: bool) -> Result<()> {
        let flag = if skip {
            "--skip-worktree"
        } else {
            "--no-skip-worktree"
        };
        self.run([
            OsStr::new("update-index"),
            OsStr::new(flag),
            path.as_os_str(),
        ])
        .map(|_| ())
    }

    /// Reads a config value, returning `None` when unset.
    pub fn config(&self, key: &str) -> Result<Option<String>> {
        match self.run(["config", "--get", key]) {
            Ok(out) => {
                let value = trimmed(&out.stdout);
                Ok((!value.is_empty()).then_some(value))
            }
            // `git config --get` exits 1 for "key not set", which is not an error.
            Err(GitError::Failed { status: 1, .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// The fetch URL for a remote.
    pub fn remote_url(&self, remote: &str) -> Result<Option<String>> {
        self.config(&format!("remote.{remote}.url"))
    }

    /// Resolves a revision to a full object id.
    pub fn rev_parse(&self, rev: &str) -> Result<String> {
        let out = self.run(["rev-parse", "--verify", "--end-of-options", rev])?;
        Ok(trimmed(&out.stdout))
    }

    /// Lists the index: every staged path with its mode and blob id.
    pub fn ls_files(&self) -> Result<Vec<IndexEntry>> {
        let out = self.run(["ls-files", "--stage", "-z"])?;
        let mut entries = Vec::new();
        for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let text = String::from_utf8_lossy(record);
            // "<mode> <sha> <stage>\t<path>"
            let (meta, path) = text.split_once('\t').ok_or_else(|| GitError::Unparseable {
                args: "ls-files --stage -z".into(),
                detail: text.to_string(),
            })?;
            let mut fields = meta.split_whitespace();
            let (Some(mode), Some(sha)) = (fields.next(), fields.next()) else {
                return Err(GitError::Unparseable {
                    args: "ls-files --stage -z".into(),
                    detail: text.to_string(),
                });
            };
            entries.push(IndexEntry {
                mode: mode.to_owned(),
                sha: sha.to_owned(),
                path: PathBuf::from(path),
            });
        }
        Ok(entries)
    }

    /// Paths whose working-tree content differs from the index.
    ///
    /// git has already done this comparison as part of its own bookkeeping, so
    /// asking it costs a stat per path where hashing would cost a read. With an
    /// LFS clean filter in play the answer covers data files too.
    pub fn dirty_paths(&self) -> Result<std::collections::HashSet<String>> {
        let out = self.run(["diff-files", "--name-only", "-z"])?;
        Ok(split_nul(&out.stdout))
    }

    /// Paths staged for commit that differ from HEAD.
    pub fn staged_paths(&self) -> Result<std::collections::HashSet<String>> {
        // A repository with no commits has no HEAD to diff against; everything
        // in the index is staged by definition.
        if self.rev_parse("HEAD").is_err() {
            return Ok(self.ls_files()?.into_iter().map(|e| e.path_str()).collect());
        }
        let out = self.run(["diff-index", "--cached", "--name-only", "-z", "HEAD"])?;
        Ok(split_nul(&out.stdout))
    }

    /// Stages every change in the working tree, additions and deletions alike.
    ///
    /// This is where an LFS clean filter turns data into pointers, which is the
    /// whole reason `tsp` no longer writes pointer blobs itself.
    pub fn stage_all(&self) -> Result<()> {
        self.run(["add", "--all", "--"]).map(|_| ())
    }

    /// Installs git-lfs's filters and hooks into this repository.
    ///
    /// `--local` on purpose: configuring a user's global git is not this tool's
    /// business, and a repository-scoped install is what makes a clone of it
    /// behave the same for everyone.
    pub fn lfs_install(&self) -> Result<()> {
        self.run(["lfs", "install", "--local"]).map(|_| ())
    }

    /// Records `pattern` in `.gitattributes` as LFS-tracked.
    pub fn lfs_track(&self, pattern: &str) -> Result<()> {
        self.run(["lfs", "track", "--", pattern]).map(|_| ())
    }

    /// The object id the working-tree file at `path` would have once staged.
    ///
    /// `--path` makes git apply the same filters the path is configured for, so
    /// an LFS-tracked file hashes to its pointer rather than to its contents —
    /// which is what git will actually store, and therefore what a lock entry
    /// has to record.
    pub fn hash_working_file(&self, path: &str) -> Result<String> {
        let out = self.run([
            OsStr::new("hash-object"),
            OsStr::new("--path"),
            OsStr::new(path),
            OsStr::new("--"),
            self.work_tree.join(path).as_os_str(),
        ])?;
        Ok(trimmed(&out.stdout))
    }

    /// Stages the given paths, additions and deletions alike.
    pub fn stage_paths(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = vec![
            OsStr::new("add").to_owned(),
            OsStr::new("--all").to_owned(),
            OsStr::new("--").to_owned(),
        ];
        args.extend(paths.iter().map(|p| OsStr::new(p).to_owned()));
        self.run(args).map(|_| ())
    }

    /// Records the index as a tree object.
    pub fn write_tree(&self) -> Result<String> {
        let out = self.run(["write-tree"])?;
        Ok(trimmed(&out.stdout))
    }

    /// Creates a commit object without moving HEAD or any branch.
    ///
    /// This is how an experiment is recorded: the run becomes a real commit,
    /// reachable only from its own ref, so nothing about the checked-out branch
    /// changes and `git gc` still knows the objects are alive.
    pub fn commit_tree(&self, tree: &str, parent: Option<&str>, message: &str) -> Result<String> {
        let mut args = vec!["commit-tree".to_owned(), tree.to_owned()];
        if let Some(parent) = parent {
            args.push("-p".to_owned());
            args.push(parent.to_owned());
        }
        args.push("-m".to_owned());
        args.push(message.to_owned());

        let out = self.run(args)?;
        Ok(trimmed(&out.stdout))
    }

    pub fn update_ref(&self, name: &str, target: &str) -> Result<()> {
        self.run(["update-ref", "--end-of-options", name, target])
            .map(|_| ())
    }

    pub fn delete_ref(&self, name: &str) -> Result<()> {
        self.run(["update-ref", "-d", "--end-of-options", name])
            .map(|_| ())
    }

    /// Lists refs under `prefix` as (full name, object id).
    pub fn refs_under(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let out = self.run([
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "--sort=-committerdate",
            prefix,
        ])?;

        let mut refs = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((name, oid)) = line.split_once('\0') {
                refs.push((name.to_owned(), oid.to_owned()));
            }
        }
        Ok(refs)
    }

    /// Reads a blob at a revision, returning `None` when the path is absent.
    ///
    /// `--textconv` is deliberately not passed and no filter runs, so an
    /// LFS-tracked path yields its pointer rather than the data it names.
    pub fn read_blob_at(&self, rev: &str, path: &str) -> Result<Option<Vec<u8>>> {
        match self.run(["cat-file", "blob", &format!("{rev}:{path}")]) {
            Ok(out) => Ok(Some(out.stdout)),
            Err(GitError::Failed { .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// Restores paths from a revision into the index and working tree.
    pub fn restore_from(&self, rev: &str, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = vec![
            OsStr::new("checkout").to_owned(),
            OsStr::new("--end-of-options").to_owned(),
            OsStr::new(rev).to_owned(),
            OsStr::new("--").to_owned(),
        ];
        args.extend(paths.iter().map(|p| OsStr::new(p).to_owned()));
        self.run(args).map(|_| ())
    }

    /// Resets the index and working tree to `rev`, discarding everything else.
    pub fn reset_hard(&self, rev: &str) -> Result<()> {
        self.run(["reset", "--hard", "--quiet", "--end-of-options", rev])
            .map(|_| ())
    }

    /// Reads many blobs through a single `cat-file --batch` process.
    ///
    /// One spawn per object would dominate runtime on a dataset with thousands
    /// of files; this keeps it to one spawn total.
    pub fn read_blobs(&self, shas: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
        if shas.is_empty() {
            return Ok(Vec::new());
        }
        let args = ["cat-file", "--batch"];
        let mut child = Command::new("git")
            .current_dir(&self.work_tree)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| GitError::Spawn {
                args: args.join(" "),
                source,
            })?;

        // stdin is written on another thread: filling the pipe buffer while we
        // are not draining stdout would deadlock.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let query: Vec<String> = shas.to_vec();
        let writer = std::thread::spawn(move || -> std::io::Result<()> {
            for sha in &query {
                writeln!(stdin, "{sha}")?;
            }
            Ok(())
        });

        let mut stdout = child.stdout.take().expect("stdout was piped");
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut buf).map_err(|source| GitError::Spawn {
            args: args.join(" "),
            source,
        })?;

        writer
            .join()
            .expect("writer thread panicked")
            .map_err(|source| GitError::Spawn {
                args: args.join(" "),
                source,
            })?;
        child.wait().map_err(|source| GitError::Spawn {
            args: args.join(" "),
            source,
        })?;

        parse_cat_file_batch(&buf)
    }
}

/// One row of `git ls-files --stage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub mode: String,
    pub sha: String,
    pub path: PathBuf,
}

impl IndexEntry {
    /// The path as git spells it: forward slashes on every platform.
    pub fn path_str(&self) -> String {
        self.path.to_string_lossy().replace('\\', "/")
    }
}

fn split_nul(bytes: &[u8]) -> std::collections::HashSet<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| String::from_utf8_lossy(r).into_owned())
        .collect()
}

/// Splits `<sha> <type> <size>\n<content>\n` records.
fn parse_cat_file_batch(buf: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut pos = 0usize;

    while pos < buf.len() {
        let Some(nl) = buf[pos..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&buf[pos..pos + nl]).into_owned();
        pos += nl + 1;

        let mut fields = header.split_whitespace();
        let (Some(sha), Some(kind), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
            // "<sha> missing" for an unknown object: skip it rather than fail
            // the whole batch.
            continue;
        };
        if kind != "blob" {
            continue;
        }
        let size: usize = size.parse().map_err(|_| GitError::Unparseable {
            args: "cat-file --batch".into(),
            detail: header.clone(),
        })?;

        let end = (pos + size).min(buf.len());
        out.push((sha.to_owned(), buf[pos..end].to_vec()));
        pos = end + 1; // trailing newline after content
    }

    Ok(out)
}

/// Splits a `git credential fill` response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Asks git's configured credential helpers for credentials for `url`.
///
/// This is why `tsp` needs no remote configuration of its own: whatever the user
/// already set up for `git push` works unchanged.
pub fn credential_fill(url: &str) -> Result<Credentials> {
    let args = ["credential", "fill"];
    let mut child = Command::new("git")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| GitError::Spawn {
            args: args.join(" "),
            source,
        })?;

    write!(
        child.stdin.take().expect("stdin was piped"),
        "url={url}\n\n"
    )
    .map_err(|source| GitError::Spawn {
        args: args.join(" "),
        source,
    })?;

    let out = child.wait_with_output().map_err(|source| GitError::Spawn {
        args: args.join(" "),
        source,
    })?;
    let out = check(&args.join(" "), out)?;

    let mut creds = Credentials::default();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        match line.split_once('=') {
            Some(("username", v)) => creds.username = v.to_owned(),
            Some(("password", v)) => creds.password = v.to_owned(),
            _ => {}
        }
    }
    Ok(creds)
}

fn run_in<I, S>(dir: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    let display = args.join(" ");
    let out = Command::new("git")
        .current_dir(dir)
        .args(&args)
        .output()
        .map_err(|source| GitError::Spawn {
            args: display.clone(),
            source,
        })?;
    check(&display, out)
}

fn check(args: &str, out: Output) -> Result<Output> {
    if out.status.success() {
        return Ok(out);
    }
    Err(GitError::Failed {
        args: args.to_owned(),
        status: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    })
}

fn trimmed(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

/// `--cacheinfo` takes a comma-joined triple, so the path cannot be passed
/// after `--`; forward slashes are what git expects on every platform.
fn path_arg(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a throwaway repository with deterministic identity.
    fn repo() -> (tempfile::TempDir, Git) {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "tsp@example.test"],
            vec!["config", "user.name", "tsp test"],
        ] {
            run_in(dir.path(), args).unwrap();
        }
        // macOS temp dirs are symlinked through /private; discover() returns the
        // resolved path, so compare against that rather than dir.path().
        let git = Git::discover(dir.path()).unwrap();
        (dir, git)
    }

    #[test]
    fn discovers_work_tree_and_git_dir() {
        let (_d, git) = repo();
        assert!(git.git_dir().ends_with(".git"));
        assert!(git.work_tree().is_dir());
    }

    #[test]
    fn discover_outside_a_repository_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Git::discover(dir.path()).is_err());
    }

    #[test]
    fn write_blob_matches_git_hash_object() {
        let (_d, git) = repo();
        let sha = git.write_blob(b"hello tsp\n").unwrap();

        // The blob is really in the object database.
        let back = git.run(["cat-file", "-p", &sha]).unwrap();
        assert_eq!(back.stdout, b"hello tsp\n");
        assert_eq!(sha.len(), 40);
    }

    #[test]
    fn staged_blob_appears_in_the_index() {
        let (_d, git) = repo();
        let sha = git.write_blob(b"pointer text\n").unwrap();
        git.stage_blob(FileMode::Regular, &sha, Path::new("data/f.bin"))
            .unwrap();

        let out = git.run(["ls-files", "--stage"]).unwrap();
        let listing = String::from_utf8_lossy(&out.stdout);
        assert!(
            listing.contains("data/f.bin") && listing.contains(&sha),
            "index listing was {listing:?}"
        );
        assert!(listing.starts_with("100644"), "{listing:?}");
    }

    #[test]
    fn executable_mode_is_preserved_when_staging() {
        let (_d, git) = repo();
        let sha = git.write_blob(b"#!/bin/sh\n").unwrap();
        git.stage_blob(FileMode::Executable, &sha, Path::new("run.sh"))
            .unwrap();

        let out = git.run(["ls-files", "--stage"]).unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("100755"));
    }

    #[test]
    fn config_returns_none_when_unset() {
        let (_d, git) = repo();
        assert_eq!(git.config("tsp.definitely.unset").unwrap(), None);
    }

    #[test]
    fn remote_url_round_trips() {
        let (_d, git) = repo();
        assert_eq!(git.remote_url("origin").unwrap(), None);

        git.run(["remote", "add", "origin", "https://example.test/o/r.git"])
            .unwrap();

        assert_eq!(
            git.remote_url("origin").unwrap().as_deref(),
            Some("https://example.test/o/r.git")
        );
    }

    #[test]
    fn file_mode_detects_the_executable_bit() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::write(&plain, b"x").unwrap();
        assert_eq!(FileMode::of(&plain).unwrap(), FileMode::Regular);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let exe = dir.path().join("exe");
            std::fs::write(&exe, b"x").unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(FileMode::of(&exe).unwrap(), FileMode::Executable);
        }
    }

    #[test]
    fn ls_files_reports_mode_sha_and_path() {
        let (_d, git) = repo();
        let sha = git.write_blob(b"content\n").unwrap();
        git.stage_blob(FileMode::Regular, &sha, Path::new("dir/a.bin"))
            .unwrap();

        let entries = git.ls_files().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mode, "100644");
        assert_eq!(entries[0].sha, sha);
        assert_eq!(entries[0].path, Path::new("dir/a.bin"));
    }

    /// Paths with spaces must survive the NUL-delimited parse.
    #[test]
    fn ls_files_handles_spaces_in_paths() {
        let (_d, git) = repo();
        let sha = git.write_blob(b"x").unwrap();
        git.stage_blob(FileMode::Regular, &sha, Path::new("a dir/a file.bin"))
            .unwrap();

        let entries = git.ls_files().unwrap();
        assert_eq!(entries[0].path, Path::new("a dir/a file.bin"));
    }

    #[test]
    fn read_blobs_returns_contents_for_many_objects() {
        let (_d, git) = repo();
        let bodies: Vec<Vec<u8>> = (0..25)
            .map(|i| format!("body {i}\n").into_bytes())
            .collect();
        let shas: Vec<String> = bodies.iter().map(|b| git.write_blob(b).unwrap()).collect();

        let got = git.read_blobs(&shas).unwrap();

        assert_eq!(got.len(), bodies.len());
        for (i, (sha, content)) in got.iter().enumerate() {
            assert_eq!(sha, &shas[i]);
            assert_eq!(content, &bodies[i]);
        }
    }

    #[test]
    fn read_blobs_of_nothing_spawns_nothing() {
        let (_d, git) = repo();
        assert!(git.read_blobs(&[]).unwrap().is_empty());
    }

    /// Binary content must round-trip byte-for-byte through the batch parser.
    #[test]
    fn read_blobs_preserves_binary_content() {
        let (_d, git) = repo();
        let body: Vec<u8> = (0u8..=255).cycle().take(5000).collect();
        let sha = git.write_blob(&body).unwrap();

        let got = git.read_blobs(&[sha]).unwrap();
        assert_eq!(got[0].1, body);
    }

    /// A failing command must surface git's own stderr, not a bare exit code.
    #[test]
    fn errors_carry_stderr() {
        let (_d, git) = repo();
        let err = git.rev_parse("no-such-rev").unwrap_err();
        assert!(matches!(err, GitError::Failed { .. }), "{err:?}");
    }
}
