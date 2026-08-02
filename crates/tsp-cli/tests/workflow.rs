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

/// Setting a key nothing declares used to end at "nothing to run", which
/// blames the pipeline for what is a typo in the flag.
#[test]
fn an_override_naming_an_undeclared_parameter_is_refused() {
    let f = Fixture::new();

    let out = f.tsp(&["exp", "run", "--set", "train.no_such_key=3"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no_such_key"), "{stderr}");
    assert!(stderr.contains("params.yaml"), "{stderr}");
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

    // Nor should the directory show up as untracked: an ignore file that does
    // not ignore itself just swaps one stray path for another.
    let dirty = f.git(&["status", "--short"]);
    assert!(
        !dirty.contains("tsp_plots"),
        "the directory should leave no trace: {dirty}"
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

/// A realistic `dvc.lock`: md5 digests per output, which is the whole reason it
/// cannot be read here.
const DVC_LOCK: &str = "schema: '2.0'
stages:
  prepare:
    cmd: sh scripts/prepare.sh
    deps:
    - path: scripts/prepare.sh
      md5: 9f2f8f3d1b9a0c4e5d6a7b8c9d0e1f20
      size: 41
    outs:
    - path: data/prepared.txt
      md5: 1a2b3c4d5e6f708192a3b4c5d6e7f809
      size: 128
";

/// A DVC repository's every stage reports `new`, and until now nothing said
/// why. The verdict is correct — there is no record here any staleness check
/// could use — but a screen of `new` with no explanation reads as a broken
/// import rather than as a first run.
#[test]
fn a_dvc_lock_is_reported_as_unread_rather_than_silently_ignored() {
    let f = Fixture::new();
    f.write("dvc.lock", DVC_LOCK);

    let out = f.tsp(&["status"]);
    assert!(out.status.success(), "a dvc.lock is not an error");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("dvc.lock") && stderr.contains("tsp.lock"),
        "the note should name both files:\n{stderr}"
    );
    assert!(
        stderr.contains("content hash") && stderr.contains("object id"),
        "and say why one cannot stand in for the other:\n{stderr}"
    );
    // Nothing is wrong, so it must not be dressed up as something to fix.
    assert!(
        !stderr.contains("warning:"),
        "this is a note, not a warning:\n{stderr}"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("never run"),
        "the verdicts themselves are unchanged:\n{stdout}"
    );
}

/// The note is not a state file: it is the presence of one lock and the absence
/// of the other, so writing `tsp.lock` ends it.
#[test]
fn the_dvc_lock_note_stops_once_the_pipeline_has_run() {
    let f = Fixture::new();
    f.write("dvc.lock", DVC_LOCK);

    let out = f.tsp(&["repro"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("dvc.lock"),
        "the first repro explains itself"
    );

    let out = f.tsp(&["status"]);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("dvc.lock"),
        "and the next status has nothing left to explain"
    );
}

/// `dvc.lock` is not read, and it is not written, moved or removed either. A
/// repository that runs both tools keeps working with both.
#[test]
fn a_dvc_lock_is_never_read_or_rewritten() {
    let f = Fixture::new();
    f.write("dvc.lock", DVC_LOCK);

    f.tsp_ok(&["repro"]);

    assert_eq!(f.read("dvc.lock"), DVC_LOCK, "dvc.lock is left untouched");
    assert!(
        f.read("tsp.lock").contains("schema: 3"),
        "and the record tsp can use is written beside it"
    );
}

/// On a DVC repository the first `repro` rebuilds everything, so the reason has
/// to arrive while interrupting it is still worth doing.
#[test]
fn repro_explains_an_unread_dvc_lock_before_it_runs_anything() {
    let f = Fixture::new();
    f.write("dvc.lock", DVC_LOCK);

    let out = f.tsp(&["repro"]);
    assert!(out.status.success());

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stderr.contains("dvc.lock"), "{stderr}");
    // The note is on stderr and the run log on stdout, so a reader watching
    // either stream sees the explanation before or beside the work, never after.
    assert!(
        stdout.contains("prepare"),
        "the run still reports what it did:\n{stdout}"
    );
}

/// A metric only one experiment produced is still a column for every row.
///
/// The columns cannot be known until every experiment has been read, so the
/// projection onto them has to happen afterwards. Getting this wrong leaves the
/// baseline blank under any key it does not itself carry — which reads as "the
/// baseline scored nothing" rather than "this metric is new".
#[test]
fn a_metric_only_one_experiment_produced_is_a_column_for_every_row() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "baseline"]);

    // A second run whose train stage also writes a key the baseline never had.
    f.write_exec(
        "scripts/train.sh",
        &format!("{TRAIN}printf '{{\"extra\": 1}}\\n' > extra.json\n"),
    );
    f.write(
        "tsp.yaml",
        &PIPELINE.replace(
            "      - metrics.json:\n          cache: false\n",
            "      - metrics.json:\n          cache: false\n      - extra.json:\n          cache: false\n",
        ),
    );
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "measure one more thing"]);
    f.tsp_ok(&[
        "exp",
        "run",
        "--set",
        "train.factor=10",
        "--name",
        "tenfold",
    ]);

    let listed = f.tsp_ok(&["exp", "list"]);
    assert!(
        listed.contains("extra"),
        "the late key is a column:\n{listed}"
    );

    // `extra` is the last column, so the baseline's last cell is the one it
    // never wrote: a dash, not a blank the reader would take for a zero.
    let header = listed.lines().next().expect("a header row");
    assert_eq!(
        header.split_whitespace().last(),
        Some("extra"),
        "extra should be the late column:\n{listed}"
    );
    let head_row = listed
        .lines()
        .find(|l| l.split_whitespace().next() == Some("HEAD"))
        .expect("a baseline row");
    assert_eq!(
        head_row.split_whitespace().last(),
        Some("-"),
        "the baseline reads as absent under a key it never wrote:\n{listed}"
    );
}

