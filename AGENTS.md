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
- **`dvc.yaml` is read as-is**, templating included: `vars`, `${...}`, `foreach`
  and `matrix` expand, with DVC's generated names. That is how an existing DVC
  repository renders without being migrated first, and it is a feature rather
  than a legacy. Expansion happens before the typed parse, which is what keeps
  the lock, the graph and the second implementation unaware that templating
  exists — and what makes a variable a tracked input rather than a hidden one.
  It stops at the pipeline: `dvc.lock` records content hashes, and reading them to
  decide staleness is the content hashing the invariant above forbids. A DVC
  repository therefore reports every stage new until its first `tsp repro`, and
  must be *told* so rather than left to work it out.
- **A file from a newer schema is refused whole**, never parsed for the parts we
  recognise. That is what makes adding a field safe for older versions.
- **Machine-readable output is a contract.** `--json` documents carry `schema`.
  Adding a field is safe; changing, retyping or removing one is a bump. Human
  text never moves onto stdout in JSON mode, and an empty result is an empty
  document rather than a sentence. See [docs/json.md](docs/json.md).

## The formats are a contract

`tsp.yaml` and `tsp.lock` have a second implementation that renders them in a
browser. Any change to parsing, or to how a stage's status is decided, means
regenerating `tests/vectors.json` **and** re-running the replay on the reader's
side. The vectors carry reason strings as well as verdicts, because a reader
told two different things by two tools is being lied to by one of them.

`run-tests.sh` now checks the reader's copy when it is checked out beside this
repository, and CI checks it when `READER_REPO` is set. This used to be prose
alone, and prose alone was not enough: the reader's copy fell eight cases
behind, every one of them a rejection, so its replay went on passing while the
two disagreed about which pipelines are readable at all. A subset of the
vectors passes exactly as well as the whole, which is why the reader cannot
detect this itself and why the check belongs here.

## Commit policy

Human contributors should follow [CONTRIBUTING.md](CONTRIBUTING.md), which
describes the pull request route.

- **Commit as you go**, at each coherent unit of work, rather than one batch at
  the end. A commit should leave the tree green.
- **Do not push.** Committing is the agent's job; publishing is the
  maintainer's, and it is theirs to time.
- **Commit to `main`.** No feature branches for routine work.
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
