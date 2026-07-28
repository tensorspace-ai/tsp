//! The repository as `ds` sees it: a pipeline, a lock, and the git plumbing
//! needed to answer questions about them.
//!
//! There is no data layer here. A dataset is whatever `.gitattributes` sends
//! through the LFS filter; `ds` reads the pointer git already holds and never
//! touches the bytes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ds_core::git::Git;
use ds_core::graph::{self, Resolver, Status};
use ds_core::lock::{Lock, LockEntry, LockStage};
use ds_core::params::Params;
use ds_core::pipeline::Pipeline;
use ds_core::{Pointer, hash};

pub struct Repo {
    git: Git,
    root: PathBuf,
    /// The pipeline file that was found, e.g. `ds.yaml`.
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
                "no pipeline file in {}; create a ds.yaml describing your stages",
                root.display()
            );
        };
        let lock = Lock::read_or_default(&root, &pipeline_file)?;

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
        self.root.join(Lock::name_for(&self.pipeline_file))
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

        let git_sha_of = |path: &str| index.get(path).cloned();
        let param_of = |file: &str, key: &str| params.get(file).and_then(|p| p.get(key)).cloned();
        let now = Resolver {
            git_sha_of: &git_sha_of,
            param_of: &param_of,
            dirty: &dirty,
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

    /// Builds the lock entry for one path.
    ///
    /// The digest of an LFS-tracked file is read out of its pointer rather than
    /// computed: the pointer *is* the sha256 of the content, so locking a
    /// multi-gigabyte output costs a blob read instead of a full pass over the
    /// data. Only small files that git stores literally are hashed.
    pub fn lock_entry(
        &self,
        path: &str,
        index: &HashMap<String, String>,
        dirty: &HashSet<String>,
    ) -> Result<LockEntry> {
        let absolute = self.root.join(path);
        let mut entry = LockEntry {
            path: path.to_owned(),
            hash: Some("sha256".to_owned()),
            git_sha: self.blob_id(path, index, dirty)?,
            ..Default::default()
        };

        if absolute.is_dir() {
            let (digest, size, nfiles) = self.directory_digest(path, index, dirty)?;
            entry.sha256 = Some(digest);
            entry.size = Some(size);
            entry.nfiles = Some(nfiles);
            return Ok(entry);
        }
        if !absolute.is_file() {
            return Ok(entry);
        }

        match self.pointer_for(path, index)? {
            Some(pointer) => {
                entry.sha256 = Some(pointer.oid.as_str().to_owned());
                entry.size = Some(pointer.size);
            }
            None => {
                let hashed =
                    hash::hash_file(&absolute).with_context(|| format!("hashing {path}"))?;
                entry.sha256 = Some(hashed.oid.as_str().to_owned());
                entry.size = Some(hashed.size);
            }
        }
        Ok(entry)
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

    /// The LFS pointer git holds for `path`, if it holds one.
    fn pointer_for(&self, path: &str, index: &HashMap<String, String>) -> Result<Option<Pointer>> {
        let Some(sha) = index.get(path) else {
            return Ok(None);
        };
        let blobs = self.git.read_blobs(std::slice::from_ref(sha))?;
        let Some((_, content)) = blobs.first() else {
            return Ok(None);
        };
        if !Pointer::could_be_pointer(content.len() as u64) {
            return Ok(None);
        }
        Ok(Pointer::try_from(content.as_slice()).ok())
    }

    /// Summarises a directory from the index rather than the filesystem, so
    /// ignored and untracked files cannot change the recorded identity.
    fn directory_digest(
        &self,
        path: &str,
        index: &HashMap<String, String>,
        dirty: &HashSet<String>,
    ) -> Result<(String, u64, usize)> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let mut members: Vec<&String> = index.keys().filter(|p| p.starts_with(&prefix)).collect();
        members.sort();

        let mut digests = Vec::with_capacity(members.len());
        let mut total = 0u64;
        for member in &members {
            let entry = self.lock_entry(member, index, dirty)?;
            digests.push(((*member).clone(), entry.sha256.clone().unwrap_or_default()));
            total += entry.size.unwrap_or(0);
        }

        Ok((hash::digest_of_members(&digests), total, members.len()))
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
                locked.deps.push(self.lock_entry(dep, &index, &dirty)?);
            }
            for out in &stage.outs {
                locked
                    .outs
                    .push(self.lock_entry(&out.path, &index, &dirty)?);
            }
            // Plots ride in the metrics group: the lock has no group of their
            // own, and every reader looks a path up across all of them.
            let plot_paths = stage.plots.iter().map(|p| p.artifact.path.as_str());
            for path in stage
                .metrics
                .iter()
                .map(|m| m.path.as_str())
                .chain(plot_paths)
            {
                locked.metrics.push(self.lock_entry(path, &index, &dirty)?);
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
        paths.push(Lock::name_for(&self.pipeline_file).to_owned());
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
