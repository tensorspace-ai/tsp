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

    println!("{}", serde_json::to_string_pretty(&cases).unwrap());
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
