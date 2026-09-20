#!/usr/bin/env bash
set -euo pipefail

C1_DIR="$(cd -P -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
HOST="${HOST:-37.27.110.241}"
KNOWN_HOSTS="${KNOWN_HOSTS:?set KNOWN_HOSTS to the out-of-band-authenticated known_hosts file}"
REMOTE_ROOT=/root

[[ -f "$KNOWN_HOSTS" && -s "$KNOWN_HOSTS" ]] \
  || { printf 'known_hosts file is missing or empty: %s\n' "$KNOWN_HOSTS" >&2; exit 2; }
ssh-keygen -F "$HOST" -f "$KNOWN_HOSTS" >/dev/null

SSH_OPTS=(
  -o BatchMode=yes
  -o IdentitiesOnly=yes
  -o StrictHostKeyChecking=yes
  -o GlobalKnownHostsFile=/dev/null
  -o "UserKnownHostsFile=$KNOWN_HOSTS"
)

ARCHIVE="$(mktemp)"
REMOTE_STAGE=
cleanup() {
  local status=$?
  trap - EXIT
  set +e
  rm -f -- "$ARCHIVE"
  if [[ "$status" -ne 0 && -n "$REMOTE_STAGE" ]]; then
    ssh "${SSH_OPTS[@]}" "root@$HOST" bash -s -- "$REMOTE_STAGE" "$REMOTE_ROOT" <<'REMOTE_CLEANUP' >/dev/null 2>&1 || true
set -euo pipefail
stage="$1"
remote_root="$2"
prefix="${remote_root%/}/.c1-host-setup.stage."
suffix="${stage#"$prefix"}"
[[ "$remote_root" == /* && "$remote_root" != / && -d "$remote_root" \
  && ! -L "$remote_root" && "$stage" == "$prefix"* \
  && "$suffix" =~ ^[[:alnum:]]{6}$ ]] \
  || exit 2
rm -rf -- "$stage"
REMOTE_CLEANUP
  fi
  exit "$status"
}
trap cleanup EXIT

tar -czf "$ARCHIVE" -C "$(dirname -- "$C1_DIR")" "$(basename -- "$C1_DIR")"
DIGEST="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
[[ "$DIGEST" =~ ^[[:xdigit:]]{64}$ ]] || { printf 'could not compute archive digest\n' >&2; exit 2; }

REMOTE_STAGE="$(ssh "${SSH_OPTS[@]}" "root@$HOST" \
  'mktemp -d /root/.c1-host-setup.stage.XXXXXX')"
if [[ ! "$REMOTE_STAGE" =~ ^/root/\.c1-host-setup\.stage\.[[:alnum:]]{6}$ ]]; then
  printf 'remote staging directory is missing or unsafe: %q\n' "$REMOTE_STAGE" >&2
  exit 2
fi

scp "${SSH_OPTS[@]}" "$ARCHIVE" "root@$HOST:$REMOTE_STAGE/payload.tar.gz"
ssh "${SSH_OPTS[@]}" "root@$HOST" bash -s -- "$DIGEST" "$REMOTE_STAGE" "$REMOTE_ROOT" \
  < "$C1_DIR/promote-staged-tree.sh"