/// The listing shows what staleness is decided from. A key sitting in the same
/// file that no stage declares is not a parameter of this pipeline, and showing
/// it would disagree with `tsp status` about what matters.
#[test]
fn params_are_listed_for_the_keys_stages_declare_and_no_others() {
    let f = Fixture::new();
    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  factor: 2\nunused:\n  knob: 99\n",
    );

    let out = f.tsp_ok(&["params"]);
    assert!(out.contains("prepare.repeat"), "{out}");
    assert!(out.contains("train.factor"), "{out}");
    assert!(
        !out.contains("knob"),
        "an undeclared key is not shown:\n{out}"
    );
}

#[test]
fn params_are_compared_against_a_revision() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "baseline"]);

    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  factor: 5\n",
    );

    let out = f.tsp_ok(&["params", "--compare", "HEAD"]);
    assert!(out.contains("PARAMETER"), "{out}");
    assert!(out.contains("train.factor"), "{out}");
    // 5 now against 2 then.
    assert!(out.contains('5') && out.contains('2'), "{out}");
    assert!(out.contains("+3"), "the delta is shown:\n{out}");
}

/// `metrics::compare` judges a row whenever the key's name implies a direction,
/// so a parameter named for a loss comes back marked improved. A parameter is a
/// setting, not a result, and calling one better is meaningless — the fixture
/// name here is chosen to make the trap live.
#[test]
fn a_parameter_change_is_not_called_better_or_worse() {
    let f = Fixture::new();
    f.write(
        "tsp.yaml",
        &PIPELINE.replace("          - train.factor", "          - train.loss_weight"),
    );
    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  loss_weight: 2\n",
    );
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "a parameter named for a loss"]);

    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  loss_weight: 1\n",
    );

    let out = f.tsp_ok(&["params", "--compare", "HEAD"]);
    assert!(out.contains("train.loss_weight"), "{out}");
    assert!(out.contains("-1"), "the delta is still shown:\n{out}");
    assert!(
        !out.contains("better") && !out.contains("worse"),
        "a parameter has no direction to be better in:\n{out}"
    );
}

/// The same reading, in a repository that declares none.
#[test]
fn params_says_so_when_a_pipeline_declares_none() {
    let f = Fixture::new();
    f.write(
        "tsp.yaml",
        "stages:\n  train:\n    cmd: sh scripts/train.sh\n",
    );

    let out = f.tsp_ok(&["params"]);
    assert!(out.contains("No parameters declared"), "{out}");
}

/// The document must not grow a second vocabulary. `status` and `status --json`
/// are the same verdicts, and the strings are the ones the conformance vectors
/// pin and the second implementation is tested against.
#[test]
fn status_json_uses_the_same_words_the_table_does() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);
    // One stage current, one stale.
    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  factor: 9\n",
    );

    let table = f.tsp_ok(&["status"]);
    let doc: serde_json::Value = serde_json::from_str(&f.tsp_ok(&["status", "--json"])).unwrap();

    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["kind"], "status");
    assert_eq!(doc["pipeline_file"], "tsp.yaml");

    for stage in doc["stages"].as_array().unwrap() {
        let label = stage["status"].as_str().unwrap();
        assert!(
            table.contains(label),
            "the table should use the same label {label:?}:\n{table}"
        );
        if let Some(reason) = stage["reason"].as_str() {
            assert!(
                table.contains(reason),
                "and the same reason {reason:?}:\n{table}"
            );
        }
    }

    let summary = &doc["summary"];
    assert_eq!(summary["total"], 2);
    assert_eq!(summary["needs_run"], 1);
    assert_eq!(summary["stale"], 1);
    assert_eq!(summary["current"], 1);
}

