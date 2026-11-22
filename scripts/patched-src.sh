#!/usr/bin/env bash
# Copies the stock pingora-proxy and pingora-core 0.9.1 sources from the cargo registry into
# target/patched-src/ and applies patches/*.patch in order. Safe to run again: it starts over.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/target/patched-src
version=0.9.1

registry_copy() {
  local dir
  dir=$(compgen -G "${CARGO_HOME:-$HOME/.cargo}/registry/src/*/$1-$version" | head -n1 || true)
  if [[ -z $dir ]]; then
    (cd "$root" && cargo fetch --locked >&2)
    dir=$(compgen -G "${CARGO_HOME:-$HOME/.cargo}/registry/src/*/$1-$version" | head -n1)
  fi
  printf '%s\n' "$dir"
}

rm -rf "$out"
mkdir -p "$out"
for crate in pingora-proxy pingora-core; do
  cp -r "$(registry_copy "$crate")" "$out/$crate"
  chmod -R u+w "$out/$crate"
done

# Patches 0001 to 0003 are for pingora-proxy, 0004 and 0005 are for pingora-core.
for patch in "$root"/patches/000[123]-*.patch; do
  patch -p1 --directory="$out/pingora-proxy" --input="$patch"
done
for patch in "$root"/patches/000[45]-*.patch; do
  patch -p1 --directory="$out/pingora-core" --input="$patch"
done
