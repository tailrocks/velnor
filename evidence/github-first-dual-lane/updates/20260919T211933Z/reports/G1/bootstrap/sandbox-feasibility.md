# G1 bootstrap sandbox feasibility

Observed 2026-09-20 Asia/Ho_Chi_Minh. Design research only. No source, generated
workflow, Docker, OrbStack, or Velnor runtime was changed or run. Commands below
are for a fresh GitHub-hosted Ubuntu execute job; do not run them on the macOS
host before G3.

## Decision

The required boundary is feasible on a standard `ubuntu-24.04` GitHub-hosted
x64 VM only if the execute job is base-owned and uses the host Docker daemon with
a separate minimal image, an immutable platform digest, a private PID namespace,
an explicit numeric non-root user, read-only root/input/candidate mounts, and
disposable bounded output/tmp mounts. Docker/image/preflight failure is a hard
failure. There is no host-shell, `unshare`, token-blanking, mutable-tag, or
unbounded-output fallback.

The current `ghcr.io/tailrocks/velnor-job-ubuntu` image is not that image. Its
final stage is root-owned, has a `velnorctl` entrypoint, inherits the large job
image's `HOME`, Mise, Cargo, MBX, and extended `PATH` environment, and contains a
Docker client. Build a separate final image for the candidate (prefer a dedicated
Dockerfile) and carry both its index digest and the exact platform-manifest digest.

The current design names `/output` as a writable bind mount
(`../dual-lane-evidence/G1/bootstrap/isolation-design.md:L50-L59`). A generic bind
mount has no portable hard quota on hosted Docker. The safe implementation is a
disposable `/output` tmpfs with `size` and `nr_inodes` limits, copied out by the
fixed wrapper before container removal. This narrows the write surface and
preserves the six security amendments; it is an implementation detail requiring
approval before source/design changes. If a literal writable bind is mandatory,
use the disposable loopback option below and fail closed if host mount tools are
unavailable. Never substitute Docker `--storage-opt size` as a hosted quota.

## Invariants retained

1. Producer, acquire, execute, and verify are separate base-owned jobs with
   fresh hosted VMs, workspaces, and process trees. The producer has no ordinary
   CI/Velnor dependency and no privileged token.
2. Acquire binds the exact workflow path/event, target/head repository names and
   numeric IDs, PR head SHA (not merge SHA), successful producer REST job `id` and
   stable name, artifact ID/name/run/service digest/expiry, profile/features,
   platform, archive SHA-256, and the trusted full closure. Candidate JSON,
   artifact names, and self-reports are assertions only.
3. Candidate execution has no GitHub/runner/secret/package/cloud/registry
   credential or workflow command channel. `permissions: {}` is necessary but
   not the process boundary; the fixed wrapper passes an allow-list only.
4. The source and measured binary are read-only. The only candidate writes are
   disposable output/tmp (and the optional disposable home under tmpfs). No
   checkout, `.git`, `$RUNNER_TEMP`, command file, artifact-service path, Docker
   socket, or authoritative verifier state is mounted.
5. Docker is `network=none`, read-only rootfs, `cap-drop=ALL`, default seccomp,
   `no-new-privileges`, private PID namespace, numeric non-root UID/GID, bounded
   PID/memory/CPU/tmp/output, and bounded timeout. Any unmet control fails closed.
6. Verify uses a fresh checkout, re-downloads/re-hashes handoff and candidate
   artifacts by trusted IDs/digests, independently materializes clean source, and
   alone compares bytes and emits policy status.

## Repository findings

