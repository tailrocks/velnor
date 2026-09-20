# G1 bootstrap image build contract proposal

Status: design for independent review. No source, generated workflow, Docker
daemon, registry, or GitHub state was changed. This proposal is for the exact
candidate source `3ed0023b038335d7b22dfa2758457e3808f777ee`.

## Decision

Use two separate public, Linux/amd64, digest-pinned OCI images:

```text
ghcr.io/<target>/velnor-bootstrap-builder
ghcr.io/<target>/velnor-bootstrap-sandbox
```

The generic `velnor-workflow` engine owns schema validation and workflow
rendering. The scanned repository's generation config owns image repositories,
Dockerfile paths, dependency-contract digest, and all reviewed image pins. No
repository name, image tag, digest, or consumer path belongs in generic Rust
code. Remove the current `s2/mod.rs` image literals during implementation; no
alias or fallback remains.

The trusted publication workflow runs only from the canonical default branch
(push after review, plus an explicitly trusted manual run). Pull requests never
build or push images. Candidate jobs consume only public images by exact index
digest; a private GHCR package would require a credential and is not accepted
by the current `permissions: {}` producer/execute boundary.

## Target-owned contract

Add one strict target-config section (exact field names are proposal-level and
must be implemented with `deny_unknown_fields`):

```toml
[bootstrap_images]
enabled = true
builder_repository = "ghcr.io/tailrocks/velnor-bootstrap-builder"
sandbox_repository = "ghcr.io/tailrocks/velnor-bootstrap-sandbox"
builder_dockerfile = "docker/bootstrap-builder.Dockerfile"
sandbox_dockerfile = "docker/bootstrap-sandbox.Dockerfile"
context = "."
platform = "linux/amd64"

# All are empty until the trusted publication record exists. Generation must
# fail closed while any required value is empty.
source_revision = "<trusted commit that built the images>"
builder_recipe_digest = "sha256:<real Dockerfile/recipe digest>"
sandbox_recipe_digest = "sha256:<real Dockerfile/recipe digest>"
builder_dependency_digest = "sha256:<real dependency-contract digest>"
builder_index_digest = "sha256:<real OCI index digest>"
builder_platform_digest = "sha256:<real linux/amd64 manifest digest>"
builder_config_digest = "sha256:<real config digest>"
sandbox_index_digest = "sha256:<real OCI index digest>"
sandbox_platform_digest = "sha256:<real linux/amd64 manifest digest>"
sandbox_config_digest = "sha256:<real config digest>"
builder_attestation_digest = "sha256:<real provenance/referrer digest>"
sandbox_attestation_digest = "sha256:<real provenance/referrer digest>"
```

`<real ...>` is documentation notation only; no placeholder may enter the
repository. The generated PR/policy workflows receive the repository and index
digests from this section and independently derive/check the platform and
config digests against the pinned values. A tag, base `FROM` digest, local
Docker cache, or Dockerfile string is not an image identity.

