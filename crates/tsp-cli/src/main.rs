//! `tsp` — reproducible pipelines and experiments on top of git.
//!
//! Data management is not here. Datasets belong in Git LFS through an ordinary
//! `filter=lfs` gitattribute, which means `git add`, `git push`, `git checkout`
//! and `git clone` move bytes with no help from this tool, and `git lfs prune`
//! and `git lfs fsck` maintain them. What `tsp` adds is the layer git has no
//! opinion about: which stages produced which artifacts, whether that record
//! still holds, and what a given experiment changed.

mod exp;
mod output;

mod plots;
mod repo;
mod run;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tsp_core::git::Git;
use tsp_core::metrics;
use tsp_core::params::{self, Override};

use output::{Format, Note};
use repo::Repo;

#[derive(Parser)]
#[command(
    name = "tsp",
    version,
    about = "Reproducible pipelines and experiments, versioned in git",
    long_about = "Reproducible pipelines and experiments, versioned in git.

`tsp` records which stages produced which artifacts, whether that record still
holds, and what a given experiment changed. It does not move your data: datasets
go through an ordinary `filter=lfs` gitattribute, so git and git-lfs move the
bytes. There is no database, no daemon and no server.

Requires `git` and `git-lfs` on PATH.",
    after_help = "\
Getting started:
  tsp init --lfs 'data/**' --lfs 'models/**'   set the repository up
  $EDITOR tsp.yaml                             describe your stages
  tsp repro                                    run what is out of date

A minimal tsp.yaml:
  stages:
    train:
      cmd: python src/train.py
      deps:
        - src/train.py
        - data/prepared.csv
      params:
        - params.yaml:
            - train.max_depth
      outs:
        - models/model.pkl
      metrics:
        - metrics.json