| Finding | Evidence | Consequence |
| --- | --- | --- |
| The product base is pinned, but the final job image is not a sandbox image. | `Dockerfile:L9,L24-L40,L182-L206`; `docker/job-ubuntu.Dockerfile:L21,L31,L59-L86,L272-L285` | Do not run candidate bytes in the job image. Its inherited env and root/entrypoint violate the image contract even if runtime flags override some values. |
| The release image workflow builds `Dockerfile`, not `docker/job-ubuntu.Dockerfile`. | `.github/workflows/release.yml:L3206-L3215` | The latter is not the current final-image build path. |
| Release stages expect `release-binaries/${TARGETARCH}/velnor-workflow`. | `Dockerfile:L272-L285` | The current release step copies `velnor-runner` at `.github/workflows/release.yml:L3184-L3194`; this mismatch must be fixed before treating any resulting image as a trusted product. |
| Release publishes a multi-platform image and records platform/index digests. | `.github/workflows/release.yml:L3232-L3250,L3295-L3346` | Reuse the digest discipline, but add sandbox config/user/env/mount admission. A tag is only a locator. |
| Project metadata points consumers at `ghcr.io/tailrocks/velnor-job-ubuntu`. | `.github/ci/project.toml:L57-L76`; `.github-gen/velnor-workflow.toml:L65-L76` | A separate sandbox image/ref and trusted handoff field are required; do not silently repurpose the job image. |

## Hosted job contract

Use standard `ubuntu-24.04`, not `ubuntu-slim`, self-hosted, or a macOS runner.
GitHub documents standard hosted runners as fresh VMs; `ubuntu-slim` is a shared
container/VM and cannot provide this Docker/mount boundary. Pin every action used
by acquire, download, wrapper, and verify to a full commit SHA. End the fixed
artifact-download step before invoking the wrapper. Candidate code must never be
checked out or used as an action under `pull_request_target`.

The execute preflight must run only on the hosted Linux VM:

```sh
set -euo pipefail
test "${RUNNER_OS:-}" = Linux
test "${RUNNER_ARCH:-}" = X64
test "$(uname -m)" = x86_64
command -v docker >/dev/null
docker info >/dev/null                 # no daemon means hard failure
test "$(docker info --format '{{.OSType}}')" = linux
```

The fixed setup may authenticate to GHCR to pull a private image, but that
credential must not be in the candidate container environment or mounted paths.
Prefer a public read-only sandbox image so candidate execution needs no registry
credential. Check the runner's free space against a trusted staging budget before
extracting handoff bytes; if `df -Pk "$RUNNER_TEMP"` cannot prove the budget,
fail closed.

## Minimal sandbox image

Do not inherit `jobimage`. Add a separately built final image (dedicated
`docker/bootstrap-sandbox.Dockerfile` is clearest) from the already pinned Ubuntu
base:

```Dockerfile
FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b
ENV PATH=/usr/bin:/bin
WORKDIR /
ENTRYPOINT []
CMD []
```

Install no package, copy no runner/toolchain, declare no volume/port/healthcheck,
and do not set `USER` in the image. Runtime always supplies the measured numeric
UID/GID. Build and publish this image only from a trusted base/release commit, for
example (trusted release job, not a candidate step):

```sh
set -euo pipefail
docker buildx build --platform linux/amd64 \
  --file docker/bootstrap-sandbox.Dockerfile \
  --push --provenance=true --sbom=true \
  --label "org.opencontainers.image.revision=$TRUSTED_COMMIT" \
  --label 'org.velnor.sandbox=true' \
  --tag "ghcr.io/tailrocks/velnor-bootstrap-sandbox:release-$TRUSTED_COMMIT-amd64" .
```

The build job records the exact index digest, platform manifest digest, config
digest, layer digests/sizes, platform, trusted source commit, and expected config
fields in the acquire handoff. Runtime accepts only the full digest, never a tag.

## Image manifest/config admission

The following is execute-job preflight pseudocode. `$SANDBOX_INDEX_DIGEST` and
the expected platform set come only from trusted acquire metadata. The image may
be public or pulled during fixed setup; `--pull=never` is mandatory at candidate
start.

