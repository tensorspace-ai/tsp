# tsp

Reproducible pipelines and experiments, versioned in git.

This crate is the `tsp` command line tool. It is published as `tsp-cli` because
the name `tsp` on crates.io belongs to an unrelated crate last touched in 2017;
the installed binary is still called `tsp`.

```sh
cargo install tsp-cli
```

`tsp` records which stages produced which artifacts, whether that record still
holds, and what a given experiment changed. It does not move your data — that is
Git LFS's job, for every git client rather than only for this one.

Staleness is a tree lookup: `tsp.lock` records the **git object id** of each
dependency, so deciding whether a stage is out of date costs the same whatever
the file weighs. Parameters are recorded per key, as values, so retuning one
model does not stale the two beside it that share a `params.yaml`.

Requires `git` and `git-lfs` on PATH.

See the [repository](https://github.com/tensorspace-ai/tsp) for the full README,
the [format reference](https://github.com/tensorspace-ai/tsp/blob/main/docs/format.md),
and the library behind it, [`tsp-core`](https://crates.io/crates/tsp-core).

## License

MIT. See [LICENSE](LICENSE).
