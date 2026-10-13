#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source_root="${VELNOR_WORKFLOW_SOURCE_DIR:-$repo_root/../velnor}"
manifest="$source_root/Cargo.toml"
velnor_repository="https://github.com/tailrocks/velnor.git"
velnor_revision="${VELNOR_WORKFLOW_SOURCE_REV:-c3a38b5ffffb844ab0dffc730a67f953b6af9eb7}"

if test -f "$manifest"; then
  exec cargo run --manifest-path "$manifest" --locked -p velnor-workflow -- "$@"
fi

cache_root="${VELNOR_WORKFLOW_CACHE_DIR:-$repo_root/target/velnor-workflow-$velnor_revision}"
binary="$cache_root/bin/velnor-workflow"

if test ! -x "$binary"; then
  cargo install \
    --git "$velnor_repository" \
    --rev "$velnor_revision" \
    --locked \
    velnor-workflow \
    --root "$cache_root"
fi

exec "$binary" "$@"