```sh
set -euo pipefail
image='ghcr.io/tailrocks/velnor-bootstrap-sandbox'
sha_re='^sha256:[0-9a-f]{64}$'
case "$SANDBOX_INDEX_DIGEST" in
  sha256:[0-9a-f]{64}) ;;
  *) echo 'bad sandbox index digest' >&2; exit 1 ;;
esac
tmp="$(mktemp -d "$RUNNER_TEMP/sandbox-image.XXXXXX")"
index_ref="$image@$SANDBOX_INDEX_DIGEST"
docker buildx imagetools inspect --raw "$index_ref" >"$tmp/index.json"

# Expected platform set is an exact trusted value. This example admits x64 only.
jq -e '
  ([.manifests[]? |
    select(.platform.os == "linux" and .platform.architecture == "amd64") |
    select((.annotations["vnd.docker.reference.type"] // "") != "attestation-manifest")]
   | length) == 1
' "$tmp/index.json" >/dev/null
platform_digest="$(jq -er '
  [.manifests[]? |
   select(.platform.os == "linux" and .platform.architecture == "amd64") |
   select((.annotations["vnd.docker.reference.type"] // "") != "attestation-manifest") |
   .digest]
  | if length == 1 then .[0] else error("platform digest is not unique") end
' "$tmp/index.json")"
case "$platform_digest" in
  sha256:[0-9a-f]{64}) ;;
  *) echo 'bad sandbox platform digest' >&2; exit 1 ;;
esac

platform_ref="$image@$platform_digest"
docker buildx imagetools inspect --raw "$platform_ref" >"$tmp/platform.json"
jq -e '
  (.mediaType == "application/vnd.oci.image.manifest.v1+json" or
   .mediaType == "application/vnd.docker.distribution.manifest.v2+json") and
  (.config.digest | test("^sha256:[0-9a-f]{64}$")) and
  (([.layers[].size] | add // 0) <= 536870912)
' "$tmp/platform.json" >/dev/null
config_digest="$(jq -er '.config.digest' "$tmp/platform.json")"

# `.Image` is the remote image-config view; verify it before local execution.
docker buildx imagetools inspect "$platform_ref" \
  --format '{{json .Image}}' >"$tmp/config.json"
jq -e --arg trusted "$TRUSTED_COMMIT" '
  .architecture == "amd64" and .os == "linux" and
  ((.config.Env // []) | sort == ["PATH=/usr/bin:/bin"]) and
  ((.config.User // "") == "") and
  ((.config.Entrypoint // []) == []) and
  ((.config.Cmd // []) == []) and
  ((.config.WorkingDir // "/") == "/") and
  (.config.Volumes == null) and (.config.ExposedPorts == null) and
  (.config.Healthcheck == null) and
  (.config.Labels["org.velnor.sandbox"] // "") == "true" and
  (.config.Labels["org.opencontainers.image.revision"] // "") == $trusted
' "$tmp/config.json" >/dev/null

docker pull --quiet --platform linux/amd64 "$platform_ref"
docker image inspect "$platform_ref" --format '{{json .}}' >"$tmp/local.json"
jq -e --arg id "$config_digest" --arg ref "$platform_ref" '
  .[0].Id == $id and
  (.[0].RepoDigests | index($ref) != null) and
  .[0].Os == "linux" and .[0].Architecture == "amd64" and
  ((.[0].Config.Env // []) | sort == ["PATH=/usr/bin:/bin"]) and
  ((.[0].Config.Entrypoint // []) == []) and
  ((.[0].Config.Cmd // []) == []) and
  ((.[0].Config.User // "") == "") and
  (.[0].Config.Volumes == null) and
  (.[0].Config.ExposedPorts == null) and
  (.[0].Config.Healthcheck == null)
' "$tmp/local.json" >/dev/null
```

If the daemon cannot report the exact platform ref/config ID, or any config
field differs, reject the image. Do not fall back to tag inspection, a local
cache hit, a different platform, or `docker pull` without the digest. Record the
manifest/config/layer evidence outside candidate output.

## Staging and exact Docker boundary

