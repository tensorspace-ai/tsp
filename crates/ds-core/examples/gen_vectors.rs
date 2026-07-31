//! Emit cross-language conformance vectors for the Go pipeline/lock parsers.
//!
//! `ds.yaml` and `ds.lock` have two implementations: this crate writes them,
//! and Gitea's `modules/ds` reads them to draw the DAG. A disagreement between
//! the two does not crash anything — it renders a lineage graph that is quietly
//! wrong, which is the worst failure this format has. So the cases below are
//! generated with the Rust interpretation attached, and a Go test replays them
//! and asserts it reaches the same reading.
//!
//! What is compared is a *projection*, not the structs: each side carries
//! fields the other has no use for (Go tracks `foreach` and `vars`, Rust tracks
//! plot axis labels), and forcing those into the contract would make the
//! vectors fail on differences that do not matter. The projection is what both
//! implementations must agree on for the DAG and the staleness of a stage to
//! come out the same.
//!
//! Run: `cargo run -p ds-core --example gen_vectors > tests/vectors.json`
//! Every value in the output is derived from the inputs, so regenerating on an
//! unchanged tree produces a byte-identical file.

use ds_core::graph::{self, Resolver};
use ds_core::lock::Lock;
use ds_core::pipeline::{Artifact, Pipeline, Stage};
use serde_json::{Value, json};

fn main() {
    let mut cases: Vec<Value> = Vec::new();

    for (name, input) in pipeline_cases() {
        cases.push(match Pipeline::parse(input, "ds.yaml") {
            Ok(p) => json!({
                "name": name,
                "kind": "pipeline",
                "input": input,
                "expected": project_pipeline(&p),
            }),
            // A rejection is as much a part of the contract as a reading: the
            // Go side bounds the same inputs, and a parser that accepts what
            // the other refuses is the drift this is meant to catch.
            Err(_) => json!({
                "name": name,
                "kind": "pipeline",
                "input": input,
                "rejected": true,
            }),
        });
    }

    for (name, input) in lock_cases() {
        cases.push(match Lock::parse(input, "ds.lock") {
            Ok(lock) if lock.schema == ds_core::lock::SCHEMA => json!({
                "name": name,
                "kind": "lock",
                "input": input,
                "expected": project_lock(&lock),
            }),
            _ => json!({
                "name": name,
                "kind": "lock",
                "input": input,
                "rejected": true,
            }),
        });
    }

    for case in staleness_cases() {
        cases.push(project_staleness(case));
    }

    println!("{}", serde_json::to_string_pretty(&cases).unwrap());
}

/// One staleness scenario: a pipeline, the lock a run left, and what the
/// repository holds now.
struct Staleness {
    name: &'static str,
    pipeline: &'static str,
    lock: &'static str,
    /// Each dependency's current git object id. A path that is absent from this
    /// list does not exist at the viewed commit.
    deps: Vec<(&'static str, &'static str)>,
    /// Each parameter's current value, by file and dotted key, already
    /// flattened — what a params file resolves to, not the file itself, so a
    /// staleness case fails on staleness rather than on parsing.
    params: Vec<(&'static str, &'static str, Value)>,
}

/// Emits every stage's verdict, which is the part the two implementations have
/// to agree on and the part nothing was watching.
///
/// The parsers agreed on `ds.lock` for months while disagreeing about what it
/// meant: the CLI compared the parameter values the lock records and the server
/// did not, so retuning a model left the CLI saying stale and the Data tab
/// saying current about the same commit. Parsing vectors cannot catch that.
/// These can.
fn project_staleness(case: Staleness) -> Value {
    let pipeline = Pipeline::parse(case.pipeline, "ds.yaml").expect("fixture pipeline parses");
    let lock = Lock::parse(case.lock, "ds.lock").expect("fixture lock parses");

    let git_sha_of = |path: &str| {
        case.deps
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, sha)| (*sha).to_owned())
    };
    let param_of = |file: &str, key: &str| {
        case.params
            .iter()
            .find(|(f, k, _)| *f == file && *k == key)
            .map(|(_, _, v)| v.clone())
    };
    let now = Resolver {
        git_sha_of: &git_sha_of,
        param_of: &param_of,
    };

    let stages: Vec<Value> = pipeline
        .stages
        .keys()
        .map(|name| {
            let status = graph::status_of(&pipeline, &lock, name, &now);
            json!({
                "name": name,
                "status": status.label(),
                "reason": status.reason().unwrap_or_default(),
            })
        })
        .collect();

    json!({
        "name": case.name,
        "kind": "staleness",
        "input": case.pipeline,
        "lock": case.lock,
        "deps": case.deps.iter().map(|(p, s)| json!({"path": p, "sha": s})).collect::<Vec<_>>(),
        "params": case.params.iter()
            .map(|(f, k, v)| json!({"file": f, "key": k, "value": v}))
            .collect::<Vec<_>>(),
        "expected": { "stages": stages },
    })
}

