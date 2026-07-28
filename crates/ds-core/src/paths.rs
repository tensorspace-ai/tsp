//! Path hygiene applied when data is first tracked.
//!
//! These checks belong at `ds track` time, not at checkout time. A dataset
//! built on Linux can contain paths that simply cannot exist on macOS or
//! Windows; discovering that halfway through materializing 300 GB is the worst
//! possible moment to find out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    #[error(
        "paths {first} and {second} differ only by case and cannot coexist on macOS or Windows"
    )]
    CaseCollision { first: PathBuf, second: PathBuf },
}

/// Normalizes a path to Unicode NFC.
///
/// APFS hands back decomposed (NFD) filenames while Linux stores whatever was
/// written, usually NFC. Without normalizing, the same dataset tracked on a Mac
/// and on Linux produces different paths and a spurious diff.
pub fn normalize(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().nfc().collect::<String>())
}

/// Rejects sets of paths that cannot all exist on a case-insensitive filesystem.
pub fn check_case_collisions(paths: &[PathBuf]) -> Result<(), PathError> {
    let mut seen: HashMap<String, &PathBuf> = HashMap::with_capacity(paths.len());
    for path in paths {
        let key = path.to_string_lossy().to_lowercase();
        if let Some(first) = seen.insert(key, path) {
            return Err(PathError::CaseCollision {
                first: first.clone(),
                second: path.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfd_and_nfc_normalize_to_the_same_path() {
        // "é" as a single code point vs. "e" + combining acute.
        let composed = Path::new("caf\u{e9}/data.bin");
        let decomposed = Path::new("cafe\u{301}/data.bin");
        assert_ne!(composed, decomposed);
        assert_eq!(normalize(composed), normalize(decomposed));
    }

    #[test]
    fn ascii_paths_are_unchanged() {
        let p = Path::new("data/train/shard-0001.parquet");
        assert_eq!(normalize(p), p);
    }

    #[test]
    fn distinct_paths_are_accepted() {
        let paths = vec![PathBuf::from("data/a.bin"), PathBuf::from("data/b.bin")];
        assert!(check_case_collisions(&paths).is_ok());
    }

    #[test]
    fn case_only_differences_are_rejected() {
        let paths = vec![PathBuf::from("data/A.png"), PathBuf::from("data/a.png")];
        assert!(matches!(
            check_case_collisions(&paths),
            Err(PathError::CaseCollision { .. })
        ));
    }

    #[test]
    fn case_differences_in_directories_are_also_rejected() {
        let paths = vec![PathBuf::from("Data/x.bin"), PathBuf::from("data/x.bin")];
        assert!(check_case_collisions(&paths).is_err());
    }
}
