# Independent review — dedicated bootstrap image build contract

Date: 2026-09-20 (UTC/Asia/Ho_Chi_Minh)

Reviewed, read-only:

- source `3ed0023b038335d7b22dfa2758457e3808f777ee`;
- inventory `G1/bootstrap/bootstrap-image-inventory-3ed0023b.md`, SHA-256
  `5ccb0c5e62f39c03dd9a3543a6d32fbb111e8409446c9388e492c239e4099c6c`;
- proposal `G1/bootstrap/bootstrap-image-build-contract-proposal-20260920.md`,
  SHA-256 `d08a0468066d2bf2331a6417f0c930806b5a384fa4de81a753b36f505c2aadb5`.

No Docker/OrbStack/Velnor command, image pull/build/push, GitHub dispatch, or
source/host mutation was performed. Mac Cargo checks below are metadata/cache
checks only; they are not Linux build or hostile-canary evidence.

## Verdict

**CHANGES REQUIRED; no image can be accepted yet.**

The proposal correctly rejects the current product image, separates builder and
sandbox roles, uses immutable image/config/platform identities, and identifies
that the current `/tmp` tmpfs masks an image-baked `/tmp/cargo`. The proposed
`/opt/velnor/cargo-home` to writable `/target/cargo-home` design is viable in
principle.

It is not executable as an admission contract until it adds an acyclic,
base-owned lock-refresh path. A PR that changes `Cargo.lock`, manifests,
toolchain files, or `.cargo/**` cannot pass the isolated producer while the
builder cache is pinned to the default branch. The proposal currently says to
reject that PR until a new image is rebuilt, but only describes a publisher
that reads trusted default-branch content. If the isolated check is required to
be green before merge, this is a dependency-upgrade cycle. The refresh must
consume reviewed lock/manifests as data, never execute PR source, and publish
no acceptance claim until the new builder and pins are independently reviewed.

## Evidence from the exact source

The inventory's no-image conclusion is sound. Both source digests are empty:

```text
builder: ghcr.io/tailrocks/velnor-bootstrap-builder, ""
sandbox: ghcr.io/tailrocks/velnor-bootstrap-sandbox, ""
```

The generated producer fails closed at `.github/workflows/ci-pr.yml:127-156`.
The generated execute lane fails closed at `.github/workflows/ci-policy.yml:482-566`.
Anonymous GHCR `401`/`403` and API `404` responses are access evidence only;
they do not prove private package absence. No manifest, config, provenance, or
accepted final digest was available.

The current producer command is, in relevant part,

```text
--network=none --read-only --pid=private --cap-drop=ALL
--security-opt no-new-privileges=true --user 65532:65532
--tmpfs /tmp ... --tmpfs /target ... --tmpfs /output ...
--env CARGO_HOME=/tmp/cargo --env CARGO_NET_OFFLINE=true
--env CARGO_TARGET_DIR=/target
--entrypoint /bin/sh <builder> -ceu
  'cargo build --locked --offline -p velnor-workflow &&
   install -m 0555 target/debug/velnor-workflow /output/velnor-workflow'
```

`build.rs` calls `git rev-parse HEAD`, `git rev-parse --show-toplevel`, and
`git ls-tree -r HEAD` over the closure paths. The source bind therefore needs
the checkout's `.git` metadata; the final builder image must not bake candidate
source or candidate `.git` data.

## Offline closure result

Read-only metadata checks against an archive of exact commit `3ed0023b...`:

```sh
CARGO_NET_OFFLINE=true cargo tree \
  --manifest-path crates/velnor-workflow/Cargo.toml \
  --locked --offline --target x86_64-unknown-linux-gnu \
  --edges normal,build --prefix none --format '{p}'
```

Result: exit `0`; after removing the workspace root, **115 unique package/version
nodes**; one git source, `termrock v0.11.0`, at the locked revision:

```text
https://github.com/tailrocks/termrock.git
rev=5283c2acf9154d0cfcd37b1ffe821c00faf90ea2
```

This is a Cargo-resolution result, not proof that a Linux compiler or final
image works. The clean-cache negative is decisive:

