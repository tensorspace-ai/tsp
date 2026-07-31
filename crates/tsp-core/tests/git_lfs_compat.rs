//! Cross-checks our pointer parsing against the real `git-lfs` binary.
//!
//! git-lfs writes the pointers now, and `tsp` reads them to learn what a stage
//! produced. Agreement on the format is therefore a precondition for the lock
//! recording the right digest — and for reading it back at all.
//!
//! Skipped when `git-lfs` is not installed.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use tsp_core::hash::hash_reader;

fn git_lfs_available() -> bool {
    Command::new("git-lfs")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Runs `git lfs pointer --file` over `content` and returns the exact bytes.
fn reference_pointer(content: &[u8]) -> Vec<u8> {
    // Unique per call: tests run concurrently and each one removes its dir.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tsp-lfs-compat-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("object.bin");
    std::fs::File::create(&path)
        .unwrap()
        .write_all(content)
        .unwrap();

    let out = Command::new("git-lfs")
        .arg("pointer")
        .arg(format!("--file={}", path.display()))
        .output()
        .expect("git-lfs pointer");
    assert!(out.status.success(), "git-lfs pointer failed: {out:?}");

    std::fs::remove_dir_all(&dir).ok();
    out.stdout
}

fn assert_matches_git_lfs(content: &[u8]) {
    let ours = hash_reader(content).unwrap().to_bytes();
    assert_eq!(
        String::from_utf8_lossy(&ours),
        String::from_utf8_lossy(&reference_pointer(content)),
        "pointer bytes diverged from git-lfs for {} bytes of input",
        content.len(),
    );
}

#[test]
fn pointer_bytes_match_git_lfs() {
    if !git_lfs_available() {
        eprintln!("skipping: git-lfs not installed");
        return;
    }

    assert_matches_git_lfs(b"hello tsp\n");
    assert_matches_git_lfs(&[0u8; 4096]);
    assert_matches_git_lfs("non-ascii: \u{1f600}\n".as_bytes());
}

/// git-lfs emits no pointer for a zero-length file, so neither do we — an
/// empty file stays an ordinary empty git blob. See `pointer::is_lfs_eligible`.
#[test]
fn git_lfs_emits_no_pointer_for_empty_files() {
    if !git_lfs_available() {
        eprintln!("skipping: git-lfs not installed");
        return;
    }

    assert!(reference_pointer(b"").is_empty());
    assert!(!tsp_core::pointer::is_lfs_eligible(0));
}
