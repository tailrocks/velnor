#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  mise_bin="$(command -v mise 2>/dev/null || true)"
  [[ -n "$mise_bin" ]] || mise_bin="$HOME/.local/bin/mise"
  if [[ -x "$mise_bin" ]]; then
    exec "$mise_bin" exec -- "$0" "$@"
  fi
  echo "cargo is unavailable and mise was not found" >&2
  exit 2
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MODE=full
REPO="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
OUTPUT="$PWD/benchmark.ndjson"
JOBS=2
ITERATIONS=5

usage() {
  cat <<'EOF'
usage: benchmark.sh [--quick] [--repo PATH] [--output PATH] [--jobs N] [--iterations N]

Runs reproducible Cargo CI scenarios in disposable git worktrees. Full mode
defaults to the Velnor workspace; --quick creates a bounded local fixture.
EOF
}

while (($#)); do
  case "$1" in
    --quick) MODE=quick; shift ;;
    --repo) REPO="$2"; shift 2 ;;
    --output) OUTPUT="$2"; shift 2 ;;
    --jobs) JOBS="$2"; shift 2 ;;
    --iterations) ITERATIONS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
[[ "$JOBS" =~ ^[1-9][0-9]*$ ]] || { echo "--jobs must be a positive integer" >&2; exit 2; }
[[ "$ITERATIONS" =~ ^[1-9][0-9]*$ ]] || { echo "--iterations must be a positive integer" >&2; exit 2; }
command -v git >/dev/null
command -v python3 >/dev/null
[[ -x /usr/bin/time ]] || { echo "GNU /usr/bin/time is required" >&2; exit 2; }

tmp="$(mktemp -d "${TMPDIR:-/tmp}/velnor-benchmark.XXXXXX")"
worktrees=()
fixture=""
cleanup() {
  local worktree
  for worktree in "${worktrees[@]}"; do
    git -C "$REPO" worktree remove --force "$worktree" >/dev/null 2>&1 || true
  done
  [[ -z "$fixture" ]] || rm -rf -- "$fixture"
  rm -rf -- "$tmp"
}
trap cleanup EXIT INT TERM

if [[ "$MODE" == quick ]]; then
  fixture="$tmp/fixture"
  mkdir -p "$fixture/src" "$fixture/native-sys/src"
  cat >"$fixture/Cargo.toml" <<'EOF'
[workspace]
members = ["native-sys"]
resolver = "2"
[package]
name = "bench-fixture"
version = "0.1.0"
edition = "2021"
EOF
  cat >"$fixture/src/lib.rs" <<'EOF'
pub fn answer() -> u32 { 42 }
#[cfg(test)] mod tests { #[test] fn answer_is_stable() { assert_eq!(super::answer(), 42); } }
EOF
  cat >"$fixture/native-sys/Cargo.toml" <<'EOF'
[package]
name = "native-sys"
version = "0.1.0"
edition = "2021"
build = "build.rs"
EOF
  cat >"$fixture/native-sys/build.rs" <<'EOF'
use std::{env, process::Command};
fn main() {
    let out = env::var("OUT_DIR").unwrap();
    assert!(Command::new("cc").args(["-c", "native.c", "-o", &format!("{out}/native.o")]).status().unwrap().success());
    assert!(Command::new("ar").args(["crs", &format!("{out}/libnative_fixture.a"), &format!("{out}/native.o")]).status().unwrap().success());
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=native_fixture");
    println!("cargo:rerun-if-changed=native.c");
}
EOF
  printf 'int native_fixture(void) { return 42; }\n' >"$fixture/native-sys/native.c"
  printf 'pub fn marker() {}\n' >"$fixture/native-sys/src/lib.rs"
  git -C "$fixture" init -q
  git -C "$fixture" config user.name benchmark
  git -C "$fixture" config user.email benchmark.invalid
  git -C "$fixture" config commit.gpgsign false
  git -C "$fixture" add .
  git -C "$fixture" commit -qm fixture
  cargo generate-lockfile --manifest-path "$fixture/Cargo.toml" --offline
  git -C "$fixture" add Cargo.lock
  git -C "$fixture" commit -qm lock
  REPO="$fixture"
fi

