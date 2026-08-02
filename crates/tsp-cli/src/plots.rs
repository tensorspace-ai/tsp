//! `tsp plots`: collecting plot data across revisions and writing it out.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use tsp_core::figure::{self, Loaded};
use tsp_core::plotdata;
use tsp_core::plots::Plot;
use tsp_core::svg;

use crate::repo::Repo;

/// Where `tsp plots` writes.
pub const OUT_DIR: &str = "tsp_plots";

/// Written into the output directory so the generated page stays out of git.
///
/// It goes beside the page rather than into the repository's own `.gitignore`
/// because `--out` can point anywhere, and an entry naming one directory would
/// not cover the next.
const IGNORE: &str =
    "# Written by `tsp plots`. The page is generated; regenerate it instead.\n/index.html\n";

/// The name for data read from the working tree rather than a commit.
pub const WORKSPACE: &str = "workspace";

/// Every plot the pipeline declares: the top-level section first, then each
/// stage's own, in pipeline order.
pub fn declared(repo: &Repo) -> Vec<Plot> {
    let mut plots: Vec<Plot> = repo.pipeline.plots.iter().cloned().collect();
    for stage in repo.pipeline.stages.values() {
        for plot in stage.plot_defs() {
            // A path declared in both places is configured at the top level;
            // the stage entry only says the file is an output.
            if !plots.iter().any(|p| p.name == plot.name) {
                plots.push(plot.clone());
            }
        }
    }
    plots
}

/// Reads the files a set of plots needs, at one revision.
///
/// A file missing at a revision is not an error: a plot added last week has
/// nothing to show for the release before it, and the figure simply has one
/// fewer series.
pub fn load_at(repo: &Repo, plots: &[Plot], rev: &str) -> Result<BTreeMap<String, Vec<Row>>> {
    let mut files: BTreeMap<String, Vec<Row>> = BTreeMap::new();

    for plot in plots {
        for source in &plot.sources {
            for file in [Some(source.file.as_str()), source.x_file.as_deref()]
                .into_iter()
                .flatten()
            {
                if files.contains_key(file) {
                    continue;
                }
                let raw = if rev == WORKSPACE {
                    std::fs::read(repo.root().join(file)).ok()
                } else {
                    repo.git().read_blob_at(rev, file)?
                };
                let Some(raw) = raw else { continue };

                match plotdata::parse_with_header(file, &raw, source.header) {
                    Ok(mut rows) => {
                        // `step` is written per file per revision, so a plot
                        // that names no x axis counts from zero either way.
                        plotdata::add_index(&mut rows);
                        files.insert(file.to_owned(), rows);
                    }
                    // One unreadable file must not lose the other plots.
                    Err(err) => eprintln!("warning: {err}"),
                }
            }
        }
    }
    Ok(files)
}

type Row = plotdata::Row;

/// Builds every declared plot across the given revisions.
pub fn build(repo: &Repo, revs: &[String]) -> Result<Vec<figure::Rendered>> {
    let plots = declared(repo);
    if plots.is_empty() {
        return Ok(Vec::new());
    }

    let mut loaded: Loaded = Vec::new();
    for rev in revs {
        loaded.push((rev.clone(), load_at(repo, &plots, rev)?));
    }

    Ok(plots.iter().map(|p| figure::build(p, &loaded)).collect())
}

/// Writes the figures as one self-contained page, returning its path.
pub fn write_page(
    repo: &Repo,
    figures: &[figure::Rendered],
    out: &str,
) -> Result<std::path::PathBuf> {
    let dir = out_dir(repo.root(), out)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    // Rewritten every time: a user who deleted it wants the page tracked, and
    // will delete it again. Writing it once on creation would instead depend on
    // whether the directory happened to exist.
    let ignore = dir.join(".gitignore");
    std::fs::write(&ignore, IGNORE).with_context(|| format!("writing {}", ignore.display()))?;

    let rendered: Vec<String> = figures.iter().map(svg::render).collect();
    let path = dir.join("index.html");
    std::fs::write(&path, svg::page("tsp plots", &rendered))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Resolves `--out` against the repository, refusing anything that leaves it.
///
/// A generated page is repository content, and a command that writes outside
/// the tree on the strength of a flag is the "escaping the repository" case
/// SECURITY.md puts in scope.
fn out_dir(root: &std::path::Path, out: &str) -> Result<std::path::PathBuf> {
    if out.is_empty() {
        anyhow::bail!("--out needs a directory name");
    }
    let candidate = std::path::Path::new(out);
    if candidate.is_absolute() {
        anyhow::bail!("--out must be inside the repository, and {out:?} is an absolute path");
    }
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("--out must be inside the repository, and {out:?} climbs out of it");
    }
    Ok(root.join(candidate))
}

/// The revisions to compare, following DVC: with none given, the working tree
/// against HEAD; with one, that revision against the working tree.
pub fn revisions(explicit: &[String]) -> Vec<String> {
    match explicit.len() {
        0 => vec!["HEAD".to_owned(), WORKSPACE.to_owned()],
        _ => {
            let mut revs = explicit.to_vec();
            if !revs.iter().any(|r| r == WORKSPACE) {
                revs.push(WORKSPACE.to_owned());
            }
            revs
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_revisions_compares_head_with_the_working_tree() {
        assert_eq!(revisions(&[]), ["HEAD", "workspace"]);
    }

    #[test]
    fn a_named_revision_is_compared_against_the_working_tree() {
        assert_eq!(revisions(&["v1.0".to_owned()]), ["v1.0", "workspace"]);
    }

    #[test]
    fn the_workspace_is_not_added_twice() {
        assert_eq!(revisions(&["workspace".to_owned()]), ["workspace"]);
    }

    #[test]
    fn the_output_directory_must_stay_inside_the_repository() {
        let root = std::path::Path::new("/repo");
        for bad in ["", "/tmp/escape", "../escape", "a/../../escape"] {
            assert!(
                out_dir(root, bad).is_err(),
                "{bad:?} should be refused as --out"
            );
        }
        assert_eq!(
            out_dir(root, "tsp_plots").unwrap(),
            std::path::Path::new("/repo/tsp_plots")
        );
    }
}
