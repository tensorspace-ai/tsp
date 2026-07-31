# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — unreleased

First public release.

Versioned `0.x` deliberately. The tool works and is tested, but `tsp.yaml` and
`tsp.lock` are read by a second implementation and the two are still settling;
the `0.x` series signals that. Breaking format changes bump the schema, are
listed here, and never make an older file unreadable.

### Added

- `tsp init`, which writes the `.gitattributes` entries Git LFS needs and
  installs a `pre-commit` hook that refuses a large blob no LFS filter claimed.
- `tsp repro`, running the stages that are out of date and rewriting the lock.
- `tsp status`, reporting each stage as current, stale or new — with the reason
  in words, naming the dependency or parameter responsible.
- `tsp metrics`, optionally against another revision, with a delta that is only
  judged better or worse when the metric's name settles which direction is.
- `tsp plots`, rendering a pipeline's declared plots to a self-contained HTML
  page, including confusion matrices and multi-file series.
- `tsp exp run | list | show | apply | remove`, recording a parameter experiment
  as an ordinary git commit under `refs/tsp/exps/`.
- `schema:` on the pipeline file. A reader that meets a schema past its own
  refuses the file rather than reading the half it recognises.
- Cross-language conformance vectors in `tests/vectors.json`, covering both how
  the files parse and what verdict a stage's status reaches.

### Changed

- **The tool is called `tsp`.** It was `ds`, which was two letters, unsearchable
  and already a command on plenty of systems.

  New repositories get `tsp.yaml`, `tsp.lock` and `refs/tsp/exps/`. Everything
  written before the rename keeps working: `ds.yaml`, `dvc.yaml`, `ds.lock` and
  `refs/ds/exps/` are read forever, a repository that already has a lock keeps
  it where it is, and one that has never been locked takes its lock's name from
  its pipeline's.

- Data management belongs to Git LFS. There is no cache directory, no remote and
  no transfer command; `git push` and `git clone` move the bytes, and
  `git lfs prune` and `git lfs fsck` maintain them.

- `tsp.lock` is schema 3 and records the git object id of each dependency rather
  than a content hash, which is what makes a staleness check a tree lookup
  instead of a pass over the data.
