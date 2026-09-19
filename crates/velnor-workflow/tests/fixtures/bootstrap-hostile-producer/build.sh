#!/usr/bin/env bash
set -euo pipefail

output="${1:?usage: build.sh OUTPUT}"
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

# Rust std only. The caller owns OUTPUT and must place it in disposable staging.
rustc \
  --edition=2024 \
  --deny warnings \
  -C debuginfo=0 \
  -C strip=symbols \
  "$script_dir/probe.rs" \
  -o "$output"
chmod 0555 "$output"
test -x "$output"

