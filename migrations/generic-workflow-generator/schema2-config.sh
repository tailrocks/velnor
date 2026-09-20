#!/usr/bin/env bash

# Schema-1 conversion lives in Python so TOML strings, arrays, comments, and
# nested tables are parsed as TOML instead of rewritten as source lines.
ensure_schema2_generation_config() {
  local config_path="$1"
  local repository="$2"
  local revision="$3"
  local default_branch="$4"
  local script_dir

  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  python3 "$script_dir/schema2-config.py" \
    "$config_path" \
    --repository "$repository" \
    --revision "$revision" \
    --default-branch "$default_branch"
}

# Validate the generator executable before touching a target repository. The
# config pin must name the same clean source commit that stamped this binary.
require_matching_generator_binary() {
  local expected_revision="$1"
  local generator_root="$2"
  local binary="$3"
  local actual_revision
  local actual_closure
  local dirty

  if [[ ! "$expected_revision" =~ ^[0-9a-f]{40}$ ]]; then
    echo "Cannot pin generator: $generator_root did not resolve to a full lowercase 40-character commit SHA" >&2
    return 1
  fi
  if [[ ! -x "$binary" ]]; then
    echo "Cannot pin generator: $binary is missing or not executable; build velnor-workflow from $expected_revision" >&2
    return 1
  fi

  if ! dirty="$(git -C "$generator_root" status --porcelain --untracked-files=all)"; then
    echo "Cannot inspect generator checkout $generator_root; verify it is a git repository" >&2
    return 1
  fi
  if [[ -n "$dirty" ]]; then
    echo "Cannot pin generator: $generator_root has tracked or untracked changes; use a clean checkout at $expected_revision" >&2
    return 1
  fi

  if ! actual_revision="$("$binary" --revision 2>/dev/null)"; then
    echo "Cannot read generator revision from $binary; rebuild it from $expected_revision" >&2
    return 1
  fi
  if [[ "$actual_revision" != "$expected_revision" ]]; then
    echo "Cannot migrate with stale generator: $binary reports $actual_revision, but $generator_root is pinned at $expected_revision; rebuild it from that commit" >&2
    return 1
  fi

  if ! actual_closure="$("$binary" --closure 2>/dev/null)"; then
    echo "Cannot read generator source closure from $binary; rebuild velnor-workflow from $expected_revision" >&2
    return 1
  fi
  if [[ ! "$actual_closure" =~ ^[0-9a-f]{64}$ ]]; then
    echo "Cannot verify generator source closure: $binary reports '$actual_closure'; rebuild it from a complete clean checkout at $expected_revision" >&2
    return 1
  fi

  printf '%s\n' "$actual_closure"
}
