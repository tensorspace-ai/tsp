# tsp-core

The `tsp.yaml` and `tsp.lock` formats: parsing, staleness, metrics and plots.

This is the library behind the [`tsp`](https://crates.io/crates/tsp-cli)
command. It is published so that the command can be, and because these formats
are a contract rather than an internal structure — a second implementation reads
the same files to render a pipeline in a browser, and the two agree by replaying
[`tests/vectors.json`](https://github.com/tensorspace-ai/tsp/blob/main/tests/vectors.json)
rather than by inspection.

What it does *not* contain is a data layer. There is no cache, no remote and no
transfer path; `tsp.lock` records the git object id of each dependency, so
deciding whether a stage is stale is a tree lookup rather than a pass over the
data it was built from.

The [format reference](https://github.com/tensorspace-ai/tsp/blob/main/docs/format.md)
documents every field these types parse.

## Stability

`0.x`, and the API moves with the tool's needs. The *file formats* are the
stable surface here and carry their own `schema:` version; the Rust API is not
yet covered by that promise. Pin an exact version if you depend on it directly.

## License

MIT. See [LICENSE](LICENSE).
