//! End-to-end tests for the pipeline and experiment commands.
//!
//! The fixture uses Git LFS exactly as a user would — `git lfs track`, then
//! ordinary `git add` — so these also pin the assumption the whole tool now
//! rests on: that git and git-lfs handle the data by themselves.

use std::path::PathBuf;
use std::process::Command;

const TSP: &str = env!("CARGO_BIN_EXE_tsp");

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// A repository with LFS configured, a two-stage pipeline, and one commit.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let f = Self { _dir: dir, root };

        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "tsp@example.test"],
            vec!["config", "user.name", "tsp test"],
        ] {
            f.git(&args);
        }

        f.write("data/raw.txt", "alpha\nbeta\n");
        f.write(
            "params.yaml",
            "prepare:\n  repeat: 3\ntrain:\n  factor: 2\n",
        );
        f.write("tsp.yaml", PIPELINE);
        f.write_exec("scripts/prepare.sh", PREPARE);
        f.write_exec("scripts/train.sh", TRAIN);

        f.tsp_ok(&["init", "--lfs", "data/**", "--lfs", "models/**"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "pipeline"]);
        f
    }

    fn tsp(&self, args: &[&str]) -> std::process::Output {
        Command::new(TSP)
            .current_dir(&self.root)
            .args(args)
            .output()
            .unwrap()
    }

    fn tsp_ok(&self, args: &[&str]) -> String {
        let out = self.tsp(args);
        assert!(
            out.status.success(),
            "tsp {args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
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

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
    }

    fn write_exec(&self, rel: &str, content: &str) {
        self.write(rel, content);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = self.root.join(rel);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn metrics(&self) -> serde_json::Value {
        serde_json::from_str(&self.read("metrics.json")).unwrap()
    }
}

const PIPELINE: &str = r#"
stages:
  prepare:
    desc: Repeat the raw rows
    cmd: sh scripts/prepare.sh
    deps:
      - scripts/prepare.sh
      - data/raw.txt
    params:
      - params.yaml:
          - prepare.repeat
    outs:
      - data/prepared.txt
  train:
    cmd: sh scripts/train.sh
    deps:
      - scripts/train.sh
      - data/prepared.txt
    params:
      - params.yaml:
          - train.factor
    outs:
      - models/model.txt
    metrics:
      - metrics.json:
          cache: false
"#;

const PREPARE: &str = r#"#!/bin/sh
set -e
mkdir -p data
n=$(awk '/repeat:/ {print $2}' params.yaml)
: > data/prepared.txt
i=0
while [ "$i" -lt "$n" ]; do
    cat data/raw.txt >> data/prepared.txt
    i=$((i + 1))
done
"#;

const TRAIN: &str = r#"#!/bin/sh
set -e
mkdir -p models
factor=$(awk '/factor:/ {print $2}' params.yaml)
lines=$(wc -l < data/prepared.txt | tr -d ' ')
score=$((lines * factor))
printf 'model score=%s\n' "$score" > models/model.txt
printf '{"accuracy": %s, "lines": %s}\n' "$score" "$lines" > metrics.json
"#;

#[test]
fn init_configures_lfs_and_the_guard_hook() {
    let f = Fixture::new();

    let attributes = f.read(".gitattributes");
    assert!(attributes.contains("data/**"), "{attributes}");
    assert!(attributes.contains("filter=lfs"), "{attributes}");

    let hook = std::fs::read_to_string(f.root.join(".git/hooks/pre-commit")).unwrap();
    assert!(hook.contains("tsp-guard"), "{hook}");

    // git-lfs owns the transfer hooks; tsp must not have replaced them.
    assert!(f.root.join(".git/hooks/pre-push").exists());
}

/// A hook the user wrote is not ours to remove.
#[test]
fn init_leaves_a_foreign_pre_push_hook_alone() {
    let f = Fixture::new();
    std::fs::remove_file(f.root.join(".git/hooks/pre-push")).unwrap();
    f.write(".git/hooks/pre-push", "#!/bin/sh\necho mine\n");

    // git-lfs refuses to overwrite it too, so init surfaces that rather than
    // pretending the repository is ready.
    let out = f.tsp(&["init"]);
    assert!(!out.status.success());
    assert_eq!(
        std::fs::read_to_string(f.root.join(".git/hooks/pre-push")).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
}

/// The whole premise: data reaches git as a pointer without tsp touching it.
#[test]
fn data_becomes_a_pointer_through_the_lfs_filter_alone() {
    let f = Fixture::new();
    let committed = f.git(&["cat-file", "-p", "HEAD:data/raw.txt"]);
    assert!(
        committed.starts_with("version https://git-lfs.github.com/spec/v1\n"),
        "{committed}"
    );
    assert_eq!(f.read("data/raw.txt"), "alpha\nbeta\n");
}

#[test]
fn repro_runs_stages_in_dependency_order_and_writes_the_lock() {
    let f = Fixture::new();
    let out = f.tsp_ok(&["repro"]);

    let prepare = out.find("==> prepare").expect("prepare should run");
    let train = out.find("==> train").expect("train should run");
    assert!(prepare < train, "producers run first:\n{out}");

    assert_eq!(f.metrics()["lines"], 6);
    assert_eq!(f.metrics()["accuracy"], 12);

    let lock = f.read("tsp.lock");
    assert!(lock.contains("schema: 3"), "{lock}");
    assert!(lock.contains("prepare"), "{lock}");
    // A dependency is one line: the path, and the id of what it held.
    let expected = f.git(&["hash-object", "scripts/prepare.sh"]);
    assert!(
        lock.contains(&format!("scripts/prepare.sh: {}", expected.trim())),
        "{lock}"
    );
}

/// A lock from a future schema is discarded whole, never read field by field.
///
/// This is what makes `schema:` load-bearing rather than decorative: adding a
/// field is safe for older versions precisely because they never see one. If
/// this version instead parsed what it recognised and dropped the rest, the
/// first `tsp repro` run by an older binary would silently delete the newer
/// one's work — and the lock would still look plausible afterwards.
#[test]
fn a_lock_from_a_newer_schema_is_discarded_rather_than_partly_read() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    // A schema this binary does not write, carrying a field it cannot know.
    f.write(
        "tsp.lock",
        "schema: 4\nstages:\n  prepare:\n    cmd: sh scripts/prepare.sh\n    \
         outs_digest: deadbeef\n",
    );

    let out = f.tsp(&["status"]);
    assert!(
        out.status.success(),
        "an unreadable lock is not a hard error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("schema 4") && stderr.contains("tsp.lock"),
        "the warning should name the file and the schema it found:\n{stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("2 need running"),
        "every stage reads as new, not just the one the foreign lock named:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // And the next run replaces it outright: no trace of the field survives.
    f.tsp_ok(&["repro"]);
    let lock = f.read("tsp.lock");
    assert!(lock.contains("schema: 3"), "{lock}");
    assert!(!lock.contains("outs_digest"), "{lock}");
}

/// An LFS-tracked dependency is recorded by its *pointer's* object id, never by
/// hashing the data behind it — which is what makes locking a large input cost
/// a blob read rather than a pass over gigabytes.
#[test]
fn an_lfs_dependency_is_locked_by_its_pointer_id() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);

    let pointer_id = f.git(&["rev-parse", ":data/prepared.txt"]);
    let pointer_id = pointer_id.trim();
    // --no-filters is what makes this the hash of the bytes themselves; without
    // it git applies the path's clean filter and hands back the pointer id.
    let content_hash = f.git(&["hash-object", "--no-filters", "data/prepared.txt"]);
    let content_hash = content_hash.trim();
    assert_ne!(pointer_id, content_hash, "the fixture must be LFS-tracked");

    let lock = f.read("tsp.lock");
    assert!(
        lock.contains(&format!("data/prepared.txt: {pointer_id}")),
        "expected the pointer id {pointer_id}:\n{lock}"
    );
    assert!(
        !lock.contains(content_hash),
        "the data itself is not hashed"
    );
}

