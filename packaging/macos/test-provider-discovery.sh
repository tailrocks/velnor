#!/bin/sh
# Launcher integration fixtures. They exercise discovery without starting a
# real runner or touching the user's Docker configuration.
set -eu

root=$(CDPATH="" cd -- "$(dirname -- "$0")/../.." && pwd -P)
launcher="$root/packaging/macos/velnor-runner-launch"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/velnor-provider.XXXXXX")
tmp=$(CDPATH="" cd -- "$tmp" && pwd -P)
provider_root="$tmp/provider"
socket_path="$provider_root/run/docker.sock"
config_path="$tmp/docker-config.json"

cleanup() {
  if [ -n "${socket_pid:-}" ]; then
    kill "$socket_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

mkdir -p "$tmp/bin" "$tmp/libexec" "$tmp/state" "$provider_root/bin" "$provider_root/run"
cp "$launcher" "$tmp/libexec/velnor-runner-launch"
chmod 755 "$tmp/libexec/velnor-runner-launch"
printf '%s\n' '{"currentContext":"desktop-linux"}' > "$config_path"

cat > "$provider_root/bin/docker-credential-fixture" <<'FAKE_HELPER'
#!/bin/sh
exit 0
FAKE_HELPER
chmod 755 "$provider_root/bin/docker-credential-fixture"

# Leave a real socket inode for the launcher's -S check. The listener is not
# needed: the fake Docker CLI supplies the version/info responses.
ruby -rsocket -e 'UNIXServer.new(ARGV.fetch(0)).close' "$socket_path"

cat > "$tmp/bin/docker" <<'FAKE_DOCKER'
#!/bin/sh
set -eu

printf '%s\n' "$*" >> "$FAKE_DOCKER_LOG"
host=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --host|--context)
      [ "$#" -ge 2 ] || exit 64
      selected=$2
      if [ "$1" = "--host" ]; then
        host=$2
      fi
      shift 2
      ;;
    *)
      break
      ;;
  esac
done

command_name=${1:-}
shift || true
case "$command_name" in
  context)
    subcommand=${1:-}
    case "$subcommand" in
      show)
        printf '%s\n' "${FAKE_DOCKER_CONTEXT_NAME:-desktop-linux}"
        ;;
      inspect)
        printf '{"Name":"%s","Metadata":{"Description":"%s"},"Endpoints":{"docker":{"Host":"%s"}}}\n' \
          "${FAKE_DOCKER_CONTEXT_NAME-desktop-linux}" \
          "${FAKE_DOCKER_DESCRIPTION-Docker Desktop}" \
          "${FAKE_DOCKER_ENDPOINT-unix://${FAKE_DOCKER_SOCKET}}"
        ;;
      use)
        : > "$FAKE_DOCKER_CONTEXT_USE_MARKER"
        exit 99
        ;;
      *)
        exit 64
        ;;
    esac
    ;;
  version)
    [ "$host" = "${FAKE_EXPECT_HOST:-}" ] || exit 65
    printf '%s\n' '{"Os":"linux","Arch":"arm64"}'
    ;;
  info)
    [ "$host" = "${FAKE_EXPECT_HOST:-}" ] || exit 65
    printf '{"ID":"%s","OperatingSystem":"%s","OSType":"linux","Architecture":"aarch64","Name":"%s","ClientInfo":{"Os":"darwin"}}\n' \
      "${FAKE_DOCKER_DAEMON_ID-fixture-engine}" \
      "${FAKE_DOCKER_SERVER_OS-Docker Desktop}" \
      "${FAKE_DOCKER_SERVER_NAME-docker-desktop}"
    ;;
  *)
    exit 64
    ;;
esac
FAKE_DOCKER
chmod 755 "$tmp/bin/docker"