The fixed wrapper creates a fresh mode-0700 staging directory, validates archive
headers before extraction, rejects absolute/`..` members, symlink/hardlink/device
members, and enforces trusted archive byte/unpacked-byte/file-count budgets. It
copies only the measured binary and clean source snapshot into it, then makes
both trees non-writable. Numeric IDs are discovered, not assumed:

```sh
set -euo pipefail
umask 077
stage="$(mktemp -d "$RUNNER_TEMP/velnor-sandbox.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
input="$stage/input"
candidate="$stage/candidate"
hostout="$stage/output-copy"
mkdir -m 0755 "$input" "$candidate"
mkdir -m 0700 "$hostout"
uid="$(id -u)"; gid="$(id -g)"
test "$uid" -ne 0
test "$gid" -ge 0
# Trusted archive validator/extractor runs here; failure stops the job.
chmod -R a-w "$input" "$candidate"
find -P "$input" "$candidate" -type l -print -quit | grep -q . && exit 1 || :
find -P "$input" "$candidate" ! -type f ! -type d -print -quit | grep -q . && exit 1 || :
```

The `find` checks are postconditions; the archive validator must reject unsafe
headers before extraction, not repair them afterward. The candidate binary is
mode `0555`; source files are mode `0555` or otherwise read-only. No `.git` or
workflow command files are staged.

Preferred invocation uses tmpfs for both writable paths. `--read-only` makes the
image layer non-writable; only the two read-only input binds and disposable
tmpfs/`/dev/shm` mounts remain. The candidate is PID 1 because `--init` is not
used.

```sh
set -euo pipefail
cid="$(docker create --name "velnor-sandbox-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT" \
  --platform linux/amd64 --pull=never \
  --network=none --read-only \
  --cap-drop=ALL --security-opt 'no-new-privileges=true' \
  --pids-limit=128 --memory=512m --memory-swap=512m --cpus=1 \
  --ulimit fsize=67108864:67108864 --ulimit nofile=1024:1024 --ulimit core=0 \
  --shm-size=16m --stop-timeout=5 --log-driver=none \
  --user "$uid:$gid" --workdir /input --hostname=velnor-sandbox \
  --tmpfs "/tmp:rw,noexec,nosuid,nodev,size=64m,mode=700,uid=$uid,gid=$gid,nr_inodes=4096" \
  --tmpfs "/output:rw,noexec,nosuid,nodev,size=64m,mode=700,uid=$uid,gid=$gid,nr_inodes=4096" \
  --mount "type=bind,src=$input,dst=/input,readonly,bind-propagation=rprivate" \
  --mount "type=bind,src=$candidate,dst=/candidate,readonly,bind-propagation=rprivate" \
  --env "SOURCE_HEAD_SHA=$head_sha" \
  --env "SOURCE_REPOSITORY=$source_repository" \
  --env "SOURCE_CLOSURE=$source_closure" \
  --env 'HOME=/tmp/home' --env 'PATH=/usr/bin:/bin' \
  --env 'HOSTNAME=velnor-sandbox' \
  --entrypoint /candidate/velnor-workflow "$platform_ref" \
  /input --output /output --plain --force --default-branch main)")"
```

Validate every variable used in `--env` as a single nonsecret line before
constructing the command. Do not use host environment inheritance (`--env NAME`),
an env-file containing secrets, `--pid=host`, `--privileged`, `--cap-add`, an
unconfined seccomp profile, `--init`, a Docker socket, or any additional bind.
Docker-managed `/etc/hosts`, `/etc/hostname`, and `/etc/resolv.conf` may appear
as read-only internal mounts; permit only those fixed system mounts plus the two
staged read-only binds and disposable tmpfs mounts. Any other host source,
unexpected propagation, or writable bind fails closed.

Immediately inspect the stopped container configuration before starting it. The
checks must assert at minimum:

