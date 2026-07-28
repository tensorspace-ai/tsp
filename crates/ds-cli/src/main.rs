//! `ds` — reproducible pipelines and experiments on top of git.
//!
//! Data management is not here. Datasets belong in Git LFS through an ordinary
//! `filter=lfs` gitattribute, which means `git add`, `git push`, `git checkout`
//! and `git clone` move bytes with no help from this tool, and `git lfs prune`
//! and `git lfs fsck` maintain them. What `ds` adds is the layer git has no
//! opinion about: which stages produced which artifacts, whether that record
//! still holds, and what a given experiment changed.

mod exp;

mod repo;
mod run;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use ds_core::git::Git;
use ds_core::metrics;
use ds_core::params::Override;

use repo::Repo;

#[derive(Parser)]
#[command(
    name = "ds",
    version,
    about = "Reproducible pipelines and experiments, versioned in git"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Set the repository up for `ds` and Git LFS
    Init {
        /// Path patterns to send to Git LFS, e.g. "data/**" "models/**"
        #[arg(long = "lfs", value_name = "PATTERN")]
        lfs: Vec<String>,
    },
    /// Run the stages that are out of date and update the lock file
    Repro {
        /// Only this stage and the stages it depends on
        stage: Option<String>,
        /// Run every stage in the plan, current or not
        #[arg(long)]
        force: bool,
    },
    /// Show which stages are current, stale or new
    Status,
    /// Show metric values, optionally against another revision
    Metrics {
        /// Revision to compare against, e.g. a branch, tag or experiment
        #[arg(long)]
        compare: Option<String>,
    },
    /// Run and compare parameter experiments
    Exp(ExpArgs),
}

#[derive(Args)]
struct ExpArgs {
    #[command(subcommand)]
    command: ExpCommand,
}

#[derive(Subcommand)]
enum ExpCommand {
    /// Run the pipeline with parameter overrides and record the result
    Run {
        /// Override a parameter, e.g. --set train.max_depth=8
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Name for the experiment; one is derived from the commit otherwise
        #[arg(long)]
        name: Option<String>,
    },
    /// List recorded experiments and their metrics
    List,
    /// Show one experiment in detail
    Show { name: String },
    /// Bring an experiment's parameters and outputs into the working tree
    Apply { name: String },
    /// Delete experiments
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;

    match cli.command {
        Command::Init { lfs } => init(&cwd, &lfs),
        Command::Repro { stage, force } => repro(&cwd, stage.as_deref(), force),
        Command::Status => status(&cwd),
        Command::Metrics { compare } => show_metrics(&cwd, compare.as_deref()),
        Command::Exp(args) => match args.command {
            ExpCommand::Run { set, name } => exp_run(&cwd, &set, name.as_deref()),
            ExpCommand::List => exp_list(&cwd),
            ExpCommand::Show { name } => exp_show(&cwd, &name),
            ExpCommand::Apply { name } => exp_apply(&cwd, &name),
            ExpCommand::Remove { names } => exp_remove(&cwd, &names),
        },
    }
}

/// Configures Git LFS and installs the guard hook.
///
/// `ds` does not move data, so this is mostly a matter of handing the job to
/// git-lfs properly: install its filters, then record the patterns that decide
/// what counts as data.
fn init(cwd: &std::path::Path, patterns: &[String]) -> Result<()> {
    let git = Git::discover(cwd).context("not inside a git repository")?;

    remove_obsolete_push_hook(&git)?;
    git.lfs_install()
        .context("running `git lfs install` — is git-lfs installed?")?;
    println!("Configured Git LFS in {}", git.work_tree().display());

    for pattern in patterns {
        git.lfs_track(pattern)
            .with_context(|| format!("tracking {pattern:?} with Git LFS"))?;
        println!("  data: {pattern}");
    }

    install_guard_hook(&git)?;

    if patterns.is_empty() {
        println!("\nTell Git LFS what counts as data, e.g.:");
        println!("  git lfs track \"data/**\" \"models/**\"");
    }
    println!("\nDescribe your stages in ds.yaml, then run `ds repro`.");
    Ok(())
}

/// Clears the `pre-push` hook older versions of `ds` installed.
///
/// That hook ran `ds push`, a command that no longer exists, so leaving it in
/// place would fail every push. It also occupies the slot `git lfs install`
/// wants, which is what makes this a migration step rather than a nicety. Only
/// a hook carrying our own marker is touched.
fn remove_obsolete_push_hook(git: &Git) -> Result<()> {
    let path = git.git_dir().join("hooks").join("pre-push");
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    if !existing.contains("ds-push") {
        return Ok(());
    }

    std::fs::remove_file(&path)?;
    println!("Removed the obsolete ds pre-push hook; git-lfs uploads data now.");
    Ok(())
}

