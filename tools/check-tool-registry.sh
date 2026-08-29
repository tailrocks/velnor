#!/usr/bin/env bash
# Check the generator-owned mise tool registry through the Rust parser.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
fixtures=""
fleet=""

usage() {
  printf 'usage: %s (--fixtures DIR | --fleet FILE)\n' "$0" >&2
  exit 2
}

while (($# > 0)); do
  case "$1" in
    --fixtures)
      (($# >= 2)) || usage
      fixtures=$2
      shift 2
      ;;
    --fleet)
      (($# >= 2)) || usage
      fleet=$2
      shift 2
      ;;
    *)
      usage
      ;;
  esac
done

if [[ -n "$fixtures" && -n "$fleet" ]] || [[ -z "$fixtures" && -z "$fleet" ]]; then
  usage
fi

run_checker() {
  (
    cd -- "$repo_root"
    mise exec -- cargo run --quiet --locked -p velnor-actions-generator -- tool-registry "$@"
  )
}

if [[ -n "$fleet" ]]; then
  fleet_path=$fleet
  [[ "$fleet_path" == /* ]] || fleet_path="$repo_root/$fleet_path"
  run_checker --root "$repo_root" --fleet "$fleet_path"
  exit 0
fi

fixture_root=$fixtures
[[ "$fixture_root" == /* ]] || fixture_root="$repo_root/$fixture_root"
registry="$fixture_root/registry.toml"
[[ -f "$registry" ]] || { printf 'missing fixture registry: %s\n' "$registry" >&2; exit 1; }

check_case() {
  local name=$1
  local expectation=$2
  local case_dir="$fixture_root/$name"
  local output
  if output=$(run_checker \
    --root "$case_dir" \
    --registry "$registry" \
    --mise "$case_dir/mise.toml" \
    --lock "$case_dir/mise.lock" 2>&1); then
    if [[ "$expectation" != pass ]]; then
      printf 'fixture %s unexpectedly passed:\n%s\n' "$name" "$output" >&2
      exit 1
    fi
    printf 'fixture %s: pass\n' "$name"
  else
    if [[ "$expectation" == pass ]]; then
      printf 'fixture %s failed unexpectedly:\n%s\n' "$name" "$output" >&2
      exit 1
    fi
    if ! grep -Fq "$expectation" <<<"$output"; then
      printf 'fixture %s failed for the wrong reason (expected %s):\n%s\n' \
        "$name" "$expectation" "$output" >&2
      exit 1
    fi
    printf 'fixture %s: rejected (%s)\n' "$name" "$expectation"
  fi
}

check_case clean pass
check_case registry-drift diverges
check_case rust-pin 'rust pin is forbidden'
check_case unpinned 'unpinned or invalid version'
check_case floating 'unpinned or invalid version'
check_case lock-drift 'no lock entry'
