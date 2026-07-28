//! Experiments: a pipeline run recorded as a commit nobody's branch points at.
//!
//! An experiment is an ordinary git commit under `refs/ds/exps/`, parented on
//! the HEAD it was run from. That buys three things for free: the outputs are
//! real objects so `git gc` keeps them alive, LFS-tracked data in an experiment
//! is pushed and fetched by the same machinery as anything else, and comparing
//! two experiments is comparing two commits.
//!
//! The working tree is put back exactly as it was found, so a sweep leaves no
//! trace beyond the refs it created.

use anyhow::{Context, Result, bail};
use ds_core::params::{self, Override};

use crate::metrics::{self, Metric};
use crate::repo::Repo;
use crate::run;

/// Where experiment commits live. Outside `refs/heads/` so they never appear as
/// branches, and outside `refs/tags/` so they are not pushed by default.
pub const REF_PREFIX: &str = "refs/ds/exps";

pub struct Experiment {
    pub name: String,
    pub commit: String,
}

impl Experiment {
    pub fn ref_name(&self) -> String {
        format!("{REF_PREFIX}/{}", self.name)
    }
}

/// Runs the pipeline with `overrides` applied and records the result.
pub fn run(repo: &mut Repo, overrides: &[Override], name: Option<&str>) -> Result<Experiment> {
    repo.require_clean_tree()?;

    let head = repo
        .git()
        .rev_parse("HEAD")
        .context("an experiment is recorded against HEAD, which does not exist yet")?;

    apply_overrides(repo, overrides)?;

    // Whatever happens from here, the tree goes back to HEAD: a half-finished
    // experiment must not leave overridden parameters behind.
    let outcome = record(repo, &head, overrides, name);
    let restored = repo.git().reset_hard(&head);

    let experiment = outcome?;
    restored.context("restoring the working tree after the experiment")?;
    Ok(experiment)
}

fn record(
    repo: &mut Repo,
    head: &str,
    overrides: &[Override],
    name: Option<&str>,
) -> Result<Experiment> {
    let ran = run::repro(repo, None, false)?;
    if ran.is_empty() {
        bail!("nothing to run: every stage is already current at these parameters");
    }

    repo.stage_run_outputs()?;
    let tree = repo.git().write_tree()?;
    let summary = match overrides.is_empty() {
        true => "ds experiment".to_owned(),
        false => format!(
            "ds experiment: {}",
            overrides
                .iter()
                .map(|o| format!("{}={}", o.key, o.value))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    };
    let commit = repo.git().commit_tree(&tree, Some(head), &summary)?;

    // Named from the commit so the name is traceable back to the object.
    let name = match name {
        Some(given) => validate_name(given)?.to_owned(),
        None => format!("exp-{}", &commit[..7]),
    };
    let experiment = Experiment { name, commit };
    repo.git()
        .update_ref(&experiment.ref_name(), &experiment.commit)?;
    Ok(experiment)
}

fn apply_overrides(repo: &Repo, overrides: &[Override]) -> Result<()> {
    for (file, entries) in params::by_file(overrides) {
        let path = repo.root().join(file);
        let mut params = ds_core::params::Params::read_optional(&path)?;
        for entry in entries {
            params.set(&entry.key, entry.value.clone())?;
        }
        params
            .write(&path)
            .with_context(|| format!("writing {file}"))?;
    }
    Ok(())
}

/// A name becomes a ref, so it must survive `git check-ref-format`.
fn validate_name(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!(
            "{name:?} is not usable as an experiment name; \
             use letters, digits, dash, underscore and dot"
        );
    }
    Ok(name)
}

pub fn list(repo: &Repo) -> Result<Vec<Experiment>> {
    Ok(repo
        .git()
        .refs_under(REF_PREFIX)?
        .into_iter()
        .filter_map(|(name, commit)| {
            name.strip_prefix(&format!("{REF_PREFIX}/"))
                .map(|short| Experiment {
                    name: short.to_owned(),
                    commit,
                })
        })
        .collect())
}

pub fn find(repo: &Repo, name: &str) -> Result<Experiment> {
    list(repo)?
        .into_iter()
        .find(|e| e.name == name)
        .with_context(|| format!("no experiment named {name:?}; `ds exp list` shows them"))
}

/// The metrics an experiment produced.
pub fn metrics_of(repo: &Repo, experiment: &Experiment) -> Result<Vec<Metric>> {
    Ok(metrics::read(
        repo.git(),
        repo.root(),
        &repo.pipeline,
        Some(&experiment.commit),
    )?)
}

/// Brings an experiment's parameters and outputs into the working tree.
///
/// The experiment stays where it is; this only restores paths, so HEAD and the
/// current branch are untouched and the change can be committed or discarded
/// like any other edit.
pub fn apply(repo: &Repo, experiment: &Experiment) -> Result<Vec<String>> {
    let mut paths: Vec<String> = Vec::new();
    for stage in repo.pipeline.stages.values() {
        paths.extend(stage.out_paths().into_iter().map(str::to_owned));
        paths.extend(stage.params.iter().map(|p| p.file.clone()));
    }
    paths.push(ds_core::lock::FILE_NAME.to_owned());
    paths.sort();
    paths.dedup();

    // Restoring a path the experiment does not contain would fail the whole
    // command, so ask git which of them it actually has.
    let present: Vec<String> = paths
        .into_iter()
        .filter(|p| {
            repo.git()
                .read_blob_at(&experiment.commit, p)
                .ok()
                .flatten()
                .is_some()
        })
        .collect();

    repo.git().restore_from(&experiment.commit, &present)?;
    Ok(present)
}

pub fn remove(repo: &Repo, names: &[String]) -> Result<usize> {
    let mut removed = 0;
    for name in names {
        let experiment = find(repo, name)?;
        repo.git().delete_ref(&experiment.ref_name())?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_would_break_a_ref_are_refused() {
        assert!(validate_name("tuned-forest").is_ok());
        assert!(validate_name("exp_1.2").is_ok());

        for bad in [
            "",
            "-leading",
            ".hidden",
            "trailing.",
            "a..b",
            "has space",
            "sl/ash",
            "ti~lde",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn ref_name_is_namespaced() {
        let experiment = Experiment {
            name: "exp-abc1234".to_owned(),
            commit: "abc".to_owned(),
        };
        assert_eq!(experiment.ref_name(), "refs/ds/exps/exp-abc1234");
    }
}