/// A stage is current the moment it has been reproduced, before anything is
/// committed. This is the state git cannot describe on its own — the content a
/// run consumed is not in any commit yet — and it is why the lock exists.
#[test]
fn a_stage_is_current_as_soon_as_it_has_run() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    f.write_exec("scripts/train.sh", &format!("{TRAIN}# tweaked\n"));
    assert!(
        f.tsp_ok(&["status"]).contains("scripts/train.sh changed"),
        "the edit should register before it is run"
    );

    f.tsp_ok(&["repro"]);

    let out = f.tsp_ok(&["status"]);
    assert!(
        out.contains("0 need running"),
        "nothing is committed yet, but the run happened:\n{out}"
    );
}

/// The flow this has to survive: edit a script, reproduce, commit. The lock
/// records what the run consumed, so committing that same content must not
/// leave the stage reading stale.
#[test]
fn a_stage_stays_current_after_committing_the_edit_that_ran_it() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    // Edited but not staged, which is exactly how a script is when tsp runs it.
    f.write_exec("scripts/train.sh", &format!("{TRAIN}# tweaked\n"));
    f.tsp_ok(&["repro"]);

    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "retune"]);

    let out = f.tsp_ok(&["status"]);
    assert!(
        out.contains("0 need running"),
        "the committed run should be current:\n{out}"
    );
}

