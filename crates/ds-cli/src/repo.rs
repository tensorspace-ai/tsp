//! Repository operations: the layer that ties git, the cache and LFS together.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ds_core::cache::{Cache, Materialize};
use ds_core::git::{FileMode, Git, credential_fill};
use ds_core::{Oid, Pointer, hash, paths, pointer};
use ds_lfs::Client;
use ds_lfs::endpoint;

/// A dataset file: where it lives and what it points at.
#[derive(Clone, Debug)]
pub struct Tracked {
    pub path: PathBuf,
    pub pointer: Pointer,
}

/// What `ds status` reports for one tracked file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Working-tree content matches the pointer.
    Current,
    /// The working tree still holds the pointer text itself, which is what a
    /// plain `git clone` produces. Needs `ds pull`, not re-tracking.
    NotMaterialized,
    /// Present locally but the content no longer matches; needs re-tracking.
    Modified,
    /// Not in the working tree, but the object is cached, so `ds pull` is local.
    Cached,
    /// Neither in the working tree nor cached; `ds pull` must hit the network.
    Missing,
}

pub struct Repo {
    git: Git,
    cache: Cache,
}

impl Repo {
    pub fn open(start: impl AsRef<Path>) -> Result<Self> {
        let git = Git::discover(start).context("not inside a git repository")?;
        let cache = Cache::in_git_dir(git.git_dir());
        Ok(Self { git, cache })
    }

