# Contributing to tsp

Thanks for taking a look. This is a small, focused codebase and contributions
are welcome.

## Getting set up

```sh
cargo build --workspace
./run-tests.sh
```

The end-to-end tests drive a real repository through real `git` and `git-lfs`,
so both have to be installed and `git lfs install` must have been run. If the
LFS filter is missing the tests still pass while proving the opposite of what
they claim — the data ends up in git rather than behind a pointer.

## Opening a pull request

`./run-tests.sh` is the gate. It runs `cargo fmt --check`, clippy with warnings
denied, the whole suite, and a regeneration of the conformance vectors — the
same checks CI runs, in the same order, so green here means green there.

**Do not open a PR that fails it.** There are no exceptions for small or obvious
changes. The worst bug this codebase has shipped looked small: the reader
compared commands and dependency object ids but not parameter values, so
retuning a model left the CLI saying *stale* and the browser saying *current*
about the same commit. Nothing about the parse was wrong. A test caught it;
reading the diff did not.

Beyond that:

- **Branch from `main` and open a PR.** One logical change per commit.
- No issue is required first. For anything large, opening one to agree on the
  shape will save you work.
- CI must be green, and a maintainer reviews. Expect a few days; a ping on the
  PR after a week is welcome rather than rude.
- No CLA and no DCO. Contributions are under the MIT license the project
  carries.

If a test fails and you believe the test is wrong, say so in the PR and explain
why. Do not delete, skip or weaken a test to make a change pass.

## What will be declined

Three things are settled, and a PR that crosses them will be turned down however
well it is written. They are listed here so nobody finds out afterwards.

- **A data-management layer** — a cache, a remote, a transfer command. Git LFS
  does that job for every git client rather than only for this one.
- **Content hashing to decide staleness.** The lock records git object ids;
  hashing dependencies makes the tool unusable on the datasets it exists for.
- **Parameters recorded per file rather than per key.** Several stages share one
  params file, and one stage's retune must not stale its siblings.

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

## Commits

Conventional Commits — `type(scope): subject`, with `!` before the colon for a
breaking change. One logical change per commit. Explain *why* in the body; the
diff already says what.

Comments follow the same rule: short, and about the reason rather than the
mechanics.
