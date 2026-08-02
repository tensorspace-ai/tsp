#!/bin/sh
# The quality gate. Run this before every commit.
#
# It runs the same checks CI does, in the same order, so a green run here means
# a green run there. The vector regeneration is part of the gate rather than a
# separate chore: `tsp.yaml` and `tsp.lock` have a second implementation, and a
# stale committed copy means the two agree about a format neither of them writes.
set -eu

cd "$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"

echo "==> fmt"
cargo fmt --all -- --check

echo "==> clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> test"
cargo test --workspace

echo "==> conformance vectors"
cargo run -q -p tsp-core --example gen_vectors > /tmp/tsp-vectors.json
if ! diff -u tests/vectors.json /tmp/tsp-vectors.json; then
	echo >&2
	echo "tests/vectors.json is stale. Regenerate it:" >&2
	echo "  cargo run -q -p tsp-core --example gen_vectors > tests/vectors.json" >&2
	echo "and re-run the reader's replay in the repository that consumes them." >&2
	exit 1
fi

# The reader keeps its own copy of the file above and replays it. Its copy once
# fell eight cases behind — every one of them a rejection — so its replay went
# on passing while the two implementations disagreed about which pipelines are
# readable at all. Nothing on that side can detect this: a subset of the vectors
# passes exactly as well as the whole.
#
# So it is checked from here, at the moment the drift is created, which is the
# only moment anyone has the context to fix it. Set TSP_READER_VECTORS if the
# reader is not checked out beside this repository.
echo "==> the reader's copy"
reader="${TSP_READER_VECTORS:-../gitea/modules/tsp/testdata/vectors.json}"
if [ ! -f "$reader" ]; then
	echo "    not checked out at $reader; skipped"
elif ! diff -u tests/vectors.json "$reader"; then
	echo >&2
	echo "the reader's vectors are stale. Copy them across:" >&2
	echo "  cp tests/vectors.json $reader" >&2
	echo "then re-run its replay:" >&2
	echo "  go test ./modules/tsp/... ./services/tsp/..." >&2
	exit 1
fi

echo
echo "all checks passed"
