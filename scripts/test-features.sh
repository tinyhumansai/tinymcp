#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
python3 scripts/test-e2e-version.py
cargo test --locked --workspace
cargo test --locked -p tinymcp --no-default-features --test library_feature_tests
cargo test --locked -p tinymcp --no-default-features --features tools,ui,server-http --test library_feature_tests
cargo test --locked --workspace --all-features


set -euo pipefail
# The contract remains data and wire vocabulary with every feature enabled.
# Capture each tree first so a failing `cargo tree` aborts under `set -e` instead of
# reading as an empty (clean) graph.
default_tree="$(cargo tree -p tinymcp-bus -e normal,build --prefix none)"
all_features_tree="$(cargo tree -p tinymcp-bus --all-features -e normal,build --prefix none)"
forbidden="$(printf '%s\n%s\n' "$default_tree" "$all_features_tree" \
  | grep -Ei 'tinybus|tokio|reqwest|ureq|hyper|rusqlite|git2|tinystoragedrivers-(core|client|secrets|busproxy)' || true)"
if [ -n "$forbidden" ]; then
  echo "tinymcp-bus pulled in a dependency its manifest forbids:" >&2
  echo "$forbidden" >&2
  echo >&2
  echo "The contract is what a host compiles against. It must stay free" >&2
  echo "of transports, async runtimes, HTTP clients and native libraries." >&2
  exit 1
fi
case "${RUNNER_OS:-$(uname -s)}" in
  Windows|MINGW*|MSYS*|CYGWIN*) ;;
  *) python3 scripts/test-package-layout.py ;;
esac