const TUNED_PIPELINE: &str = "stages:\n  train:\n    cmd: python train.py\n    deps:\n      - src/train.py\n    params:\n      - params.yaml:\n          - train.max_depth\n          - train.seed\n    outs:\n      - models/model.pkl\n  report:\n    cmd: python report.py\n    params:\n      - params.yaml:\n          - report.title\n";

const TUNED_LOCK: &str = "schema: 3\nstages:\n  train:\n    cmd: python train.py\n    params:\n      params.yaml:\n        train.max_depth: 4\n        train.seed: 42\n    deps:\n      src/train.py: aaaa\n  report:\n    cmd: python report.py\n    params:\n      params.yaml:\n        report.title: Results\n";

fn staleness_cases() -> Vec<Staleness> {
    vec![
        Staleness {
            name: "everything matches",
            pipeline: TUNED_PIPELINE,
            lock: TUNED_LOCK,
            deps: vec![("src/train.py", "aaaa")],
            params: vec![
                ("params.yaml", "train.max_depth", json!(4)),
                ("params.yaml", "train.seed", json!(42)),
                ("params.yaml", "report.title", json!("Results")),
            ],
        },
        // The case that was wrong: nothing the stage depends on moved, only a
        // value inside a file it shares with another stage.
        Staleness {
            name: "a parameter changed",
            pipeline: TUNED_PIPELINE,
            lock: TUNED_LOCK,
            deps: vec![("src/train.py", "aaaa")],
            params: vec![
                ("params.yaml", "train.max_depth", json!(8)),
                ("params.yaml", "train.seed", json!(42)),
                ("params.yaml", "report.title", json!("Results")),
            ],
        },
        // And the other half of it: the file is shared, so the stage that does
        // not read the changed key must stay current.
        Staleness {
            name: "a sibling's parameter changed",
            pipeline: TUNED_PIPELINE,
            lock: TUNED_LOCK,
            deps: vec![("src/train.py", "aaaa")],
            params: vec![
                ("params.yaml", "train.max_depth", json!(4)),
                ("params.yaml", "train.seed", json!(42)),
                ("params.yaml", "report.title", json!("Rewritten")),
            ],
        },
        Staleness {
            name: "a parameter is gone",
            pipeline: TUNED_PIPELINE,
            lock: TUNED_LOCK,
            deps: vec![("src/train.py", "aaaa")],
            params: vec![
                ("params.yaml", "train.seed", json!(42)),
                ("params.yaml", "report.title", json!("Results")),
            ],
        },
        Staleness {
            name: "a dependency changed",
            pipeline: TUNED_PIPELINE,
            lock: TUNED_LOCK,
            deps: vec![("src/train.py", "bbbb")],
            params: vec![
                ("params.yaml", "train.max_depth", json!(4)),
                ("params.yaml", "train.seed", json!(42)),
                ("params.yaml", "report.title", json!("Results")),
            ],
        },
        Staleness {
            name: "a dependency is missing",
            pipeline: TUNED_PIPELINE,
            lock: TUNED_LOCK,
            deps: vec![],
            params: vec![
                ("params.yaml", "train.max_depth", json!(4)),
                ("params.yaml", "train.seed", json!(42)),
                ("params.yaml", "report.title", json!("Results")),
            ],
        },
        Staleness {
            name: "the command changed",
            pipeline: TUNED_PIPELINE,
            lock: "schema: 3\nstages:\n  train:\n    cmd: python OLD.py\n    params:\n      params.yaml:\n        train.max_depth: 4\n        train.seed: 42\n    deps:\n      src/train.py: aaaa\n",
            deps: vec![("src/train.py", "aaaa")],
            params: vec![
                ("params.yaml", "train.max_depth", json!(4)),
                ("params.yaml", "train.seed", json!(42)),
            ],
        },
        Staleness {
            name: "never run",
            pipeline: TUNED_PIPELINE,
            lock: "schema: 3\nstages: {}\n",
            deps: vec![("src/train.py", "aaaa")],
            params: vec![("params.yaml", "train.max_depth", json!(4))],
        },
        // A lock that recorded no parameters cannot vouch for the ones the
        // stage reads now, so each of them is new to it.
        Staleness {
            name: "the run recorded no parameters",
            pipeline: TUNED_PIPELINE,
            lock: "schema: 3\nstages:\n  train:\n    cmd: python train.py\n    deps:\n      src/train.py: aaaa\n",
            deps: vec![("src/train.py", "aaaa")],
            params: vec![
                ("params.yaml", "train.max_depth", json!(4)),
                ("params.yaml", "train.seed", json!(42)),
            ],
        },
    ]
}