```sh
CARGO_HOME=$(mktemp -d) CARGO_NET_OFFLINE=true cargo tree \
  --manifest-path crates/velnor-workflow/Cargo.toml \
  --locked --offline --target x86_64-unknown-linux-gnu \
  --edges normal,build --prefix none --format '{p}'
```

Result: exit `101`; Cargo reported it could not checkout the locked `termrock`
git source while offline. Registry archives/indexes and the git checkout/index
must be hydrated before isolation. A cache-mount hit is not proof: the final
image must work from a clean, immutable image layer.

Cargo's official contracts support this split: [`cargo fetch`](https://doc.rust-lang.org/nightly/cargo/commands/cargo-fetch.html)
hydrates dependencies, `--locked` rejects lockfile mutation, and
[`cargo vendor`](https://doc.rust-lang.org/cargo/commands/cargo-vendor.html)
can create a checked-in/read-only source closure. [`cargo config offline`](https://doc.rust-lang.org/cargo/reference/config.html)
and `--offline` prevent network use; they do not manufacture missing sources.

## Required amendment: an acyclic lock-refresh path

The image publisher must have two distinct inputs and phases:

1. **Lock-only acquisition.** A base-owned workflow obtains a bounded,
   reviewed bundle containing only the declarative dependency inputs required to
   resolve the package: the relevant `Cargo.toml` workspace/package manifests,
   `Cargo.lock`, and the explicitly reviewed toolchain inputs. It computes the
   versioned dependency-contract digest from canonical `git ls-tree` records.
   Candidate Rust source, `build.rs`, tests, examples, generated workflow, and
   candidate `.cargo/config*` are not copied into the networked prefetch
   context. If `.cargo` must be part of the contract, use a base-owned fixed
   Cargo config and treat candidate `.cargo` bytes as data; never execute or
   trust candidate source-replacement configuration.

2. **Networked fetch, no candidate execution.** The trusted job uses public
   crates.io and the exact public `termrock` URL/revision to run `cargo fetch
   --locked` (or an equivalent vendor operation) against that lock-only
   bundle. It does not run `cargo build`, `build.rs`, tests, proc-macros, or
   candidate code while network access exists. It must allow only reviewed
   source hosts, bound download/member/graph sizes and time, verify registry
   checksums and the exact git commit, and emit a cache bundle keyed by the
   dependency-contract digest. A later network-disabled stage runs
   `cargo tree --locked --offline` and the exact producer build against the
   trusted source or a controlled fixture.

3. **New builder and independent review.** Build a fresh builder image from
   the lock-only cache, attach a provenance/SBOM record, inspect its index,
   platform manifest, config, labels, layers, cache census, and toolchain
   checksums, then have an independent verifier approve the resulting
   immutable digests. Only that reviewed record may update target-owned image
   pins. The PR remains unavailable to the isolated lane until the pin exists;
   the refresh job is not an isolated-candidate pass and must not create one.

This can be implemented as a maintainer-approved `workflow_call`/trusted
default-branch refresh or as a separate dependency-refresh change followed by
image publication. It must not be a `pull_request_target` job that checks out
and executes the PR tree. If repository policy requires every PR check to be
green before merge, the dependency-contract check needs an explicit
`blocked/unavailable` state plus this trusted refresh route, or the lock update
must be split into a statically reviewed dependency change. A normal isolated
success must never be faked to break the cycle.

No token is necessary for the current public crates.io and public `termrock`
inputs. The refresh job must run with an empty credential environment and no
Git credential helper/netrc. At minimum scrub `GITHUB_TOKEN`, `ACTIONS_*`,
`GH_TOKEN`, cloud credential variables, and registry credentials; set a fixed
`HOME`, `CARGO_HOME`, and Git config. If a future private source needs a token,
the current `permissions: {}`/public-image contract cannot accept it without a
separate reviewed secret-handling design. Never pass a token as Docker `ARG` or
`ENV`; Docker's [build-secret contract](https://docs.docker.com/build/building/secrets/)
requires an ephemeral secret mount and no persisted value.

Before the producer container is created, the candidate job must recompute the
dependency-contract digest and fail closed unless it equals the builder image
label/attestation and reviewed target pin. This gate is needed even with
`--locked`: a changed lock can resolve to already-cached packages and otherwise
pass offline while silently changing the trusted dependency input.

## `/tmp` cache masking: proposal is directionally correct, not yet proven

Docker's [tmpfs behavior](https://docs.docker.com/engine/storage/tmpfs/)
obscures pre-existing files at a mount point. Thus an image containing
`/tmp/cargo` cannot satisfy the current command after `--tmpfs /tmp` is mounted.
The proposal's fix is the correct shape:

```text
immutable image cache: /opt/velnor/cargo-home
writable runtime home: /target/cargo-home  (inside bounded /target tmpfs)
CARGO_HOME=/target/cargo-home
```

The trusted wrapper must, under UID/GID `65532:65532`, create the destination,
copy only the measured image cache, reject unexpected links/special files, and
then run the exact locked/offline build. Required proof:

- `/opt/velnor/cargo-home` and every parent are traversable/readable by 65532;
- the copy is bounded by the target quota and cannot follow an out-of-tree link;
- the copied registry index, crate archives/source, and `termrock` git DB/
  checkout are present;
- the wrapper is base-owned and digest-bound; candidate `.cargo` cannot replace
  its command/config;
- a clean Linux `linux/amd64` run passes with network disabled and a deliberately
  empty host Cargo cache;
- measured cache-copy bytes plus debug target output fit the existing 2 GiB
  `/target` tmpfs, or a reviewed bounded value replaces it.

The current generated command has no such copy/wrapper. An image-only change
cannot fix it. The producer workflow must change before any builder digest can
be accepted.

## Builder image admission

The proposal's builder requirements are necessary, with these exact additions:

- Runtime `PATH` must resolve direct pinned binaries at the paths named by the
  generated contract. `mise` presence or a `stable` alias is not proof. Record
  `cargo -V`, `rustc -Vv`, target triple, toolchain artifact checksums, and the
  path resolution. `rust-toolchain.toml` says Rust `1.98.1`; `docker/build-mise.lock`
  alone is not a final toolchain hash.
- Pin or record every package installed by the builder recipe. A pinned Ubuntu
  base digest with floating `apt` repository state is not a reproducible input.
  The image attestation must include package/SBOM data and the exact base
  manifest.
- Do not rely on BuildKit cache mounts for acceptance. They may accelerate the
  trusted build, but the final image must contain the complete read-only cache
  and pass from a clean builder pull.
- Make the builder image's inherited environment exact and credential-free.
  The current producer gate only rejects selected prefixes and permits arbitrary
  other image `ENV` keys. Require `Config.Env == []` (preferred) or a small
  exact reviewed allow-list, `Volumes == null`, no devices, and no default
  entrypoint/cmd that can run before the runtime override. The explicit runtime
  environment must be the only environment visible to PID 1.
- Image `Config.User == ""` is acceptable only because the runtime must enforce
  `--user 65532:65532` and inspect it before start. The builder image itself
  must not run a build during pull/start. Test `/proc/1/environ` in the future
  Linux hostile canary; source/config strings alone are not evidence.

## Sandbox image/runtime admission

The final sandbox config in the proposal matches the current execute gate and
is appropriately minimal:

```text
architecture=amd64, os=linux
Config.Env=["PATH=/usr/bin:/bin"]
Config.User="", Entrypoint=[], Cmd=[], WorkingDir="/"
Volumes=null, ExposedPorts=null, Healthcheck=null
Labels[org.velnor.sandbox]="true"
```

The runtime, not the image config, supplies the nonroot boundary and must be
inspected before start:

```text
--network=none --read-only --pid=private --ipc=private
--cap-drop=ALL --security-opt no-new-privileges=true
--user 65532:65532 --pids-limit=128
--memory=512m --memory-swap=512m --cpus=1
--ulimit fsize=67108864 --ulimit nofile=1024 --ulimit core=0
--tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m,nr_inodes=4096
--tmpfs /output:rw,noexec,nosuid,nodev,size=64m,nr_inodes=4096
```

Only read-only `/input` and `/candidate` binds are permitted. The existing
workflow's 900-second wall timeout plus 10-second kill-after, exit/OOM checks,
output symlink/special-file census, 4096-file limit, and 64 MiB apparent-size
limit are the minimum acceptance. The hostile canary must additionally prove
that image ENV and `/proc/1/environ` contain no forbidden credentials, that
PID/process escape does not survive cleanup, and that all quota failures are
nonzero/fail-closed. No such hosted evidence exists yet.

## Exact identity and attestation requirements

For each image, the reviewed record must bind all of the following without
deriving identity from a tag or Dockerfile text:

```text
OCI index digest
exact linux/amd64 platform-manifest digest
platform config digest
all layer digests and sizes
canonical recipe/context/base digest
trusted source revision
versioned dependency-contract digest (builder)
toolchain/package checksums and SBOM
signed provenance/referrer digest
```

The verifier must fetch raw index and platform JSON, require exactly one
non-attestation `linux/amd64` manifest, compare `manifest.config.digest` to
the reviewed config digest, pull the exact platform digest, and compare local
`RepoDigest` and image ID to those values. The signed attestation's subject
must be the exact image digest being accepted (platform and/or index as
recorded), and its predicate must bind source revision, recipe digest,
dependency digest, platform, and builder workflow. Define the recipe digest
canonically (Dockerfile plus context file-list/bytes, base digest, build args,
and recipe-version); a free-form label is not enough.

The sandbox has no candidate source in its layers. Its provenance still needs
to bind the reviewed trusted source/recipe commit and final image subject.
Labels alone do not constitute provenance. No attestation or digest has been
observed for either current bootstrap image.

## Acceptance command set (future hosted Linux only; not run here)

The following is the minimum shape; each command must fail closed and persist
its JSON/log evidence. It is not permission to dispatch or publish now.

```sh
# Before any builder pull/create: dependency identity gate.
dep_listing="$(git ls-tree -r "$CANDIDATE_HEAD_SHA" -- \
  Cargo.toml Cargo.lock rust-toolchain.toml rust-toolchain .cargo | LC_ALL=C sort)"
dep_digest="$(printf '%s\n' "$dep_listing" | sha256sum | awk '{print $1}')"
test "$dep_digest" = "$REVIEWED_BUILDER_DEPENDENCY_DIGEST"

# Trusted wrapper first copies the final image cache, with bounded size and
# no links/special files, into the writable /target tmpfs. All Cargo checks
# then use that copy, not a mutable BuildKit cache or a read-only image path.
mkdir -p /target/cargo-home
cp -R --dereference /opt/velnor/cargo-home/. /target/cargo-home/
test "$(find -P /target/cargo-home -type l -o ! -type f ! -type d | head -1)" = ""

env -i HOME=/tmp/home PATH="$PINNED_PATH" CARGO_HOME=/target/cargo-home \
  CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/target \
  cargo tree --locked --offline --target x86_64-unknown-linux-gnu \
    --edges normal,build --prefix none --format '{p}' > /tmp/tree.txt
test "$(awk '{line=$0; sub(/ \\(\*.*/,"",line); sub(/ \\(.*$/, "", line); print line}' /tmp/tree.txt | \
  rg -v '^velnor-workflow v' | sort -u | wc -l | tr -d ' ')" = 115
test "$(rg -c '^termrock v0\.11\.0 ' /tmp/tree.txt)" = 1

env -i HOME=/tmp/home PATH="$PINNED_PATH" CARGO_HOME=/target/cargo-home \
  CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/target \
  cargo build --locked --offline -p velnor-workflow
```

The first command is a metadata check; the second must execute only after the
trusted wrapper has copied the image cache into the writable tmpfs. The future
record must show the 115-node closure, exact `termrock` revision, Linux ELF
output, stamped candidate revision/closure, and all runtime inspect predicates.

## Remaining limits

- Current evidence proves neither final image existence nor Linux runtime
  behavior. No G1 image or hostile-canary approval follows from this review.
- The 115-node count was reproduced with Cargo metadata on macOS for the exact
  source and Linux target selection. It is not a substitute for the required
  hosted `linux/amd64` toolchain/build proof.
- The current repository has no dedicated bootstrap Dockerfiles, publisher,
  image manifest record, or accepted digest. Implementing those is a later,
  approved source change; this review made none.
- A public, token-free prefetch is feasible for the current lock, but only in
  the lock-only trusted path above. Prefetching candidate code, running a
  candidate build while networked, inheriting runner credentials, or accepting
  a mutable cache would invalidate the isolation claim.

**Disposition:** return to image owner for the acyclic lock-refresh design,
exact builder-env admission, measured cache-copy proof, canonical recipe and
toolchain/package identity, and independent manifest/config/provenance review.
Keep all source image digests empty until those records and a hosted Linux
canary exist. Actual Mac/OrbStack operation remains prohibited before G3.

## Amendment A — concrete acyclic lock-only refresh

The preceding disposition requires a concrete path. The state graph must be
acyclic and must not use a stale default-branch cache as proof:

```text
base-owned review of PR lock data
  -> immutable lock-input bundle + reviewed manifest
  -> networked fetch (lock data only; no PR source/build/scripts/token)
  -> cache bundle keyed by dependency/toolchain digests
  -> offline builder-image build
  -> fresh Linux image/manifest/config/attestation review
  -> reviewed target-config pin update
  -> candidate producer/execute
```

There is no edge from candidate build, candidate output, candidate status, or
candidate-provided workflow back into refresh or image publication. Refresh
never claims candidate acceptance.

### Base-owned refresh jobs

Add a base/default-branch-owned `bootstrap-lock-refresh.yml` with a maintainer
approval gate. It is not a PR workflow and does not check out or execute a PR
tree. Inputs are `head_sha`, `pr_number`, and expected `base_sha`; the base job
uses only GitHub API/tree-object reads to prove that the head is a same-repo PR
and that the requested commit/tree are exact.

The base job extracts only this allowlist as data:

```text
Cargo.toml
Cargo.lock
crates/*/Cargo.toml                 # workspace member manifests only
tools/*/Cargo.toml                   # workspace member manifests only
rust-toolchain.toml
rust-toolchain                       # if present
```

Candidate `.rs`, `build.rs`, tests, examples, scripts, actions, workflows,
Dockerfiles, `.git`, generated files, and candidate `.cargo/config*` are never
copied to the networked job. The current `.cargo` tree is empty. If a future
dependency contract needs Cargo configuration, a base-owned fixed config is
generated separately; candidate source-replacement/config bytes are rejected,
not executed.

The base job emits a closed `velnor.bootstrap.lock-input.v1` bundle and a
manifest before approval. The manifest must contain exactly these fields:

```json
{
  "schema": "velnor.bootstrap.lock-input.v1",
  "target_repository": "owner/repository",
  "base_sha": "40-hex",
  "head_sha": "40-hex",
  "head_tree_sha": "40-hex",
  "files": [{"path":"...","blob_sha":"40-hex","size":0,"sha256":"64-hex"}],
  "cargo_lock_sha256": "64-hex",
  "dependency_contract_digest": "sha256:64-hex",
  "toolchain_input_digest": "sha256:64-hex",
  "registry_sources": ["registry+https://github.com/rust-lang/crates.io-index"],
  "registry_transport": "sparse+https://index.crates.io/",
  "git_sources": [{"url":"https://github.com/tailrocks/termrock.git","rev":"5283c2acf9154d0cfcd37b1ffe821c00faf90ea2"}],
  "workspace_manifest_count": "<observed integer>",
  "lock_package_count": "<observed integer>",
  "bundle_sha256": "64-hex"
}
```

The angle-bracket values above are schema placeholders, not evidence or
accepted values. The emitted manifest must contain observed integers.

`files` is sorted by path and contains only the allowlist. The base validator
rejects extra keys, duplicate paths, symlinks/special files, path traversal,
oversize members, unknown registries, every git URL/revision other than the
reviewed public `termrock` source, and lock entries without the expected
registry checksum. `workspace_manifest_count` and `lock_package_count` are
observed values, not candidate claims. The bundle and manifest are immutable
Actions artifacts with service digest recorded; no candidate artifact is used
as an input to refresh.

The approval record binds the manifest hash, `head_sha`, tree SHA, dependency
digest, toolchain digest, reviewer/environment, and refresh run. If any of
these differ, refresh stops before network access.

### Networked fetch: exact no-secret/no-build recipe

The approved refresh bundle is the only input to the networked job. It contains
manifests/lock/toolchain data, never PR source. The job has `permissions: {}`
and no registry/package/cloud token. Run Cargo with an empty environment except
this fixed allowlist (paths are base-owned and pinned by the refresh image):

```sh
refresh_home="$RUNNER_TEMP/bootstrap-refresh-home"
refresh_cargo="$RUNNER_TEMP/bootstrap-refresh-cargo"
mkdir -m 0700 "$refresh_home" "$refresh_cargo"
env -i \
  HOME="$refresh_home" \
  PATH="/opt/velnor/rust/bin:/usr/bin:/bin" \
  CARGO_HOME="$refresh_cargo" \
  CARGO_NET_OFFLINE=false \
  CARGO_NET_RETRY=0 \
  CARGO_HTTP_TIMEOUT=30 \
  CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
  CARGO_TERM_COLOR=never \
  CARGO_NET_GIT_FETCH_WITH_CLI=false \
  GIT_CONFIG_NOSYSTEM=1 \
  GIT_CONFIG_GLOBAL=/dev/null \
  GIT_TERMINAL_PROMPT=0 \
  GIT_ASKPASS=/bin/false \
  SSH_AUTH_SOCK=/dev/null \
  RUSTC=/bin/false \
  RUSTDOC=/bin/false \
  CARGO_BUILD_RUSTC_WRAPPER=/bin/false \
  /opt/velnor/rust/bin/cargo fetch --locked \
    --manifest-path "$RUNNER_TEMP/bootstrap-lock-input/Cargo.toml" \
    >"$RUNNER_TEMP/bootstrap-fetch.log" 2>&1
```

`env -i` removes `GITHUB_TOKEN`, `ACTIONS_*`, `GH_TOKEN`, `AWS_*`, `AZURE_*`,
`GOOGLE_*`, `CARGO_REGISTRIES_*` credentials, proxy variables, `RUSTFLAGS`,
`RUSTC_WRAPPER`, `NETRC`, and every ambient credential. The only explicit
`CARGO_REGISTRIES_CRATES_IO_PROTOCOL` value selects the reviewed public sparse
index; it carries no credential. The job must assert that no netrc, Git
credential helper, SSH key, or credential-bearing Cargo config exists under
`HOME`, `CARGO_HOME`, or the lock-input bundle.

The false compiler/doc/wrapper paths make an accidental compile, build script,
or proc-macro execution fail closed. The fetch step must contain no Rust source,
so it cannot execute a candidate `build.rs` or proc-macro even if Cargo behavior
changes. Fail if the fetch log contains a compiler/build-script invocation.
Run a base-owned source/manifest audit before Cargo fetch to reject candidate
manifest keys that introduce an unreviewed source replacement, registry, path,
or git URL. `--locked` must fail if any lock rewrite would be needed.

Run Cargo fetch in verbose mode (the log is retained) and validate the locked
source URL/revision before and after fetch. Cargo intentionally gives a
checkout's `remote.origin.url` a local `file://` Cargo git-db URL, so requiring
the checkout remote to equal the public URL would be a false failure. The
public URL is instead bound by the reviewed lock manifest and the exact Cargo
fetch log; the fetched db/checkout is then bound by commit and object checks:

```sh
rg -F 'https://github.com/tailrocks/termrock.git' \
  "$RUNNER_TEMP/bootstrap-fetch.log" >/dev/null
rg -F 'source = "git+https://github.com/tailrocks/termrock.git?rev=5283c2acf9154d0cfcd37b1ffe821c00faf90ea2#5283c2acf9154d0cfcd37b1ffe821c00faf90ea2"' \
  "$RUNNER_TEMP/bootstrap-lock-input/Cargo.lock" >/dev/null
termrock_checkout=""
while IFS= read -r checkout; do
  [ -d "$checkout/.git" ] || continue
  rev="$(git -C "$checkout" rev-parse HEAD 2>/dev/null || true)"
  if [ "$rev" = "5283c2acf9154d0cfcd37b1ffe821c00faf90ea2" ]; then
    [ -z "$termrock_checkout" ] || exit 1
    termrock_checkout="$checkout"
  fi
done < <(find "$refresh_cargo/git/checkouts" -mindepth 2 -maxdepth 2 -type d -print)
test -n "$termrock_checkout"
test "$(git -C "$termrock_checkout" rev-parse HEAD)" = \
  5283c2acf9154d0cfcd37b1ffe821c00faf90ea2
git -C "$termrock_checkout" cat-file -e \
  5283c2acf9154d0cfcd37b1ffe821c00faf90ea2^{commit}
termrock_db="$(find "$refresh_cargo/git/db" -mindepth 1 -maxdepth 1 -type d \
  -name 'termrock-*' -print -quit)"
test -n "$termrock_db"
git -C "$termrock_db" cat-file -e \
  5283c2acf9154d0cfcd37b1ffe821c00faf90ea2^{commit}
```

Resolve `termrock_checkout` by bounded enumeration of Cargo's fetched source
directories, not a mutable guessed revision path. Record the exact Cargo source
directory, git database digest, lock revision, URL from the reviewed lock/fetch
record, and registry checksum census. Reject zero or multiple matching
checkouts, a different URL/revision in the lock/fetch record, or any extra
unreviewed git source. No candidate build, source checkout, Docker build
script, or image push runs in this networked fetch job.

### Offline/image-independent proof

The fetch artifact is keyed by both `dependency_contract_digest` and
`toolchain_input_digest`; a cache hit with either key different is invalid. The
image build consumes that exact bundle and the pinned recipe. It may use a
network-disabled BuildKit stage, but it must not use a mutable cache mount as
the only source of dependencies.

Before any target-config pin update, a separate fresh `linux/amd64` verifier
job must:

1. obtain the published image by its exact platform digest, with a clean daemon
   and no BuildKit cache;
2. fetch raw index/platform/config JSON independently and compare index,
   platform, config, layer, environment, label, platform, and `RepoDigest`
   values to the reviewed manifest;
3. verify the image contains the expected cache census and toolchain checksum
   record, not merely a label claiming one;
4. run the exact producer command with `network=none`, UID/GID `65532`, and a
   trusted source snapshot carrying the reviewed lock/toolchain inputs; and
5. compare `cargo tree --locked --offline`, the 115-node closure, the exact
   `termrock` revision, Linux ELF output, and build-stamped trusted source
   identity to independently computed values.

This is image/runtime proof only. It is not a candidate PR acceptance or a
substitute for the later hostile canary.

## Amendment B — exact identity/env/toolchain/copy obligations

### Separate dependency and toolchain input digests

The current proposal's dependency digest is insufficient by itself. Record
these separately in the target config and every builder manifest/attestation:

```text
dependency_contract_digest = SHA-256(sorted Git tree records for
  Cargo.toml, Cargo.lock, workspace Cargo.toml files, rust-toolchain.toml,
  rust-toolchain, and reviewed .cargo data)

toolchain_input_digest = SHA-256(versioned records for
  rust-toolchain.toml, rust-toolchain, docker/build-mise.toml,
  docker/build-mise.lock, exact base image index/platform digest,
  direct toolchain archive URLs/checksums, and toolchain install recipe)

recipe_digest = SHA-256(versioned Dockerfile bytes, fixed copy-wrapper bytes,
  canonical context file list/bytes, build args, and base digest)
```

The exact Rust channel (`1.98.1`), target
`x86_64-unknown-linux-gnu`, components, `mbx 1.11.1`, base digest, installer
checksums, and resolved `cargo -V`/`rustc -Vv` are all input evidence. Updating
any toolchain/base/installer input creates a new builder and requires a new
reviewed pin; a floating apt repository or `stable` alias is not a pin.

### Attestation subject must be the accepted image

For each builder and sandbox platform, the signed provenance/SBOM subject must
be the exact accepted `linux/amd64` platform manifest digest. If the publisher
also emits an index-level attestation, record and verify both subjects; never
accept an attestation over a tag, a layer, a source archive, or a different
platform. The predicate must bind:

```text
subject.name = exact image repository
subject.digest = reviewed platform_digest
sourceRevision = trusted base commit
recipeDigest = reviewed recipe_digest
dependencyContractDigest = reviewed builder dependency digest (builder only)
toolchainInputDigest = reviewed toolchain digest (builder only)
baseImageDigest = exact base platform/index digest
platform = linux/amd64
workflow = trusted bootstrap publisher identity
```

The referrer/attestation digest, SBOM digest, source commit, recipe digest,
and predicate fields are copied into the reviewed image record. Labels are
cross-checks only; they never replace signed provenance. The candidate's PR
source revision belongs in the candidate manifest, not in the reusable builder
image's dependency identity.

### Final builder environment and credential scrub

The builder final image must have `Config.Env == []`, `Volumes == null`, no
devices, empty/default entrypoint and command, and no credential/helper files.
The container's inspected env must be exactly the seven base-owned keys after
the Cargo-home fix:

```text
CANDIDATE_HEAD_SHA
CARGO_HOME=/target/cargo-home
CARGO_NET_OFFLINE=true
CARGO_TARGET_DIR=/target
CARGO_TERM_COLOR=never
HOME=/tmp/home
PATH=<direct pinned Rust bin>:/usr/bin:/bin
```

No image ENV is allowed to add a key. The build wrapper must receive no host
environment and no candidate-controlled command/config. The existing prefix
blacklist (`GITHUB_*`, `ACTIONS_*`, etc.) is a negative check, not sufficient
admission; exact equality is required.

### Trusted copy wrapper and measured quota

Do not use an unbounded `cp -R` as the cache proof. Bake one base-owned wrapper
at a recipe-pinned path, for example
`/usr/local/libexec/velnor-bootstrap-build`, and include its SHA-256 in
`recipe_digest` and provenance. The wrapper must:

1. run under numeric UID/GID `65532:65532` with `umask 077`;
2. inspect `/opt/velnor/cargo-home` with `find -P`, reject links/special files,
   mount escapes, path traversal, and unexpected top-level names;
3. enforce the reviewed cache census before copying:
   `cache_apparent_bytes`, `cache_file_count`, `cache_dir_count`, and
   `cache_max_path_bytes`; these values come from the immutable cache manifest,
   never from candidate env or a stale host cache;
4. copy into the writable `/target/cargo-home` tmpfs using a no-following,
   owner-neutral method, then re-census the destination; and
5. exec Cargo only after the copy and quota checks succeed.

The cache manifest must include `cache_bundle_sha256`, exact file count/bytes,
source/git census, and the dependency/toolchain digests. The wrapper uses the
reviewed limits, not a self-reported size. Define
`offline_build_peak_bytes` as the maximum **additional** `/target` usage after
the cache copy (the cache bytes are counted separately). The container
admission record must include `target_tmpfs_bytes`, `cache_copy_limit_bytes`,
and independently measured `offline_build_peak_bytes`, proving:

```text
cache_copy_limit_bytes + offline_build_peak_bytes <= target_tmpfs_bytes
```

The copy limit and target tmpfs size are hard bounds. If the measured 2 GiB
`/target` tmpfs cannot satisfy the inequality, choose a larger but still
explicitly bounded tmpfs or a separate bounded Cargo tmpfs and update all
inspect predicates. Never silently use a host directory, Docker cache mount,
or free-space check as quota. The trusted Linux verifier must run once from a
clean image layer with an empty host Cargo cache and persist the copy census,
peak usage, exit/OOM state, and output census.

## Required disposition after amendments

The proposal remains **CHANGES REQUIRED** until the lock-input schema,
base-owned approval/refresh workflow, exact scrub recipe, source/git/config
validation, separate dependency/toolchain/recipe digests, platform-attestation
subject rule, and measured copy quota are implemented and independently
reviewed. Keep both image digests empty. Do not add Dockerfiles, publish, pull,
dispatch, or claim a canary from this document.

## Successor hash

SHA-256 of this review's complete content through the preceding line (before
this marker): `6c591ad57a281f8accfada997a152330d9fb51fe69f000b7701304fc6f3582a9`.
