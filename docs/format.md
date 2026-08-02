# The tsp file formats

Reference for `tsp.yaml` and `tsp.lock`. Both are read by a second
implementation that renders them in a browser, so this page describes a
contract rather than one program's behaviour.

- [`tsp.yaml`](#tspyaml) — the pipeline you write
- [Stage keys](#stage-keys)
- [Artifacts: `outs`, `metrics`](#artifacts-outs-and-metrics)
- [Parameters](#parameters)
- [Plots](#plots)
- [`tsp.lock`](#tsplock) — the record a run leaves
- [Metric direction](#metric-direction)
- [What is not supported](#what-is-not-supported)

The CLI's `--json` output is a separate contract with the opposite rule for
unknown fields, and the second implementation takes no part in it. It is
documented in [json.md](json.md).

A key this version does not define is an error, not something skipped. That is
deliberate: a misspelled `outs:` would otherwise read as "this stage writes
nothing", and the stage would report itself up to date forever.

## `tsp.yaml`

`tsp` reads the first of `tsp.yaml` or `dvc.yaml` that exists. A complete
example:

```yaml
schema: 1

stages:
  prepare:
    desc: Split the raw table
    cmd: python src/prepare.py
    deps:
      - src/prepare.py
      - data/raw.csv
    params:
      - params.yaml:
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
      - models/model.pkl:
          cache: false
    metrics:
      - metrics.json
    plots:
      - plots/confusion.json:
          template: confusion
          x: actual
          y: predicted

plots:
  - Loss:
      x: step
      y:
        train_loss.csv: loss
        valid_loss.csv: loss
```

| Key | Meaning |
| --- | --- |
| `schema` | The shape the file is written in. Optional; absence means `1`, which is also the only value this version accepts. A file declaring a higher number is refused whole rather than read in part. |
| `stages` | A map of stage name to [stage](#stage-keys). **Declaration order matters**: it breaks ties in run order and fixes the order of `tsp.lock`, so the same inputs always produce the same file. |
| `plots` | Plots for the pipeline as a whole. Unlike a stage's `plots`, these are not artifacts — an entry may carry a display name and pull data from several files. See [Plots](#plots). |

## Stage keys

| Key | Shape | Meaning |
| --- | --- | --- |
| `cmd` | string, or list of strings | **Required.** Run through `sh -c` (`cmd /C` on Windows), so pipes and redirection work. A list runs in order and stops at the first failure. |
| `wdir` | string | Directory to run in, relative to the repository root. Defaults to the root. Paths elsewhere in the stage stay relative to the root regardless. |
| `deps` | string, or list of strings | What the stage reads. A stage is stale when any of these has a different git object id than the lock records. An entry containing `${` is skipped, having nothing to compare. |
| `outs` | list of [artifacts](#artifacts-outs-and-metrics) | What the stage writes. A stage that exits 0 without writing one of these fails the run. |
| `metrics` | list of [artifacts](#artifacts-outs-and-metrics) | Outputs that `tsp metrics` reads. Same shape as `outs`. |
| `plots` | list of [plot entries](#plots) | Outputs that `tsp plots` draws. Both an artifact and a drawing instruction. |
| `params` | list | The parameter keys the stage reads. See [Parameters](#parameters). |
| `desc` | string | A human description. Carried through the parse and shown by readers that display it. |

A stage with no `cmd` is refused: nothing would run, and the staleness check has
no command to compare, so it would report itself current without ever having
done anything.

## Artifacts: `outs` and `metrics`

Either a bare path, or a single-key map carrying options:

```yaml
outs:
  - models/model.pkl              # bare path
  - data/keep.csv:
      cache: false
      persist: true
```

| Option | Default | Meaning |
| --- | --- | --- |
| `cache` | `true` | Whether the artifact is data. `false` marks a small file meant to be read in diffs, such as `metrics.json`. |
| `persist` | `false` | The file is not deleted between runs. |

Large outputs belong behind a `filter=lfs` gitattribute — `tsp init --lfs` writes
those entries. `tsp` never moves the bytes either way.

## Parameters

Parameters are recorded **per key, as values**, not as the file's object id.
Several stages usually share one `params.yaml`, and a stage that reads only
`train.max_depth` must not go stale because a sibling's key moved.

Two spellings:

```yaml
params:
  - train.max_depth        # a bare key, from params.yaml
  - models.yaml:           # keys scoped to another file
      - forest.n_estimators
      - forest.max_depth
```

A bare key means `params.yaml`. Keys are dotted paths into the document.

`tsp exp run --set` may only move a key some stage declares here — staleness is
decided from these, so setting anything else changes nothing and reruns nothing.
Scope an override to a file with `--set models.yaml:forest.max_depth=8`.

## Plots

A plot entry is a bare path, or a single-key map of options. The key is the file
for a stage's `plots:`, and a display name for the top-level `plots:`.

| Option | Meaning |
| --- | --- |
| `template` | How to draw it. See below. Default `linear`. |
| `title` | Heading for the figure. Defaults to the entry's name. |
| `x` | Field for the x axis. Omitted, points are drawn against row index (`step`). |
| `y` | Field(s) for the y axis. Omitted, the file's last field is used. |
| `x_label`, `y_label` | Axis captions. Default to the field names. |
| `header` | Delimited files only. `false` when the file has no header row. |
| `cache`, `persist` | As for [artifacts](#artifacts-outs-and-metrics). Meaningful only on a stage's own plots. |

`x` and `y` each accept three shapes:

```yaml
y: loss                      # one field
y: [precision, recall]       # several fields
y:                           # a field per file — how one plot spans files
  train.csv: loss
  valid.csv: loss
```

### Templates

A name outside this list is an error, not a fallback.

| Template | Draws |
| --- | --- |
| `linear` | Points joined in row order. The default. |
| `simple` | `linear` without point markers. |
| `scatter` | Points only. |
| `scatter_jitter` | `scatter` with a small offset, separating overlapping points. |
| `smooth` | `linear` with the series smoothed. |
| `confusion` | A matrix of actual against predicted, shaded by count. |
| `confusion_normalized` | `confusion`, each row scaled to sum to one. |
| `bar_horizontal` | Horizontal bars in the data's own order. |
| `bar_horizontal_sorted` | Horizontal bars ordered by length. |

The confusion templates read two categorical fields (`x` and `y`) rather than an
axis pair.

### Data files

Read by extension: `.csv` (comma), `.tsv` (tab), `.yaml`/`.yml`, and anything
else as JSON, falling back to YAML. `header` applies to the delimited formats.

## `tsp.lock`

Generated by `tsp repro`. It is committed, and not meant to be edited by hand.

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
      data/prepared.csv: ab70643141a7717ac63c98bc9d26395660196ef2
```

Each dependency is one line: the path, and the **git object id** of what it held
when the stage last ran. Git already computed that id, so deciding whether a
stage is stale is a tree lookup — constant work whatever the file weighs. For an
LFS-tracked path the id is the pointer's, and the pointer states the content's
sha256 itself.

`schema:` is **mandatory** here, unlike in the pipeline. A lock whose schema is
not 3 is discarded with a warning and every stage reported new; the lock only
records what the last run saw, so throwing it away costs a rerun and nothing
else. (The pipeline is refused instead, because it is your definition and
guessing at it would run the wrong thing.)

## Metric direction

`tsp metrics --compare` calls a change better or worse only when the metric's
name settles which direction is which. The match is on substrings, and
loss-like names are tested first because several contain a higher-is-better name
inside them.

**Lower is better:** `loss`, `error`, `rmse`, `mse`, `mae`, `mape`,
`perplexity`, `latency`, `duration`, `cost`

**Higher is better:** `accuracy`, `acc`, `f1`, `precision`, `recall`, `auc`,
`iou`, `dice`, `bleu`, `rouge`, `r2`

Anything else gets the delta with no verdict. Metrics files are flattened to
dotted keys, with array elements indexed.

## What is not supported

`dvc.yaml` is read as the same shape, but these DVC features are **refused**
rather than ignored, because silently dropping them produces a pipeline that
looks fine and does the wrong thing:

| Key | Why |
| --- | --- |
| `foreach`, `matrix`, `do` | Templated stages are not expanded. The command lives under `do:`, so dropping these leaves a stage with no command — one that runs nothing and reports itself current. Write the stages out, or keep running that pipeline with `dvc`. |
| `vars` | No variable substitution. |
| `frozen`, `always_changed` | Staleness comes from the lock alone. |
| `artifacts` | No artifact registry. |

`dvc.lock` is not read, and that is a limit rather than a missing feature. A DVC
lock records a content hash (md5) per output; `tsp.lock` records a git object id
per dependency. Neither number can be computed from the other without reading the
data.

A repository holding a `dvc.lock` and no `tsp.lock` therefore has no staleness
record any reader of this format can use: every stage is `new` with the reason
`never run`, which is correct and uninformative at the same time. **An
implementation must say so.** The CLI prints a note on `status` and before
`repro` runs anything; a reader rendering the pipeline should show the
equivalent beside the graph. This is not a `Status` — the verdict and its reason
string are unchanged, and nothing about it is decided per stage.

The first `tsp repro` rebuilds the record as `tsp.lock` and leaves `dvc.lock`
untouched.

There is also no data-management layer at all: no cache directory, no remote, no
transfer command. `git push` and `git clone` move the bytes, and `git lfs prune`
and `git lfs fsck` maintain them.