REPO="$(git -C "$REPO" rev-parse --show-toplevel)"
commit="$(git -C "$REPO" rev-parse HEAD)"
mkdir -p "$(dirname -- "$OUTPUT")"
OUTPUT="$(cd -- "$(dirname -- "$OUTPUT")" && pwd)/$(basename -- "$OUTPUT")"
: >"$OUTPUT"
result_dir="$tmp/results"; mkdir -p "$result_dir"

new_worktree() {
  local name="$1"
  local path="$tmp/$name"
  git -C "$REPO" worktree add --detach -q "$path" "$commit"
  worktrees+=("$path")
  LAST_WORKTREE="$path"
}

size_bytes() {
  local kind="$1" path="$2"
  [[ -e "$path" ]] || { echo 0; return; }
  if [[ "$kind" == apparent ]]; then du -s --block-size=1 --apparent-size "$path" | awk '{print $1}'
  else du -s --block-size=1 "$path" | awk '{print $1}'; fi
}

metadata="$tmp/metadata.json"
python3 - "$REPO" "$commit" "$MODE" "$ITERATIONS" >"$metadata" <<'PY'
import json, os, platform, subprocess, sys
repo, commit, mode, iterations = sys.argv[1:]
def command(*args):
    return subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout.strip()
fs = command("findmnt", "-n", "-o", "FSTYPE", "--target", repo) if subprocess.call(["sh", "-c", "command -v findmnt >/dev/null"]) == 0 else "unknown"
print(json.dumps({"schema_version":1,"record_type":"metadata","mode":mode,"commit":commit,"iterations":int(iterations),
  "toolchain":{"rustc":command("rustc","--version","--verbose"),"cargo":command("cargo","--version")},
  "cpu":{"logical_count":os.cpu_count(),"model":platform.processor() or command("uname","-m")},
  "filesystem":fs,"kernel":platform.release()}, separators=(",",":")))
PY
cat "$metadata" >>"$OUTPUT"

# This is observational only: these commands never prune or mutate daemon state.
docker_info="$tmp/docker.json"
python3 - >"$docker_info" <<'PY'
import json, shutil, subprocess
def get(cmd):
    try:
        p=subprocess.run(cmd, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=20)
        return {"available":p.returncode == 0,"exit_status":p.returncode,"output":p.stdout.strip()}
    except Exception as e: return {"available":False,"error":type(e).__name__}
data={"schema_version":1,"record_type":"daemon_disk","docker":get(["docker","system","df","--format","{{json .}}"] if shutil.which("docker") else ["false"]),
      "buildkit":get(["docker","buildx","du"] if shutil.which("docker") else ["false"])}
print(json.dumps(data,separators=(",",":")))
PY
cat "$docker_info" >>"$OUTPUT"

new_worktree main
main="$LAST_WORKTREE"
target="$tmp/targets/main"
export CARGO_TERM_COLOR=never CARGO_INCREMENTAL=0
if [[ "$MODE" == quick ]]; then cargo_args=(--locked --offline); else cargo_args=(--workspace --all-targets --locked); fi