#[test]
fn repro_is_a_noop_once_everything_is_current() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    let out = f.tsp_ok(&["repro"]);
    assert!(out.contains("up to date"), "{out}");
    assert!(!out.contains("==> train"), "{out}");
}

#[test]
fn status_explains_why_a_stage_is_stale() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    f.write_exec("scripts/train.sh", &format!("{TRAIN}# tweaked\n"));

    let out = f.tsp_ok(&["status"]);
    assert!(out.contains("prepare"), "{out}");
    assert!(out.contains("current"), "{out}");
    assert!(
        out.contains("scripts/train.sh changed"),
        "the reason should name the dependency:\n{out}"
    );
}

/// A parameter change is the case experiments turn on, so it must register even
/// though no file a stage lists as a dependency moved.
#[test]
fn a_changed_parameter_makes_its_stage_stale() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  factor: 5\n",
    );

    let out = f.tsp_ok(&["status"]);
    assert!(out.contains("parameter train.factor changed"), "{out}");
}

#[test]
fn rerunning_an_upstream_stage_reruns_what_depends_on_it() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    f.write(
        "params.yaml",
        "prepare:\n  repeat: 5\ntrain:\n  factor: 2\n",
    );
    let out = f.tsp_ok(&["repro"]);

    assert!(out.contains("==> prepare"), "{out}");
    assert!(
        out.contains("==> train"),
        "train must follow prepare:\n{out}"
    );
    assert_eq!(f.metrics()["lines"], 10);
}

#[test]
fn repro_can_be_limited_to_one_stage_and_its_ancestors() {
    let f = Fixture::new();
    let out = f.tsp_ok(&["repro", "prepare"]);

    assert!(out.contains("==> prepare"), "{out}");
    assert!(!out.contains("==> train"), "train is downstream:\n{out}");
    assert!(!f.root.join("models/model.txt").exists());
}

#[test]
fn an_unknown_stage_is_refused() {
    let f = Fixture::new();
    let out = f.tsp(&["repro", "nope"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nope"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_failing_stage_stops_the_run() {
    let f = Fixture::new();
    f.write_exec("scripts/train.sh", "#!/bin/sh\nexit 3\n");

    let out = f.tsp(&["repro"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("train"), "{stderr}");
    assert!(
        stderr.contains('3'),
        "the exit code should surface: {stderr}"
    );
}

/// A command that exits 0 without writing its output used to leave a lock entry
/// tracking nothing, and the stage read as current from then on.
#[test]
fn a_stage_that_skips_its_declared_output_stops_the_run() {
    let f = Fixture::new();
    f.write_exec("scripts/train.sh", "#!/bin/sh\nexit 0\n");

    let out = f.tsp(&["repro"]);
    assert!(!out.status.success(), "a missing output must fail the run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("train"), "names the stage: {stderr}");
    assert!(
        stderr.contains("models/model.txt"),
        "names the missing path: {stderr}"
    );

    let status = f.tsp_ok(&["status"]);
    assert!(
        !status.contains("current"),
        "the stage must not read as current: {status}"
    );
}

/// A name is refused on its spelling alone, so checking it after the run costs
/// the whole run to learn something knowable before it started.
#[test]
fn a_bad_experiment_name_is_refused_before_anything_runs() {
    let f = Fixture::new();

    let out = f.tsp(&["exp", "run", "--set", "train.factor=9", "--name", "my exp"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("==> train"),
        "the pipeline must not have run: {stdout}"
    );
}

#[test]
fn an_experiment_name_is_not_reused_without_force() {
    let f = Fixture::new();
    f.tsp_ok(&["exp", "run", "--set", "train.factor=9", "--name", "tuned"]);

    let out = f.tsp(&["exp", "run", "--set", "train.factor=11", "--name", "tuned"]);
    assert!(!out.status.success(), "a reused name must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--force"), "{stderr}");

    f.tsp_ok(&[
        "exp",
        "run",
        "--set",
        "train.factor=11",
        "--name",
        "tuned",
        "--force",
    ]);
}

/// The README's own quickstart ends in `git add -A`, so a generated page that
/// is not ignored lands in history the first time anyone follows it.
#[test]
fn the_plots_page_is_generated_and_stays_out_of_git() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.write("tsp.yaml", &format!("{PIPELINE}plots:\n  - metrics.json\n"));
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "run"]);

    let out = f.tsp_ok(&["plots"]);
    assert!(out.contains("tsp_plots"), "writes under tsp_plots: {out}");

    f.git(&["add", "-A"]);
    let tracked = String::from_utf8(
        Command::new("git")
            .current_dir(&f.root)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        !tracked.contains("index.html"),
        "the page must not be staged: {tracked}"
    );
}

/// Reading a file at a revision cannot tell a missing file from a missing
/// revision, so a typo used to render as a plausible, empty comparison.
#[test]
fn an_unknown_revision_is_refused_rather_than_compared_against_nothing() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    for args in [
        vec!["metrics", "--compare", "no-such-rev"],
        vec!["plots", "no-such-rev"],
    ] {
        let out = f.tsp(&args);
        assert!(!out.status.success(), "{args:?} should fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("no-such-rev"), "{stderr}");
    }
}

#[test]
fn metrics_are_listed_and_compared() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    let listed = f.tsp_ok(&["metrics"]);
    assert!(listed.contains("accuracy"), "{listed}");
    assert!(listed.contains("12"), "{listed}");

    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  factor: 3\n",
    );
    f.tsp_ok(&["repro"]);

    let compared = f.tsp_ok(&["metrics", "--compare", "HEAD"]);
    assert!(compared.contains("accuracy"), "{compared}");
    assert!(compared.contains("+6"), "delta should show:\n{compared}");
    assert!(
        compared.contains("better"),
        "more accuracy is better:\n{compared}"
    );
}