cat > "$tmp/bin/velnor-runner" <<'FAKE_RUNNER'
#!/bin/sh
set -eu
{
  printf 'args=%s\n' "$*"
  env | sort | grep -E '^(DOCKER_CONTEXT|DOCKER_HOST|VELNOR_DOCKER_CONTEXT|VELNOR_DOCKER_HOST|VELNOR_DOCKER_DAEMON_ID|VELNOR_GITHUB_HTTP_TRANSPORT|PATH)=' || true
} > "$FAKE_RUNNER_ENV"
FAKE_RUNNER
chmod 755 "$tmp/bin/velnor-runner"

write_env() {
  env_path=$1
  extra=$2
  cat > "$env_path" <<ENV
GITHUB_TOKEN=test-token
VELNOR_URL=https://github.com/tailrocks/velnor
VELNOR_HOST_MODE=native-only
VELNOR_NAME=fixture
VELNOR_LABELS=fixture
VELNOR_SLOTS=1
VELNOR_DOCKER_IMAGE=fixture:latest
$extra
ENV
}

run_launcher() {
  case_name=$1
  extra=${2:-}
  fake_endpoint=${3-unix://$socket_path}
  fake_description=${4-Docker Desktop}
  fake_server_os=${5-Docker Desktop}
  env_path="$tmp/$case_name.env"
  runner_env="$tmp/$case_name.runner-env"
  docker_log="$tmp/$case_name.docker-log"
  write_env "$env_path" "$extra"
  : > "$docker_log"
  : > "$runner_env"
  env \
    -u VELNOR_DOCKER_CONTEXT \
    -u VELNOR_DOCKER_HOST \
    -u DOCKER_CONTEXT \
    -u DOCKER_HOST \
    -u VELNOR_GITHUB_HTTP_TRANSPORT \
    FAKE_DOCKER_CONFIG="$config_path" \
    FAKE_DOCKER_CONTEXT_NAME=desktop-linux \
    FAKE_DOCKER_DESCRIPTION="Docker Desktop" \
    FAKE_DOCKER_ENDPOINT="unix://$socket_path" \
    FAKE_DOCKER_SOCKET="$socket_path" \
    FAKE_DOCKER_EXPECT_HOST="unix://$socket_path" \
    FAKE_EXPECT_HOST="unix://$socket_path" \
    FAKE_DOCKER_SERVER_OS="Docker Desktop" \
    FAKE_DOCKER_SERVER_NAME=docker-desktop \
    FAKE_DOCKER_DAEMON_ID=fixture-engine \
    FAKE_DOCKER_LOG="$docker_log" \
    FAKE_DOCKER_CONTEXT_USE_MARKER="$tmp/$case_name.context-use" \
    FAKE_RUNNER_ENV="$runner_env" \
    VELNOR_ENV_FILE="$env_path" \
    VELNOR_CONFIG_DIR="$tmp/$case_name/config" \
    VELNOR_STORAGE_ROOT="$tmp/$case_name/storage" \
    VELNOR_WORK_DIR="$tmp/$case_name/work" \
    VELNOR_LOG_DIR="$tmp/$case_name/log" \
    VELNOR_STATE_DB="$tmp/$case_name/state.db" \
    VELNOR_PERMIT_LEDGER="$tmp/$case_name/permit-ledger.db" \
    VELNOR_MODE_STATE="$tmp/$case_name/mode-state.json" \
    VELNOR_PATH="$tmp/bin:/usr/bin:/bin" \
    VELNOR_WORKER_VERIFIER=gh \
    VELNOR_WORKER_VERIFIER_CONTRACT='attestation verify --help' \
    PATH="$tmp/bin:/usr/bin:/bin" \
    FAKE_DOCKER_LOG="$docker_log" \
    FAKE_DOCKER_CONTEXT_USE_MARKER="$tmp/$case_name.context-use" \
    FAKE_DOCKER_SOCKET="$socket_path" \
    FAKE_EXPECT_HOST="unix://$socket_path" \
    FAKE_DOCKER_CONFIG="$config_path" \
    FAKE_DOCKER_CONTEXT_NAME="${FAKE_DOCKER_CONTEXT_NAME:-desktop-linux}" \
    FAKE_DOCKER_DESCRIPTION="$fake_description" \
    FAKE_DOCKER_ENDPOINT="$fake_endpoint" \
    FAKE_DOCKER_SERVER_OS="$fake_server_os" \
    FAKE_DOCKER_SERVER_NAME="${FAKE_DOCKER_SERVER_NAME-docker-desktop}" \
    "$tmp/libexec/velnor-runner-launch" > "$tmp/$case_name.stdout" 2> "$tmp/$case_name.stderr"
}

before_config=$(shasum -a 256 "$config_path")
run_launcher success 'VELNOR_DOCKER_CONTEXT=desktop-linux'
after_config=$(shasum -a 256 "$config_path")
[ "$before_config" = "$after_config" ]
grep -F -- '--host unix://' "$tmp/success.docker-log" >/dev/null
grep -F 'version --format' "$tmp/success.docker-log" >/dev/null
grep -F 'info --format' "$tmp/success.docker-log" >/dev/null
grep -F "DOCKER_HOST=unix://$socket_path" "$tmp/success.runner-env" >/dev/null
grep -F "VELNOR_DOCKER_HOST=unix://$socket_path" "$tmp/success.runner-env" >/dev/null
grep -F 'VELNOR_DOCKER_CONTEXT=desktop-linux' "$tmp/success.runner-env" >/dev/null
grep -F 'VELNOR_DOCKER_DAEMON_ID=fixture-engine' "$tmp/success.runner-env" >/dev/null
grep -F "PATH=$tmp/bin:/usr/bin:/bin:$provider_root/bin" "$tmp/success.runner-env" >/dev/null
grep -F 'VELNOR_GITHUB_HTTP_TRANSPORT=curl' "$tmp/success.runner-env" >/dev/null
! grep -F 'DOCKER_CONTEXT=' "$tmp/success.runner-env"
! test -e "$tmp/success.context-use"

run_launcher explicit_native 'VELNOR_DOCKER_CONTEXT=desktop-linux
VELNOR_GITHUB_HTTP_TRANSPORT=native'
grep -F 'VELNOR_GITHUB_HTTP_TRANSPORT=native' "$tmp/explicit_native.runner-env" >/dev/null

run_launcher explicit_curl 'VELNOR_DOCKER_CONTEXT=desktop-linux
VELNOR_GITHUB_HTTP_TRANSPORT=curl'
grep -F 'VELNOR_GITHUB_HTTP_TRANSPORT=curl' "$tmp/explicit_curl.runner-env" >/dev/null

run_launcher invalid_transport 'VELNOR_DOCKER_CONTEXT=desktop-linux
VELNOR_GITHUB_HTTP_TRANSPORT=invalid'
grep -F 'VELNOR_GITHUB_HTTP_TRANSPORT=invalid' "$tmp/invalid_transport.runner-env" >/dev/null
! grep -F 'VELNOR_GITHUB_HTTP_TRANSPORT=curl' "$tmp/invalid_transport.runner-env" >/dev/null

run_failure() {
  case_name=$1
  endpoint=$2
  description=$3
  server_os=$4
  if run_launcher "$case_name" "" "$endpoint" "$description" "$server_os"; then
    printf '%s\n' "fixture unexpectedly succeeded: $case_name" >&2
    exit 1
  fi
  ! test -s "$tmp/$case_name.runner-env"
}

run_failure remote 'tcp://127.0.0.1:2375' 'Docker Desktop' 'Docker Desktop'
run_failure relative 'unix://relative.sock' 'Docker Desktop' 'Docker Desktop'
printf '%s\n' 'not-a-socket' > "$tmp/not-a-socket"
run_failure non_socket "unix://$tmp/not-a-socket" 'Docker Desktop' 'Docker Desktop'
run_failure missing_provider "unix://$socket_path" '' 'Docker Desktop'
run_failure contradictory_provider "unix://$socket_path" 'Mystery Provider' 'Mystery Engine'
run_failure missing_endpoint '' 'Docker Desktop' 'Docker Desktop'

printf '%s\n' 'macOS provider discovery fixtures: PASS'