The dependency contract is a canonical SHA-256 over a versioned listing of
the trusted tree entries:

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
rust-toolchain
.cargo/**
dependency-contract-version:1
```

Use sorted `git ls-tree -r` records and the same byte encoding in the trusted
publisher and candidate preflight. This is separate from the candidate source
closure: Rust source edits may reuse the image; dependency/toolchain input
edits may not.

## Builder image

The builder Dockerfile is a dedicated multi-stage image. Its final stage:

- uses the currently pinned Ubuntu base
  `ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b`;
- contains the direct Rust/Cargo `1.98.1` x86_64 Linux toolchain, host std,
  linker/sysroot, `/bin/sh`, `install`, `git`, and the smallest native linker
  set needed by the measured build (`build-essential`, `ca-certificates`,
  `git`, `tar`; `pkg-config` if the verified build requires it);
- contains no Docker client, socket helper, cloud CLI, registry credential,
  secret, volume, default token env, or candidate source;
- contains a read-only hydrated Cargo home at
  `/opt/velnor/cargo-home`; and
- has labels binding source/recipe/dependency identity, for example:
  `org.velnor.bootstrap.builder=true`,
  `org.velnor.dependency-contract=<digest>`,
  `org.opencontainers.image.revision=<trusted commit>`, and
  `org.opencontainers.image.source=<target repository>`.

The final image must not rely on inherited `ENV`, `ENTRYPOINT`, or `CMD`.
Runtime supplies the allow-list and command. No `VOLUME` declaration is
allowed. Runtime sets numeric UID/GID `65532:65532`.

### Hydration and exact 115-node closure

The trusted image build checks out the trusted source and uses the repository's
current pins, not a PR checkout:

```text
rust-toolchain.toml: channel 1.98.1
docker/build-mise.lock: Rust 1.98.1; mr-boxington 1.11.1
Cargo.lock: termrock git rev 5283c2acf9154d0cfcd37b1ffe821c00faf90ea2
```

Hydrate a temporary build-stage Cargo home by compiling the selected package
with network access only in this trusted publication job:

```sh
CARGO_HOME=/opt/velnor/cargo-home cargo build --locked -p velnor-workflow
CARGO_HOME=/opt/velnor/cargo-home cargo build --locked --offline -p velnor-workflow
```

The second command is mandatory. The build record must show the measured
115-node normal/build closure and exactly one git source, `termrock` at the
locked revision. Cargo checksum verification remains enabled. The final image
copies only the hydrated Cargo home and resolved toolchain; it does not copy
the trusted source or build output.

The build stage may create a synthetic local Git repository for `build.rs` while
hydrating the cache; this commit is not candidate identity. Candidate builds
run against the real checkout `.git` and stamp the PR head/closure there.

### Structural fix required before image implementation

The current command is impossible to satisfy:

```text
--tmpfs /tmp ...
--env CARGO_HOME=/tmp/cargo
```

The tmpfs hides every image-layer `/tmp/cargo` cache. No image digest can fix
that. Change the generated producer contract to keep the image cache at a
read-only path and hydrate a writable tmpfs path:

```text
image cache:  /opt/velnor/cargo-home       (read-only image layer)
runtime home: /target/cargo-home           (writable /target tmpfs)
env:          CARGO_HOME=/target/cargo-home
```

The fixed shell entrypoint must perform, before Cargo and under UID 65532,
`mkdir -p "$CARGO_HOME"`, copy the image cache into it, make the copied cache
writable as needed, then run:

```sh
cargo build --locked --offline -p velnor-workflow
install -m 0555 target/debug/velnor-workflow /output/velnor-workflow
```

No additional host bind or network is introduced. The existing `/target`
tmpfs budget must be verified against the hydrated 115-node cache plus debug
build output. If that budget is insufficient, enlarge the bounded tmpfs only
after measured evidence; never move Cargo home to a host path.

The PATH must resolve the pinned compiler directly, not rely on the current
Dockerfile's `/opt/mise` shims or an unproven `stable` alias. The image build
records `cargo -V`, `rustc -Vv`, target triple, and the mapping from the direct
runtime path to Rust `1.98.1`. A toolchain update changes the dependency
contract and requires a new builder image/pin.

## Sandbox image

Dedicated `docker/bootstrap-sandbox.Dockerfile`:

```Dockerfile
FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b
ENV PATH=/usr/bin:/bin
WORKDIR /
ENTRYPOINT []
CMD []
LABEL org.velnor.sandbox=true
```

No package/toolchain/source copy, `USER`, `VOLUME`, exposed port, or healthcheck
is added. The pinned Ubuntu runtime supplies the glibc loader/libs required by
the candidate Linux binary. Runtime remains fixed UID 65532, read-only root,
private PID/IPC, no network, no capabilities, no host/socket/device mount, and
only the two read-only input/candidate binds plus bounded `/tmp`/`/output`
tmpfs mounts.

Remote and local admission must require:

```text
one non-attestation linux/amd64 manifest in the pinned index
platform manifest config.digest == pinned sandbox_config_digest
architecture=amd64, os=linux
Config.Env == ["PATH=/usr/bin:/bin"]
Config.User == ""
Entrypoint == [], Cmd == [], WorkingDir == "/"
Volumes == null, ExposedPorts == null, Healthcheck == null
Labels[org.velnor.sandbox] == "true"
```

The builder receives the same exact index/platform/config admission plus its
builder label/dependency-contract checks. Record all layer digests and sizes.

## Trusted publication workflow

Add one generic `bootstrap-images` publisher primitive driven by
`[bootstrap_images]`; do not repurpose the existing product `release.kind =
"docker"` lane. The generated workflow shape is:

1. Checkout the trusted default-branch commit with full history and verify the
   dependency-contract listing.
2. Build and push builder and sandbox as separate `linux/amd64` images with
   BuildKit, trusted base digests, immutable commit tags only as locators,
   `provenance=mode=max`, and `sbom=true`. No PR event can reach this job.
3. Inspect the pushed index/platform/config manifests remotely; pull exact
   platform digests only for local verification; reject unexpected platform,
   config, layer, label, or environment fields.
4. Run the exact no-network builder container against the trusted checkout to
   prove cache hydration and the offline 115-node build under UID 65532.
5. Verify provenance/SBOM subject digests bind to the image, trusted source
   revision, Dockerfile recipe digest, dependency digest, and build workflow.
   Persist the complete JSON record as a signed/attested build artifact.
6. A reviewed maintainer change copies the six image digests, dependency
   digest, recipe/source pins, and attestation digests into target config;
   generation stays fail-closed until all are present. Do not auto-edit source
   from an untrusted or merely successful image job.

The resulting runtime workflows use only the index digest from target config,
derive the platform/config records, and compare every derived value to the
reviewed target pins. No mutable `latest`, semver tag, local cache hit, or
Dockerfile base digest is accepted.

## Changed PR lock/toolchain behavior

Candidate source changes are divided explicitly:

| PR change | action |
|---|---|
| Rust source/tests/templates only; dependency contract unchanged | build with the pinned builder image; binary stamps exact PR head and source closure |
| `Cargo.lock`, `Cargo.toml`, `rust-toolchain.toml`, bare `rust-toolchain`, or `.cargo/**` changes | compute candidate dependency-contract digest; reject before build because the pinned cache is not proven for it |
| `termrock` git URL/revision changes | reject as a dependency-contract change; no network fetch or mutable git checkout |
| toolchain/base/mise lock update on trusted main | trusted publisher creates a new builder, proves offline build, then a reviewed pin update changes target config |

There is no fallback to host Cargo, online Cargo, an ambient cache, a tag, or a
different image. A lock-changing PR can receive ordinary network-enabled CI,
but isolated bootstrap acceptance remains unavailable until the new trusted
image and all manifest/config/provenance pins are reviewed and merged. This is
the required fail-closed behavior for changed dependency inputs.

## Review gate

Before adding either Dockerfile or changing generated/source code, independently
review and approve this proposal's `/tmp` masking fix, target-config ownership,
public-image requirement, 115-node hydration proof, and lock-change rejection
policy. After approval, implementation can add the Dockerfiles and publisher
primitive, then run static/admission tests. No final digest may be fabricated;
actual pins enter only from a real trusted publication record.

