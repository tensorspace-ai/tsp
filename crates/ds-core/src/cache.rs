//! The local content-addressed object cache.
//!
//! Deliberately rooted at `.git/ds/cache`, **not** `.git/lfs/objects`. Sharing
//! git-lfs's cache directory would look convenient but puts our data under a
//! second deletion mechanism: `git lfs prune` is a routine, documented command
//! with no grace period, and it decides what to keep from git-lfs's own view of
//! reachability. Objects we manage would be fair game.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::Digest;

use crate::git::FileMode;
use crate::hash::hash_reader;
use crate::oid::Oid;
use crate::pointer::Pointer;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("content hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: Oid, actual: Oid },
    #[error("object {0} is not in the cache")]
    Missing(Oid),
}

type Result<T> = std::result::Result<T, CacheError>;

fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> CacheError {
    let path = path.into();
    move |source| CacheError::Io { path, source }
}

/// How a cached object is placed into the working tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Materialize {
    /// Copy-on-write clone where the filesystem supports it, else a plain copy.
    ///
    /// The default. On APFS (and Btrfs/XFS/ReFS) this costs no space and the
    /// result is independently writable — the user can edit their data file
    /// without corrupting the cache.
    #[default]
    Reflink,
    /// Hard link to the cached object.
    ///
    /// Saves space everywhere, but the cache entry is read-only, so the linked
    /// working-tree file is read-only too. This is the single most confusing
    /// behaviour DVC exposes; it stays opt-in here.
    Hardlink,
    /// Always a full byte copy.
    Copy,
}

