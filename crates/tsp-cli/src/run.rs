//! Running stages and recording what they produced.

use std::process::Command;

use anyhow::{Context, Result, bail};
use tsp_core::graph::Status;

use crate::repo::Repo;

/// Runs the stages that need it and rewrites their lock entries.
///
/// Returns the stages that actually ran. Outputs are staged before the lock is
/// written, because the `git_sha` an entry records has to be the object id git
/// holds — for an LFS-tracked output that is the pointer the clean filter just
/// produced, which does not exist until the path is staged.
pub fn repro(repo: &mut Repo, target: Option<&str>, force: bool) -> Result<Vec<String>> {
    let plan = repo.plan(target)?;
    if plan.is_empty() {
        return Ok(Vec::new());
    }

    let statuses: std::collections::HashMap<String, Status> =
        repo.statuses()?.into_iter().collect();

    let mut ran = Vec::new();
    for name in &plan {
        let status = statuses.get(name).cloned().unwrap_or(Status::New);

        // A stage downstream of one that just ran is stale regardless of what
        // the lock said before this command started.
        let downstream_of_a_run = repo
            .pipeline
            .stage(name)?
            .deps
            .iter()
            .any(|dep| produced_by_any(repo, dep, &ran));

        if !force && !status.needs_run() && !downstream_of_a_run {
            println!("  {name}: up to date");
            continue;
        }

        let reason = if force {
            "forced".to_owned()
        } else if downstream_of_a_run && !status.needs_run() {
            "upstream stage re-ran".to_owned()
        } else {
            status.reason().unwrap_or("out of date").to_owned()
        };
        println!("==> {name} ({reason})");

        execute(repo, name)?;
        ran.push(name.clone());
    }

    if ran.is_empty() {
        return Ok(ran);
    }

    repo.stage_run_outputs()?;
    repo.relock(&ran)?;
    let path = repo.lock_path();
    repo.lock
        .write(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    // The lock is itself an output of the run, so it belongs in the same index
    // state as everything else the run touched.
    repo.stage_run_outputs()?;

    Ok(ran)
}

fn produced_by_any(repo: &Repo, path: &str, stages: &[String]) -> bool {
    stages.iter().any(|name| {
        repo.pipeline
            .stages
            .get(name)
            .is_some_and(|s| s.out_paths().contains(&path))
    })
}

fn execute(repo: &Repo, name: &str) -> Result<()> {
    let stage = repo.pipeline.stage(name)?;
    let cwd = match &stage.wdir {
        Some(dir) => repo.root().join(dir),
        None => repo.root().to_path_buf(),
    };

    for line in stage.cmd.iter() {
        // A shell, because pipelines in the wild use pipes and redirection and
        // a bare argv split would silently mangle them.
        let status = shell(line)
            .current_dir(&cwd)
            .status()
            .with_context(|| format!("running stage {name}"))?;

        if !status.success() {
            bail!(
                "stage {name} failed: `{line}` exited with {}",
                status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |c| c.to_string())
            );
        }
    }

    require_declared_outputs(repo, name, stage)
}

/// Refuses a stage that exited 0 without writing what it said it would.
///
/// The lock records only outputs that exist, so a missing one used to leave an
/// entry tracking nothing and a stage reported current forever after. A command
/// that succeeds while its declared artifact is absent has not done its job,
/// whatever its exit code claims.
fn require_declared_outputs(
    repo: &Repo,
    name: &str,
    stage: &tsp_core::pipeline::Stage,
) -> Result<()> {
    let missing: Vec<&str> = stage
        .out_paths()
        .into_iter()
        .filter(|path| !repo.root().join(path).exists())
        .collect();

    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "stage {name} exited 0 but did not write: {}. \
         Either the command does not produce what the pipeline declares, or the path \
         is spelled differently there than on disk.",
        missing.join(", ")
    )
}

#[cfg(unix)]
fn shell(line: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(line);
    command
}

#[cfg(not(unix))]
fn shell(line: &str) -> Command {
    let mut command = Command::new("cmd");
    command.arg("/C").arg(line);
    command
}