/// The note a person sees on stderr is a field a program can read.
#[test]
fn status_json_carries_the_dvc_note_a_terminal_would_have_shown() {
    let f = Fixture::new();
    f.write("dvc.lock", DVC_LOCK);

    let out = f.tsp(&["status", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let notes = doc["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{doc}");
    assert_eq!(notes[0]["code"], "dvc_lock_not_read");
    assert!(notes[0]["message"].as_str().unwrap().contains("dvc.lock"));

    // In JSON mode the prose does not also go to stderr: the document is the
    // whole answer, and duplicating it would double-report in a log.
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "stderr should be quiet in json mode: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn metrics_json_carries_both_the_value_and_its_rendering() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);

    let doc: serde_json::Value = serde_json::from_str(&f.tsp_ok(&["metrics", "--json"])).unwrap();
    assert_eq!(doc["kind"], "metrics");

    let accuracy = doc["values"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "accuracy")
        .expect("an accuracy value");
    assert_eq!(accuracy["file"], "metrics.json");
    assert_eq!(accuracy["value"], 12, "the raw scalar, for arithmetic");
    assert_eq!(accuracy["display"], "12", "and the rendering, for a table");
}

#[test]
fn metrics_json_reports_a_delta_and_whether_it_improved() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "baseline"]);
    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  factor: 10\n",
    );
    f.tsp_ok(&["repro"]);

    let doc: serde_json::Value =
        serde_json::from_str(&f.tsp_ok(&["metrics", "--compare", "HEAD", "--json"])).unwrap();
    assert_eq!(doc["current_label"], "workspace");
    assert_eq!(doc["compare_label"], "HEAD");

    let rows = doc["rows"].as_array().unwrap();
    let accuracy = rows.iter().find(|r| r["key"] == "accuracy").unwrap();
    assert_eq!(accuracy["improved"], true, "accuracy rose: {accuracy}");
    assert_eq!(accuracy["direction"], "higher_is_better");
    assert_eq!(accuracy["delta"], 48.0);
    assert_eq!(accuracy["delta_display"], "+48");

    // "lines" settles no direction, and that third state is not `false`.
    let lines = rows.iter().find(|r| r["key"] == "lines").unwrap();
    assert!(lines["improved"].is_null(), "{lines}");
    assert_eq!(lines["direction"], "unknown");
}

/// A parameter is a setting, not a result. The document has no field for a
/// verdict, so one cannot be published even though metrics::compare computes it.
#[test]
fn params_json_omits_the_verdict_a_metric_would_carry() {
    let f = Fixture::new();
    f.write(
        "tsp.yaml",
        &PIPELINE.replace("          - train.factor", "          - train.loss_weight"),
    );
    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  loss_weight: 2\n",
    );
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "a parameter named for a loss"]);
    f.write(
        "params.yaml",
        "prepare:\n  repeat: 3\ntrain:\n  loss_weight: 1\n",
    );

    let doc: serde_json::Value =
        serde_json::from_str(&f.tsp_ok(&["params", "--compare", "HEAD", "--json"])).unwrap();
    assert_eq!(doc["kind"], "params");

    let row = doc["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "train.loss_weight")
        .unwrap();
    let row = row.as_object().unwrap();
    assert!(!row.contains_key("improved"), "{row:?}");
    assert!(!row.contains_key("direction"), "{row:?}");
    assert_eq!(row["delta_display"], "-1", "the delta is still reported");
}

#[test]
fn exp_list_json_lines_experiments_up_under_the_same_metric_keys() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "baseline"]);
    f.tsp_ok(&[
        "exp",
        "run",
        "--set",
        "train.factor=10",
        "--name",
        "tenfold",
    ]);

    let doc: serde_json::Value =
        serde_json::from_str(&f.tsp_ok(&["exp", "list", "--json"])).unwrap();
    assert_eq!(doc["kind"], "exp_list");

    // Each column names its file as well as its key: two metrics files may use
    // the same dotted key for different measurements.
    let keys = doc["keys"].as_array().unwrap();
    let accuracy = keys
        .iter()
        .position(|k| k["key"] == "accuracy")
        .expect("an accuracy column");
    assert_eq!(keys[accuracy]["file"], "metrics.json");

    let rows = doc["rows"].as_array().unwrap();
    assert_eq!(rows[0]["name"], "HEAD");
    assert_eq!(rows[0]["baseline"], true);
    let tenfold = rows.iter().find(|r| r["name"] == "tenfold").unwrap();
    assert_eq!(tenfold["baseline"], false);
    // Cells are positional, matching keys.
    assert_eq!(tenfold["metrics"][accuracy]["value"], 60);
    assert_eq!(tenfold["metrics"][accuracy]["display"], "60");
}

