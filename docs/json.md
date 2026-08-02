# Machine-readable output

`tsp status`, `tsp metrics`, `tsp params`, `tsp exp list` and `tsp exp show`
each take `--json` and write a document instead of a table.

This is a contract. It gets read in CI, and moving a field breaks a pipeline
nobody in this repository can see.

It is a *different* contract from [the file formats](format.md), and the rule
runs the opposite way — see below. That is why it is a separate page: applying
one page's rule to the other's subject is how a reader ends up refusing a
document they could have read.

- [Versioning](#versioning)
- [Rules](#rules)
- [`status`](#status)
- [`metrics` and `params`](#metrics-and-params)
- [`exp list`](#exp-list)
- [`exp show`](#exp-show)

## Versioning

Every document carries `"schema": 1` and a `"kind"`.

A reader meeting an unknown `schema` in `tsp.yaml` or `tsp.lock` refuses the
file whole, because someone else wrote that file and half of it is a wrong
answer rather than a missing one. Nobody but `tsp` writes the documents on this
page, so there is no half-understood document to refuse. Therefore:

- **Adding a field does not bump `schema`.** A consumer that ignores unknown
  keys is safe by construction, and every consumer should.
- **Removing a field, renaming one, or changing its type or meaning bumps it.**
- **`kind` is stable** and is never reused for a different shape.

## Rules

**stdout carries the document and nothing else.** Every human message — progress,
warnings, the `dvc.lock` note — goes to stderr in JSON mode. Piping stdout
straight into a parser always works.

**An empty result is an empty document, not a sentence.** A pipeline declaring no
metrics emits `{"schema":1,"kind":"metrics","values":[],"notes":[]}` and exits
0. It never prints "No metrics files found." on stdout. This is the most common
way a `--json` flag gets it wrong, because the empty case is the one nobody
tests.

**Errors are not JSON.** A failure stays on stderr as prose, with a non-zero exit
and empty stdout. Exit status already answers "did it work", and requiring
stdout to be parsed first to find that out is worse. There is deliberately no
`{"error": ...}` shape.

**`--json` does not change exit codes.** `tsp status --json` exits 0 whether or
not stages need running; read `summary.needs_run`. A flag that made a stale
pipeline a shell failure would make `set -e` scripts break on the normal case.

## `status`

```json
{
  "schema": 1,
  "kind": "status",
  "pipeline_file": "tsp.yaml",
  "stages": [
    { "name": "prepare", "status": "current", "reason": null },
    { "name": "train",   "status": "stale",   "reason": "parameter train.factor changed" },
    { "name": "report",  "status": "new",     "reason": "never run" }
  ],
  "summary": {
    "total": 3, "current": 1, "stale": 1, "new": 1, "unknown": 0, "needs_run": 2
  },
  "notes": []
}
```

| Field | Notes |
| --- | --- |
| `stages[].status` | `current`, `stale`, `new` or `unknown`. The same strings the [format reference](format.md) defines and the second implementation is tested against; this document invents no vocabulary of its own. |
| `stages[].reason` | `null` for `current`, a sentence otherwise. Never `""` — the distinction between "no reason" and "an empty reason" is real. |
| `stages` order | The plan order: every producer before its consumers, ties on declaration order. |
| `summary.needs_run` | `total - current`. Precomputed because it is the number CI branches on, and `stale` alone is the wrong answer — a stage that is `new` or `unknown` also needs a run. |
| `notes` | Always present, possibly empty. See [notes](#notes). |

## `metrics` and `params`

Without `--compare`, both emit values:

```json
{
  "schema": 1,
  "kind": "metrics",
  "values": [
    { "file": "metrics.json", "key": "accuracy", "value": 0.9211, "display": "0.9211" }
  ],
  "notes": []
}
```

`value` is the raw scalar so a consumer can do arithmetic; `display` is the
rendering this CLI uses, so a consumer drawing a table matches it byte for byte.
Both are present because dropping either forces the other to be reimplemented,
and the rendering rules — integral floats stay integral, trailing zeros go — are
subtle enough that it would be reimplemented wrongly.

With `--compare <rev>`, both emit rows:

```json
{
  "schema": 1,
  "kind": "metrics",
  "current_label": "workspace",
  "compare_label": "HEAD~1",
  "rows": [
    { "file": "metrics.json", "key": "accuracy",
      "current": "0.9211", "compare": "0.8940",
      "delta": 0.0271, "delta_display": "+0.0271",
      "improved": true, "direction": "higher_is_better" },
    { "file": "metrics.json", "key": "lines",
      "current": "6", "compare": null,
      "delta": null, "delta_display": null,
      "improved": null, "direction": "unknown" }
  ],
  "notes": []
}
```

| Field | Notes |
| --- | --- |
| `current` / `compare` | Display strings, or `null` when that side has no value. **Never `"-"`** — the dash is how the table draws a missing side and must not leak into a document. |
| `delta` | A number, or `null` when either side is missing or non-numeric. |
| `improved` | `true`, `false`, or **`null`**. The third state is real: the metric's name settles no direction. Collapsing it into `false` reports every unjudgeable metric as having got worse. |
| `direction` | `higher_is_better`, `lower_is_better` or `unknown`, per [metric direction](format.md#metric-direction). Exposed so nobody reimplements the name lists. |

**`kind: "params"` rows omit `improved` and `direction` entirely** — not `null`,
absent. A parameter is a setting rather than a result and there is nothing for
it to be better at. The omission is structural rather than cosmetic: the
comparison underneath judges a row whenever the key's name implies a direction,
and it matches on substrings, so `train.loss_weight` *does* come back judged.
Having no field to put that verdict in is what stops it being published.

## `exp list`

```json
{
  "schema": 1,
  "kind": "exp_list",
  "keys": [
    { "file": "metrics.json", "key": "accuracy" },
    { "file": "metrics.json", "key": "lines" }
  ],
  "rows": [
    { "name": "HEAD", "baseline": true,
      "metrics": [ { "value": 12, "display": "12" }, { "value": 6, "display": "6" } ] },
    { "name": "tenfold", "baseline": false,
      "metrics": [ { "value": 60, "display": "60" }, null ] }
  ]
}
```

`keys` is the column order, so a consumer reproduces the table exactly. Each
entry carries its `file` as well as its `key`, because two metrics files may use
the same dotted key for two different measurements — a column is the pair, not
the name.

Each row's `metrics` is positional, matching `keys` index for index. A `null`
cell is the one the table draws as `-`.

The first row is the baseline, and `baseline` marks it rather than leaving it to
be inferred from the name. Experiments follow, newest first.

## `exp show`

The comparison shape, with the experiment named:

```json
{
  "schema": 1,
  "kind": "exp_show",
  "experiment": { "name": "tenfold", "commit": "9f1c…" },
  "current_label": "tenfold",
  "compare_label": "HEAD",
  "rows": [ … ],
  "notes": []
}
```

`commit` is the full 40-character sha, not the short form the terminal shows.

## Notes

`notes` carries what a person would have been told on stderr, so a program sees
the same thing:

```json
{ "code": "dvc_lock_not_read",
  "message": "dvc.lock is present and tsp does not read it. …" }
```

`code` is stable and matchable; `message` is prose and may be reworded. There is
one code so far.
