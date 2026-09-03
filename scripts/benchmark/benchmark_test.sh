#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
out="$tmp/result.ndjson"
repo="$(git -C "$DIR/../.." rev-parse --show-toplevel)"
status_before="$(git -C "$repo" status --porcelain=v1)"
before="$(find "${TMPDIR:-/tmp}" -maxdepth 1 -type d -name 'velnor-benchmark.*' -print | sort)"
"$DIR/benchmark.sh" --quick --jobs 2 --iterations 3 --output "$out" >/dev/null
after="$(find "${TMPDIR:-/tmp}" -maxdepth 1 -type d -name 'velnor-benchmark.*' -print | sort)"
[[ "$before" == "$after" ]] || { echo "benchmark leaked a temporary directory" >&2; exit 1; }
[[ "$status_before" == "$(git -C "$repo" status --porcelain=v1)" ]] || { echo "benchmark mutated caller checkout" >&2; exit 1; }
python3 - "$out" <<'PY'
import json, sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert rows[0]["schema_version"] == 1 and rows[0]["record_type"] == "metadata"
assert rows[0]["iterations"] == 3
assert rows[1]["record_type"] == "daemon_disk"
scenario_rows=[r for r in rows if r["record_type"] == "scenario"]
scenarios={r["scenario"]:r for r in scenario_rows}
required={"cold_check","warm_check","noop","fresh_worktree_separate_target_cold","fresh_worktree_separate_target_warm","cross_worktree_shared_target_reuse","small_source_edit","dependency_manifest_touch","dependency_graph_change","build","clippy","native_sys_build","parallel_independent_jobs_2"}
assert required <= scenarios.keys(), required-scenarios.keys()
assert "test" in scenarios or "nextest" in scenarios
assert all(r["exit_status"] == 0 for r in scenario_rows)
assert all({"wall_seconds","user_seconds","sys_seconds","max_rss_kib"} <= r["metrics"].keys() for r in scenario_rows)
assert all({"target","cargo_cache","sccache","kache","mbx"} <= r["sizes_bytes"].keys() for r in scenario_rows)
repeated={n:[r for r in scenario_rows if r["scenario"] == n] for n in ("warm_check","fresh_worktree_separate_target_warm","cross_worktree_shared_target_reuse","warm_build")}
assert all([r["sample_index"] for r in rs] == [1,2,3] for rs in repeated.values())
aggregates={r["scenario"]:r for r in rows if r["record_type"] == "aggregate"}
assert aggregates.keys() == repeated.keys()
assert all(r["samples"] == 3 and {"median","p95_nearest_rank","mad","min"} <= r["wall_seconds"].keys() for r in aggregates.values())
PY
[[ -s "${out%.ndjson}.summary.txt" ]]
echo "benchmark quick self-test passed"
