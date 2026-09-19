#!/usr/bin/env bash
set -euo pipefail
umask 077

die() {
  printf 'trusted-bootstrap-harness: %s\n' "$1" >&2
  exit 1
}

need_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing command"
}

require_value() {
  local name="$1"
  [[ -n "${!name:-}" ]] || die "missing base-owned input"
}

is_sha256() {
  [[ "$1" =~ ^[0-9a-f]{64}$ ]]
}

is_digest() {
  [[ "$1" =~ ^sha256:[0-9a-f]{64}$ ]]
}

is_commit() {
  [[ "$1" =~ ^[0-9a-f]{40}$ ]]
}

for command in docker jq sha256sum timeout find df awk realpath mktemp cp chmod file python3 tr wc sed grep rm; do
  need_command "$command"
done

[[ "${G1_HOSTED_CANARY:-}" == 1 ]] || die "hosted canary opt-in required"
[[ "${RUNNER_OS:-}" == Linux ]] || die "Linux hosted runner required"
[[ "${RUNNER_ARCH:-}" == X64 ]] || die "x64 hosted runner required"
[[ "$(uname -s)" == Linux ]] || die "Linux kernel required"
[[ "$(uname -m)" == x86_64 ]] || die "x86_64 runner required"
docker info >/dev/null 2>&1 || die "Docker daemon unavailable"
[[ "$(docker info --format '{{.OSType}}')" == linux ]] || die "Linux Docker daemon required"
docker buildx version >/dev/null 2>&1 || die "Docker buildx unavailable"

base_inputs=(
  G1_TRUSTED_ROOT G1_EVIDENCE_DIR G1_HANDOFF_PATH G1_HANDOFF_SHA256
  G1_SOURCE_ARCHIVE G1_SOURCE_ARCHIVE_SHA256 G1_PRODUCER_ARCHIVE_SHA256
  G1_PROBE_PATH G1_PROBE_SHA256
  G1_BUILD_PATH G1_BUILD_SHA256 G1_BINARY_PATH G1_BINARY_SHA256
  G1_FIXTURE_CONTRACT_PATH G1_FIXTURE_CONTRACT_SHA256
  G1_TRUSTED_SCHEMA_PATH G1_TRUSTED_SCHEMA_SHA256
  G1_ARCHIVE_CHECK_PATH G1_ARCHIVE_CHECK_SHA256 G1_HARNESS_SHA256
  G1_EXPECTED_SOURCE_HEAD_SHA G1_EXPECTED_TREE_DIGEST G1_SOURCE_REPOSITORY
  G1_EXPECTED_SOURCE_CLOSURE G1_SANDBOX_IMAGE G1_SANDBOX_BASE_REF
  G1_SANDBOX_SOURCE_REVISION G1_SANDBOX_INDEX_DIGEST
  G1_SANDBOX_PLATFORM_DIGEST G1_SANDBOX_CONFIG_DIGEST
)
for name in "${base_inputs[@]}"; do
  require_value "$name"
done

for name in G1_HANDOFF_SHA256 G1_SOURCE_ARCHIVE_SHA256 G1_PRODUCER_ARCHIVE_SHA256 G1_PROBE_SHA256 G1_BUILD_SHA256 \
  G1_BINARY_SHA256 G1_FIXTURE_CONTRACT_SHA256 G1_TRUSTED_SCHEMA_SHA256 \
  G1_ARCHIVE_CHECK_SHA256 G1_HARNESS_SHA256 G1_EXPECTED_TREE_DIGEST \
  G1_EXPECTED_SOURCE_CLOSURE; do
  is_sha256 "${!name}" || die "bad hash input"
done
for name in G1_SANDBOX_INDEX_DIGEST G1_SANDBOX_PLATFORM_DIGEST G1_SANDBOX_CONFIG_DIGEST; do
  is_digest "${!name}" || die "bad image digest input"