    pub fn git(&self) -> &Git {
        &self.git
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Every path in the index whose blob is an LFS pointer.
    ///
    /// Detection is by content, exactly as Gitea does it, so there is no
    /// separate registry of tracked files to drift out of sync with git.
    pub fn tracked(&self) -> Result<Vec<Tracked>> {
        let entries = self.git.ls_files()?;
        let candidates: Vec<_> = entries
            .iter()
            .filter(|e| e.mode.starts_with("100"))
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let shas: Vec<String> = candidates.iter().map(|e| e.sha.clone()).collect();
        let blobs = self.git.read_blobs(&shas)?;

        let mut tracked = Vec::new();
        for (entry, (_, content)) in candidates.iter().zip(blobs.iter()) {
            if !Pointer::could_be_pointer(content.len() as u64) {
                continue;
            }
            if let Ok(p) = Pointer::try_from(content.as_slice()) {
                tracked.push(Tracked {
                    path: entry.path.clone(),
                    pointer: p,
                });
            }
        }
        Ok(tracked)
    }

    /// Classifies a tracked file without hashing unless it has to.
    pub fn state_of(&self, t: &Tracked) -> State {
        let abs = self.git.work_tree().join(&t.path);
        let Ok(meta) = std::fs::metadata(&abs) else {
            return if self.cache.contains(&t.pointer.oid) {
                State::Cached
            } else {
                State::Missing
            };
        };

        // Must precede the size check: after a plain `git clone` the working
        // tree holds the pointer text, which would otherwise look like the
        // user replaced the data with a 130-byte file.
        if is_pointer_for(&abs, &t.pointer) {
            return State::NotMaterialized;
        }

        // Size is a free first check; only hash when it could match.
        if meta.len() != t.pointer.size {
            return State::Modified;
        }
        match hash::hash_file(&abs) {
            Ok(p) if p.oid == t.pointer.oid => State::Current,
            _ => State::Modified,
        }
    }

    /// Tracks one file: ingest into the cache, replace the index entry with a
    /// pointer blob, and mark the path skip-worktree.
    pub fn track_file(&self, abs: &Path) -> Result<Option<Pointer>> {
        let rel = self.relative(abs)?;
        let meta = std::fs::metadata(abs)
            .with_context(|| format!("cannot read {}", abs.display()))?;

        // git-lfs never creates a pointer for an empty file, so neither do we;
        // it stays an ordinary empty blob.
        if !pointer::is_lfs_eligible(meta.len()) {
            self.git.stage_path(&rel)?;
            return Ok(None);
        }

        // Guard against re-tracking an unmaterialized file: in a fresh clone the
        // working tree holds pointer text, and hashing that would produce a
        // pointer to a pointer, permanently losing the reference to the data.
        if Pointer::could_be_pointer(meta.len())
            && let Ok(content) = std::fs::read(abs)
            && Pointer::try_from(content.as_slice()).is_ok()
        {
            bail!(
                "{} contains LFS pointer text, not data — run `ds pull` first",
                rel.display()
            );
        }

        let ptr = self
            .cache
            .insert_file(abs)
            .with_context(|| format!("caching {}", abs.display()))?;
        let blob = self.git.write_blob(&ptr.to_bytes())?;
        let mode = FileMode::of(abs)?;
        self.git.stage_blob(mode, &blob, &rel)?;
        self.git.set_skip_worktree(&rel, true)?;
        Ok(Some(ptr))
    }

    /// Restores a tracked file's content from the cache.
    pub fn materialize(&self, t: &Tracked) -> Result<()> {
        let dest = self.git.work_tree().join(&t.path);
        self.cache
            .materialize(&t.pointer.oid, &dest, Materialize::default())
            .with_context(|| format!("writing {}", t.path.display()))?;
        // A fresh clone has no skip-worktree bits: the index carries them
        // nowhere. Re-apply so git does not see the restored data as a change.
        self.git.set_skip_worktree(&t.path, true)?;
        Ok(())
    }

    /// Builds an LFS client for `remote`, resolving the endpoint and
    /// credentials from git's own configuration.
    pub fn client(&self, remote: &str) -> Result<Client> {
        let url = self
            .git
            .remote_url(remote)?
            .with_context(|| format!("remote {remote:?} has no URL; add one with `git remote add`"))?;
        let lfs_url = self.git.config("lfs.url")?;
        let endpoint = endpoint::determine(&url, lfs_url.as_deref())
            .with_context(|| format!("cannot derive an LFS endpoint from {url:?}"))?;

        let creds = credential_fill(endpoint.as_str())
            .with_context(|| format!("no credentials available for {endpoint}"))?;

        Ok(Client::new(endpoint, &creds.username, &creds.password))
    }

    /// Converts an absolute path into one relative to the work tree.
    fn relative(&self, abs: &Path) -> Result<PathBuf> {
        let abs = abs
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", abs.display()))?;
        let root = self.git.work_tree().canonicalize()?;
        let rel = abs
            .strip_prefix(&root)
            .with_context(|| format!("{} is outside the repository", abs.display()))?;
        Ok(paths::normalize(rel))
    }

    /// Expands the given paths into concrete files, rejecting anything that
    /// cannot be checked out everywhere.
    pub fn expand(&self, inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for input in inputs {
            if input.is_dir() {
                for entry in walkdir::WalkDir::new(input).follow_links(false) {
                    let entry = entry?;
                    // Symlinks have no meaningful content hash; skipping is
                    // safer than silently duplicating or following a loop.
                    if entry.file_type().is_symlink() {
                        eprintln!("skipping symlink {}", entry.path().display());
                        continue;
                    }
                    if entry.file_type().is_file() && !self.is_internal(entry.path()) {
                        files.push(entry.path().to_path_buf());
                    }
                }
            } else if input.is_file() {
                files.push(input.clone());
            } else {
                bail!("{} does not exist", input.display());
            }
        }

        let relatives: Vec<PathBuf> = files
            .iter()
            .map(|f| self.relative(f))
            .collect::<Result<_>>()?;
        paths::check_case_collisions(&relatives)?;

        Ok(files)
    }

    /// Never track git's own directory.
    fn is_internal(&self, path: &Path) -> bool {
        path.components()
            .any(|c| c.as_os_str() == ".git")
    }
}

/// True when the file at `abs` is the pointer text for `expected`.
fn is_pointer_for(abs: &Path, expected: &Pointer) -> bool {
    let Ok(meta) = std::fs::metadata(abs) else {
        return false;
    };
    if !Pointer::could_be_pointer(meta.len()) {
        return false;
    }
    std::fs::read(abs)
        .ok()
        .and_then(|c| Pointer::try_from(c.as_slice()).ok())
        .is_some_and(|p| p.oid == expected.oid)
}

/// Collects the object ids for a set of tracked files.
pub fn pointers_of(tracked: &[Tracked]) -> Vec<Pointer> {
    tracked.iter().map(|t| t.pointer.clone()).collect()
}

/// Formats an oid for human-facing output.
pub fn short(oid: &Oid) -> String {
    oid.as_str()[..12].to_owned()
}
