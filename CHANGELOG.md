# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-08-02

First public release.

Versioned `0.x` deliberately. The tool works and is tested, but `tsp.yaml` and
`tsp.lock` are read by a second implementation and the two are still settling;
the `0.x` series signals that. Breaking changes should be expected while it
lasts. The intent is that format changes bump the schema and are listed here, so
a file written by another version is detected rather than misread.

Install with `cargo install tsp-cli`, or take a binary from the release. The
crate is `tsp-cli` because `tsp` on crates.io belongs to an unrelated package
last published in 2017; the installed binary is `tsp`.

### Added

- `tsp init`, which writes the `.gitattributes` entries Git LFS needs and
  installs a `pre-commit` hook that refuses a blob over 1 MiB no LFS filter
  claimed. A repository that already has a `pre-commit` hook keeps it, and
  `init` says so.
- `tsp repro`, running the stages that are out of date and rewriting the lock.
- `tsp status`, reporting each stage as current, stale, new or unknown — with
  the reason in words, naming the dependency or parameter responsible.
- `tsp metrics`, optionally against another revision, with a delta that is only
  judged better or worse when the metric's name settles which direction is.
- `tsp plots`, rendering a pipeline's declared plots to a self-contained HTML
  page, including confusion matrices and multi-file series.
- `tsp exp run | list | show | apply | remove`, recording a parameter experiment
  as an ordinary git commit under `refs/tsp/exps/`.
- `tsp completions <shell>` for bash, zsh, fish and others, plus a man page.
- `schema:` on the pipeline file, which is `1`. A reader that meets a schema
  past its own refuses the file rather than reading the half it recognises.
- Cross-language conformance vectors in `tests/vectors.json`, covering both how
  the files parse and what verdict a stage's status reaches.
- [`docs/format.md`](docs/format.md): every field of `tsp.yaml` and `tsp.lock`,
  all nine plot templates, and the metric names that decide a delta's direction.

### The answers that were wrong

Each of these ended at a stage reported **current** when nothing had run, which
is the one answer this tool exists not to give.

- A DVC `foreach` stage keeps its command under `do:`. Both keys were dropped
  silently, leaving a stage with an empty `cmd` — `tsp repro` printed
  "Ran 1 stage(s)", executed nothing, and reported it current. `foreach`,
  `matrix`, `do`, `vars`, `frozen`, `always_changed` and `artifacts` are now
  refused, naming the stage and saying what to do instead.
- Any unknown key was dropped the same way. `stagez:` parsed as a pipeline with
  no stages; a misspelled `outs:` read as "this stage writes nothing".
- A stage that exited 0 without writing its declared output was locked as
  current, tracking a file that did not exist.
- A stage with no `cmd` went straight to current, having never run.
- An unknown revision could not be told apart from a missing file, so
  `tsp metrics --compare typo` printed an empty comparison and exited 0.
- An unrecognised plot `template:` fell back to `linear`, so a misspelled
  `confusion` drew a line chart of a confusion matrix.

### Fixed

- `tsp exp run --name` was validated after the pipeline ran, so a space in the
  flag cost the whole run. A name already in use was overwritten with no
  warning, leaving the previous experiment's commit unreachable; reuse now needs
  `--force`.
- `--set` on a parameter no stage declares is refused, instead of writing a
  value nothing reads and failing with "nothing to run".
- `tsp plots --out` accepted absolute paths and `../`, writing outside the
  repository.
- Errors printed their cause twice, and reported YAML mistakes in Rust's
  vocabulary ("expected struct Stage").
- Running outside a repository answered with git's plumbing rather than a
  suggestion to run `git init`.
- `tsp init` re-run in a configured repository told the reader to track patterns
  they had already tracked.

### Changed

- Plots are written to `tsp_plots/` rather than `ds_plots/`, the last reference
  to the tool's former name — and the directory now carries a `.gitignore`, so
  the README's own quickstart no longer commits a generated page.
- Both crates ship their `LICENSE` and a `README.md`.

### Deliberately absent

- **A data-management layer.** No cache directory, no remote, no transfer
  command. `git push` and `git clone` move the bytes, and `git lfs prune` and
  `git lfs fsck` maintain them — for every git client, not only for this one.

- **Content hashing.** `tsp.lock` records the git object id of each dependency,
  so deciding whether a stage is stale is a tree lookup rather than a pass over
  the data it was built from.

- **Templated stages.** `foreach` and `matrix` are refused rather than expanded.

[Unreleased]: https://github.com/tensorspace-ai/tsp/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tensorspace-ai/tsp/releases/tag/v0.1.0
