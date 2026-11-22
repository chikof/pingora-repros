#!/usr/bin/env bash
# Runs the repros.
#
#   scripts/run.sh stock [problem]     against pingora-proxy and pingora-core 0.9.1 from crates.io
#   scripts/run.sh patched [problem]   against copies with patches/*.patch applied
#
# problem is 1 to 4 or the test file name: upgrade_101_race, tunnel_timeout,
# h2_trailers_end_of_stream, rustls_peer_verifier. Without it all four run.
# Extra arguments go to the test binary, e.g. `scripts/run.sh stock 1 -- --nocapture`.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

mode=${1:-}
[[ $mode == stock || $mode == patched ]] || {
  sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
}
shift

tests=(upgrade_101_race tunnel_timeout h2_trailers_end_of_stream rustls_peer_verifier)
select=()
if [[ ${1:-} != "" && $1 != -- ]]; then
  case $1 in
    [1-4]) select=(--test "${tests[$(($1 - 1))]}") ;;
    *) select=(--test "$1") ;;
  esac
  shift
fi
[[ ${1:-} == -- ]] && shift
[[ ${#select[@]} -gt 0 ]] || for t in "${tests[@]}"; do select+=(--test "$t"); done

if [[ $mode == stock ]]; then
  exec cargo test --no-fail-fast "${select[@]}" -- "$@"
fi

"$root/scripts/patched-src.sh" >/dev/null
# Patching moves two lockfile entries from the registry to a path. Put the lockfile back after.
cp Cargo.lock target/Cargo.lock.orig
trap 'cp target/Cargo.lock.orig Cargo.lock' EXIT
cargo test --features patched --no-fail-fast \
  --config "patch.crates-io.pingora-proxy.path=\"$root/target/patched-src/pingora-proxy\"" \
  --config "patch.crates-io.pingora-core.path=\"$root/target/patched-src/pingora-core\"" \
  "${select[@]}" -- "$@"
