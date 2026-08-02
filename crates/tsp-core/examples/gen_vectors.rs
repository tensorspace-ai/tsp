//! Emit cross-language conformance vectors for the Go pipeline/lock parsers.
//!
//! `tsp.yaml` and `tsp.lock` have two implementations: this crate writes them,
//! and Gitea's `modules/tsp` reads them to draw the DAG. A disagreement between
//! the two does not crash anything — it renders a lineage graph that is quietly
//! wrong, which is the worst failure this format has. So the cases below are
//! generated with the Rust interpretation attached, and a Go test replays them
//! and asserts it reaches the same reading.
//!
//! What is compared is a *projection*, not the structs: each side carries
//! fields the other has no use for (Rust tracks plot axis labels the DAG does
//! not need), and forcing those into the contract would make the vectors fail
//! on differences that do not matter. The projection is what both
//! implementations must agree on for the DAG and the staleness of a stage to
//! come out the same.
//!
//! Note for the reader replaying these: `foreach`, `matrix` and `vars` are
//! *expanded*, not refused, and a case may carry the files expansion reads in
//! a `files` map — params.yaml is in scope without being named. Generated
//! stages are named `stage@item`, DVC's spelling, and that name reaches
//! tsp.lock. An implementation that parses the keys it knows and drops the rest
//! is left with a stage that has no command, which runs nothing and is then
//! reported current; expanding and refusing are both honest, ignoring is not.
//!
//! Run: `cargo run -p tsp-core --example gen_vectors > tests/vectors.json`
//! Every value in the output is derived from the inputs, so regenerating on an
//! unchanged tree produces a byte-identical file.

use serde_json::{Value, json};
use tsp_core::graph::{self, Resolver};
use tsp_core::lock::Lock;
use tsp_core::pipeline::{Artifact, Pipeline, Stage};

