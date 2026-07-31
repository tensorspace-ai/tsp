//! Stage ordering and staleness.
//!
//! Both answers come from the pipeline plus the lock; neither reads a byte of
//! data. That is the point: deciding whether a stage needs re-running must not
//! cost the same as running it.

use std::collections::{HashMap, HashSet};

use crate::lock::Lock;
use crate::pipeline::Pipeline;

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("stage {0:?} takes part in a dependency cycle")]
    Cycle(String),
}

type Result<T> = std::result::Result<T, GraphError>;

/// Why a stage is not current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Every dependency matches what the lock recorded.
    Current,
    /// The stage has no lock entry at all.
    New,
    /// A dependency moved. The reason names which, so `tsp status` can say why.
    Stale(String),
    /// Staleness is not decidable, because a dependency's path never resolved
    /// to something that could be looked up.
    Unknown(String),
}

impl Status {
    pub fn needs_run(&self) -> bool {
        !matches!(self, Status::Current)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Status::Current => "current",
            Status::New => "new",
            Status::Stale(_) => "stale",
            Status::Unknown(_) => "unknown",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Status::Current => None,
            Status::New => Some("never run"),
            Status::Stale(why) | Status::Unknown(why) => Some(why),
        }
    }
}

/// Orders stages so every producer runs before its consumers.
///
/// Ties break on declaration order, which keeps the lock file — and the DAG
/// Gitea renders from it — identical across runs with identical inputs.
pub fn topological_order(pipeline: &Pipeline) -> Result<Vec<String>> {
    let producer = producers(pipeline);

    let mut order = Vec::with_capacity(pipeline.stages.len());
    let mut done: HashSet<&str> = HashSet::new();
    let mut visiting: HashSet<&str> = HashSet::new();

    // An explicit stack rather than recursion: a deep chain of stages should
    // not be able to overflow the native stack.
    for root in pipeline.stages.keys() {
        if done.contains(root.as_str()) {
            continue;
        }
        let mut stack = vec![(root.as_str(), false)];

        while let Some((name, expanded)) = stack.pop() {
            if expanded {
                visiting.remove(name);
                if done.insert(name) {
                    order.push(name.to_owned());
                }
                continue;
            }
            if done.contains(name) {
                continue;
            }
            if !visiting.insert(name) {
                return Err(GraphError::Cycle(name.to_owned()));
            }

            stack.push((name, true));
            let Some(stage) = pipeline.stages.get(name) else {
                continue;
            };
            // Reversed so declaration order survives the stack's LIFO order.
            for dep in stage.deps.iter().rev() {
                if let Some(upstream) = producer.get(dep.as_str())
                    && *upstream != name
                    && !done.contains(*upstream)
                {
                    stack.push((upstream, false));
                }
            }
        }
    }
    Ok(order)
}

/// Restricts an order to `target` and everything it depends on.
pub fn ancestors(pipeline: &Pipeline, order: &[String], target: &str) -> Vec<String> {
    let producer = producers(pipeline);
    let mut needed: HashSet<&str> = HashSet::from([target]);

    // Walking the topological order backwards means a stage's consumers are
    // always seen before it, so one pass is enough.
    for name in order.iter().rev() {
        if !needed.contains(name.as_str()) {
            continue;
        }
        let Some(stage) = pipeline.stages.get(name) else {
            continue;
        };
        for dep in stage.deps.iter() {
            if let Some(upstream) = producer.get(dep.as_str()) {
                needed.insert(upstream);
            }
        }
    }
    order
        .iter()
        .filter(|n| needed.contains(n.as_str()))
        .cloned()
        .collect()
}

/// Maps each produced path to the stage that writes it.
pub fn producers(pipeline: &Pipeline) -> HashMap<&str, &str> {
    let mut map = HashMap::new();
    for (name, stage) in &pipeline.stages {
        for path in stage.out_paths() {
            map.insert(path, name.as_str());
        }
    }
    map
}

/// What the working tree currently says, for deciding staleness against a lock.
pub struct Resolver<'a> {
    /// The git object id a path's *current* content would have — the working
    /// tree's, not the index's. A run locks the content it consumed, which for
    /// an edited script is not yet staged, so comparing index ids would call a
    /// stage stale the moment it had been reproduced.
    pub git_sha_of: &'a dyn Fn(&str) -> Option<String>,
    /// A parameter's current value, by file and dotted key.
    pub param_of: &'a dyn Fn(&str, &str) -> Option<serde_json::Value>,
}

