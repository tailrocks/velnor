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