The full format reference is in docs/format.md."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Set the repository up for `tsp` and Git LFS
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
    Status {
        /// Write a JSON document instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Show metric values, optionally against another revision
    Metrics {
        /// Revision to compare against, e.g. a branch, tag or experiment
        #[arg(long, value_name = "REV")]
        compare: Option<String>,
        /// Write a JSON document instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Show parameter values, optionally against another revision
    Params {
        /// Revision to compare against, e.g. a branch, tag or experiment
        #[arg(long, value_name = "REV")]
        compare: Option<String>,
        /// Write a JSON document instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Render the pipeline's plots to a self-contained HTML page
    Plots {
        /// Revisions to draw. With none, HEAD is compared with the working
        /// tree; the working tree is always included.
        revisions: Vec<String>,
        /// Directory to write into
        #[arg(long, default_value = plots::OUT_DIR)]
        out: String,
    },
    /// Run and compare parameter experiments
    Exp(ExpArgs),
    /// Print a shell completion script
    Completions {
        /// Shell to generate for
        shell: clap_complete::Shell,
    },
    /// Print the man page in roff format
    ///
    /// Hidden because it exists for whoever builds the package, not for the
    /// person using it.
    #[command(hide = true)]
    Man,
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
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Replace an experiment of the same name
        #[arg(long)]
        force: bool,
    },
    /// List recorded experiments and their metrics
    List {
        /// Write a JSON document instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Show one experiment in detail
    Show {
        /// Experiment to show, as listed by `tsp exp list`
        name: String,
        /// Write a JSON document instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Bring an experiment's parameters and outputs into the working tree
    Apply {
        /// Experiment to apply, as listed by `tsp exp list`
        name: String,
    },
    /// Delete experiments
    Remove {
        /// Experiments to delete, as listed by `tsp exp list`
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
        Command::Status { json } => status(&cwd, Format::from_flag(json)),
        Command::Metrics { compare, json } => {
            show_metrics(&cwd, compare.as_deref(), Format::from_flag(json))
        }
        Command::Params { compare, json } => {
            show_params(&cwd, compare.as_deref(), Format::from_flag(json))
        }
        Command::Plots { revisions, out } => show_plots(&cwd, &revisions, &out),
        Command::Exp(args) => match args.command {
            ExpCommand::Run { set, name, force } => exp_run(&cwd, &set, name.as_deref(), force),
            ExpCommand::List { json } => exp_list(&cwd, Format::from_flag(json)),
            ExpCommand::Show { name, json } => exp_show(&cwd, &name, Format::from_flag(json)),
            ExpCommand::Apply { name } => exp_apply(&cwd, &name),
            ExpCommand::Remove { names } => exp_remove(&cwd, &names),
        },
        Command::Completions { shell } => {
            let mut command = <Cli as clap::CommandFactory>::command();
            clap_complete::generate(shell, &mut command, "tsp", &mut std::io::stdout());
            Ok(())
        }
        Command::Man => {
            clap_mangen::Man::new(<Cli as clap::CommandFactory>::command())
                .render(&mut std::io::stdout())?;
            Ok(())
        }
    }
}

/// Configures Git LFS and installs the guard hook.
///
/// `tsp` does not move data, so this is mostly a matter of handing the job to
/// git-lfs properly: install its filters, then record the patterns that decide
/// what counts as data.
fn init(cwd: &std::path::Path, patterns: &[String]) -> Result<()> {
    let git = Git::discover(cwd)?;

    git.lfs_install()
        .context("running `git lfs install` — is git-lfs installed?")?;
    println!("Configured Git LFS in {}", git.work_tree().display());

    for pattern in patterns {
        git.lfs_track(pattern)
            .with_context(|| format!("tracking {pattern:?} with Git LFS"))?;
        println!("  data: {pattern}");
    }

    install_guard_hook(&git)?;

    // Only advise this when there is nothing tracked yet. Re-running `tsp init`
    // in a configured repository used to tell the reader to set up something
    // they had already set up.
    if patterns.is_empty() && !tracks_anything(&git) {
        println!("\nTell Git LFS what counts as data, e.g.:");
        println!("  git lfs track \"data/**\" \"models/**\"");
    }
    println!("\nDescribe your stages in tsp.yaml, then run `tsp repro`.");
    Ok(())
}

/// Whether `.gitattributes` already sends anything through the LFS filter.
fn tracks_anything(git: &Git) -> bool {
    std::fs::read_to_string(git.work_tree().join(".gitattributes"))
        .is_ok_and(|text| text.contains("filter=lfs"))
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
        // Both markers count as ours: a repository set up before the rename
        // has a working guard hook, and warning about it would send the reader
        // to fix something that is not broken.
        if !existing.contains("tsp-guard") {
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
# tsp-guard: refuse to commit a large blob that is not an LFS pointer.
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
        echo "tsp: refusing to commit $path ($size bytes, not an LFS pointer)" >&2
        echo "tsp: track it with 'git lfs track \"$path\"', or bypass with --no-verify" >&2
        exit 1
    fi
done || fail=1
exit $fail
"#;

/// The note a DVC repository needs before it reads a screen of `new`.
///
/// It is a note rather than a warning: the existing warnings are files `tsp`
/// *failed* to read, and calling this one a warning sends the reader looking for
/// something to fix. Nothing is wrong, and the first `tsp repro` ends it.
///
/// Written to stderr, which is what keeps `--json` output parseable on stdout.
fn dvc_lock_note() -> String {
    format!(
        "{dvc} is present and tsp does not read it. DVC records content hashes; \
         {ours} records git object ids, so there is nothing in {dvc} a staleness \
         check here could use. Every stage reports `new` until the first \
         `tsp repro`, which writes {ours} and leaves {dvc} where it is.",
        dvc = tsp_core::lock::DVC_FILE_NAME,
        ours = tsp_core::lock::FILE_NAME,
    )
}

fn repro(cwd: &std::path::Path, stage: Option<&str>, force: bool) -> Result<()> {
    let mut repo = Repo::open(cwd)?;
    // Before anything runs, not after: on a DVC repository this first repro
    // rebuilds every stage, and the reader should know why while interrupting
    // it is still worth doing.
    if repo.dvc_lock_unread {
        eprintln!("note: {}\n", dvc_lock_note());
    }
    let ran = run::repro(&mut repo, stage, force)?;

    if ran.is_empty() {
        println!("Everything is up to date.");
        return Ok(());
    }
    println!(
        "\nRan {} {}; {} updated and staged.",
        ran.len(),
        plural(ran.len(), "stage", "stages"),
        tsp_core::lock::FILE_NAME
    );
    println!("Commit the result with `git commit`; `git push` uploads the data.");
    Ok(())
}

/// The notes a command has for its reader, whatever shape it is emitting in.
///
/// In a terminal they go to stderr, which is what keeps a JSON document the
/// only thing on stdout. In a document they are a field, so a program sees
/// exactly what a person would have.
fn notes_for(repo: &Repo, format: Format) -> Vec<Note> {
    let mut notes = Vec::new();
    if repo.dvc_lock_unread {
        let message = dvc_lock_note();
        if format == Format::Text {
            eprintln!("note: {message}\n");
        }
        notes.push(Note {
            code: output::DVC_LOCK_NOT_READ,
            message,
        });
    }
    notes
}

fn status(cwd: &std::path::Path, format: Format) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let notes = notes_for(&repo, format);
    let statuses = repo.statuses()?;

    if format.is_json() {
        return output::emit(&output::status_document(
            &repo.pipeline_file,
            &statuses,
            &notes,
        ));
    }

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

    let total = statuses.len();
    println!(
        "\n{total} {}; {stale} {} running.",
        plural(total, "stage", "stages"),
        plural(stale, "needs", "need")
    );
    if stale > 0 {
        println!("Bring them up to date with `tsp repro`.");
    }
    Ok(())
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

fn show_metrics(cwd: &std::path::Path, compare: Option<&str>, format: Format) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let notes = notes_for(&repo, format);
    let current = metrics::read(repo.git(), repo.root(), &repo.pipeline, None)?;

    let Some(rev) = compare else {
        if format.is_json() {
            // An empty result is an empty document, never a sentence: a
            // consumer that has to parse prose out of stdout has no contract.
            return output::emit(&output::values_document("metrics", &current, &notes));
        }
        if current.is_empty() {
            println!("No metrics files found. Declare them under a stage's `metrics:`.");
            return Ok(());
        }
        output::print_values(&current);
        return Ok(());
    };

    repo.require_rev(rev)?;
    let other = metrics::read(repo.git(), repo.root(), &repo.pipeline, Some(rev))?;
    let rows = metrics::compare(&current, &other);
    if format.is_json() {
        return output::emit(&output::comparison_document(
            "metrics",
            &rows,
            "workspace",
            rev,
            true,
            None,
            &notes,
        ));
    }
    output::print_comparison(&rows, "workspace", rev);
    Ok(())
}

/// The parameter counterpart of `show_metrics`, and deliberately its mirror:
/// the two read the same way, compare the same way and lay out the same table,
/// because a parameter and a metric are the same shape to a comparison.
///
/// What it does *not* share is the verdict. `print_comparison` only calls a
/// change better or worse when the direction is judged, and a parameter has no
/// direction — see `print_params_comparison`.
fn show_params(cwd: &std::path::Path, compare: Option<&str>, format: Format) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let notes = notes_for(&repo, format);
    let current = params::read(repo.git(), repo.root(), &repo.pipeline, None)?;

    let Some(rev) = compare else {
        if format.is_json() {
            return output::emit(&output::values_document("params", &current, &notes));
        }
        if current.is_empty() {
            println!("No parameters declared. List the keys a stage reads under its `params:`.");
            return Ok(());
        }
        output::print_values(&current);
        return Ok(());
    };

    repo.require_rev(rev)?;
    let other = params::read(repo.git(), repo.root(), &repo.pipeline, Some(rev))?;
    let rows = metrics::compare(&current, &other);
    if format.is_json() {
        // judged: false. A parameter is a setting rather than a result, so the
        // document has no field for a verdict to go in.
        return output::emit(&output::comparison_document(
            "params",
            &rows,
            "workspace",
            rev,
            false,
            None,
            &notes,
        ));
    }
    output::print_params_comparison(&rows, "workspace", rev);
    Ok(())
}

fn show_plots(cwd: &std::path::Path, revisions: &[String], out: &str) -> Result<()> {
    let repo = Repo::open(cwd)?;
    // Only what the user named: `HEAD` is a default this command adds, and a
    // repository with no commits yet has none to resolve.
    for rev in revisions {
        if rev != plots::WORKSPACE {
            repo.require_rev(rev)?;
        }
    }
    let revs = plots::revisions(revisions);
    let figures = plots::build(&repo, &revs)?;

    if figures.is_empty() {
        println!("No plots declared. Add a `plots:` entry to a stage, or a top-level");
        println!("`plots:` section naming the files to draw.");
        return Ok(());
    }

    let path = plots::write_page(&repo, &figures, out)?;
    for figure in &figures {
        let detail = match &figure.figure {
            tsp_core::figure::Figure::Empty { reason } => format!(" — {reason}"),
            _ => String::new(),
        };
        println!("  {} ({}){detail}", figure.name, figure.template);
    }
    println!("\nComparing {}.", revs.join(" vs "));
    println!("file://{}", path.display());
    Ok(())
}

fn exp_run(cwd: &std::path::Path, set: &[String], name: Option<&str>, force: bool) -> Result<()> {
    let overrides: Vec<Override> = set
        .iter()
        .map(|s| s.parse())
        .collect::<std::result::Result<_, _>>()?;

    let mut repo = Repo::open(cwd)?;
    let experiment = exp::run(&mut repo, &overrides, name, force)?;

    println!(
        "\nRecorded {} ({})",
        experiment.name,
        &experiment.commit[..7]
    );
    let produced = exp::metrics_of(&repo, &experiment)?;
    let baseline = metrics::read(repo.git(), repo.root(), &repo.pipeline, Some("HEAD"))?;
    if !produced.is_empty() {
        println!();
        output::print_comparison(
            &metrics::compare(&produced, &baseline),
            &experiment.name,
            "HEAD",
        );
    }
    println!("\nApply it with `tsp exp apply {}`.", experiment.name);
    Ok(())
}

fn exp_list(cwd: &std::path::Path, format: Format) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let experiments = exp::list(&repo)?;

    if experiments.is_empty() && !format.is_json() {
        println!("No experiments yet. Run one with `tsp exp run --set key=value`.");
        return Ok(());
    }

    // Every metric is read once and kept. Projecting onto the columns has to
    // wait until they are all known — a key only the last experiment produced
    // is still a column for every row — but that is a second pass over what is
    // already in memory, not a second pass over git.
    let baseline = metrics::read(repo.git(), repo.root(), &repo.pipeline, Some("HEAD"))?;
    let mut read: Vec<(String, Vec<metrics::Metric>)> = Vec::with_capacity(experiments.len() + 1);
    read.push(("HEAD".to_owned(), baseline));
    for experiment in &experiments {
        read.push((experiment.name.clone(), exp::metrics_of(&repo, experiment)?));
    }

    // One column per metric key, so runs line up under the same headings.
    let mut keys: Vec<String> = Vec::new();
    for (_, produced) in &read {
        for metric in produced {
            if !keys.contains(&metric.key) {
                keys.push(metric.key.clone());
            }
        }
    }

    if format.is_json() {
        return output::emit(&output::exp_list_document(&keys, &read));
    }

    let resolved: Vec<(String, Vec<output::Metricish>)> = read
        .iter()
        .map(|(name, produced)| (name.clone(), output::values_for(&keys, produced)))
        .collect();

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

fn exp_show(cwd: &std::path::Path, name: &str, format: Format) -> Result<()> {
    let repo = Repo::open(cwd)?;
    let experiment = exp::find(&repo, name)?;
    let produced = exp::metrics_of(&repo, &experiment)?;
    let baseline = metrics::read(repo.git(), repo.root(), &repo.pipeline, Some("HEAD"))?;
    let rows = metrics::compare(&produced, &baseline);

    if format.is_json() {
        return output::emit(&output::comparison_document(
            "exp_show",
            &rows,
            &experiment.name,
            "HEAD",
            true,
            Some(output::ExperimentOut {
                name: &experiment.name,
                commit: &experiment.commit,
            }),
            &[],
        ));
    }

    println!("{}  {}", experiment.name, experiment.commit);
    if produced.is_empty() {
        println!("  (no metrics recorded)");
    } else {
        println!();
        output::print_comparison(&rows, &experiment.name, "HEAD");
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
