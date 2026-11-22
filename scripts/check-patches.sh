#!/usr/bin/env bash
# Applies every patch alone to a fresh copy of its crate, then all of them in order, and
# prints what patch(1) says. Exits non-zero if any patch fails to apply.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
version=0.9.1

copy() {
  local dir
  dir=$(compgen -G "${CARGO_HOME:-$HOME/.cargo}/registry/src/*/$1-$version" | head -n1)
  mkdir -p "$work/$2"
  cp -r "$dir/." "$work/$2/"
}

for patch in "$root"/patches/*.patch; do
  name=$(basename "$patch")
  case $name in
    0004-*) crate=pingora-core ;;
    *) crate=pingora-proxy ;;
  esac
  copy "$crate" "alone-$name"
  echo "== $name alone, on a fresh $crate-$version"
  patch -p1 --directory="$work/alone-$name" --input="$patch"
done

copy pingora-proxy all-proxy
copy pingora-core all-core
echo "== 0001, 0002, 0003 in order, on a fresh pingora-proxy-$version"
for patch in "$root"/patches/000[123]-*.patch; do
  patch -p1 --directory="$work/all-proxy" --input="$patch"
done
echo "== 0004, on a fresh pingora-core-$version"
patch -p1 --directory="$work/all-core" --input="$root/patches/0004-rustls-server-cert-verifier.patch"
