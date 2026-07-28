//! End-to-end tests for the local half of the workflow: track, commit, status.
//!
//! Network transfer is covered separately; everything here runs offline and
//! pins the behaviours that make the no-clean-filter design work.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the binary under test, provided by cargo.
const DS: &str = env!("CARGO_BIN_EXE_ds");

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "ds@example.test"],
            vec!["config", "user.name", "ds test"],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        Self { _dir: dir, root }
    }

    fn ds(&self, args: &[&str]) -> std::process::Output {
        Command::new(DS)
            .current_dir(&self.root)
            .args(args)
            .output()
            .unwrap()
    }

    fn ds_ok(&self, args: &[&str]) -> String {
        let out = self.ds(args);
        assert!(
            out.status.success(),
            "ds {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, rel: &str, content: &[u8]) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[test]
fn tracked_data_is_committed_as_a_pointer_while_the_worktree_keeps_the_bytes() {
    let f = Fixture::new();
    let data = payload(200_000);
    f.write("data/big.bin", &data);
    f.ds_ok(&["init"]);
    f.ds_ok(&["track", "data"]);

    // Crucially not "AM": the skip-worktree bit hides the working-tree/index
    // difference, so `git commit -a` cannot replace the pointer with raw data.
    assert_eq!(f.git(&["status", "--porcelain"]).trim(), "A  data/big.bin");

    f.git(&["commit", "-qm", "track"]);
    let committed = f.git(&["cat-file", "-p", "HEAD:data/big.bin"]);
    assert!(
        committed.starts_with("version https://git-lfs.github.com/spec/v1\n"),
        "committed blob was not a pointer: {committed:?}"
    );
    assert!(committed.contains("size 200000"));

    // The working tree still holds the real bytes.
    assert_eq!(std::fs::read(f.root.join("data/big.bin")).unwrap(), data);
    assert!(f.git(&["status", "--porcelain"]).trim().is_empty());
}

#[test]
fn status_reports_current_then_modified() {
    let f = Fixture::new();
    f.write("data/a.bin", &payload(5000));
    f.ds_ok(&["init"]);
    f.ds_ok(&["track", "data"]);

    assert!(f.ds_ok(&["status"]).contains("1 current"));

    f.write("data/a.bin", &payload(9000));
    let out = f.ds_ok(&["status"]);
    assert!(out.contains("1 modified"), "{out}");
    assert!(out.contains("modified  data/a.bin"), "{out}");
}

/// A plain `git clone` leaves pointer text in the working tree. That is a
/// "needs pull" state, not a user modification — reporting it as modified would
/// invite the user to re-track a pointer and lose the reference to the data.
#[test]
fn a_fresh_clone_reports_not_local_rather_than_modified() {
    let f = Fixture::new();
    f.write("data/a.bin", &payload(5000));
    f.ds_ok(&["init"]);
    f.ds_ok(&["track", "data"]);
    f.git(&["commit", "-qm", "track"]);

    // Own temp dir: a fixed name in the shared temp root collides with other
    // tests running concurrently.
    let clone_dir = tempfile::tempdir().unwrap();
    let clone = clone_dir.path().join("cloned");
    let out = Command::new("git")
        .args(["clone", "-q"])
        .arg(&f.root)
        .arg(&clone)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = Command::new(DS)
        .current_dir(&clone)
        .arg("status")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("1 not-local"), "{text}");
    assert!(!text.contains("1 modified"), "{text}");
}

/// Re-tracking an unmaterialized file would hash the pointer text and produce a
/// pointer to a pointer, permanently orphaning the real object.
#[test]
fn tracking_pointer_text_is_refused() {
    let f = Fixture::new();
    f.write("data/a.bin", &payload(5000));
    f.ds_ok(&["init"]);
    f.ds_ok(&["track", "data"]);
    f.git(&["commit", "-qm", "track"]);

    // Simulate an unmaterialized checkout by restoring the pointer text.
    let pointer = f.git(&["cat-file", "-p", "HEAD:data/a.bin"]);
    f.write("data/a.bin", pointer.as_bytes());

    let out = f.ds(&["track", "data/a.bin"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("pointer text"),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// git-lfs emits no pointer for a zero-length file, so an empty file must stay
/// an ordinary git blob.
#[test]
fn empty_files_are_staged_as_ordinary_blobs() {
    let f = Fixture::new();
    f.write("data/empty.bin", b"");
    f.ds_ok(&["init"]);
    let out = f.ds_ok(&["track", "data"]);
    assert!(out.contains("1 empty file(s) staged as-is"), "{out}");

    f.git(&["commit", "-qm", "track"]);
    assert_eq!(f.git(&["cat-file", "-p", "HEAD:data/empty.bin"]), "");
}

#[test]
fn executable_bit_survives_tracking() {
    let f = Fixture::new();
    let path = f.write("scripts/run.sh", &payload(2000));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    f.ds_ok(&["init"]);
    f.ds_ok(&["track", "scripts"]);

    assert!(
        f.git(&["ls-files", "--stage"]).starts_with("100755"),
        "executable bit was lost"
    );
}

/// The guard hook is the only thing standing between a stray `git add` and an
/// unrecoverable multi-gigabyte blob in history.
#[test]
fn pre_commit_hook_refuses_a_large_non_pointer_blob() {
    let f = Fixture::new();
    f.ds_ok(&["init"]);
    f.write("sneaky.bin", &payload(2_000_000));

    assert!(
        Command::new("git")
            .current_dir(&f.root)
            .args(["add", "-f", "sneaky.bin"])
            .status()
            .unwrap()
            .success()
    );

    let out = Command::new("git")
        .current_dir(&f.root)
        .args(["commit", "-m", "oops"])
        .output()
        .unwrap();

    assert!(!out.status.success(), "hook let a large raw blob through");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not an LFS pointer"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn small_files_commit_normally_with_the_hook_installed() {
    let f = Fixture::new();
    f.ds_ok(&["init"]);
    f.write("src/main.py", b"print('hi')\n");
    f.git(&["add", "src/main.py"]);

    let out = Command::new("git")
        .current_dir(&f.root)
        .args(["commit", "-m", "code"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "hook blocked an ordinary source file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn tracking_outside_a_repository_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(DS)
        .current_dir(dir.path())
        .arg("status")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("git repository"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The cache must hold the data while git's object store stays small — that is
/// the whole point of the design.
#[test]
fn git_objects_stay_small_while_the_cache_holds_the_data() {
    let f = Fixture::new();
    f.write("data/big.bin", &payload(1_000_000));
    f.ds_ok(&["init"]);
    f.ds_ok(&["track", "data"]);
    f.git(&["commit", "-qm", "track"]);

    assert!(dir_size(&f.root.join(".git/objects")) < 50_000);
    assert!(dir_size(&f.root.join(".git/ds/cache")) >= 1_000_000);
}

fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}