done
is_commit "$G1_EXPECTED_SOURCE_HEAD_SHA" || die "bad source head input"
is_commit "$G1_SANDBOX_SOURCE_REVISION" || die "bad image source revision"
[[ "$G1_SOURCE_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "bad source repository"
[[ "$G1_SANDBOX_BASE_REF" == ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b ]] || die "unapproved sandbox base"
[[ "$G1_SANDBOX_IMAGE" =~ ^[a-z0-9./_-]+$ ]] || die "bad sandbox image name"

trusted_root="$(realpath -e -- "$G1_TRUSTED_ROOT")" || die "trusted root unavailable"
[[ -d "$trusted_root" ]] || die "trusted root is not a directory"

path_under_root() {
  local path="$1"
  case "$path" in
    "$trusted_root"/*) return 0 ;;
    *) return 1 ;;
  esac
}

trusted_file() {
  local path="$1"
  local expected="$2"
  test -f "$path" || die "trusted file missing"
  test ! -L "$path" || die "trusted file is symlink"
  local resolved
  resolved="$(realpath -e -- "$path")" || die "trusted file path unavailable"
  [[ "$resolved" == "$path" ]] || die "trusted file path is not canonical"
  path_under_root "$resolved" || die "trusted file outside root"
  local actual
  actual="$(sha256sum -- "$resolved" | awk '{print $1}')"
  [[ "$actual" == "$expected" ]] || die "trusted file hash mismatch"
}

trusted_file "$G1_HANDOFF_PATH" "$G1_HANDOFF_SHA256"
trusted_file "$G1_SOURCE_ARCHIVE" "$G1_SOURCE_ARCHIVE_SHA256"
trusted_file "$G1_PROBE_PATH" "$G1_PROBE_SHA256"
trusted_file "$G1_BUILD_PATH" "$G1_BUILD_SHA256"
trusted_file "$G1_BINARY_PATH" "$G1_BINARY_SHA256"
trusted_file "$G1_FIXTURE_CONTRACT_PATH" "$G1_FIXTURE_CONTRACT_SHA256"
trusted_file "$G1_TRUSTED_SCHEMA_PATH" "$G1_TRUSTED_SCHEMA_SHA256"
trusted_file "$G1_ARCHIVE_CHECK_PATH" "$G1_ARCHIVE_CHECK_SHA256"
script_path="$(realpath -e -- "$0")" || die "harness path unavailable"
trusted_file "$script_path" "$G1_HARNESS_SHA256"

jq empty "$G1_HANDOFF_PATH" >/dev/null || die "handoff is not JSON"
jq empty "$G1_FIXTURE_CONTRACT_PATH" >/dev/null || die "fixture contract is not JSON"
jq empty "$G1_TRUSTED_SCHEMA_PATH" >/dev/null || die "trusted schema is not JSON"
jq -e \
  --arg head "$G1_EXPECTED_SOURCE_HEAD_SHA" \
  --arg tree "$G1_EXPECTED_TREE_DIGEST" \
  --arg repo "$G1_SOURCE_REPOSITORY" \
  --arg closure "$G1_EXPECTED_SOURCE_CLOSURE" \
  --arg archive "$G1_SOURCE_ARCHIVE_SHA256" \
  --arg producer_archive "$G1_PRODUCER_ARCHIVE_SHA256" \
  --arg probe "$G1_PROBE_SHA256" \
  --arg build "$G1_BUILD_SHA256" \
  --arg binary "$G1_BINARY_SHA256" \
  --arg contract "$G1_FIXTURE_CONTRACT_SHA256" \
  --arg schema "$G1_TRUSTED_SCHEMA_SHA256" \
  --arg checker "$G1_ARCHIVE_CHECK_SHA256" \
  --arg harness "$G1_HARNESS_SHA256" \
  --arg image "$G1_SANDBOX_IMAGE" \
  --arg base "$G1_SANDBOX_BASE_REF" \
  --arg revision "$G1_SANDBOX_SOURCE_REVISION" \
  --arg index "$G1_SANDBOX_INDEX_DIGEST" \
  --arg platform "$G1_SANDBOX_PLATFORM_DIGEST" \
  --arg config "$G1_SANDBOX_CONFIG_DIGEST" '
    .schema == "velnor.bootstrap.handoff.v1" and
    .workflow_path == ".github/workflows/ci-pr.yml" and
    .event == "pull_request" and
    .head_sha == $head and
    .source_repository == $repo and
    .source_closure == $closure and
    .profile == "debug" and
    .features == [] and
    .platform == "linux-amd64" and
    (.producer.run_id | type == "number" and . > 0) and
    (.producer.job_id | type == "number" and . > 0) and
    (.producer.artifact_id | type == "number" and . > 0) and
    (.producer.job_name | type == "string" and length > 0) and
    (.producer.artifact_name | type == "string" and length > 0) and
    (.producer.artifact_service_digest | test("^sha256:[0-9a-f]{64}$")) and
    .producer.archive_sha256 == $producer_archive and
    .binary.sha256 == $binary and
    .source_archive.sha256 == $archive and
    .source_archive.head_sha == $head and
    .source_archive.tree_digest == $tree and
    .fixture.probe_sha256 == $probe and
    .fixture.build_sha256 == $build and
    .fixture.contract_sha256 == $contract and
    .fixture.schema_sha256 == $schema and
    .fixture.archive_checker_sha256 == $checker and
    .fixture.harness_sha256 == $harness and
    .sandbox.image == $image and
    .sandbox.base == $base and
    .sandbox.source_revision == $revision and
    .sandbox.index_digest == $index and
    .sandbox.platform_digest == $platform and
    .sandbox.config_digest == $config
  ' "$G1_HANDOFF_PATH" >/dev/null || die "handoff contract mismatch"
jq -e \
  --arg base "$G1_SANDBOX_BASE_REF" '
    .schema == "velnor.bootstrap-hostile-probe.v1" and
    .execution == "future-hosted-canary-only" and
    .sandbox_base_image == $base and
    .sandbox_image_ref_policy == "final-platform-manifest-digest-only" and
    .authority == "diagnostic-only; trusted wrapper and verifier decide" and
    ([.required_isolation[]] | index("network-none") != null) and
    ([.required_isolation[]] | index("bounded-output-tmp") != null) and
    ([.required_isolation[]] | index("numeric-non-root") != null)
  ' "$G1_FIXTURE_CONTRACT_PATH" >/dev/null || die "fixture contract mismatch"

runner_temp="$(realpath -e -- "${RUNNER_TEMP:-}")" || die "runner temp unavailable"
[[ -d "$runner_temp" ]] || die "runner temp is not a directory"
path_under_root "$runner_temp" || die "runner temp is outside trusted root"
available_kib="$(df -Pk -- "$runner_temp" | awk 'NR == 2 { print $4 }')"
[[ "$available_kib" =~ ^[0-9]+$ ]] || die "runner free-space check failed"
(( available_kib >= 524288 )) || die "runner staging budget unavailable"

evidence_root="$(realpath -e -- "$G1_EVIDENCE_DIR")" || die "evidence root unavailable"
[[ -d "$evidence_root" ]] || die "evidence root is not a directory"
path_under_root "$evidence_root" || die "evidence root outside trusted root"
evidence="$evidence_root/bootstrap-hostile-$G1_EXPECTED_SOURCE_HEAD_SHA"
test ! -e "$evidence" || die "evidence directory already exists"
mkdir -m 0700 -- "$evidence"

stage=""
cid=""
cleanup() {
  local status=$?
  set +e
  if [[ -n "$cid" ]]; then
    docker rm -f "$cid" >/dev/null 2>&1
    test -z "$(docker ps -aq --filter "id=$cid")" || status=1
  fi
  if [[ -n "$stage" && -d "$stage" ]]; then
    rm -rf -- "$stage"
    test ! -e "$stage" || status=1
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

stage="$(mktemp -d "$runner_temp/velnor-bootstrap-hostile.XXXXXX")" || die "staging allocation failed"
chmod 0700 "$stage"
input="$stage/input"
candidate="$stage/candidate"
hostout="$stage/output"
mkdir -m 0755 -- "$candidate"
mkdir -m 0700 -- "$hostout"

python3 -B "$G1_ARCHIVE_CHECK_PATH" "$G1_SOURCE_ARCHIVE" >"$evidence/archive-summary.json"
python3 -B "$G1_ARCHIVE_CHECK_PATH" --extract "$input" "$G1_SOURCE_ARCHIVE" >"$evidence/archive-extract-summary.json"
find -P "$input" -name .git -print -quit | grep -q . && die "source archive contains git metadata" || :
find -P "$input" -type l -print -quit | grep -q . && die "source archive extracted a link" || :
find -P "$input" ! -type f ! -type d -print -quit | grep -q . && die "source archive extracted a special file" || :
chmod -R a-w -- "$input"

cp -- "$G1_BINARY_PATH" "$candidate/velnor-hostile-producer"
chmod 0555 -- "$candidate/velnor-hostile-producer"
test ! -L "$candidate/velnor-hostile-producer"
[[ "$(sha256sum -- "$candidate/velnor-hostile-producer" | awk '{print $1}')" == "$G1_BINARY_SHA256" ]] || die "staged binary hash mismatch"
file -b -- "$candidate/velnor-hostile-producer" | grep -Eq '^ELF 64-bit LSB.*x86-64' || die "candidate is not Linux x86-64 ELF"
find -P "$candidate" ! -type f ! -type d -print -quit | grep -q . && die "candidate staging type mismatch" || :

index_ref="$G1_SANDBOX_IMAGE@$G1_SANDBOX_INDEX_DIGEST"
platform_ref="$G1_SANDBOX_IMAGE@$G1_SANDBOX_PLATFORM_DIGEST"
docker buildx imagetools inspect --raw "$index_ref" >"$evidence/image-index.json" || die "image index inspection failed"
platform_from_index="$(jq -er '
  [.manifests[]? |
   select(.platform.os == "linux" and .platform.architecture == "amd64") |
   select((.annotations["vnd.docker.reference.type"] // "") != "attestation-manifest") |
   .digest] | if length == 1 then .[0] else error("platform") end
' "$evidence/image-index.json")" || die "image platform is not unique"
[[ "$platform_from_index" == "$G1_SANDBOX_PLATFORM_DIGEST" ]] || die "image platform digest mismatch"
jq -e '
  ([.manifests[]? |
   select(.platform.os == "linux" and .platform.architecture == "amd64") |
   select((.annotations["vnd.docker.reference.type"] // "") != "attestation-manifest")] | length) == 1
' "$evidence/image-index.json" >/dev/null || die "unexpected image platform set"

docker buildx imagetools inspect --raw "$platform_ref" >"$evidence/image-platform.json" || die "platform inspection failed"
config_from_manifest="$(jq -er '.config.digest' "$evidence/image-platform.json")" || die "image config digest missing"
[[ "$config_from_manifest" == "$G1_SANDBOX_CONFIG_DIGEST" ]] || die "image config digest mismatch"
jq -e '
  (.mediaType == "application/vnd.oci.image.manifest.v1+json" or
   .mediaType == "application/vnd.docker.distribution.manifest.v2+json") and
  ([.layers[]?.size] | add // 0) <= 536870912
' "$evidence/image-platform.json" >/dev/null || die "image manifest limit failed"
docker buildx imagetools inspect "$platform_ref" --format '{{json .Image}}' >"$evidence/image-config.json" || die "image config inspection failed"
jq -e \
  --arg revision "$G1_SANDBOX_SOURCE_REVISION" \
  --arg base "$G1_SANDBOX_BASE_REF" '
    .architecture == "amd64" and .os == "linux" and
    ((.config.Env // []) | sort == ["PATH=/usr/bin:/bin"]) and
    ((.config.User // "") == "") and
    ((.config.Entrypoint // []) == []) and
    ((.config.Cmd // []) == []) and
    ((.config.WorkingDir // "/") == "/") and
    (.config.Volumes == null) and (.config.ExposedPorts == null) and
    (.config.Healthcheck == null) and
    (.config.Labels["org.velnor.sandbox"] // "") == "true" and
    (.config.Labels["org.opencontainers.image.revision"] // "") == $revision and
    (.config.Labels["org.velnor.sandbox.base"] // "") == $base
  ' "$evidence/image-config.json" >/dev/null || die "remote image config contract failed"
docker pull --quiet --platform linux/amd64 "$platform_ref" >/dev/null || die "pinned image pull failed"
docker image inspect "$platform_ref" >"$evidence/image-local.json" || die "local image inspection failed"
jq -e \
  --arg config "$G1_SANDBOX_CONFIG_DIGEST" \
  --arg ref "$platform_ref" '
    .[0].Id == $config and
    (.[0].RepoDigests | index($ref) != null) and
    .[0].Os == "linux" and .[0].Architecture == "amd64" and
    ((.[0].Config.Env // []) | sort == ["PATH=/usr/bin:/bin"]) and
    ((.[0].Config.Entrypoint // []) == []) and
    ((.[0].Config.Cmd // []) == []) and
    ((.[0].Config.User // "") == "") and
    (.[0].Config.Volumes == null) and
    (.[0].Config.ExposedPorts == null) and
    (.[0].Config.Healthcheck == null)
  ' "$evidence/image-local.json" >/dev/null || die "local image config contract failed"

uid="$(id -u)"
gid="$(id -g)"
[[ "$uid" =~ ^[1-9][0-9]*$ ]] || die "non-root runner uid required"
[[ "$gid" =~ ^[0-9]+$ ]] || die "invalid runner gid"
container_name="velnor-bootstrap-hostile-$$"
cid="$(docker create --name "$container_name" \
  --platform linux/amd64 --pull=never \
  --network=none --read-only --ipc=private --cgroupns=private \
  --cap-drop=ALL --security-opt 'no-new-privileges=true' \
  --pids-limit=128 --memory=512m --memory-swap=512m --cpus=1 \
  --ulimit fsize=67108864:67108864 --ulimit nofile=1024:1024 --ulimit core=0 \
  --shm-size=16m --stop-timeout=5 \
  --log-driver=local --log-opt max-size=1m --log-opt max-file=1 \
  --user "$uid:$gid" --workdir /input --hostname=velnor-sandbox \
  --tmpfs "/tmp:rw,noexec,nosuid,nodev,size=64m,mode=700,uid=$uid,gid=$gid,nr_inodes=4096" \
  --tmpfs "/output:rw,noexec,nosuid,nodev,size=64m,mode=700,uid=$uid,gid=$gid,nr_inodes=4096" \
  --mount "type=bind,src=$input,dst=/input,readonly,bind-propagation=rprivate" \
  --mount "type=bind,src=$candidate,dst=/candidate,readonly,bind-propagation=rprivate" \
  --env "SOURCE_HEAD_SHA=$G1_EXPECTED_SOURCE_HEAD_SHA" \
  --env "SOURCE_REPOSITORY=$G1_SOURCE_REPOSITORY" \
  --env "SOURCE_CLOSURE=$G1_EXPECTED_SOURCE_CLOSURE" \
  --env 'HOME=/tmp/home' --env 'PATH=/usr/bin:/bin' \
  --env 'HOSTNAME=velnor-sandbox' \
  --env 'HOSTILE_PROBE_ISOLATED=1' --env 'HOSTILE_PROBE_WRITE_ROOT=/output' \
  --entrypoint /candidate/velnor-hostile-producer \
  "$platform_ref" --canary --output /output)" || die "sandbox create failed"
docker inspect "$cid" >"$evidence/container-before.json" || die "container inspect failed"
jq -e \
  --arg user "$uid:$gid" --arg input "$input" --arg candidate "$candidate" \
  --arg head "$G1_EXPECTED_SOURCE_HEAD_SHA" --arg repo "$G1_SOURCE_REPOSITORY" \
  --arg closure "$G1_EXPECTED_SOURCE_CLOSURE" '
  .[0] |
  .HostConfig.NetworkMode == "none" and
  .HostConfig.ReadonlyRootfs == true and
  .HostConfig.Privileged == false and
  ((.HostConfig.CapDrop // []) | index("ALL") != null) and
  ((.HostConfig.CapAdd // []) | length == 0) and
  ((.HostConfig.PidMode // "") == "") and
  .HostConfig.IpcMode == "private" and
  .HostConfig.CgroupnsMode == "private" and
  ((.HostConfig.SecurityOpt // []) | index("no-new-privileges=true") != null) and
  ((.HostConfig.SecurityOpt // []) | all(. != "seccomp=unconfined")) and
  .HostConfig.PidsLimit == 128 and
  .HostConfig.Memory == 536870912 and
  .HostConfig.MemorySwap == 536870912 and
  .HostConfig.NanoCpus == 1000000000 and
  .HostConfig.ShmSize == 16777216 and
  .HostConfig.LogConfig.Type == "local" and
  .HostConfig.LogConfig.Config["max-size"] == "1m" and
  .HostConfig.LogConfig.Config["max-file"] == "1" and
  ((.HostConfig.Binds // []) | length == 0) and
  ((.HostConfig.Devices // []) | length == 0) and
  .Config.User == $user and
  .Config.WorkingDir == "/input" and
  .Config.Entrypoint == ["/candidate/velnor-hostile-producer"] and
  .Config.Cmd == ["--canary", "--output", "/output"] and
  ((.Config.Env // []) | sort ==
    ([
      "HOME=/tmp/home",
      "HOSTILE_PROBE_ISOLATED=1",
      "HOSTILE_PROBE_WRITE_ROOT=/output",
      "HOSTNAME=velnor-sandbox",
      "PATH=/usr/bin:/bin",
      ("SOURCE_CLOSURE=" + $closure),
      ("SOURCE_HEAD_SHA=" + $head),
      ("SOURCE_REPOSITORY=" + $repo)
    ] | sort)) and
  ((.Mounts // []) | map(select(.Type == "bind" and .Destination == "/input" and .Source == $input and .RW == false)) | length == 1) and
  ((.Mounts // []) | map(select(.Type == "bind" and .Destination == "/candidate" and .Source == $candidate and .RW == false)) | length == 1) and
  ((.Mounts // []) | map(select(.Type == "bind" and (.Destination != "/input" and .Destination != "/candidate" and .Destination != "/etc/hosts" and .Destination != "/etc/hostname" and .Destination != "/etc/resolv.conf"))) | length == 0) and
  ((.Mounts // []) | map(select(.Type == "tmpfs" and (.Destination == "/tmp" or .Destination == "/output") and .RW == true)) | length == 2) and
  ((.HostConfig.Tmpfs // {} | keys | sort) == ["/output", "/tmp"])
' "$evidence/container-before.json" >/dev/null || die "container preflight failed"
for mount in /tmp /output; do
  jq -e --arg mount "$mount" --arg uid "$uid" --arg gid "$gid" '
    .[0].HostConfig.Tmpfs[$mount] as $options |
    ($options | type == "string") and
    ($options | contains("rw")) and ($options | contains("noexec")) and
    ($options | contains("nosuid")) and ($options | contains("nodev")) and
    ($options | contains("size=64m")) and ($options | contains("mode=700")) and
    ($options | contains("uid=" + $uid)) and ($options | contains("gid=" + $gid)) and
    ($options | contains("nr_inodes=4096"))
  ' "$evidence/container-before.json" >/dev/null || die "tmpfs contract failed"
done

docker start "$cid" >/dev/null || die "sandbox start failed"
if ! timeout --foreground --kill-after=10s 300s docker wait "$cid" >"$stage/exit-code"; then
  docker kill "$cid" >/dev/null 2>&1 || :
  die "sandbox timeout"
fi
exit_code="$(tr -d '\n' <"$stage/exit-code")"
[[ "$exit_code" =~ ^[0-9]+$ ]] || die "sandbox exit code invalid"
docker inspect "$cid" >"$evidence/container-after.json" || die "post-run inspect failed"
jq -e --argjson code "$exit_code" '
  .[0].State.Status == "exited" and
  .[0].State.ExitCode == $code and
  $code == 0 and
  .[0].State.OOMKilled == false and
  ((.[0].State.Error // "") == "")
' "$evidence/container-after.json" >/dev/null || die "sandbox execution failed"

docker logs "$cid" >"$stage/log" 2>/dev/null || die "bounded log extraction failed"
test "$(wc -c <"$stage/log" | tr -d ' ')" -le 1048576 || die "log bound failed"
awk '
  index($0, "VELNOR_HOSTILE_RESULT ") == 1 { count++; line = $0 }
  END { if (count != 1) exit 1; print line }
' "$stage/log" >"$stage/result-line" || die "hostile result record missing"
result_json="$stage/result.json"
sed 's/^VELNOR_HOSTILE_RESULT //' "$stage/result-line" >"$result_json"
jq -e '
  .schema == "velnor.bootstrap-hostile-probe.v1" and
  .fixture == "bootstrap-hostile-producer" and
  .authority == "diagnostic-only" and
  ([
    "h1_env_forbidden_names", "h1_proc1_forbidden_names", "h1_proc_scan_status",
    "h2_fixed_command_paths_present", "h2_fixed_command_paths_guarded",
    "h3_endpoint_present", "h3_token_name_present", "h3_upload_status",
    "h3_sent_without_auth", "h3_sent_with_synthetic_auth", "h4_input_write",
    "h4_candidate_write", "h4_workspace_write", "h4_runner_workspace_write",
    "h4_docker_socket", "h4_docker_socket_run",
    "h5_dns", "h5_loopback_v4", "h5_loopback_v6", "h5_metadata", "h5_proxy",
    "h5_unix_socket",
    "h6_symlink", "h6_hardlink", "h7_sparse_80m", "h7_files_created",
    "h7_file_error", "h7_traversal", "h7_fake_contract", "h7_fake_manifest", "h8_pid_is_one",
    "h8_uid_nonzero", "h8_capabilities_zero", "h8_no_new_privileges",
    "h8_seccomp_two", "h8_children_spawned", "h8_child_error", "h9_source_write",
    "h9_source_rewrite", "h9_source_delete", "h9_cache_paths_wrote", "h9_contract_minting"
  ] | all(has)) and
  .h3_sent_without_auth == false and
  .h3_sent_with_synthetic_auth == false and
  .h1_proc_scan_status == "read" and
  .h2_fixed_command_paths_present == 0 and
  .h2_fixed_command_paths_guarded == 0 and
  .h4_input_write != "wrote" and .h4_candidate_write != "wrote" and
  .h4_workspace_write != "wrote" and .h4_runner_workspace_write != "wrote" and
  .h4_docker_socket != "connected" and .h4_docker_socket_run != "connected" and
  .h5_dns != "connected" and .h5_loopback_v4 != "connected" and
  .h5_loopback_v6 != "connected" and .h5_metadata != "connected" and
  .h5_unix_socket != "connected" and
  .h7_fake_manifest != "wrote" and
  .h9_source_write != "wrote" and .h9_source_rewrite != "wrote" and
  .h9_source_delete != "deleted" and .h9_cache_paths_wrote == 0 and
  .h8_pid_is_one == true and .h8_uid_nonzero == true and
  .h8_capabilities_zero == true and .h8_no_new_privileges == true and
  .h8_seccomp_two == true
' "$result_json" >/dev/null || die "hostile result containment assertions failed"
cp -- "$result_json" "$evidence/hostile-result.json"

docker cp "$cid:/output/." "$hostout/" >/dev/null || die "disposable output copy failed"
set +e
python3 -B "$G1_ARCHIVE_CHECK_PATH" --check-tree "$hostout" >"$evidence/output-check.json"
output_check_status=$?
set -e
[[ "$output_check_status" == 2 ]] || die "hostile output was not rejected"
jq -e '.schema == "velnor.bootstrap-hostile-tree.v1" and .status == "rejected"' "$evidence/output-check.json" >/dev/null || die "output rejection record invalid"

docker rm -f "$cid" >/dev/null || die "container cleanup failed"
test -z "$(docker ps -aq --filter "id=$cid")" || die "container remains after cleanup"
cid=""

jq -n \
  --arg schema "velnor.bootstrap-hostile-check.v1" \
  --arg status "contained-diagnostic-output-rejected" \
  --arg image "$G1_SANDBOX_IMAGE" \
  --arg index "$G1_SANDBOX_INDEX_DIGEST" \
  --arg platform "$G1_SANDBOX_PLATFORM_DIGEST" \
  --arg config "$G1_SANDBOX_CONFIG_DIGEST" \
  --arg result_sha "$(sha256sum -- "$evidence/hostile-result.json" | awk '{print $1}')" \
  --arg output_sha "$(sha256sum -- "$evidence/output-check.json" | awk '{print $1}')" \
  '{schema:$schema,status:$status,authority:"diagnostic-only",image:$image,index_digest:$index,platform_digest:$platform,config_digest:$config,result_sha256:$result_sha,output_check_sha256:$output_sha}' \
  >"$evidence/harness-summary.json"
sha256sum -- "$evidence"/*.json >"$evidence/evidence.sha256"
cat "$evidence/harness-summary.json"