fn main() {
    let mut cases: Vec<Value> = Vec::new();

    for (name, input, files) in pipeline_cases() {
        // Expansion reads files beside the pipeline — params.yaml without being
        // asked, anything else `vars:` names. A case carries them so the reader
        // resolves the same variables from the same bytes.
        let read = |want: &str| {
            files
                .iter()
                .find(|(file, _)| *file == want)
                .map(|(_, body)| body.as_bytes().to_vec())
        };
        let mut case = json!({ "name": name, "kind": "pipeline", "input": input });
        if !files.is_empty() {
            case["files"] = json!(
                files
                    .iter()
                    .map(|(file, body)| ((*file).to_owned(), Value::from(*body)))
                    .collect::<serde_json::Map<String, Value>>()
            );
        }
        match Pipeline::parse_with(input, "tsp.yaml", &read) {
            Ok(p) => case["expected"] = project_pipeline(&p),
            // A rejection is as much a part of the contract as a reading: the
            // Go side bounds the same inputs, and a parser that accepts what
            // the other refuses is the drift this is meant to catch.
            Err(_) => case["rejected"] = Value::Bool(true),
        }
        cases.push(case);
    }

    for (name, input) in lock_cases() {
        cases.push(match Lock::parse(input, "tsp.lock") {
            Ok(lock) if lock.schema == tsp_core::lock::SCHEMA => json!({
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
/// The parsers agreed on `tsp.lock` for months while disagreeing about what it
/// meant: the CLI compared the parameter values the lock records and the server
/// did not, so retuning a model left the CLI saying stale and the Data tab
/// saying current about the same commit. Parsing vectors cannot catch that.
/// These can.
fn project_staleness(case: Staleness) -> Value {
    let pipeline = Pipeline::parse(case.pipeline, "tsp.yaml").expect("fixture pipeline parses");
    let lock = Lock::parse(case.lock, "tsp.lock").expect("fixture lock parses");

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
const NO_FILES: &[(&str, &str)] = &[];

type PipelineCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

fn pipeline_cases() -> Vec<PipelineCase> {
    vec![
        ("empty document", "", NO_FILES),
        ("no stages", "stages: {}\n", NO_FILES),
        (
            "minimal stage",
            "stages:\n  train:\n    cmd: python train.py\n",
            NO_FILES,
        ),
        // `cmd` is a scalar or a sequence; DVC accepts both, so files in the
        // wild use both, and the two must join to the same command.
        (
            "cmd as a sequence",
            "stages:\n  train:\n    cmd:\n      - python a.py\n      - python b.py\n",
            NO_FILES,
        ),
        (
            "deps as a scalar",
            "stages:\n  train:\n    cmd: run\n    deps: src/train.py\n",
            NO_FILES,
        ),
        (
            "outs as bare paths",
            "stages:\n  train:\n    cmd: run\n    outs:\n      - models/model.pkl\n      - data/out.csv\n",
            NO_FILES,
        ),
        // The single-key mapping spelling, and the `cache: false` that decides
        // whether a path is expected to be an LFS pointer.
        (
            "outs as mappings with options",
            "stages:\n  train:\n    cmd: run\n    outs:\n      - models/model.pkl:\n          cache: false\n      - data/keep.csv:\n          persist: true\n",
            NO_FILES,
        ),
        (
            "metrics with cache false",
            "stages:\n  eval:\n    cmd: run\n    metrics:\n      - metrics.json:\n          cache: false\n",
            NO_FILES,
        ),
        (
            "params scoped to a file",
            "stages:\n  train:\n    cmd: run\n    params:\n      - params.yaml:\n          - train.seed\n          - train.depth\n",
            NO_FILES,
        ),
        // Bare keys mean the default file; a reader that skips this attributes
        // the parameters to the wrong file and every stage looks current.
        (
            "params as bare keys",
            "stages:\n  train:\n    cmd: run\n    params:\n      - train.seed\n      - train.depth\n",
            NO_FILES,
        ),
        (
            "params from two files",
            "stages:\n  train:\n    cmd: run\n    params:\n      - params.yaml:\n          - seed\n      - models.yaml:\n          - forest.depth\n",
            NO_FILES,
        ),
        (
            "stage plots with a template",
            "stages:\n  eval:\n    cmd: run\n    plots:\n      - plots/confusion.json:\n          template: confusion\n          x: actual\n          y: predicted\n",
            NO_FILES,
        ),
        (
            "stage plot as a bare path",
            "stages:\n  eval:\n    cmd: run\n    plots:\n      - plots/roc.csv\n",
            NO_FILES,
        ),
        // A top-level entry is a display name, not a path, and pulls its data
        // from several files. This is the model-comparison shape.
        (
            "top-level multi-file plot",
            "stages:\n  eval:\n    cmd: run\nplots:\n  - Learning curves:\n      template: linear\n      x: epoch\n      y:\n        plots/a.csv: acc\n        plots/b.csv: acc\n",
            NO_FILES,
        ),
        (
            "top-level plot as a bare path",
            "stages:\n  eval:\n    cmd: run\nplots:\n  - plots/roc.csv:\n      x: fpr\n      y: tpr\n",
            NO_FILES,
        ),
        (
            "declaration order is preserved",
            "stages:\n  zeta:\n    cmd: z\n  alpha:\n    cmd: a\n  mid:\n    cmd: m\n",
            NO_FILES,
        ),
        (
            "wdir and desc",
            "stages:\n  train:\n    desc: Fit the model\n    wdir: src\n    cmd: run\n",
            NO_FILES,
        ),
        (
            "schema stated as current",
            "schema: 1\nstages:\n  a:\n    cmd: run\n",
            NO_FILES,
        ),
        // Refused by both: reading half a newer pipeline draws a wrong DAG.
        (
            "schema from the future",
            "schema: 2\nstages:\n  a:\n    cmd: run\n",
            NO_FILES,
        ),
        // A templated stage keeps its command under `do:`. An implementation
        // that drops the keys it does not expand is left with a stage that has
        // no command, runs nothing, and then reports itself current — so both
        // sides must refuse rather than read what they recognise.
        (
            "dvc foreach",
            "stages:\n  train:\n    foreach: [a, b]\n    do:\n      cmd: train ${item}\n",
            NO_FILES,
        ),
        (
            "dvc matrix",
            "stages:\n  train:\n    matrix:\n      model: [logreg, forest]\n    cmd: train ${item.model}\n",
            NO_FILES,
        ),
        (
            "dvc vars",
            "vars:\n  - params.yaml\nstages:\n  a:\n    cmd: run ${train.depth}\n",
            &[("params.yaml", "train:\n  depth: 4\n")],
        ),
        (
            "unknown stage key",
            "stages:\n  a:\n    cmd: run\n    outz:\n      - m.bin\n",
            NO_FILES,
        ),
        (
            "unknown top-level key",
            "stagez:\n  a:\n    cmd: run\n",
            NO_FILES,
        ),
        // Nothing to run, and no command to compare, so it would go straight to
        // current having never done anything.
        (
            "stage without a command",
            "stages:\n  a:\n    outs:\n      - m.bin\n",
            NO_FILES,
        ),
        // A name no renderer knows must not quietly become the default one.
        (
            "unknown plot template",
            "stages:\n  eval:\n    cmd: run\n    plots:\n      - m.json:\n          template: confusionn\n",
            NO_FILES,
        ),
        ("stages is not a map", "stages:\n  - a\n", NO_FILES),
        // Not a rule either side chose — the YAML parser refuses this before
        // any rule applies — but worth pinning all the same. A reader that
        // recovered from a broken document would render whichever half it
        // managed to read, and half a pipeline is a wrong DAG rather than a
        // missing one.
        (
            "not valid yaml",
            "stages: [unclosed\n  train: : :\n",
            NO_FILES,
        ),
        // params.yaml is in scope without being named, which is what DVC does
        // and is why an existing dvc.yaml resolves here at all.
        (
            "interpolation from params.yaml",
            "stages:\n  train:\n    cmd: run --depth ${train.depth}\n    deps:\n      - \"${data.root}/raw.csv\"\n",
            &[("params.yaml", "train:\n  depth: 4\ndata:\n  root: data\n")],
        ),
        // A mapping binds both halves: the key names the stage, the value
        // carries its settings.
        (
            "foreach over a mapping",
            "stages:\n  build:\n    foreach:\n      uk:\n        level: 1\n      us:\n        level: 2\n    do:\n      cmd: build ${key} ${item.level}\n",
            NO_FILES,
        ),
        // The set of stages can live in a params file rather than the pipeline.
        (
            "foreach naming a variable",
            "stages:\n  train:\n    foreach: ${models}\n    do:\n      cmd: train ${item}\n",
            &[("params.yaml", "models: [cnn, rnn]\n")],
        ),
        // Substitution reaches paths, not only commands — a generated stage
        // needs its own outputs or they all write to one file.
        (
            "foreach substitutes outs and deps",
            "stages:\n  build:\n    foreach: [us, eu]\n    do:\n      cmd: build ${item}\n      deps:\n        - \"src/${item}.py\"\n      outs:\n        - \"out/${item}.bin\"\n",
            NO_FILES,
        ),
        // Declaration order fixes the generated names, so the same matrix
        // always produces the same stages and the same lock.
        (
            "matrix cross product",
            "stages:\n  t:\n    matrix:\n      model: [cnn, rnn]\n      seed: [1, 2]\n    cmd: run ${item.model} ${item.seed}\n",
            NO_FILES,
        ),
        // A command that genuinely needs the characters can have them.
        (
            "an escaped reference is literal",
            "stages:\n  t:\n    cmd: echo \\${HOME}\n",
            NO_FILES,
        ),
        // Refused: a dep still spelled ${...} names no file, so nothing would
        // compare it and the stage would report itself current.
        (
            "an unknown variable",
            "stages:\n  t:\n    cmd: run ${nope.here}\n",
            NO_FILES,
        ),
        (
            "foreach without do",
            "stages:\n  t:\n    foreach: [a, b]\n",
            NO_FILES,
        ),
        (
            "foreach over a scalar",
            "stages:\n  t:\n    foreach: 3\n    do:\n      cmd: run\n",
            NO_FILES,
        ),
        // Everything but the list and the body belongs inside do:.
        (
            "a stray key beside foreach",
            "stages:\n  t:\n    foreach: [a]\n    cmd: x\n    do:\n      cmd: y\n",
            NO_FILES,
        ),
        (
            "a misspelled key inside do",
            "stages:\n  t:\n    foreach: [a]\n    do:\n      cmd: x\n      outz:\n        - m.bin\n",
            NO_FILES,
        ),
        // Two items rendering one name would silently drop a stage.
        (
            "two foreach items generating one name",
            "stages:\n  t:\n    foreach: [a, a]\n    do:\n      cmd: run ${item}\n",
            NO_FILES,
        ),
        // A file vars names explicitly and cannot be read was asked for.
        (
            "a vars file that is missing",
            "vars:\n  - absent.yaml\nstages:\n  t:\n    cmd: run\n",
            NO_FILES,
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