/// Two metrics files carrying the same dotted key are two measurements. Keying
/// a column on the name alone collapsed them, and the first match won — so one
/// file's number was reported under both headings.
#[test]
fn two_metrics_files_sharing_a_key_are_two_columns() {
    let f = Fixture::new();
    f.write_exec(
        "scripts/train.sh",
        &format!(
            "{TRAIN}printf '{{\"accuracy\": 1}}\n' > other.json
"
        ),
    );
    f.write(
        "tsp.yaml",
        &PIPELINE.replace(
            "      - metrics.json:\n          cache: false\n",
            "      - metrics.json:\n          cache: false\n      - other.json:\n          cache: false\n",
        ),
    );
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "two metrics files"]);
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qam", "baseline"]);
    f.tsp_ok(&[
        "exp",
        "run",
        "--set",
        "train.factor=10",
        "--name",
        "tenfold",
    ]);

    let doc: serde_json::Value =
        serde_json::from_str(&f.tsp_ok(&["exp", "list", "--json"])).unwrap();
    let keys = doc["keys"].as_array().unwrap();
    let accuracy: Vec<&serde_json::Value> =
        keys.iter().filter(|k| k["key"] == "accuracy").collect();
    assert_eq!(accuracy.len(), 2, "one column per file: {doc}");

    // And the table qualifies both headings, since the bare name is ambiguous.
    let listed = f.tsp_ok(&["exp", "list"]);
    assert!(listed.contains("metrics.json:accuracy"), "{listed}");
    assert!(listed.contains("other.json:accuracy"), "{listed}");
}

#[test]
fn exp_show_json_names_the_experiment_and_its_commit() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "baseline"]);
    f.tsp_ok(&[
        "exp",
        "run",
        "--set",
        "train.factor=10",
        "--name",
        "tenfold",
    ]);

    let doc: serde_json::Value =
        serde_json::from_str(&f.tsp_ok(&["exp", "show", "tenfold", "--json"])).unwrap();
    assert_eq!(doc["kind"], "exp_show");
    assert_eq!(doc["experiment"]["name"], "tenfold");
    assert_eq!(
        doc["experiment"]["commit"].as_str().unwrap().len(),
        40,
        "the full sha, not the short one"
    );
    assert_eq!(doc["current_label"], "tenfold");
    assert_eq!(doc["compare_label"], "HEAD");
}

/// The single most common way a --json flag gets it wrong: printing prose on
/// stdout when there is nothing to report, so a consumer's parse fails on the
/// empty case and only on the empty case.
#[test]
fn an_empty_result_is_still_a_valid_json_document() {
    let f = Fixture::new();
    // A pipeline that declares no metrics, no params and no experiments.
    f.write(
        "tsp.yaml",
        "stages:\n  train:\n    cmd: sh scripts/train.sh\n",
    );

    for args in [
        vec!["metrics", "--json"],
        vec!["params", "--json"],
        vec!["exp", "list", "--json"],
    ] {
        let out = f.tsp_ok(&args);
        let doc: serde_json::Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("tsp {args:?} did not emit a document: {e}\n{out}"));
        assert_eq!(doc["schema"], 1, "tsp {args:?}: {doc}");
    }
}

/// stdout carries the document and nothing else, whatever else is going on.
#[test]
fn json_output_is_the_only_thing_on_stdout() {
    let f = Fixture::new();
    f.tsp_ok(&["repro"]);
    f.git(&["commit", "-qm", "run"]);
    // A dvc.lock, so there is a note competing for the reader's attention.
    f.write("dvc.lock", DVC_LOCK);

    for args in [
        vec!["status", "--json"],
        vec!["metrics", "--json"],
        vec!["metrics", "--compare", "HEAD", "--json"],
        vec!["params", "--json"],
        vec!["params", "--compare", "HEAD", "--json"],
        vec!["exp", "list", "--json"],
    ] {
        let out = f.tsp_ok(&args);
        serde_json::from_str::<serde_json::Value>(&out)
            .unwrap_or_else(|e| panic!("tsp {args:?} put something else on stdout: {e}\n{out}"));
    }
}
