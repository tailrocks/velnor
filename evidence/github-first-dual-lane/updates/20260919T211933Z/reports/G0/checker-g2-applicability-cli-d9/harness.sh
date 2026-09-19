#!/bin/zsh
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
BASE=${BASE:-$SCRIPT_DIR/fixtures}
OUT=${OUT:-$SCRIPT_DIR/results}
CHECKER=${CHECKER:-/private/tmp/g3-checker-g2-app-d9/target/debug/velnor-tools}

/bin/rm -rf "$OUT"
/bin/mkdir -p "$OUT"

run_case() {
  local name="$1"
  local m="$OUT/$name/manifest.json"
  local s="$OUT/$name/snapshot.json"
  local e="$OUT/$name/evidence.json"
  local r="$OUT/$name/release.json"
  local report="$OUT/$name/report.json"
  /bin/mkdir -p "$OUT/$name"
  /bin/cp "$BASE/manifest.json" "$m"
  /bin/cp "$BASE/snapshot.json" "$s"
  /bin/cp "$BASE/evidence.json" "$e"
  /bin/cp "$BASE/release.json" "$r"
  case "$name" in
    applicable-install-na)
      /usr/bin/jq '.repositories |= map(if .repository == "tailrocks/velnor" then .release_applicability="applicable" else . end)' "$m" > "$m.tmp"
      /bin/mv "$m.tmp" "$m"
      /usr/bin/jq '.records |= map(if .repository == "tailrocks/velnor" and .pr_number == null then .install={applicability:"not-applicable",justification:"fixture waiver",environment:null,operations:null,installed:null,service:null,functional_result:null} else . end)' "$e" > "$e.tmp"
      /bin/mv "$e.tmp" "$e"
      ;;
    applicable-release-install-na)
      /usr/bin/jq '.repositories |= map(if .repository == "tailrocks/velnor" then .release_applicability="applicable" else . end)' "$m" > "$m.tmp"
      /bin/mv "$m.tmp" "$m"
      /usr/bin/jq '.records |= map(if .repository == "tailrocks/velnor" and .pr_number == null then .release={applicability:"not-applicable",justification:"fixture release waiver",execution:null} | .install={applicability:"excluded",justification:"fixture install waiver",environment:null,operations:null,installed:null,service:null,functional_result:null} else . end)' "$e" > "$e.tmp"
      /bin/mv "$e.tmp" "$e"
      ;;
    required-release-applicable)
      /usr/bin/jq '.records |= map(if .repository == "tailrocks/velnor" and .pr_number == null then .release.applicability="applicable" | .release.justification="record downgrade" else . end)' "$e" > "$e.tmp"
      /bin/mv "$e.tmp" "$e"
      ;;
    required-install-applicable)
      /usr/bin/jq '.records |= map(if .repository == "tailrocks/velnor" and .pr_number == null then .install.applicability="applicable" | .install.justification="record downgrade" else . end)' "$e" > "$e.tmp"
      /bin/mv "$e.tmp" "$e"
      ;;
    required-install-na)
      /usr/bin/jq '.records |= map(if .repository == "tailrocks/velnor" and .pr_number == null then .install={applicability:"not-applicable",justification:"record waiver",environment:null,operations:null,installed:null,service:null,functional_result:null} else . end)' "$e" > "$e.tmp"
      /bin/mv "$e.tmp" "$e"
      ;;
    required-install-excluded)
      /usr/bin/jq '.records |= map(if .repository == "tailrocks/velnor" and .pr_number == null then .install={applicability:"excluded",justification:"record exclusion",environment:null,operations:null,installed:null,service:null,functional_result:null} else . end)' "$e" > "$e.tmp"
      /bin/mv "$e.tmp" "$e"
      ;;
    required-missing-upgrade)
      /usr/bin/jq '.records |= map(if .repository == "tailrocks/velnor" and .pr_number == null then .install.operations.same_channel_upgrade.predecessor=null | .install.operations.channel_switch.predecessor=null else . end)' "$e" > "$e.tmp"
      /bin/mv "$e.tmp" "$e"
      ;;
    required-platform-mismatch)
      /usr/bin/jq '.records |= map(if .repository == "tailrocks/velnor" and .pr_number == null then .install.environment.architecture="arm64" else . end)' "$e" > "$e.tmp"
      /bin/mv "$e.tmp" "$e"
      ;;
    baseline)
      ;;
  esac
  set +e
  "$CHECKER" evidence-check --stage G2 --manifest "$m" --snapshot "$s" --evidence "$e" --release-manifest "$r" --json > "$report" 2> "$OUT/$name/stderr"
  local rc=$?
  set -e
  local result
  result=$(/usr/bin/jq -c --arg case "$name" --argjson exit_code "$rc" '{case:$case,exit_code:$exit_code,status,findings:(.findings|length),codes:(.findings|map(.code)|unique)}' "$report")
  print -r -- "$result" >> "$OUT/results.ndjson"
}

: > "$OUT/results.ndjson"
run_case baseline
run_case applicable-install-na
run_case applicable-release-install-na
run_case required-release-applicable
run_case required-install-applicable
run_case required-install-na
run_case required-install-excluded
run_case required-missing-upgrade
run_case required-platform-mismatch
cat "$OUT/results.ndjson"