/// Refuses to commit a large blob that no LFS filter claimed.
///
/// git-lfs converts anything matching `.gitattributes`, so what is left to
/// catch is the file nobody remembered to add a pattern for. That mistake is
/// only visible once it is in history, where it cannot be removed without a
/// rewrite.
fn install_guard_hook(git: &Git) -> Result<()> {
    let hooks = git.git_dir().join("hooks");
    std::fs::create_dir_all(&hooks)?;
    let path = hooks.join("pre-commit");

    if path.exists() {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.contains("ds-guard") {
            eprintln!(
                "warning: {} already exists and was left alone; \
                 large-file protection is not installed",
                path.display()
            );
        }
        return Ok(());
    }

    std::fs::write(&path, GUARD_HOOK)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

const GUARD_HOOK: &str = r#"#!/bin/sh
# ds-guard: refuse to commit a large blob that is not an LFS pointer.
# git-lfs converts whatever .gitattributes matches; this catches the file that
# no pattern covers, which is the one that ends up stuck in history.
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
        echo "ds: track it with 'git lfs track \"$path\"', or bypass with --no-verify" >&2
        exit 1
    fi
done || fail=1
exit $fail
"#;

fn repro(cwd: &std::path::Path, stage: Option<&str>, force: bool) -> Result<()> {
    let mut repo = Repo::open(cwd)?;
    let ran = run::repro(&mut repo, stage, force)?;

    if ran.is_empty() {
        println!("Everything is up to date.");
        return Ok(());
    }
    println!(
        "\nRan {} stage(s); {} updated and staged.",
        ran.len(),
        ds_core::lock::Lock::name_for(&repo.pipeline_file)
    );
    println!("Commit the result with `git commit`; `git push` uploads the data.");
    Ok(())
}

fn status(cwd: &std::path::Path) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let statuses = repo.statuses()?;

    if statuses.is_empty() {
        println!("No stages defined in {}.", repo.pipeline_file);
        return Ok(());
    }

    let width = statuses.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut stale = 0;
    for (name, status) in &statuses {
        if status.needs_run() {
            stale += 1;
        }
        match status.reason() {
            Some(reason) => println!("  {name:<width$}  {:<8}  {reason}", status.label()),
            None => println!("  {name:<width$}  {}", status.label()),
        }
    }

    println!("\n{} stage(s); {stale} need running.", statuses.len());
    if stale > 0 {
        println!("Bring them up to date with `ds repro`.");
    }
    Ok(())
}

fn show_metrics(cwd: &std::path::Path, compare: Option<&str>) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let current = metrics::read(repo.git(), repo.root(), &repo.pipeline, None)?;

    let Some(rev) = compare else {
        if current.is_empty() {
            println!("No metrics files found. Declare them under a stage's `metrics:`.");
            return Ok(());
        }
        let width = current.iter().map(|m| m.key.len()).max().unwrap_or(0);
        for metric in &current {
            println!("  {:<width$}  {}", metric.key, metric.display());
        }
        return Ok(());
    };

    let other = metrics::read(repo.git(), repo.root(), &repo.pipeline, Some(rev))?;
    print_comparison(&metrics::compare(&current, &other), "workspace", rev);
    Ok(())
}

fn print_comparison(rows: &[metrics::Row], current_label: &str, compare_label: &str) {
    let key_width = rows
        .iter()
        .map(|r| r.key.len())
        .chain([6])
        .max()
        .unwrap_or(6);
    println!(
        "  {:<key_width$}  {:>12}  {:>12}  DELTA",
        "METRIC", current_label, compare_label
    );

    for row in rows {
        let delta = match (row.delta, row.improved) {
            (Some(d), Some(true)) => format!("{} better", metrics::format_delta(d)),
            (Some(d), Some(false)) => format!("{} worse", metrics::format_delta(d)),
            (Some(d), None) => metrics::format_delta(d),
            (None, _) => String::new(),
        };
        println!(
            "  {:<key_width$}  {:>12}  {:>12}  {delta}",
            row.key,
            row.current.as_deref().unwrap_or("-"),
            row.compare.as_deref().unwrap_or("-"),
        );
    }
}