/// Decides whether a stage's recorded result still applies.
pub fn status_of(pipeline: &Pipeline, lock: &Lock, stage_name: &str, now: &Resolver) -> Status {
    let Some(stage) = pipeline.stages.get(stage_name) else {
        return Status::New;
    };
    let Some(locked) = lock.stages.get(stage_name) else {
        return Status::New;
    };

    let cmd = stage.command_text();
    if !cmd.is_empty() && !locked.cmd.is_empty() && cmd != locked.cmd {
        return Status::Stale("command changed".to_owned());
    }

    for param in &stage.params {
        for key in &param.keys {
            let recorded = locked.params.get(&param.file).and_then(|f| f.get(key));
            let current = (now.param_of)(&param.file, key);
            match (recorded, current) {
                (None, _) => return Status::Stale(format!("new parameter {}:{key}", param.file)),
                // A value that moved is the whole reason an experiment reruns.
                (Some(was), Some(is)) if *was != is => {
                    return Status::Stale(format!("parameter {key} changed"));
                }
                (Some(_), None) => {
                    return Status::Stale(format!("parameter {key} is gone"));
                }
                _ => {}
            }
        }
    }

    let mut comparable = 0usize;
    for dep in stage.deps.iter() {
        // An unresolved `${...}` interpolation has nothing to compare against.
        if dep.contains("${") {
            continue;
        }
        let Some(recorded) = locked.deps.get(dep) else {
            return Status::Stale(format!("new dependency {dep}"));
        };
        comparable += 1;
        match (now.git_sha_of)(dep) {
            None => return Status::Stale(format!("missing dependency {dep}")),
            Some(current) if &current != recorded => {
                return Status::Stale(format!("{dep} changed"));
            }
            Some(_) => {}
        }
    }

    if comparable == 0 && !stage.deps.is_empty() {
        return Status::Unknown("every dependency is an unresolved reference".to_owned());
    }
    Status::Current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockStage;
    use indexmap::IndexMap;

    fn pipeline(text: &str) -> Pipeline {
        Pipeline::parse(text, "tsp.yaml").unwrap()
    }

    const CHAIN: &str = r#"
stages:
  evaluate:
    cmd: e
    deps: [model.pkl, test.csv]
    metrics: [metrics.json]
  prepare:
    cmd: p
    deps: [src/prep.py]
    outs: [train.csv, test.csv]
  train:
    cmd: t
    deps: [train.csv]
    outs: [model.pkl]
"#;

    /// Declaration order here is deliberately wrong; the graph must fix it.
    #[test]
    fn producers_run_before_consumers() {
        let order = topological_order(&pipeline(CHAIN)).unwrap();
        assert_eq!(order, ["prepare", "train", "evaluate"]);
    }

    #[test]
    fn independent_stages_keep_declaration_order() {
        let order = topological_order(&pipeline(
            "stages:\n  b:\n    cmd: x\n  a:\n    cmd: y\n  c:\n    cmd: z\n",
        ))
        .unwrap();
        assert_eq!(order, ["b", "a", "c"]);
    }

    #[test]
    fn a_cycle_is_reported_rather_than_looping() {
        let cyclic = pipeline(
            "stages:\n  a:\n    cmd: x\n    deps: [b.out]\n    outs: [a.out]\n  b:\n    cmd: y\n    deps: [a.out]\n    outs: [b.out]\n",
        );
        assert!(matches!(
            topological_order(&cyclic),
            Err(GraphError::Cycle(_))
        ));
    }

    #[test]
    fn ancestors_drop_unrelated_stages() {
        let p = pipeline(CHAIN);
        let order = topological_order(&p).unwrap();
        assert_eq!(ancestors(&p, &order, "train"), ["prepare", "train"]);
        assert_eq!(ancestors(&p, &order, "prepare"), ["prepare"]);
        assert_eq!(
            ancestors(&p, &order, "evaluate"),
            ["prepare", "train", "evaluate"]
        );
    }

    fn locked_chain() -> Lock {
        let mut lock = Lock::default();
        lock.stages.insert(
            "train".to_owned(),
            LockStage {
                cmd: "t".to_owned(),
                deps: IndexMap::from([("train.csv".to_owned(), "aaa".to_owned())]),
                ..Default::default()
            },
        );
        lock
    }

    /// Builds a Resolver over fixed answers, so each test states only what it
    /// is about.
    struct Now {
        shas: HashMap<String, String>,
        params: HashMap<(String, String), serde_json::Value>,
    }

    impl Now {
        fn with_shas(pairs: &[(&str, &str)]) -> Self {
            Self {
                shas: pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                params: HashMap::new(),
            }
        }

        fn status(&self, lock: &Lock) -> Status {
            let git_sha_of = |path: &str| self.shas.get(path).cloned();
            let param_of = |file: &str, key: &str| {
                self.params.get(&(file.to_owned(), key.to_owned())).cloned()
            };
            status_of(
                &pipeline(CHAIN),
                lock,
                "train",
                &Resolver {
                    git_sha_of: &git_sha_of,
                    param_of: &param_of,
                },
            )
        }
    }

    #[test]
    fn a_stage_with_matching_ids_is_current() {
        let now = Now::with_shas(&[("train.csv", "aaa")]);
        assert_eq!(now.status(&locked_chain()), Status::Current);
    }

    #[test]
    fn a_moved_dependency_names_itself() {
        let now = Now::with_shas(&[("train.csv", "bbb")]);
        assert_eq!(
            now.status(&locked_chain()),
            Status::Stale("train.csv changed".to_owned())
        );
    }

    /// Staleness is decided on the id of the content as it stands now, so an
    /// edit that has not been staged still registers — and, just as important,
    /// an edit a run already consumed does not.
    #[test]
    fn an_unstaged_edit_is_judged_on_its_content() {
        let edited = Now::with_shas(&[("train.csv", "bbb")]);
        assert_eq!(
            edited.status(&locked_chain()),
            Status::Stale("train.csv changed".to_owned())
        );

        // The lock records what the run read, which for an unstaged edit is the
        // working tree's content rather than the index's.
        let just_ran = Now::with_shas(&[("train.csv", "aaa")]);
        assert_eq!(just_ran.status(&locked_chain()), Status::Current);
    }

    #[test]
    fn a_stage_with_no_lock_entry_is_new() {
        let now = Now::with_shas(&[]);
        let status = now.status(&Lock::default());
        assert_eq!(status, Status::New);
        assert!(status.needs_run());
    }

    #[test]
    fn a_changed_command_is_stale() {
        let mut lock = locked_chain();
        lock.stages.get_mut("train").unwrap().cmd = "something else".to_owned();
        let now = Now::with_shas(&[("train.csv", "aaa")]);
        assert_eq!(
            now.status(&lock),
            Status::Stale("command changed".to_owned())
        );
    }

    #[test]
    fn a_deleted_dependency_is_stale() {
        let now = Now::with_shas(&[]);
        assert_eq!(
            now.status(&locked_chain()),
            Status::Stale("missing dependency train.csv".to_owned())
        );
    }

    /// The case an experiment turns on: same code, same data, different value.
    #[test]
    fn a_changed_parameter_value_is_stale() {
        let p = pipeline(
            "stages:\n  train:\n    cmd: t\n    deps: [train.csv]\n    params:\n      - params.yaml:\n          - train.max_depth\n",
        );
        let mut lock = locked_chain();
        lock.stages.get_mut("train").unwrap().params.insert(
            "params.yaml".to_owned(),
            IndexMap::from([("train.max_depth".to_owned(), serde_json::json!(4))]),
        );

        let shas = HashMap::from([("train.csv".to_owned(), "aaa".to_owned())]);
        let git_sha_of = |path: &str| shas.get(path).cloned();

        let unchanged = |_: &str, _: &str| Some(serde_json::json!(4));
        assert_eq!(
            status_of(
                &p,
                &lock,
                "train",
                &Resolver {
                    git_sha_of: &git_sha_of,
                    param_of: &unchanged
                }
            ),
            Status::Current
        );

        let changed = |_: &str, _: &str| Some(serde_json::json!(8));
        assert_eq!(
            status_of(
                &p,
                &lock,
                "train",
                &Resolver {
                    git_sha_of: &git_sha_of,
                    param_of: &changed
                }
            ),
            Status::Stale("parameter train.max_depth changed".to_owned())
        );
    }
}
