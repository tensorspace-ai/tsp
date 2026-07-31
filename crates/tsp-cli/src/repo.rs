//! The repository as `tsp` sees it: a pipeline, a lock, and the git plumbing
//! needed to answer questions about them.
//!
//! There is no data layer here. A dataset is whatever `.gitattributes` sends
//! through the LFS filter; `tsp` reads the pointer git already holds and never
//! touches the bytes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tsp_core::git::Git;
use tsp_core::graph::{self, Resolver, Status};
use tsp_core::lock::{Lock, LockStage};
use tsp_core::params::Params;
use tsp_core::pipeline::Pipeline;

pub struct Repo {
    git: Git,
    root: PathBuf,
    /// The pipeline file that was found, e.g. `tsp.yaml`.
    pub pipeline_file: String,
    pub pipeline: Pipeline,
    pub lock: Lock,
}

impl Repo {
    /// Opens the repository containing `start`, requiring a pipeline.
    pub fn open(start: &Path) -> Result<Self> {
        let git = Git::discover(start).context("not inside a git repository")?;
        let root = git.work_tree().to_path_buf();

        let Some((pipeline_file, pipeline)) = Pipeline::find(&root)? else {
            bail!(
                "no pipeline file in {}; create a tsp.yaml describing your stages",
                root.display()
            );
        };
        // A lock this version cannot read is not a failure to recover from by
        // hand: it only records what the last run saw, so discarding it costs a
        // rerun and nothing else. Failing here would leave no way forward,
        // since every command reads the lock before it can rewrite one.
        let lock = match Lock::read_or_default(&root) {
            Ok(lock) if lock.schema == tsp_core::lock::SCHEMA => lock,
            Ok(lock) => {
                eprintln!(
                    "warning: {} is schema {} and this tsp writes {}; \
                     treating every stage as new until the next `tsp repro`",
                    tsp_core::lock::FILE_NAME,
                    lock.schema,
                    tsp_core::lock::SCHEMA
                );
                Lock::default()
            }
            Err(err) => {
                eprintln!(
                    "warning: cannot read {}: {err}; \
                     treating every stage as new until the next `tsp repro`",
                    tsp_core::lock::FILE_NAME
                );
                Lock::default()
            }
        };

        Ok(Self {
            git,
            root,
            pipeline_file,
            pipeline,
            lock,
        })
    }

    pub fn git(&self) -> &Git {
        &self.git
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join(tsp_core::lock::FILE_NAME)
    }

    /// Stages in dependency order, restricted to `target` and its ancestors
    /// when one is named.
    pub fn plan(&self, target: Option<&str>) -> Result<Vec<String>> {
        let order = graph::topological_order(&self.pipeline)?;
        Ok(match target {
            Some(name) => {
                self.pipeline.stage(name)?; // reject an unknown name up front
                graph::ancestors(&self.pipeline, &order, name)
            }
            None => order,
        })
    }

    /// The status of every stage, in dependency order.
    pub fn statuses(&self) -> Result<Vec<(String, Status)>> {
        let index = self.index_shas()?;
        let dirty = self.git.dirty_paths()?;
        let params = self.param_cache()?;

        // The same identity the lock records: what the content *is*, which for
        // an unstaged edit means hashing it rather than trusting the index.
        let git_sha_of = |path: &str| self.blob_id(path, &index, &dirty).ok().flatten();
        let param_of = |file: &str, key: &str| params.get(file).and_then(|p| p.get(key)).cloned();
        let now = Resolver {
            git_sha_of: &git_sha_of,
            param_of: &param_of,
        };

        Ok(self
            .plan(None)?
            .into_iter()
            .map(|name| {
                let status = graph::status_of(&self.pipeline, &self.lock, &name, &now);
                (name, status)
            })
            .collect())
    }

    /// Every parameter file the pipeline references, parsed once.
    pub fn param_cache(&self) -> Result<HashMap<String, Params>> {
        let mut cache = HashMap::new();
        for stage in self.pipeline.stages.values() {
            for reference in &stage.params {
                if cache.contains_key(&reference.file) {
                    continue;
                }
                let params = Params::read_optional(&self.root.join(&reference.file))?;
                cache.insert(reference.file.clone(), params);
            }
        }
        Ok(cache)
    }

