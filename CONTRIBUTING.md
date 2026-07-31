# Contributing to tsp

Thanks for taking a look. This is a small, focused codebase and contributions
are welcome.

## Getting set up

```sh
cargo build --workspace
cargo test --workspace
```

The end-to-end tests drive a real repository through real `git` and `git-lfs`,
so both have to be installed and `git lfs install` must have been run. If the
LFS filter is missing the tests still pass while proving the opposite of what
they claim — the data ends up in git rather than behind a pointer.

Before opening a pull request:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## The formats are a contract

`tsp.yaml` and `tsp.lock` have a second implementation: a git host reads them to
draw the pipeline in a browser. The two agree by replaying the same cases, not
by inspection.

Any change to how a file is parsed, or to how a stage's status is decided, means
regenerating the vectors:

```sh
cargo run -q -p tsp-core --example gen_vectors > tests/vectors.json
```

CI regenerates them and fails if the committed copy moved, so this is not
optional. The vectors carry the *reason string* as well as the verdict: a reader
told "stale" by one implementation and "current" by the other is being lied to
by one of them, and the wording is how you tell which.

Adding a field to a file format also means bumping its `schema:`. A reader that
meets a schema it does not know refuses the file rather than reading the parts
it recognises — which is what makes adding a field safe.

## Things worth knowing

- **Staleness is a tree lookup.** The lock records git object ids, never content
  hashes. A change that makes the tool hash a dependency to decide whether it
  moved is a change that makes it unusable on a real dataset.
- **Parameters are recorded as values**, per key, because several stages share
  one params file and one stage's retune must not stale its siblings.
- **`tsp` does not move bytes.** Anything that adds a cache, a remote, or a
  transfer path is going in the wrong direction; git-lfs already does that job
  for every git client rather than only for this one.

## Commits

Conventional Commits — `type(scope): subject`, with `!` before the colon for a
breaking change. One logical change per commit. Explain *why* in the body; the
diff already says what.

Comments follow the same rule: short, and about the reason rather than the
mechanics.