/// The part of a parsed pipeline both implementations must read alike.
fn project_pipeline(p: &Pipeline) -> Value {
    json!({
        "stages": p.stages.iter().map(|(name, stage)| json!({
            "name": name,
            "stage": project_stage(stage),
        })).collect::<Vec<_>>(),
        "plots": p.plots.iter().map(|plot| json!({
            "name": plot.name,
            "template": plot.template.as_str(),
            "files": plot.files(),
        })).collect::<Vec<_>>(),
    })
}

fn project_stage(s: &Stage) -> Value {
    json!({
        // Joined rather than listed, because a one-line `cmd` and a one-element
        // sequence are the same command and must not read as different stages.
        "cmd": s.command_text(),
        "wdir": s.wdir.clone().unwrap_or_default(),
        "deps": s.deps.0,
        "outs": s.outs.iter().map(project_artifact).collect::<Vec<_>>(),
        "metrics": s.metrics.iter().map(project_artifact).collect::<Vec<_>>(),
        "plots": s.plots.iter().map(|p| json!({
            "path": p.artifact.path,
            "cache": p.artifact.cache,
            "template": p.plot.template.as_str(),
            "files": p.plot.files(),
        })).collect::<Vec<_>>(),
        "params": s.params.iter().map(|p| json!({
            "file": p.file,
            "keys": p.keys,
        })).collect::<Vec<_>>(),
        "desc": s.desc.clone().unwrap_or_default(),
    })
}

fn project_artifact(a: &Artifact) -> Value {
    json!({ "path": a.path, "cache": a.cache, "persist": a.persist })
}

fn project_lock(lock: &Lock) -> Value {
    json!({
        "schema": lock.schema,
        "stages": lock.stages.iter().map(|(name, stage)| json!({
            "name": name,
            "cmd": stage.cmd,
            "params": stage.params,
            "deps": stage.deps,
        })).collect::<Vec<_>>(),
    })
}

