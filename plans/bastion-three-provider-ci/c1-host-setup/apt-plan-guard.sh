#!/usr/bin/env bash
set -euo pipefail

die() {
  printf 'APT transaction plan guard: %s\n' "$1" >&2
  exit 2
}

[[ "$#" -eq 1 ]] || die 'expected one approved plan file'
plan_file="$1"
[[ ! -L "$plan_file" && -f "$plan_file" ]] || die 'approved plan is not a regular file'
metadata="$(stat -c '%u:%g %a %h' -- "$plan_file")" \
  || die 'cannot inspect approved plan metadata'
read -r owner mode links <<< "$metadata"
[[ "$owner" == 0:0 && "$mode" == 600 && "$links" == 1 ]] \
  || die 'approved plan has unsafe owner, mode, or link count'

guard_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
expected_file="$(mktemp "${plan_file}.expected.XXXXXX")" \
  || die 'cannot create expected-action snapshot'
actual_file="$(mktemp "${plan_file}.actual.XXXXXX")" \
  || die 'cannot create actual-action snapshot'
trap 'rm -f -- "$expected_file" "$actual_file"' EXIT

declare -A policy_hashes=() archive_hashes=() seen_actions=()
input_config=
input_source_list=
input_source_parts=
input_fingerprint=
input_architecture=
input_count=0
input_keyrings=()
declare -A hook_config_values=()
while IFS=$'\t' read -r kind first second third fourth fifth sixth; do
  [[ -n "$kind" ]] || continue
  case "$kind" in
    META)
      case "$first" in
        INPUTS)
          [[ "$input_count" == 0 && -n "$second" && -n "$third" \
            && -n "$fourth" && "$fifth" =~ ^[[:xdigit:]]{64}$ \
            && "$sixth" =~ ^[a-z0-9][a-z0-9-]*$ ]] \
            || die 'malformed or duplicate APT input evidence'
          input_config="$second"
          input_source_list="$third"
          input_source_parts="$fourth"
          input_fingerprint="$fifth"
          input_architecture="$sixth"
          input_count=1
          ;;
        KEYRING)
          [[ -n "$second" && -z "${third:-}" && -z "${fourth:-}" \
            && -z "${fifth:-}" && -z "${sixth:-}" ]] \
            || die 'malformed APT keyring evidence'
          input_keyrings+=("$second")
          ;;
        POLICY)
          evidence_key="$second|$third"
          [[ -n "$second" && -n "$third" && "$fourth" =~ ^[[:xdigit:]]{64}$ \
            && -z "${fifth:-}" && -z "${sixth:-}" \
            && -z "${policy_hashes["$evidence_key"]+present}" ]] \
            || die 'malformed or duplicate APT policy evidence'
          policy_hashes["$evidence_key"]="$fourth"
          ;;
        ARCHIVE)
          evidence_key="$second|$third"
          [[ -n "$second" && -n "$third" && "$fourth" =~ ^[[:xdigit:]]{64}$ \
            && -z "${fifth:-}" && -z "${sixth:-}" \
            && -z "${archive_hashes["$evidence_key"]+present}" ]] \
            || die 'malformed or duplicate APT archive evidence'
          archive_hashes["$evidence_key"]="$fourth"
          ;;
        *) die "unknown APT evidence record: $first" ;;
      esac
      ;;
    ACTION)
      action="$first"
      package="${second%%:*}"
      old_version="$third"
      direction="$fourth"
      new_version="$fifth"
      [[ ( "$action" == UNPACK || "$action" == CONFIGURE ) \
        && -n "$package" && -n "$old_version" \
        && ( "$direction" == '<' || "$direction" == '>' || "$direction" == '=' ) \
        && -n "$new_version" && -z "${sixth:-}" ]] \
        || die 'malformed approved APT action'
      action_key="$action"$'\t'"$package"$'\t'"$old_version"$'\t'"$direction"$'\t'"$new_version"
      [[ -z "${seen_actions["$action_key"]+present}" ]] \
        || die "duplicate approved action for $package"
      seen_actions["$action_key"]=1
      printf '%s\t%s\t%s\t%s\t%s\n' \
        "$action" "$package" "$old_version" "$direction" "$new_version" \
        >> "$expected_file"
      ;;
    *) die "unknown approved plan record: $kind" ;;
  esac