#[test]
fn an_experiment_is_recorded_as_a_ref_and_leaves_no_trace() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    let head_before = f.git(&["rev-parse", "HEAD"]);
    let params_before = f.read("params.yaml");

    let out = f.tsp_ok(&["exp", "run", "--set", "train.factor=10"]);
    assert!(out.contains("Recorded"), "{out}");

    // The branch did not move and the tree is back as it was.
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head_before);
    assert_eq!(f.read("params.yaml"), params_before);
    assert_eq!(f.metrics()["accuracy"], 12, "workspace metrics restored");
    assert!(f.git(&["status", "--porcelain"]).trim().is_empty());

    // But the experiment is a real commit under its own ref.
    let refs = f.git(&["for-each-ref", "--format=%(refname)", "refs/tsp/exps"]);
    assert_eq!(refs.lines().count(), 1, "{refs}");
    assert!(!f.git(&["branch", "--list"]).contains("exp-"));
}

#[test]
fn an_experiment_records_the_metrics_its_overrides_produced() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);
    f.tsp_ok(&[
        "exp",
        "run",
        "--set",
        "train.factor=10",
        "--name",
        "tenfold",
    ]);

    let listed = f.tsp_ok(&["exp", "list"]);
    assert!(listed.contains("tenfold"), "{listed}");
    assert!(
        listed.contains("HEAD"),
        "the baseline should be a row:\n{listed}"
    );
    assert!(listed.contains("60"), "6 lines x factor 10:\n{listed}");

    let shown = f.tsp_ok(&["exp", "show", "tenfold"]);
    assert!(shown.contains("accuracy"), "{shown}");
    assert!(shown.contains("better"), "{shown}");
}

#[test]
fn applying_an_experiment_brings_its_parameters_into_the_tree() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);
    f.tsp_ok(&[
        "exp",
        "run",
        "--set",
        "train.factor=10",
        "--name",
        "tenfold",
    ]);

    f.tsp_ok(&["exp", "apply", "tenfold"]);

    assert!(
        f.read("params.yaml").contains("factor: 10"),
        "{}",
        f.read("params.yaml")
    );
    assert_eq!(f.metrics()["accuracy"], 60);
}

#[test]
fn experiments_can_be_removed() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);
    f.tsp_ok(&[
        "exp",
        "run",
        "--set",
        "train.factor=10",
        "--name",
        "tenfold",
    ]);

    f.tsp_ok(&["exp", "remove", "tenfold"]);
    assert!(f.tsp_ok(&["exp", "list"]).contains("No experiments"));

    let missing = f.tsp(&["exp", "remove", "tenfold"]);
    assert!(!missing.status.success());
}

/// An experiment must not fold the user's in-progress edits into its result,
/// and must not discard them either.
#[test]
fn an_experiment_refuses_a_dirty_tree() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);

    f.write(
        "params.yaml",
        "prepare:\n  repeat: 9\ntrain:\n  factor: 2\n",
    );

    let out = f.tsp(&["exp", "run", "--set", "train.factor=10"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("uncommitted"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        f.read("params.yaml").contains("repeat: 9"),
        "the refused run must leave the edit alone"
    );
}

/// The guard is the only thing left catching a large file no pattern covers.
#[test]
fn the_guard_hook_refuses_an_untracked_large_blob() {
    let f = Fixture::new();
    f.write("sneaky.bin", &"x".repeat(2_000_000));
    f.git(&["add", "-f", "sneaky.bin"]);

    let out = Command::new("git")
        .current_dir(&f.root)
        .args(["commit", "-m", "oops"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not an LFS pointer"), "{stderr}");
    assert!(stderr.contains("git lfs track"), "{stderr}");
}

#[test]
fn commands_outside_a_repository_fail_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(TSP)
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
