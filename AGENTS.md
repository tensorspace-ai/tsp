# Agent instructions

Rules for any AI agent working in this repository. Read this before changing
anything.

## Quality gate — required before every commit

```sh
./run-tests.sh
```

**Do not commit if it fails.** It runs `cargo fmt --check`, clippy with warnings
denied, the whole test suite, and a regeneration of the conformance vectors.

There are no exceptions for "small" or "obvious" changes. The worst bug this
codebase has shipped looked small: the reader compared commands and dependency
object ids but not parameter values, so retuning a model left the CLI saying
*stale* and the browser saying *current* about the same commit. Nothing about
the parse was wrong. It was caught by a test, not by reading the diff.

If a test fails and you believe the test is wrong, say so explicitly and explain
why. Do not delete, skip or weaken a test to make a change pass.

## Invariants that must not be broken

- **Staleness is a tree lookup.** The lock records git object ids, never content
  hashes. Anything that hashes a dependency to decide whether it moved makes the
  tool unusable on the datasets it exists for.
- **Parameters are recorded per key, as values.** Several stages share one params
  file; one stage's retune must not stale its siblings.
- **This tool does not move bytes.** No cache, no remote, no transfer command.
  Git LFS does that job for every git client rather than only for this one.
- **Old names are read forever.** `ds.yaml`, `dvc.yaml`, `ds.lock` and
  `refs/ds/exps/` are files and refs people already committed. A rename is this
  tool's problem, not theirs.
- **A file from a newer schema is refused whole**, never parsed for the parts we
  recognise. That is what makes adding a field safe for older versions.

## The formats are a contract

`tsp.yaml` and `tsp.lock` have a second implementation that renders them in a
browser. Any change to parsing, or to how a stage's status is decided, means
regenerating `tests/vectors.json` **and** re-running the replay on the reader's
side. The vectors carry reason strings as well as verdicts, because a reader
told two different things by two tools is being lied to by one of them.

## Commit policy

- **Commit directly to `main`.** No feature branches for routine work.
- **Commit regularly** — at each coherent unit of work, not one large batch.
- **Conventional Commits**: `type(scope): subject`, `!` before the colon for a
  breaking change.
- Explain *why* in the body. The diff already says what.
- Never force-push, amend or squash unless asked.
- Add an `Assisted-by: AGENT_NAME:MODEL_VERSION` trailer. Never add
  `Co-Authored-By` or `Signed-off-by` — sign-off is a human's to give.

## Style

- Comments are short, explain the reason rather than the mechanics, and go on
  the same line where they fit.
- Preserve existing comments that are still true.
- No trailing whitespace.