done < "$plan_file"

[[ "$input_count" == 1 && -n "$input_fingerprint" \
  && "${#input_keyrings[@]}" -gt 0 ]] \
  || die 'approved plan omitted source/config evidence'
[[ "${#policy_hashes[@]}" -gt 0 && "${#archive_hashes[@]}" -gt 0 ]] \
  || die 'approved plan omitted APT origin/archive evidence'
[[ "${#seen_actions[@]}" -gt 0 ]] || die 'approved plan contains no package actions'
[[ "${APT_CONFIG:-}" == "$input_config" ]] \
  || die 'APT_CONFIG changed after resolver review'

current_fingerprint="$(python3 "$guard_dir/apt-input-fingerprint.py" \
  "$input_config" "$input_source_list" "$input_source_parts" "${input_keyrings[@]}")" \
  || die 'cannot recheck APT config, source, and key inputs'
[[ "$current_fingerprint" == "$input_fingerprint" ]] \
  || die 'APT config, active source files, or keyring changed after resolver review'

apt_policy_fingerprint() {
  local package="$1" policy
  policy="$(LC_ALL=C apt-cache policy "$package" 2>/dev/null)" \
    || die "cannot recheck APT origin policy for $package"
  printf '%s\n' "$policy" | sha256sum | awk '{ print $1 }'
}

verify_direction() {
  local old_version="$1" direction="$2" new_version="$3" expected
  if [[ "$old_version" == - ]]; then
    expected='<'
  elif dpkg --compare-versions "$new_version" gt "$old_version"; then
    expected='<'
  elif dpkg --compare-versions "$new_version" lt "$old_version"; then
    expected='>'
  elif dpkg --compare-versions "$new_version" eq "$old_version"; then
    expected='='
  else
    die "cannot compare APT old/new versions for $old_version -> $new_version"
  fi
  [[ "$direction" == "$expected" ]] \
    || die "APT direction $direction disagrees with $old_version -> $new_version"
}

IFS= read -r protocol || die 'APT did not provide a pre-install protocol header'
[[ "$protocol" == 'VERSION 2' ]] || die "unsupported APT hook protocol: ${protocol:-empty}"
separator_seen=0
while IFS= read -r line; do
  if [[ -z "$line" ]]; then
    separator_seen=1
    break
  fi
  [[ "$line" == *=* && "$line" != Config-Item:* ]] \
    || die "malformed APT hook config record: $line"
  config_key="${line%%=*}"
  config_value="${line#*=}"
  config_key="${config_key//[[:space:]]/}"
  config_value="${config_value#"${config_value%%[![:space:]]*}"}"
  config_value="${config_value%"${config_value##*[![:space:]]}"}"
  config_value="${config_value%;}"
  if (( ${#config_value} >= 2 )); then
    case "$config_value" in
      \"*\") config_value="${config_value:1:${#config_value}-2}" ;;
      \'*\') config_value="${config_value:1:${#config_value}-2}" ;;
    esac
  fi
  config_key="${config_key,,}"
  case "$config_key" in
    acquire::allowinsecurerepositories|acquire::allowdowngradetoinsecurerepositories|\
      acquire::allowweakrepositories|apt::get::allowunauthenticated|debug::nolocking|\
      dir::etc::sourcelist|dir::etc::sourceparts|apt::architecture)
      [[ -z "${hook_config_values[$config_key]+present}" ]] \
        || die "duplicate effective APT hook config record: $config_key"
      hook_config_values["$config_key"]="$config_value"
      ;;
  esac
  normalized_config_value="${config_value,,}"
  case "$config_key" in
    acquire::allowinsecurerepositories|acquire::allowdowngradetoinsecurerepositories|\
      acquire::allowweakrepositories|apt::get::allowunauthenticated|debug::nolocking)
      case "$normalized_config_value" in
        true|yes|1) die "unsafe APT option was enabled in transaction config: $config_key" ;;
      esac
      ;;
    dir::etc::sourcelist)
      [[ "$config_value" == "$input_source_list" ]] \
        || die 'APT sourcelist changed in transaction config'
      ;;
    dir::etc::sourceparts)
      [[ "$config_value" == "$input_source_parts" ]] \
        || die 'APT sourceparts changed in transaction config'
      ;;
    apt::architecture)
      [[ "$config_value" == "$input_architecture" ]] \
        || die 'APT architecture changed in transaction config'
      ;;
  esac
