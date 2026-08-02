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

## Installing

`tsp` needs `git` and `git-lfs` on your PATH, and `git lfs install` to have been
run once for your user.

```sh
cargo install tsp-cli          # the binary is `tsp`
```

The crate is `tsp-cli` because the name `tsp` on crates.io belongs to an
unrelated crate last published in 2017.

Prebuilt binaries for macOS, Linux and Windows are attached to each
[release](https://github.com/tensorspace-ai/tsp/releases). Building from source
needs Rust 1.88 or newer.

```sh
tsp --version
tsp completions zsh > ~/.zfunc/_tsp    # optional
```

## Using it

```sh
tsp init --lfs 'data/**' --lfs 'models/**'
# describe your stages in tsp.yaml, then
tsp repro
git add -A && git commit -m 'add a pipeline'
git push
```

A pipeline is a `tsp.yaml`:

```yaml
stages:
  prepare:
    cmd: python src/prepare.py
    deps:
      - src/prepare.py
      - data/raw.csv
    params:
      - prepare.seed
    outs:
      - data/prepared.csv
  train:
    cmd: python src/train.py
    deps:
      - src/train.py
      - data/prepared.csv
    params:
      - train.max_depth
    outs:
      - models/model.pkl
    metrics:
      - metrics.json
```

Every field is documented in [docs/format.md](docs/format.md).

`tsp init` writes the `.gitattributes` entries and installs a `pre-commit` hook
that refuses a blob over 1 MiB which no LFS filter claimed — the mistake you can
only see once it is in history. A repository that already has its own
`pre-commit` hook keeps it, and `init` says so rather than replacing it, so
under husky or pre-commit that guard is not installed.

Retuning one model in a pipeline that trains three, where all three read the
same `models.yaml`:

```
$ tsp status
  prepare         current
  train_logreg    current
  train_forest    stale     parameter forest.n_estimators changed
  train_boosting  current
  evaluate        current

5 stages; 1 needs running.
Bring them up to date with `tsp repro`.
```

Staleness is per key, not per file, so the other two models stay current.

| Command | What it does |
| --- | --- |
| `tsp init --lfs <pattern>` | Set the repository up for `tsp` and Git LFS |
| `tsp repro [stage] [--force]` | Run the stages that are out of date and update the lock |
| `tsp status` | Show which stages are current, stale, new or unknown, and why |
| `tsp metrics [--compare <rev>]` | Show metric values, optionally against another revision |
| `tsp params [--compare <rev>]` | Show the parameter values stages declare, optionally against another revision |
| `tsp plots [revisions...] [--out <dir>]` | Render the pipeline's plots to a self-contained HTML page |
| `tsp exp run --set k=v [--name <name>] [--force]` | Run with parameters overridden and record the result |
| `tsp exp list \| show \| apply \| remove` | Work with recorded experiments |
| `tsp completions <shell>` | Print a shell completion script |

`tsp plots` writes to `tsp_plots/`, and drops a `.gitignore` beside the page so
the generated HTML stays out of history.

### Machine-readable output

`status`, `metrics`, `params`, `exp list` and `exp show` take `--json` and write
a document instead of a table, so a verdict can be read in CI without parsing
one:

```sh
$ tsp status --json | jq .summary.needs_run
2
```

stdout carries the document and nothing else — notes and warnings go to stderr —
and an empty result is an empty document rather than a sentence. The shapes and
the rule for changing them are in [docs/json.md](docs/json.md).

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
`refs/tags/` so they are not pushed by default. Share one explicitly:

```sh
git push origin 'refs/tsp/exps/*:refs/tsp/exps/*'
```

## Relationship to DVC

The pipeline format is DVC's shape, and `dvc.yaml` is read directly — the same
stage keys, the same polymorphic spellings. What is missing is DVC's
data-management layer: no cache directory, no remotes, no `dvc push`. Git LFS
does that job and does it for every git client, not just this one.

Templated stages are read. `vars`, `${...}` interpolation, `foreach` with `do`,
and `matrix` all expand, and `params.yaml` is in scope without being named, the
way DVC does it. Generated stages take DVC's names too — `train@cnn-mnist` — so
a `dvc.yaml` and a `tsp.yaml` describe the same pipeline down to what each stage
is called.

Expansion happens before anything else, which is what makes a variable a tracked
input rather than a hidden one. `tsp.lock` records the command a stage ran and
the object id of each dependency, both after substitution, so moving
`${data.path}` changes the recorded command or the recorded path and the stage
goes stale by the ordinary rule.

An unresolved reference is an error rather than text left as it was found. A
`deps` entry still spelled `${train.dataset}` names no file, so nothing would
compare it and the stage would report itself current against an input that was
never checked.

`frozen`, `always_changed` and `artifacts` are still **refused**, not ignored:
staleness comes from the lock alone, and there is no artifact registry.

`dvc.lock` is not read either, and will not be. DVC records a content hash (md5)
per output; `tsp.lock` records a git object id per dependency, and neither can be
derived from the other without reading the data — which is the cost this format
exists to avoid.

So a DVC repository's first `tsp status` reports **every stage new**. The DAG is
right and the commands are right; the staleness column simply has nothing behind
it. `tsp` says so rather than leaving you to work it out:

```
$ tsp status
note: dvc.lock is present and tsp does not read it. DVC records content hashes;
tsp.lock records git object ids, so there is nothing in dvc.lock a staleness
check here could use. Every stage reports `new` until the first `tsp repro`,
which writes tsp.lock and leaves dvc.lock where it is.

  prepare  new  never run
  train    new  never run
```

The first `tsp repro` runs everything once and writes `tsp.lock`, after which
staleness is answered normally. Your `dvc.lock` is left exactly where it is —
nothing rewrites or removes it, and `dvc` keeps working against it.

The [format reference](docs/format.md) lists every supported field.

## Building

```sh
cargo build --release        # target/release/tsp
./run-tests.sh               # fmt, clippy, tests, and the vectors
```

The end-to-end tests drive a real repository through real `git` and `git-lfs`,
so both must be installed and `git lfs install` must have been run.

`tests/vectors.json` is generated and committed. A second implementation reads
these formats to render them in a browser, so the two agree by replaying the
same cases rather than by inspection:

```sh
cargo run -q -p tsp-core --example gen_vectors > tests/vectors.json
```

CI regenerates it and fails if it moved.

## License

MIT. See [LICENSE](LICENSE).

Provided as is, without warranty of any kind, as the license sets out. It is
`0.x` software that runs commands you wrote against data you care about — keep
your own backups.
