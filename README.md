# tsp

Reproducible pipelines and experiments, versioned in git.

`tsp` records which stages produced which artifacts, whether that record still
holds, and what a given experiment changed. It does not move your data. Datasets
go through an ordinary `filter=lfs` gitattribute, which means `git add`,
`git push`, `git checkout` and `git clone` move the bytes with no help from this
tool, and `git lfs prune` and `git lfs fsck` maintain them.

There is no database, no daemon, and no server. Everything `tsp` knows is a file
in your repository or a ref in it.

## The idea

A stage is stale when something it read has changed. Answering that usually
means hashing every input, which on a real dataset means reading gigabytes to
learn that nothing moved.

`tsp.lock` instead records the **git object id** of each dependency:

```yaml
schema: 3
stages:
  train:
    cmd: python src/train.py
    params:
      params.yaml:
        train.max_depth: 4
    deps:
      src/train.py: e753b03a96da287cb864f732b70a4d17329e6277
      data/prepared/train.csv: ab70643141a7717ac63c98bc9d26395660196ef2
```

Git already computed those ids, so staleness is a tree lookup — constant work,
whatever the file weighs. For an LFS-tracked path the id is the pointer's, and
the pointer states the content's sha256 itself, so nothing here duplicates a
digest git already stores.

Parameters are recorded as **values**, not as the file's id. Several stages
usually share one `params.yaml`, and a stage that reads only `train.max_depth`
must not go stale because a sibling's key moved.

## Using it

```sh
tsp init --lfs 'data/**' --lfs 'models/**'
# describe your stages in tsp.yaml, then
tsp repro
git add -A && git commit -m 'add a pipeline'
git push
```

`tsp init` writes the `.gitattributes` entries and installs a `pre-commit` hook
that refuses a large blob no LFS filter claimed — the mistake you can only see
once it is in history.

Retuning one model in a pipeline that trains three, where all three read the
same `models.yaml`:

```
$ tsp status
  prepare         current
  train_logreg    current
  train_forest    stale     parameter forest.n_estimators changed
  train_boosting  current
  evaluate        current

5 stage(s); 1 need running.
Bring them up to date with `tsp repro`.
```

Staleness is per key, not per file, so the other two models stay current.

| Command | What it does |
| --- | --- |
| `tsp init --lfs <pattern>` | Set the repository up for `tsp` and Git LFS |
| `tsp repro [stage]` | Run the stages that are out of date and update the lock |
| `tsp status` | Show which stages are current, stale or new, and why |
| `tsp metrics [--compare <rev>]` | Show metric values, optionally against another revision |
| `tsp plots [revisions...]` | Render the pipeline's plots to a self-contained HTML page |
| `tsp exp run --set k=v` | Run with parameters overridden and record the result |
| `tsp exp list \| show \| apply \| remove` | Work with recorded experiments |

## Experiments are commits

`tsp exp run --set train.max_depth=8` applies the override, runs what that
makes stale, and records the result as an ordinary git commit under
`refs/tsp/exps/`, parented on the HEAD it ran from. The working tree goes back
exactly as it was found.

That buys three things without any machinery: the outputs are real objects so
`git gc` keeps them alive, LFS-tracked data inside an experiment is pushed and
fetched by the same commands as anything else, and comparing two experiments is
comparing two commits.

They live outside `refs/heads/` so they never appear as branches, and outside
`refs/tags/` so they are not pushed by default.

## Relationship to DVC

The pipeline format is DVC's shape, and `dvc.yaml` is read directly. What is
missing is DVC's data-management layer: no cache directory, no remotes, no
`dvc push`. Git LFS does that job and does it for every git client, not just
this one.

`tsp.lock` is not `dvc.lock`. It is schema 3 and records object ids rather than
content hashes, which is what makes the staleness check cheap.

## Building

```sh
cargo build --release        # target/release/tsp
cargo test --workspace
```

`tests/vectors.json` is generated and committed. A second implementation reads
these formats to render them in a browser, so the two agree by replaying the
same cases rather than by inspection:

```sh
cargo run -q -p tsp-core --example gen_vectors > tests/vectors.json
```

CI regenerates it and fails if it moved.

## License

MIT. See [LICENSE](LICENSE).
