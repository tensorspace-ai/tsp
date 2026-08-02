<!--
Explain *why* in the description. The diff already says what.
-->

## What this changes and why

## Checklist

- [ ] `./run-tests.sh` passes (fmt, clippy, the whole suite, and the vectors)
- [ ] Commits follow Conventional Commits, with `!` before the colon if breaking
- [ ] If parsing or a status verdict changed: `tests/vectors.json` is
      regenerated, and the reader's replay has been re-run
- [ ] If a file format gained a field: its `schema:` is bumped and CHANGELOG.md
      records it

<!--
The three invariants a PR cannot cross, so nobody spends an evening on one:

  - Staleness is a tree lookup. The lock records git object ids, never content
    hashes.
  - Parameters are recorded per key, as values.
  - tsp does not move bytes. No cache, no remote, no transfer command.

CONTRIBUTING.md explains each.
-->