run_record() {
  local order="$1" scenario="$2" cwd="$3" target_dir="$4"; shift 4
  local timing="$tmp/time.$order" log="$tmp/log.$order" status=0
  local ta0 tp0 ca0 cp0 ta1 tp1 ca1 cp1
  ta0="$(size_bytes apparent "$target_dir")"; tp0="$(size_bytes physical "$target_dir")"
  ca0="$(size_bytes apparent "${CARGO_HOME:-$HOME/.cargo}/registry/cache")"; cp0="$(size_bytes physical "${CARGO_HOME:-$HOME/.cargo}/registry/cache")"
  set +e
  (cd "$cwd" && CARGO_TARGET_DIR="$target_dir" /usr/bin/time -f '%e %U %S %M' -o "$timing" "$@") >"$log" 2>&1
  status=$?
  set -e
  ta1="$(size_bytes apparent "$target_dir")"; tp1="$(size_bytes physical "$target_dir")"
  ca1="$(size_bytes apparent "${CARGO_HOME:-$HOME/.cargo}/registry/cache")"; cp1="$(size_bytes physical "${CARGO_HOME:-$HOME/.cargo}/registry/cache")"
  python3 - "$scenario" "$status" "$timing" "$ta0" "$tp0" "$ta1" "$tp1" "$ca0" "$cp0" "$ca1" "$cp1" "$*" "${SAMPLE_INDEX:-}" >"$result_dir/$order.json" <<'PY'
import json, pathlib, sys
s, status, timing, *rest = sys.argv[1:]
v, command, sample = rest[:8], rest[8], rest[9]
parts = pathlib.Path(timing).read_text().strip().split() if pathlib.Path(timing).exists() else []
metric = dict(zip(("wall_seconds","user_seconds","sys_seconds","max_rss_kib"), map(float, parts))) if len(parts)==4 else {}
nums=list(map(int,v))
def size(path):
 import os
 if not os.path.exists(path): return {"apparent":0,"physical":0,"detected":False}
 st=subprocess.run(["du","-s","--block-size=1","--apparent-size",path],capture_output=True,text=True)
 ph=subprocess.run(["du","-s","--block-size=1",path],capture_output=True,text=True)
 return {"apparent":int(st.stdout.split()[0]),"physical":int(ph.stdout.split()[0]),"detected":True}
import os, subprocess
sizes={"target":{"before":{"apparent":nums[0],"physical":nums[1]},"after":{"apparent":nums[2],"physical":nums[3]}},"cargo_cache":{"before":{"apparent":nums[4],"physical":nums[5]},"after":{"apparent":nums[6],"physical":nums[7]}},
 "sccache":size(os.environ.get("SCCACHE_DIR",os.path.expanduser("~/.cache/sccache"))),
 "kache":size(os.environ.get("KACHE_CACHE_DIR",os.path.expanduser("~/.cache/kache"))),
 "mbx":size(os.environ.get("MBX_CACHE_DIR",os.path.expanduser("~/.cache/mbx")))}
r={"schema_version":1,"record_type":"scenario","scenario":s,"command":command,"exit_status":int(status),"metrics":metric,"sizes_bytes":sizes}
if sample: r["sample_index"]=int(sample)
print(json.dumps(r,separators=(",",":")))
PY
}

repeat_warm() {
  local base="$1" start="$2" cwd="$3" target_dir="$4"; shift 4
  # One explicit warm-up is intentionally not a measured record.
  set +e; (cd "$cwd" && CARGO_TARGET_DIR="$target_dir" "$@") >"$tmp/${base}.warmup.log" 2>&1; set -e
  local i
  for ((i=1; i<=ITERATIONS; i++)); do
    SAMPLE_INDEX="$i" run_record "$((start+i))" "$base" "$cwd" "$target_dir" "$@"
  done
  unset SAMPLE_INDEX
}

rm -rf -- "$target"
run_record 010 cold_check "$main" "$target" cargo check "${cargo_args[@]}"
repeat_warm warm_check 020 "$main" "$target" cargo check "${cargo_args[@]}"
run_record 030 noop "$main" "$target" cargo check "${cargo_args[@]}"
new_worktree fresh
fresh="$LAST_WORKTREE"
run_record 040 fresh_worktree_separate_target_cold "$fresh" "$tmp/targets/fresh" cargo check "${cargo_args[@]}"
repeat_warm fresh_worktree_separate_target_warm 050 "$fresh" "$tmp/targets/fresh" cargo check "${cargo_args[@]}"
repeat_warm cross_worktree_shared_target_reuse 070 "$fresh" "$target" cargo check "${cargo_args[@]}"
source_file="$(git -C "$main" ls-files '*.rs' | LC_ALL=C sort | head -1)"
[[ -n "$source_file" ]] || { echo "repository has no tracked Rust source" >&2; exit 2; }
printf '\n// benchmark source edit\n' >>"$main/$source_file"
run_record 090 small_source_edit "$main" "$target" cargo check "${cargo_args[@]}"
git -C "$main" restore .
touch "$main/Cargo.toml"
run_record 100 dependency_manifest_touch "$main" "$target" cargo check "${cargo_args[@]}"
if [[ "$MODE" == quick ]]; then
  printf '\n[dependencies]\nnative-sys = { path = "native-sys" }\n' >>"$main/Cargo.toml"
  run_record 110 dependency_graph_change "$main" "$target" cargo check --offline
