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

echo
echo "all checks passed"