```sh
docker inspect "$cid" >"$stage/container.json"
jq -e --arg user "$uid:$gid" --arg input "$input" --arg candidate "$candidate" '
  .[0].HostConfig.NetworkMode == "none" and
  .[0].HostConfig.ReadonlyRootfs == true and
  .[0].HostConfig.Privileged == false and
  (.[0].HostConfig.CapDrop | index("ALL") != null) and
  ((.[0].HostConfig.CapAdd // []) | length == 0) and
  (.[0].HostConfig.PidMode == null or .[0].HostConfig.PidMode == "") and
  (.[0].HostConfig.IpcMode == "private") and
  ((.[0].HostConfig.SecurityOpt // []) | index("no-new-privileges=true") != null) and
  ((.[0].HostConfig.SecurityOpt // []) | all(. != "seccomp=unconfined")) and
  .[0].HostConfig.PidsLimit == 128 and
  .[0].HostConfig.Memory == 536870912 and
  .[0].HostConfig.MemorySwap == 536870912 and
  .[0].HostConfig.NanoCpus == 1000000000 and
  .[0].HostConfig.LogConfig.Type == "none" and
  .[0].Config.User == $user and
  .[0].Config.WorkingDir == "/input" and
  .[0].Config.Entrypoint == ["/candidate/velnor-workflow"] and
  (.[0].Config.Env | map(split("=")[0]) | sort ==
    ["HOME","HOSTNAME","PATH","SOURCE_CLOSURE","SOURCE_HEAD_SHA","SOURCE_REPOSITORY"]) and
  ([.[0].Mounts[] | select(.Type == "bind" and .Destination == "/input" and .Source == $input and .RW == false)] | length == 1) and
  ([.[0].Mounts[] | select(.Type == "bind" and .Destination == "/candidate" and .Source == $candidate and .RW == false)] | length == 1) and
  ([.[0].Mounts[] | select(.Type == "bind" and (.Destination == "/input" or .Destination == "/candidate"))] | length == 2) and
  ([.[0].Mounts[] | select(.Type == "bind" and (.Destination != "/input" and .Destination != "/candidate" and .Destination != "/etc/hosts" and .Destination != "/etc/hostname" and .Destination != "/etc/resolv.conf")] | length == 0)
' "$stage/container.json" >/dev/null
```

```sh
for mount in /tmp /output; do
  jq -e --arg mount "$mount" --arg uid "$uid" --arg gid "$gid" '
    .[0].HostConfig.Tmpfs[$mount] as $o |
    ($o | type == "string") and
    ($o | contains("rw")) and ($o | contains("noexec")) and
    ($o | contains("nosuid")) and ($o | contains("nodev")) and
    ($o | contains("size=64m")) and ($o | contains("mode=700")) and
    ($o | contains(("uid=" + $uid))) and ($o | contains(("gid=" + $gid))) and
    ($o | contains("nr_inodes=4096"))
  ' "$stage/container.json" >/dev/null
done
```

Also assert no devices, no custom `Binds`, no host UTS/user namespace, and no
extra environment key/value. If the daemon's inspect representation differs from
these expected invariants, use equivalent strict checks and fail closed; do not
weaken the checks to make a runner pass. If the daemon canonicalizes option
spelling, normalize exact option tokens before comparing; an absent token is a
failure.

## Start, timeout, result extraction, cleanup

Do not use `--rm` because the fixed wrapper must inspect exit/OOM state and copy
the disposable output first. A timeout or cleanup error is a failure, even if
the candidate produced plausible output:

