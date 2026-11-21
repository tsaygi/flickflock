#!/bin/sh
# Generates the manual-testing / benchmark fixtures into tests/fixtures/out
# (ignored by git). Everything is synthesised; no binaries are committed.
set -eu
cd "$(dirname "$0")/../.."
exec cargo run --release -p ff-core --example make_fixtures -- "${1:-tests/fixtures/out}"
