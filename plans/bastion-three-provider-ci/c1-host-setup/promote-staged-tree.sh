#!/usr/bin/env bash
set -euo pipefail

digest="${1:-}"
stage="${2:-}"
remote_root="${3:-}"
[[ "$digest" =~ ^[[:xdigit:]]{64}$ ]] || { printf 'invalid archive digest\n' >&2; exit 2; }
[[ "$remote_root" == /* && "$remote_root" != / && -d "$remote_root" && ! -L "$remote_root" ]] \
  || { printf 'remote root must be an existing real directory\n' >&2; exit 2; }
promotion_owner="${PROMOTION_LOCK_OWNER:-0:0}"
root_owner="$(stat -c '%u:%g' -- "$remote_root")" \
  || { printf 'cannot inspect remote root ownership\n' >&2; exit 2; }
root_mode="$(stat -c '%a' -- "$remote_root")" \
  || { printf 'cannot inspect remote root mode\n' >&2; exit 2; }
[[ "$root_owner" == "$promotion_owner" && "$root_mode" =~ ^[0-7]{3,4}$ ]] \
  && (( (8#$root_mode & 0022) == 0 )) \
  || { printf 'remote root must be owned by %s and not group/world writable\n' "$promotion_owner" >&2; exit 2; }
stage_prefix="${remote_root%/}/.c1-host-setup.stage."
stage_suffix="${stage#"$stage_prefix"}"
[[ "$stage" == "$stage_prefix"* && "$stage_suffix" =~ ^[[:alnum:]]{6}$ \
  && -d "$stage" && ! -L "$stage" ]] \
  || { printf 'remote staging directory is missing or unsafe\n' >&2; exit 2; }

target="${remote_root%/}/c1-host-setup"
promotion_lock="${remote_root%/}/.c1-host-setup.promote.lock"
promotion_flock_bin="${PROMOTION_FLOCK_BIN:-/usr/bin/flock}"
promotion_lock_timeout="${PROMOTION_LOCK_TIMEOUT_SECONDS:-120}"
[[ "$promotion_lock_timeout" =~ ^[1-9][0-9]*$ ]] \
  && (( promotion_lock_timeout <= 3600 )) \
  || { printf 'promotion lock timeout must be from 1 through 3600 seconds\n' >&2; exit 2; }
promotion_lock_fd=
promotion_lock_held=0
payload="$stage/payload.tar.gz"
release=
link=
previous_symlink=0
previous_target=
target_state_captured=0

restore_previous_target() {
  local current_target=
  if [[ -L "$target" ]]; then
    current_target="$(readlink -- "$target")" || return 1
  fi

  if [[ "$previous_symlink" == 1 ]]; then
    [[ "$current_target" != "$previous_target" ]] || return 0
    if [[ -e "$target" || -L "$target" ]]; then
      [[ "$current_target" == "$release" ]] || return 1
    fi
    rm -f -- "$link" || return 1
    ln -s -- "$previous_target" "$link" || return 1
    mv -Tf -- "$link" "$target" || return 1
  else
    if [[ -e "$target" || -L "$target" ]]; then
      [[ "$current_target" == "$release" ]] || return 1
      rm -f -- "$target" || return 1
    fi
  fi
}

cleanup() {
  local status=$?
  local rollback_failed=0
  trap - EXIT
  set +e
  if [[ "$status" -ne 0 ]]; then
    if [[ "$target_state_captured" == 1 ]] && ! restore_previous_target; then
      rollback_failed=1
      printf 'warning: could not restore the previous host-setup target; preserving recovery paths\n' >&2
    fi
    if [[ "$rollback_failed" == 0 && -n "$release" ]]; then rm -rf -- "$release"; fi
  fi
  if [[ "$rollback_failed" == 0 && -n "$link" ]]; then rm -rf -- "$link"; fi
  rm -rf -- "$stage"
  if [[ "$promotion_lock_held" == 1 && -n "$promotion_lock_fd" ]]; then
    "$promotion_flock_bin" --unlock "$promotion_lock_fd" >/dev/null 2>&1 || true
    exec {promotion_lock_fd}>&-
  fi
  exit "$status"
}
trap cleanup EXIT

if [[ -L "$promotion_lock" ]]; then
  printf 'promotion lock path is a symlink\n' >&2
  exit 2
fi
if [[ ! -e "$promotion_lock" ]]; then
  (umask 077; set -o noclobber; : > "$promotion_lock") 2>/dev/null || true
fi
[[ ! -L "$promotion_lock" && -f "$promotion_lock" ]] \
  || { printf 'promotion lock is not a regular file\n' >&2; exit 2; }
lock_mode="$(stat -c '%a' -- "$promotion_lock")" \
  || { printf 'cannot inspect promotion lock mode\n' >&2; exit 2; }
lock_links="$(stat -c '%h' -- "$promotion_lock")" \
  || { printf 'cannot inspect promotion lock link count\n' >&2; exit 2; }
lock_owner="$(stat -c '%u:%g' -- "$promotion_lock")" \
  || { printf 'cannot inspect promotion lock ownership\n' >&2; exit 2; }
[[ "$lock_mode" == 600 && "$lock_links" == 1 && "$lock_owner" == "$promotion_owner" ]] \
  || { printf 'promotion lock must be owned by %s, single-link, mode 0600\n' "$promotion_owner" >&2; exit 2; }
exec {promotion_lock_fd}<>"$promotion_lock" \
  || { printf 'cannot open promotion lock\n' >&2; exit 2; }
lock_deadline=$((SECONDS + promotion_lock_timeout))
while :; do
  if "$promotion_flock_bin" --exclusive --nonblock "$promotion_lock_fd"; then
    promotion_lock_held=1
    break
  else
    lock_status=$?
  fi
  if [[ "$lock_status" != 1 ]]; then
    printf 'cannot acquire promotion lock\n' >&2
    exit 2
  fi
  if (( SECONDS >= lock_deadline )); then
    printf 'timed out acquiring promotion lock\n' >&2
    exit 2
  fi
  sleep 1
done

[[ ! -L "$payload" && -f "$payload" ]] \
  || { printf 'staged archive is missing or unsafe\n' >&2; exit 2; }
printf '%s  %s\n' "$digest" "$payload" | sha256sum -c -
tar -tzf "$payload" | awk '
  /^\// || /(^|\/)\.\.(\/|$)/ || $0 !~ /^c1-host-setup(\/|$)/ { exit 1 }
' || { printf 'payload has an unexpected path\n' >&2; exit 2; }

if [[ -L "$target" ]]; then
  previous_symlink=1
  previous_target="$(readlink -- "$target")"
elif [[ -e "$target" ]]; then
  printf 'existing physical c1-host-setup directories require an explicit one-time migration before promotion\n' >&2
  exit 2
fi
target_state_captured=1

mkdir "$stage/tree"
tar --no-same-owner --no-same-permissions -xzf "$payload" -C "$stage/tree"
[[ -f "$stage/tree/c1-host-setup/provision-bastion.sh" \
  && ! -L "$stage/tree/c1-host-setup/provision-bastion.sh" ]] \
  || { printf 'archive does not contain a regular provisioner script\n' >&2; exit 2; }
tree_root="$stage/tree/c1-host-setup"
if [[ -n "$(find "$tree_root" -type l -print -quit)" ]]; then
  printf 'archive contains a symlink in the host-setup release\n' >&2
  exit 2
fi
if [[ -n "$(find "$tree_root" -type f -links +1 -print -quit)" ]]; then
  printf 'archive contains a hard link in the host-setup release\n' >&2
  exit 2
fi
# Tar metadata comes from the control-host checkout. Normalize before making
# the release reachable: root owns every path, directories are not writable
# by others, and files preserve only their executable bit.
chown -R 0:0 -- "$tree_root"
find "$tree_root" -type d -exec chmod 0755 -- {} +
find "$tree_root" -type f -perm /111 -exec chmod 0755 -- {} +
find "$tree_root" -type f ! -perm /111 -exec chmod 0644 -- {} +
release="$(mktemp -d "${remote_root%/}/.c1-host-setup.release.XXXXXX")"
rmdir "$release"
mv -- "$stage/tree/c1-host-setup" "$release"
link="$(mktemp -d "${remote_root%/}/.c1-host-setup.next.XXXXXX")"
rmdir "$link"
ln -s -- "$release" "$link"

mv -Tf -- "$link" "$target"
printf 'deployed %s to %s\n' "$digest" "$target"