```sh
set -euo pipefail
if ! timeout --foreground --kill-after=10s 300s docker wait "$cid" >"$stage/exit"; then
  docker kill "$cid" >/dev/null 2>&1 || true
  docker rm -f "$cid" >/dev/null 2>&1 || true
  echo 'candidate timeout' >&2
  exit 1
fi
rc="$(cat "$stage/exit")"
docker inspect "$cid" >"$stage/after.json"
jq -e --argjson rc "$rc" '
  .[0].State.Status == "exited" and
  .[0].State.ExitCode == $rc and $rc == 0 and
  .[0].State.OOMKilled == false and .[0].State.Error == ""
' "$stage/after.json" >/dev/null

docker cp "$cid:/output/." "$hostout/"
find -P "$hostout" -type l -print -quit | grep -q . && exit 1 || :
find -P "$hostout" ! -type f ! -type d -print -quit | grep -q . && exit 1 || :
test "$(find -P "$hostout" -type f | wc -l | tr -d ' ')" -le 4096
test "$(du -sx --bytes --apparent-size "$hostout" | awk '{print $1}')" -le 67108864

docker rm -f "$cid" >/dev/null
test -z "$(docker ps -aq --filter "id=$cid")"
```

The real wrapper additionally rejects absolute/`..` relative paths, missing or
unexpected render files, duplicate names, and output hash mismatches before a
fixed SHA-pinned artifact upload. Candidate output/manifest is never authoritative.
The cleanup trap must retry `docker rm -f` and fail if the container or staging
mount remains. The candidate job never executes a host fallback after any of
these failures.

## Literal-bind compatibility option

If review requires `/output` to remain a bind mount, create a disposable fixed
size filesystem under the fresh staging directory in the trusted wrapper, then
bind only its mountpoint. This is a host operation in the Linux execute VM, not a
candidate privilege:

```sh
set -euo pipefail
command -v sudo >/dev/null
command -v truncate >/dev/null
command -v mkfs.ext4 >/dev/null
command -v mount >/dev/null
command -v umount >/dev/null
quota_img="$stage/output.ext4"
quota_mount="$stage/output-mount"
truncate -s 67108864 "$quota_img"
mkfs.ext4 -F -m 0 -N 4096 "$quota_img" >/dev/null
mkdir -m 0700 "$quota_mount"
sudo mount -o loop,nodev,nosuid,noexec "$quota_img" "$quota_mount"
sudo chown "$uid:$gid" "$quota_mount"
# Use only this disposable mount as --mount type=bind,src=$quota_mount,dst=/output,rw.
# Copy/validate output, then unmount; any mount or unmount failure is fatal.
sudo umount "$quota_mount"
```

The wrapper must verify the actual filesystem size/inode budget, reserve no
authoritative files in the image, and unmount before deleting the staging tree.
This option depends on the hosted runner permitting `sudo` loop mounts and
`e2fsprogs`; a canary must prove it. If not available, fail closed and use the
tmpfs design rather than an unbounded host bind. Docker `--storage-opt size` is
not portable: Docker documents that overlay2 quota requires an XFS backing
filesystem with `pquota` (and other drivers differ).

## Hosted hostile canary

Run a fixed adversarial fixture as the candidate and require all assertions below
before accepting the boundary:

* `/proc/1/environ`, every visible `/proc/*/environ`, `env`, and command-line
  inspection contain only `SOURCE_HEAD_SHA`, `SOURCE_REPOSITORY`,
  `SOURCE_CLOSURE`, `HOME`, `PATH`, and `HOSTNAME`; no `GITHUB_*`, `GH_*`,
  `ACTIONS_*`, runner, artifact, cloud, registry, package, proxy, or secret name.
  `/proc/1` is the candidate PID namespace, not acquire/verify or host PID 1.
* `/proc/1/status` reports nonzero UID, zero effective capabilities,
  `NoNewPrivs: 1`, and `Seccomp: 2`. A fork storm cannot exceed the PID limit;
  timeout kills descendants and leaves no container.
* `/input` and `/candidate` cannot be written; rootfs, `/workspace`, `/__w`,
  GitHub command paths, and `/var/run/docker.sock` are absent/unreachable.
  `/output` and `/tmp` can write only as the candidate UID; `execve` from them
  fails because `noexec` is set.
* Filling either tmpfs reaches `ENOSPC` at the declared byte/inode bound;
  output copy and host staging remain within their fixed budget. OOM, nonzero
  exit, timeout, copy failure, symlink, special file, traversal, extra/missing
  file, or hash mismatch is red.