fn exp_run(cwd: &std::path::Path, set: &[String], name: Option<&str>) -> Result<()> {
    let overrides: Vec<Override> = set
        .iter()
        .map(|s| s.parse())
        .collect::<std::result::Result<_, _>>()?;

    let mut repo = Repo::open(cwd)?;
    let experiment = exp::run(&mut repo, &overrides, name)?;

    println!(
        "\nRecorded {} ({})",
        experiment.name,
        &experiment.commit[..7]
    );
    let produced = exp::metrics_of(&repo, &experiment)?;
    let baseline = metrics::read(repo.git(), repo.root(), &repo.pipeline, Some("HEAD"))?;
    if !produced.is_empty() {
        println!();
        print_comparison(
            &metrics::compare(&produced, &baseline),
            &experiment.name,
            "HEAD",
        );
    }
    println!("\nApply it with `ds exp apply {}`.", experiment.name);
    Ok(())
}

fn exp_list(cwd: &std::path::Path) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let experiments = exp::list(&repo)?;

    if experiments.is_empty() {
        println!("No experiments yet. Run one with `ds exp run --set key=value`.");
        return Ok(());
    }

    // One column per metric key, so runs line up under the same headings.
    let mut keys: Vec<String> = Vec::new();
    let mut rows: Vec<(String, Vec<Metricish>)> = Vec::new();

    let baseline = metrics::read(repo.git(), repo.root(), &repo.pipeline, Some("HEAD"))?;
    for metric in &baseline {
        if !keys.contains(&metric.key) {
            keys.push(metric.key.clone());
        }
    }
    rows.push(("HEAD".to_owned(), values_for(&keys, &baseline)));

    for experiment in &experiments {
        let produced = exp::metrics_of(&repo, experiment)?;
        for metric in &produced {
            if !keys.contains(&metric.key) {
                keys.push(metric.key.clone());
            }
        }
        rows.push((experiment.name.clone(), values_for(&keys, &produced)));
    }

    // Re-resolve now that every key is known, so late columns are not blank.
    let mut resolved: Vec<(String, Vec<Metricish>)> = Vec::with_capacity(rows.len());
    resolved.push(("HEAD".to_owned(), values_for(&keys, &baseline)));
    for experiment in &experiments {
        let produced = exp::metrics_of(&repo, experiment)?;
        resolved.push((experiment.name.clone(), values_for(&keys, &produced)));
    }

    let name_width = resolved
        .iter()
        .map(|(n, _)| n.len())
        .chain([4])
        .max()
        .unwrap();
    let widths: Vec<usize> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            resolved
                .iter()
                .map(|(_, vals)| vals[i].0.len())
                .chain([key.len()])
                .max()
                .unwrap()
        })
        .collect();

    print!("  {:<name_width$}", "NAME");
    for (key, width) in keys.iter().zip(&widths) {
        print!("  {key:>width$}");
    }
    println!();

    for (name, values) in &resolved {
        print!("  {name:<name_width$}");
        for (value, width) in values.iter().zip(&widths) {
            print!("  {:>width$}", value.0);
        }
        println!();
    }
    Ok(())
}

/// A metric value already rendered for the table, or "-" when absent.
struct Metricish(String);

fn values_for(keys: &[String], metrics: &[metrics::Metric]) -> Vec<Metricish> {
    keys.iter()
        .map(|key| {
            Metricish(
                metrics
                    .iter()
                    .find(|m| &m.key == key)
                    .map_or_else(|| "-".to_owned(), |m| m.display()),
            )
        })
        .collect()
}

fn exp_show(cwd: &std::path::Path, name: &str) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let experiment = exp::find(&repo, name)?;

    println!("{}  {}", experiment.name, experiment.commit);
    let produced = exp::metrics_of(&repo, &experiment)?;
    let baseline = metrics::read(repo.git(), repo.root(), &repo.pipeline, Some("HEAD"))?;
    if produced.is_empty() {
        println!("  (no metrics recorded)");
    } else {
        println!();
        print_comparison(
            &metrics::compare(&produced, &baseline),
            &experiment.name,
            "HEAD",
        );
    }
    Ok(())
}

fn exp_apply(cwd: &std::path::Path, name: &str) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let experiment = exp::find(&repo, name)?;
    let restored = exp::apply(&repo, &experiment)?;

    println!(
        "Applied {} to the working tree ({} path(s)).",
        experiment.name,
        restored.len()
    );
    println!("Review with `git diff --cached`, then commit or `git reset --hard` to discard.");
    Ok(())
}

fn exp_remove(cwd: &std::path::Path, names: &[String]) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let removed = exp::remove(&repo, names)?;
    println!("Removed {removed} experiment(s).");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
