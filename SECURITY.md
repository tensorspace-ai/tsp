# Security Policy

## Reporting a vulnerability

Please report security issues privately. Do not open a public issue.

- **Email:** security@tensorspace.ai
- **GitHub:** use [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
  on this repository.

Please include the version or commit, a description of the issue, and the steps
to reproduce it. If you have a proof of concept, include it — it makes triage
much faster.

## Scope

`tsp` is a command-line tool. It reads files in a repository you already trust
enough to have cloned, and it spawns `git`. It opens no sockets, serves no
requests, and has no network client of any kind. That makes the interesting
boundary a narrow one.

In scope:

- **Injection through pipeline content.** A stage's `cmd` is executed by design
  — cloning a repository and running `tsp repro` runs its author's code, exactly
  as `make` does. What would be a bug is a *path*, *parameter value* or
  *experiment name* escaping into a command, or into a `git` argument it was
  never meant to reach.
- **Escaping the repository.** A pipeline naming `../../etc/…` as an output, or
  an experiment name resolving to a ref outside `refs/tsp/`.
- **Denial of service on untrusted input.** A pipeline, lock or params file that
  makes the parser allocate or recurse without bound.
- **The `pre-commit` guard failing open**, letting a large blob reach history
  while being believed to be an LFS pointer.

Out of scope: a stage command doing what its author wrote it to do. If you clone
a repository and run its pipeline, you have run its code. That is the same
bargain as `make`, `npm run` or a `Justfile`, and it is stated plainly rather
than defended against.

## Supported versions

Pre-1.0. Fixes land on `main`; there are no backports.