* DNS, route, and socket-connect attempts fail under `network=none`; source/git/
  workflow rewrite/delete, `.git` mutation, forged revision/closure/manifest,
  and command-file writes cannot affect trusted verification.
* Wrong image tag/digest/config/user/entrypoint, `--pid=host`, privileged mode,
  cap-add, seccomp-unconfined, writable input bind, extra mount/env, absent
  Docker, empty cache, shallow checkout, producer failure, candidate nonzero,
  and cleanup failure all fail closed before policy can be green.

The verifier separately re-materializes the clean exact head and compares the
candidate render; canary success never replaces that provenance check.

## Capabilities and limits

* GitHub standard hosted Linux is an ephemeral VM with finite plan-dependent CPU,
  memory, and disk. The wrapper must check `df`, archive sizes, image layer size,
  file counts, and all timeouts. Separate jobs are fresh VM/process/workspace
  boundaries, but a Docker container still shares the hosted VM kernel/daemon;
  this is not a formal VM escape-proof guarantee.
* Docker tmpfs is Linux-only, temporary, and memory-backed; Docker documents that
  tmpfs bytes count toward the container memory limit and may be swapped. Equal
  `--memory`/`--memory-swap`, small tmpfs sizes, and post-copy limits bound normal
  use but do not turn Docker into a microVM. No host disk quota is claimed for
  arbitrary `RUNNER_TEMP`; use the fixed loopback option or fail closed.
* Default Docker PID isolation is private; `--pid=host` and
  `--pid=container:<id>` are explicit unsafe alternatives. Inspect and canary
  both, rather than relying on defaults.
* Docker's default seccomp, `cap-drop=ALL`, read-only root, no-new-privileges,
  no socket, and no network remove the required credential/filesystem/network
  channels; they do not promise kernel/daemon immunity. Never run the candidate
  in the host shell or grant Docker access.
* OCI image config `Env` is part of the image's immutable config and is merged as
  runtime defaults. Docker also supplies automatic `HOME`, `HOSTNAME`, and
  `PATH`; therefore image-config inspection plus `/proc/1/environ` canary are
  both mandatory. Runtime blanking alone is insufficient.

## Official primary references

* [Docker `run` isolation and runtime environment](https://docs.docker.com/engine/containers/run/)
  — image digest refs, automatic environment, namespaces, resource controls,
  capabilities, and default seccomp.
* [Docker `container run` reference](https://docs.docker.com/reference/cli/docker/container/run/)
  — `--read-only`, `--tmpfs`, `--user`, `--network`, `--pid`, `--pids-limit`,
  `--memory`, `--cpus`, `--cap-drop`, `--security-opt`, and the non-portable
  `--storage-opt size` caveat.
* [Docker tmpfs mounts](https://docs.docker.com/engine/storage/tmpfs/) —
  temporary memory-backed mounts, `size`, `nr_inodes`, and swap/memory limits.
* [Docker bind mounts](https://docs.docker.com/engine/storage/bind-mounts/) —
  read-only bind behavior, recursive read-only caveat, and propagation controls.
* [Docker `container cp`](https://docs.docker.com/reference/cli/docker/container/cp/)
  — fixed-wrapper extraction of disposable output before container removal.
* [Docker image inspect](https://docs.docker.com/reference/cli/docker/image/inspect/)
  and [`buildx imagetools inspect`](https://docs.docker.com/reference/cli/docker/buildx/imagetools/inspect/)
  — local/registry config, platform manifests, config digest, and layer evidence.
* [OCI image configuration](https://github.com/opencontainers/image-spec/blob/main/config.md)
  — immutable config `Env`, `User`, `Entrypoint`, `Cmd`, `WorkingDir`, and image
  identity as the config digest.
* [GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
  and [secure use](https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions)
  — fresh standard VMs, `ubuntu-slim` limits, workspace/credential exposure,
  least privilege, and full-SHA action pinning.