    /// Path to git object id, for everything in the index.
    pub fn index_shas(&self) -> Result<HashMap<String, String>> {
        Ok(self
            .git
            .ls_files()?
            .into_iter()
            .map(|e| (e.path_str(), e.sha))
            .collect())
    }

    /// The object id this path will carry once committed.
    ///
    /// The index is the cheap answer and the right one for anything already
    /// staged — which includes every output, since a run stages them before
    /// locking. A dependency the user has edited but not staged is the case
    /// that matters: taking the index id there would record the *previous*
    /// content, and the stage would read stale the moment the edit is
    /// committed.
    fn blob_id(
        &self,
        path: &str,
        index: &HashMap<String, String>,
        dirty: &HashSet<String>,
    ) -> Result<Option<String>> {
        if !dirty.contains(path)
            && let Some(sha) = index.get(path)
        {
            return Ok(Some(sha.clone()));
        }
        if !self.root.join(path).is_file() {
            return Ok(index.get(path).cloned());
        }
        Ok(Some(self.git.hash_working_file(path)?))
    }

    /// Rebuilds the lock entry for every stage in `names`, leaving other stages
    /// as they were.
    ///
    /// Called after the commands have run and their outputs are staged, so the
    /// git ids recorded here are the ones a commit would carry.
    pub fn relock(&mut self, names: &[String]) -> Result<()> {
        let index = self.index_shas()?;
        let dirty = self.git.dirty_paths()?;
        let params = self.param_cache()?;

        for name in names {
            let stage = self.pipeline.stage(name)?.clone();
            let mut locked = LockStage {
                cmd: stage.command_text(),
                ..Default::default()
            };

            for reference in &stage.params {
                let values = locked.params.entry(reference.file.clone()).or_default();
                for key in &reference.keys {
                    let value = params
                        .get(&reference.file)
                        .and_then(|p| p.get(key))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    values.insert(key.clone(), value);
                }
            }

            for dep in stage.deps.iter() {
                if let Some(id) = self.blob_id(dep, &index, &dirty)? {
                    locked.deps.insert(dep.clone(), id);
                }
            }

            self.lock.stages.insert(name.clone(), locked);
        }

        // Keep the lock in pipeline order so its diffs read like the pipeline.
        let order: Vec<String> = self.pipeline.stages.keys().cloned().collect();
        self.lock.stages.sort_by_cached_key(|name, _| {
            order.iter().position(|n| n == name).unwrap_or(usize::MAX)
        });
        Ok(())
    }

    /// Stages the paths a run touches: the pipeline's outputs, its parameter
    /// files, and the lock.
    ///
    /// Deliberately not `git add --all`: a run must never sweep up unrelated
    /// files a user happens to have left in the tree.
    pub fn stage_run_outputs(&self) -> Result<()> {
        let mut paths: Vec<String> = Vec::new();
        for stage in self.pipeline.stages.values() {
            paths.extend(stage.out_paths().into_iter().map(str::to_owned));
            paths.extend(stage.params.iter().map(|p| p.file.clone()));
        }
        paths.push(tsp_core::lock::FILE_NAME.to_owned());
        paths.sort();
        paths.dedup();

        // A declared output a stage never produced is not an error here; the
        // run itself already reported whatever went wrong.
        let existing: Vec<String> = paths
            .into_iter()
            .filter(|p| self.root.join(p).exists())
            .collect();
        self.git.stage_paths(&existing)?;
        Ok(())
    }

    /// Refuses to continue when the tree carries changes an experiment would
    /// otherwise fold into its result.
    pub fn require_clean_tree(&self) -> Result<()> {
        let dirty: HashSet<String> = self
            .git
            .dirty_paths()?
            .union(&self.git.staged_paths()?)
            .cloned()
            .collect();
        if dirty.is_empty() {
            return Ok(());
        }

        let mut names: Vec<&String> = dirty.iter().collect();
        names.sort();
        bail!(
            "the working tree has uncommitted changes to {} file(s), starting with {}. \
             Commit or stash them first: an experiment records what HEAD plus its own \
             overrides produce, and cannot tell your edits apart from its own.",
            names.len(),
            names[0]
        );
    }
}