else
  sed -i 's/^anyhow = "1\.0\.98"/anyhow = "=1.0.103"/' "$main/Cargo.toml"
  run_record 110 dependency_graph_change "$main" "$target" cargo check --workspace --all-targets
fi
git -C "$main" restore .
run_record 120 build "$main" "$target" cargo build "${cargo_args[@]}"
repeat_warm warm_build 120 "$main" "$target" cargo build "${cargo_args[@]}"
if ! cargo nextest --version >/dev/null 2>&1; then
  echo "cargo-nextest is required for benchmark test execution" >&2
  exit 2
fi
run_record 130 nextest "$main" "$target" cargo nextest run "${cargo_args[@]}"
run_record 140 clippy "$main" "$target" cargo clippy "${cargo_args[@]}" -- -D warnings
if [[ "$MODE" == quick ]]; then
  run_record 150 native_sys_build "$main" "$tmp/targets/native" cargo build -p native-sys --locked --offline
else
  run_record 150 native_sys_build "$main" "$tmp/targets/native" cargo build --workspace --locked
fi

parallel="$tmp/parallel"; mkdir -p "$parallel"
parallel_cmd="$tmp/parallel.sh"
cat >"$parallel_cmd" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
for dir in "$@"; do (CARGO_TARGET_DIR="$dir/target" cargo check --locked --offline --manifest-path "$dir/Cargo.toml") & done
wait
EOF
chmod +x "$parallel_cmd"
parallel_dirs=()
for ((i=1; i<=JOBS; i++)); do
  new_worktree "parallel-$i"
  parallel_dirs+=("$LAST_WORKTREE")
done
if [[ "$MODE" == full ]]; then
  cat >"$parallel_cmd" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
for dir in "$@"; do (CARGO_TARGET_DIR="$dir/target" cargo check --workspace --all-targets --locked --manifest-path "$dir/Cargo.toml") & done
wait
EOF
  chmod +x "$parallel_cmd"
fi
run_record 160 "parallel_independent_jobs_$JOBS" "$main" "$parallel" "$parallel_cmd" "${parallel_dirs[@]}"

for record in "$result_dir"/*.json; do cat "$record" >>"$OUTPUT"; done
python3 - "$OUTPUT" >>"$OUTPUT" <<'PY'
import json, math, statistics, sys
rows=[json.loads(x) for x in open(sys.argv[1]) if x.strip()]
names=sorted({r["scenario"] for r in rows if r.get("sample_index")})
for name in names:
 rs=[r for r in rows if r.get("scenario")==name and r.get("sample_index")]
 vals=[r["metrics"]["wall_seconds"] for r in rs if "wall_seconds" in r["metrics"]]
 if vals:
  med=statistics.median(vals); ordered=sorted(vals)
  stats={"median":med,"p95_nearest_rank":ordered[math.ceil(.95*len(ordered))-1],"mad":statistics.median(abs(x-med) for x in vals),"min":min(vals)}
 else: stats={}
 print(json.dumps({"schema_version":1,"record_type":"aggregate","scenario":name,"samples":len(rs),"successful_samples":len(vals),"wall_seconds":stats},separators=(",",":")))
PY
python3 - "$OUTPUT" >"${OUTPUT%.ndjson}.summary.txt" <<'PY'
import json, sys
rows=[json.loads(x) for x in open(sys.argv[1]) if x.strip()]
scenarios=[r for r in rows if r["record_type"]=="scenario"]
print("Velnor Rust CI benchmark schema 1")
print(f"scenarios: {len(scenarios)}; failures: {sum(r['exit_status'] != 0 for r in scenarios)}")
for r in scenarios: print(f"{r['scenario']}: status={r['exit_status']} wall={r['metrics'].get('wall_seconds','n/a')}s rss={r['metrics'].get('max_rss_kib','n/a')}KiB")
PY
cat "${OUTPUT%.ndjson}.summary.txt"
python3 - "$OUTPUT" <<'PY'
import json, sys
raise SystemExit(any(r.get("record_type")=="scenario" and r["exit_status"] != 0 for r in map(json.loads,open(sys.argv[1]))))
PY