/// Pipeline inputs, chosen for the spellings a reader can get wrong rather than
/// for coverage of the happy path.
fn pipeline_cases() -> Vec<(&'static str, &'static str)> {
    vec![
        ("empty document", ""),
        ("no stages", "stages: {}\n"),
        (
            "minimal stage",
            "stages:\n  train:\n    cmd: python train.py\n",
        ),
        // `cmd` is a scalar or a sequence; DVC accepts both, so files in the
        // wild use both, and the two must join to the same command.
        (
            "cmd as a sequence",
            "stages:\n  train:\n    cmd:\n      - python a.py\n      - python b.py\n",
        ),
        (
            "deps as a scalar",
            "stages:\n  train:\n    cmd: run\n    deps: src/train.py\n",
        ),
        (
            "outs as bare paths",
            "stages:\n  train:\n    cmd: run\n    outs:\n      - models/model.pkl\n      - data/out.csv\n",
        ),
        // The single-key mapping spelling, and the `cache: false` that decides
        // whether a path is expected to be an LFS pointer.
        (
            "outs as mappings with options",
            "stages:\n  train:\n    cmd: run\n    outs:\n      - models/model.pkl:\n          cache: false\n      - data/keep.csv:\n          persist: true\n",
        ),
        (
            "metrics with cache false",
            "stages:\n  eval:\n    cmd: run\n    metrics:\n      - metrics.json:\n          cache: false\n",
        ),
        (
            "params scoped to a file",
            "stages:\n  train:\n    cmd: run\n    params:\n      - params.yaml:\n          - train.seed\n          - train.depth\n",
        ),
        // Bare keys mean the default file; a reader that skips this attributes
        // the parameters to the wrong file and every stage looks current.
        (
            "params as bare keys",
            "stages:\n  train:\n    cmd: run\n    params:\n      - train.seed\n      - train.depth\n",
        ),
        (
            "params from two files",
            "stages:\n  train:\n    cmd: run\n    params:\n      - params.yaml:\n          - seed\n      - models.yaml:\n          - forest.depth\n",
        ),
        (
            "stage plots with a template",
            "stages:\n  eval:\n    cmd: run\n    plots:\n      - plots/confusion.json:\n          template: confusion\n          x: actual\n          y: predicted\n",
        ),
        (
            "stage plot as a bare path",
            "stages:\n  eval:\n    cmd: run\n    plots:\n      - plots/roc.csv\n",
        ),
        // A top-level entry is a display name, not a path, and pulls its data
        // from several files. This is the model-comparison shape.
        (
            "top-level multi-file plot",
            "stages:\n  eval:\n    cmd: run\nplots:\n  - Learning curves:\n      template: linear\n      x: epoch\n      y:\n        plots/a.csv: acc\n        plots/b.csv: acc\n",
        ),
        (
            "top-level plot as a bare path",
            "stages:\n  eval:\n    cmd: run\nplots:\n  - plots/roc.csv:\n      x: fpr\n      y: tpr\n",
        ),
        (
            "declaration order is preserved",
            "stages:\n  zeta:\n    cmd: z\n  alpha:\n    cmd: a\n  mid:\n    cmd: m\n",
        ),
        (
            "wdir and desc",
            "stages:\n  train:\n    desc: Fit the model\n    wdir: src\n    cmd: run\n",
        ),
        (
            "schema stated as current",
            "schema: 1\nstages:\n  a:\n    cmd: run\n",
        ),
        // Refused by both: reading half a newer pipeline draws a wrong DAG.
        (
            "schema from the future",
            "schema: 2\nstages:\n  a:\n    cmd: run\n",
        ),
    ]
}

fn lock_cases() -> Vec<(&'static str, &'static str)> {
    vec![
        ("empty lock", "schema: 3\nstages: {}\n"),
        (
            "one stage with deps",
            "schema: 3\nstages:\n  train:\n    cmd: python train.py\n    deps:\n      src/train.py: e753b03a96da287cb864f732b70a4d17329e6277\n",
        ),
        (
            "params carry their values",
            "schema: 3\nstages:\n  train:\n    cmd: run\n    params:\n      params.yaml:\n        train.depth: 4\n        train.name: forest\n        train.rate: 0.01\n        train.flag: true\n",
        ),
        (
            "two stages",
            "schema: 3\nstages:\n  prepare:\n    cmd: p\n    deps:\n      a.py: aaaa\n  train:\n    cmd: t\n    deps:\n      b.py: bbbb\n",
        ),
        // Every reader discards these outright rather than reading what it
        // recognises, which is what makes `schema:` mean anything.
        ("older schema", "schema: 2\nstages:\n  a:\n    cmd: run\n"),
        ("newer schema", "schema: 4\nstages:\n  a:\n    cmd: run\n"),
        ("missing schema", "stages:\n  a:\n    cmd: run\n"),
    ]
}