done
[[ "$separator_seen" == 1 ]] || die 'APT hook protocol omitted the config/action separator'

[[ "${hook_config_values[dir::etc::sourcelist]:-}" == "$input_source_list" ]] \
  || die 'APT hook omitted or changed the reviewed effective sourcelist'
[[ "${hook_config_values[dir::etc::sourceparts]:-}" == "$input_source_parts" ]] \
  || die 'APT hook omitted or changed the reviewed effective sourceparts'
[[ "${hook_config_values[apt::architecture]:-}" == "$input_architecture" ]] \
  || die 'APT hook omitted or changed the reviewed architecture'
current_architecture="$(dpkg --print-architecture 2>/dev/null)" \
  || die 'cannot recheck the native package architecture'
[[ "$current_architecture" == "$input_architecture" ]] \
  || die 'native package architecture changed after resolver review'
for key in \
  acquire::allowinsecurerepositories \
  acquire::allowdowngradetoinsecurerepositories \
  acquire::allowweakrepositories \
  apt::get::allowunauthenticated \
  debug::nolocking; do
  [[ "${hook_config_values[$key]:-}" == false ]] \
    || die "APT hook omitted or enabled required safe option: $key"
done

action_count=0
while IFS= read -r line; do
  [[ -n "$line" ]] || continue
  read -r raw_package old_version direction new_version action extra <<< "$line"
  package="${raw_package%%:*}"
  [[ -n "$package" && -n "$old_version" && -n "$direction" \
    && -n "$new_version" && -n "$action" && -z "${extra:-}" ]] \
    || die "malformed APT package action: $line"
  case "$action" in
    '**REMOVE**') die "APT transaction would remove $package" ;;
    '**CONFIGURE**')
      action_kind=CONFIGURE
      ;;
    *.deb)
      action_kind=UNPACK
      archive="$action"
      [[ "$archive" == /* && ! -L "$archive" && -f "$archive" ]] \
        || die "APT archive is not a regular absolute file for $package"
      ;;
    *) die "unsupported APT package action for $package: $action" ;;
  esac
  [[ "$new_version" != '-' ]] || die "APT action has no target version for $package"
  verify_direction "$old_version" "$direction" "$new_version"
  actual_key="$action_kind"$'\t'"$package"$'\t'"$old_version"$'\t'"$direction"$'\t'"$new_version"
  printf '%s\t%s\t%s\t%s\t%s\n' \
    "$action_kind" "$package" "$old_version" "$direction" "$new_version" \
    >> "$actual_file"
  [[ -n "${seen_actions["$actual_key"]+present}" ]] \
    || die "APT transaction changed package, old version, direction, or target for $package"
  if [[ "$action_kind" == UNPACK ]]; then
    evidence_key="$package|$new_version"
    [[ -n "${policy_hashes["$evidence_key"]:-}" \
      && -n "${archive_hashes["$evidence_key"]:-}" ]] \
      || die "approved policy/archive evidence is absent for $package=$new_version"
    current_policy_hash="$(apt_policy_fingerprint "$package")"
    [[ "$current_policy_hash" == "${policy_hashes["$evidence_key"]}" ]] \
      || die "APT candidate origin evidence changed for $package=$new_version"
    current_archive_hash="$(sha256sum -- "$archive" | awk '{ print $1 }')" \
      || die "cannot hash APT archive for $package=$new_version"
    [[ "$current_archive_hash" == "${archive_hashes["$evidence_key"]}" ]] \
      || die "APT archive content differs from reviewed metadata for $package=$new_version (reviewed ${archive_hashes["$evidence_key"]}, actual $current_archive_hash)"
  fi
  action_count=$((action_count + 1))
done
(( action_count > 0 )) || die 'APT transaction reported no package actions'

expected_sorted="$(LC_ALL=C sort "$expected_file")" || die 'cannot sort approved plan'
actual_sorted="$(LC_ALL=C sort "$actual_file")" || die 'cannot sort APT transaction actions'
[[ "$actual_sorted" == "$expected_sorted" ]] \
  || die 'APT transaction actions differ from the reviewed resolver plan'

printf 'APT transaction actions and source/config/origin/archive evidence match the approved plan.\n'