/// A content-addressed store of objects keyed by sha256.
#[derive(Clone, Debug)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// Opens (without creating) a cache rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The conventional location inside a repository's git directory.
    pub fn in_git_dir(git_dir: impl AsRef<Path>) -> Self {
        Self::new(git_dir.as_ref().join("ds").join("cache"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/ab/cd/<full-oid>`.
    pub fn path_for(&self, oid: &Oid) -> PathBuf {
        let (a, b, full) = oid.fanout();
        self.root.join(a).join(b).join(full)
    }

    pub fn contains(&self, oid: &Oid) -> bool {
        self.path_for(oid).is_file()
    }

    pub fn open(&self, oid: &Oid) -> Result<File> {
        let path = self.path_for(oid);
        File::open(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => CacheError::Missing(oid.clone()),
            _ => CacheError::Io { path, source: e },
        })
    }

    /// Streams `src` into the cache, verifying it hashes to `expected`.
    ///
    /// Writes to a temporary file in the destination directory and renames, so
    /// a concurrent `ds pull` can never observe a partially written object.
    pub fn insert_verified(&self, expected: &Oid, mut src: impl Read) -> Result<()> {
        let mut writer = self.writer(expected)?;
        if writer.already_present() {
            return Ok(());
        }
        io::copy(&mut src, &mut writer).map_err(io_err(self.path_for(expected)))?;
        writer.finish()
    }

    /// Opens a streaming write handle for `expected`.
    ///
    /// Exists so async callers (the LFS download path) can feed chunks in
    /// without buffering a multi-gigabyte object in memory, while reusing this
    /// module's verify-then-atomically-rename guarantee.
    pub fn writer(&self, expected: &Oid) -> Result<CacheWriter> {
        let dest = self.path_for(expected);
        if dest.is_file() {
            return Ok(CacheWriter {
                state: WriterState::AlreadyPresent,
                expected: expected.clone(),
                dest,
            });
        }
        let dir = dest.parent().expect("fanout always yields a parent");
        fs::create_dir_all(dir).map_err(io_err(dir))?;
        let tmp = tempfile::NamedTempFile::new_in(dir).map_err(io_err(dir))?;
        Ok(CacheWriter {
            state: WriterState::Writing {
                tmp,
                hasher: Sha256Digest::new(),
            },
            expected: expected.clone(),
            dest,
        })
    }

    /// Hashes `path` and stores it, returning the pointer describing it.
    pub fn insert_file(&self, path: impl AsRef<Path>) -> Result<Pointer> {
        let path = path.as_ref();
        let pointer = hash_reader(File::open(path).map_err(io_err(path))?).map_err(io_err(path))?;
        self.insert_verified(&pointer.oid, File::open(path).map_err(io_err(path))?)?;
        Ok(pointer)
    }

    /// Places a cached object at `dest`, creating parent directories.
    ///
    /// `mode` is the mode git recorded for the path. The cached object's own
    /// permissions are never the answer: it is read-only by design and shared
    /// between every checkout, so they say nothing about the data file.
    pub fn materialize(
        &self,
        oid: &Oid,
        dest: impl AsRef<Path>,
        how: Materialize,
        mode: FileMode,
    ) -> Result<()> {
        let dest = dest.as_ref();
        let src = self.path_for(oid);
        if !src.is_file() {
            return Err(CacheError::Missing(oid.clone()));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        // Replacing an existing file: remove first so a hardlink or reflink
        // does not fail on an existing destination.
        if dest.symlink_metadata().is_ok() {
            fs::remove_file(dest).map_err(io_err(dest))?;
        }

        match how {
            // A hard link shares the cached object's inode, so it also shares
            // its permissions: setting the mode here would make the cache entry
            // — and every other checkout linked to it — writable. Staying
            // read-only is the trade this mode exists to make.
            Materialize::Hardlink => fs::hard_link(&src, dest).map_err(io_err(dest)),
            Materialize::Copy => copy_with_mode(&src, dest, mode),
            Materialize::Reflink => match reflink_copy::reflink(&src, dest) {
                Ok(()) => set_mode(dest, mode),
                // Filesystem has no CoW support (or crosses a device); a plain
                // copy is always correct, just slower.
                Err(_) => copy_with_mode(&src, dest, mode),
            },
        }
    }
}

/// Copies and applies `mode` — `fs::copy` carries the cache's read-only
/// permissions across, which is never what the working tree wants.
fn copy_with_mode(src: &Path, dest: &Path, mode: FileMode) -> Result<()> {
    fs::copy(src, dest).map_err(io_err(dest))?;
    set_mode(dest, mode)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: FileMode) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let base = default_file_permissions();
    let bits = if mode.is_executable() {
        // Execute follows read, which is what git grants an executable blob.
        base | ((base & 0o444) >> 2)
    } else {
        base
    };
    fs::set_permissions(path, fs::Permissions::from_mode(bits)).map_err(io_err(path))
}

#[cfg(not(unix))]
fn set_mode(path: &Path, _mode: FileMode) -> Result<()> {
    let mut perms = fs::metadata(path).map_err(io_err(path))?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms).map_err(io_err(path))
}

/// The permissions the kernel grants a newly created file, i.e. `0666` masked
/// by the process umask.
///
/// Measured by creating a file rather than calling `libc::umask`, which both
/// reads and writes the value and would race any other thread creating a file.
/// Probed once; a umask change mid-process is not a case worth tracking.
#[cfg(unix)]
fn default_file_permissions() -> u32 {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::OnceLock;

    static PERMISSIONS: OnceLock<u32> = OnceLock::new();
    *PERMISSIONS.get_or_init(|| {
        let probe = || -> io::Result<u32> {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("umask-probe");
            File::create(&path)?;
            Ok(fs::metadata(&path)?.permissions().mode() & 0o777)
        };
        // 0644 is what the near-universal 022 umask yields.
        probe().unwrap_or(0o644)
    })
}

/// Read-only so an accidental in-place edit through a hard link cannot
/// silently corrupt every other checkout sharing the object.
fn set_read_only(path: &Path) -> Result<()> {
    let mut perms = fs::metadata(path).map_err(io_err(path))?.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms).map_err(io_err(path))
}

type Sha256Digest = sha2::Sha256;

enum WriterState {
    /// Another writer already stored this object; writes are discarded.
    AlreadyPresent,
    Writing {
        tmp: tempfile::NamedTempFile,
        hasher: Sha256Digest,
    },
}

/// A streaming handle that hashes as it writes and only publishes the object
/// once the content is confirmed to match the expected oid.
pub struct CacheWriter {
    state: WriterState,
    expected: Oid,
    dest: PathBuf,
}

impl CacheWriter {
    /// True when the object was already cached, so the caller can skip the
    /// transfer entirely.
    pub fn already_present(&self) -> bool {
        matches!(self.state, WriterState::AlreadyPresent)
    }

    /// Verifies the accumulated hash and atomically publishes the object.
    pub fn finish(self) -> Result<()> {
        let WriterState::Writing { tmp, hasher } = self.state else {
            return Ok(());
        };

        let actual = Oid::from_bytes(&hasher.finalize().into());
        if actual != self.expected {
            // tmp drops here, removing the partial file: a corrupt transfer
            // must never leave something that looks like a cached object.
            return Err(CacheError::HashMismatch {
                expected: self.expected,
                actual,
            });
        }

        tmp.as_file().sync_all().map_err(io_err(&self.dest))?;
        set_read_only(tmp.path())?;
        // persist() is an atomic rename; a racing writer that got there first
        // simply loses, and both wrote identical verified bytes.
        tmp.persist(&self.dest).map_err(|e| CacheError::Io {
            path: self.dest.clone(),
            source: e.error,
        })?;
        Ok(())
    }
}

impl Write for CacheWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.state {
            WriterState::AlreadyPresent => Ok(buf.len()),
            WriterState::Writing { tmp, hasher } => {
                let n = tmp.as_file_mut().write(buf)?;
                hasher.update(&buf[..n]);
                Ok(n)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.state {
            WriterState::AlreadyPresent => Ok(()),
            WriterState::Writing { tmp, .. } => tmp.as_file_mut().flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> (tempfile::TempDir, Cache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("cache"));
        (dir, cache)
    }

    fn oid_of(content: &[u8]) -> Oid {
        hash_reader(content).unwrap().oid
    }

    #[test]
    fn insert_then_read_back() {
        let (_d, cache) = cache();
        let content = b"some data";
        let oid = oid_of(content);

        cache.insert_verified(&oid, &content[..]).unwrap();

        assert!(cache.contains(&oid));
        let mut got = Vec::new();
        cache.open(&oid).unwrap().read_to_end(&mut got).unwrap();
        assert_eq!(got, content);
    }

    #[test]
    fn path_uses_fanout() {
        let (_d, cache) = cache();
        let oid = oid_of(b"x");
        let rel = cache.path_for(&oid);
        let (a, b, full) = oid.fanout();
        assert!(rel.ends_with(format!("{a}/{b}/{full}")));
    }

    #[test]
    fn rejects_content_that_does_not_match_the_oid() {
        let (_d, cache) = cache();
        let claimed = oid_of(b"the real thing");

        let err = cache
            .insert_verified(&claimed, &b"something else"[..])
            .unwrap_err();

        assert!(matches!(err, CacheError::HashMismatch { .. }));
        // A corrupt transfer must leave nothing behind.
        assert!(!cache.contains(&claimed));
    }

    #[test]
    fn insert_is_idempotent() {
        let (_d, cache) = cache();
        let content = b"repeated";
        let oid = oid_of(content);

        cache.insert_verified(&oid, &content[..]).unwrap();
        cache.insert_verified(&oid, &content[..]).unwrap();

        assert!(cache.contains(&oid));
    }

    #[test]
    fn cached_objects_are_read_only() {
        let (_d, cache) = cache();
        let oid = oid_of(b"protected");
        cache.insert_verified(&oid, &b"protected"[..]).unwrap();

        let perms = fs::metadata(cache.path_for(&oid)).unwrap().permissions();
        assert!(perms.readonly());
    }

    #[test]
    fn missing_object_reports_missing_not_io() {
        let (_d, cache) = cache();
        let oid = oid_of(b"never stored");
        assert!(matches!(cache.open(&oid), Err(CacheError::Missing(_))));
    }

    #[test]
    fn materialize_produces_a_writable_copy() {
        for how in [Materialize::Reflink, Materialize::Copy] {
            let (dir, cache) = cache();
            let content = b"materialize me";
            let oid = oid_of(content);
            cache.insert_verified(&oid, &content[..]).unwrap();

            let dest = dir.path().join("nested/out.bin");
            cache
                .materialize(&oid, &dest, how, FileMode::Regular)
                .unwrap();

            assert_eq!(fs::read(&dest).unwrap(), content, "{how:?}");
            assert!(
                !fs::metadata(&dest).unwrap().permissions().readonly(),
                "{how:?} left the working-tree file read-only"
            );
            // Writable in practice, not just by permission bits.
            fs::OpenOptions::new().write(true).open(&dest).unwrap();
        }
    }

    /// The cache stores objects read-only and mode-less, so restoring one has
    /// to put back the mode git recorded rather than whatever the object has.
    #[cfg(unix)]
    #[test]
    fn materialize_restores_the_recorded_mode() {
        use std::os::unix::fs::PermissionsExt;

        for how in [Materialize::Reflink, Materialize::Copy] {
            let (dir, cache) = cache();
            let oid = oid_of(b"#!/bin/sh\necho hi\n");
            cache
                .insert_verified(&oid, &b"#!/bin/sh\necho hi\n"[..])
                .unwrap();

            let plain = dir.path().join("plain.bin");
            cache
                .materialize(&oid, &plain, how, FileMode::Regular)
                .unwrap();
            let plain_mode = fs::metadata(&plain).unwrap().permissions().mode() & 0o777;

            let exe = dir.path().join("run.sh");
            cache
                .materialize(&oid, &exe, how, FileMode::Executable)
                .unwrap();
            let exe_mode = fs::metadata(&exe).unwrap().permissions().mode() & 0o777;

            // A regular file comes back exactly as the umask would have made it.
            let reference = dir.path().join("reference");
            File::create(&reference).unwrap();
            let expected = fs::metadata(&reference).unwrap().permissions().mode() & 0o777;
            assert_eq!(plain_mode, expected, "{how:?} did not honour the umask");

            // An executable gains execute wherever read is already granted.
            assert_eq!(
                exe_mode,
                expected | ((expected & 0o444) >> 2),
                "{how:?} did not restore the executable bit"
            );
            assert_ne!(
                exe_mode & 0o100,
                0,
                "{how:?} left the owner unable to run it"
            );
        }
    }

    #[test]
    fn materialize_overwrites_an_existing_file() {
        let (dir, cache) = cache();
        let oid = oid_of(b"new");
        cache.insert_verified(&oid, &b"new"[..]).unwrap();

        let dest = dir.path().join("out.bin");
        fs::write(&dest, b"stale").unwrap();
        cache
            .materialize(&oid, &dest, Materialize::Reflink, FileMode::Regular)
            .unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"new");
    }

    #[test]
    fn materialize_missing_object_fails_cleanly() {
        let (dir, cache) = cache();
        let oid = oid_of(b"absent");
        let dest = dir.path().join("out.bin");

        assert!(matches!(
            cache.materialize(&oid, &dest, Materialize::Reflink, FileMode::Regular),
            Err(CacheError::Missing(_))
        ));
        assert!(!dest.exists());
    }

    #[test]
    fn insert_file_returns_the_pointer() {
        let (dir, cache) = cache();
        let src = dir.path().join("input.bin");
        fs::write(&src, b"file contents").unwrap();

        let pointer = cache.insert_file(&src).unwrap();

        assert_eq!(pointer.size, 13);
        assert_eq!(pointer.oid, oid_of(b"file contents"));
        assert!(cache.contains(&pointer.oid));
    }

    #[test]
    fn in_git_dir_nests_under_ds() {
        let cache = Cache::in_git_dir("/repo/.git");
        assert!(cache.root().ends_with(".git/ds/cache"));
    }
}
