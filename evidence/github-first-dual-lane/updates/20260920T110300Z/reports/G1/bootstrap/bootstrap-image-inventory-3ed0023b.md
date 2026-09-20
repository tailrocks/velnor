# G1 bootstrap image inventory — source `3ed0023b038335d7b22dfa2758457e3808f777ee`

Date: 2026-09-20 (UTC/Asia-Ho_Chi_Minh). This is a read-only inventory. No
Docker daemon, image pull, image build, image push, workflow dispatch, or source
mutation was performed.

## Verdict

**No accepted bootstrap builder or sandbox image is available. G1 remains
blocked.**

The exact source intentionally embeds empty image digests:

| role | repository | digest | source |
|---|---|---|---|
| builder | `ghcr.io/tailrocks/velnor-bootstrap-builder` | empty | `crates/velnor-workflow/src/s2/mod.rs:418-424` |
| sandbox | `ghcr.io/tailrocks/velnor-bootstrap-sandbox` | empty | `crates/velnor-workflow/src/s2/mod.rs:413-417` |

The generated callers fail closed on those values (`.github/workflows/ci-pr.yml:127-156`,
`.github/workflows/ci-policy.yml:482-566`). No digest is invented here.

## Source and publication evidence

Exact remote refs observed:

```text
3ed0023b038335d7b22dfa2758457e3808f777ee refs/heads/codex/g1-bootstrap-isolation
4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d refs/heads/main
```

Relevant exact-source blobs:

```text
ea581cc5871500e59897aa48e0a923499a579503 crates/velnor-workflow/src/s2/mod.rs
b43e95dfce7e18989e400b91a20848d1d9ea89a7 .github/workflows/ci-pr.yml
abe817dc1bd081052a1ea19eb3226b7ad6e8c097d .github/workflows/ci-policy.yml
2245de0a088ba3b1e285d007376aa5553ea17ae2 Dockerfile
9840dda84354d3205cfb10e68be98ef6157abbfd docker/job-ubuntu.Dockerfile
bd824dcc346e9ecd7e3aea378dd37e560041e2a8 Cargo.lock
bc9347eeaac887e0fbd0b90e533acc82aced99d4 crates/velnor-workflow/Cargo.toml
2821a028d23d22eb0addbfae3c6e8712f98c9521 crates/velnor-workflow/build.rs
90e78099c30e62667ac540070a7de8dadd771b23 rust-toolchain.toml
614c829ed3c06b312b18a5ac18f2d9bd59230be2 docker/build-mise.toml
aa64aeb48d0b45f3c6e1757dac322464b197e1c9 docker/build-mise.lock
```

The exact tree contains no `docker/bootstrap-*.Dockerfile`, bootstrap image
publish workflow, or bootstrap image manifest/attestation record. The only
repository image publication path is the product image
`ghcr.io/tailrocks/velnor-job-ubuntu` in `release.yml`; it is not the candidate
builder or sandbox.

Anonymous GHCR metadata requests for both bootstrap repositories returned
`401` with a pull-token challenge. The unauthenticated token endpoint returned
`403` for both. The current GitHub principal cannot prove package existence:

```text
users/tailrocks/packages/container/velnor-bootstrap-builder/versions -> 404 Package not found
users/tailrocks/packages/container/velnor-bootstrap-sandbox/versions -> 404 Package not found
```

These responses do not prove that a private package is absent; they prove that
no usable manifest, digest, config, or provenance was available to this
inventory. Therefore neither bootstrap repository has a legitimate immutable
candidate identity.

Registry query sources (read-only):

```text
https://ghcr.io/v2/tailrocks/velnor-bootstrap-builder/tags/list
https://ghcr.io/v2/tailrocks/velnor-bootstrap-sandbox/tags/list
https://api.github.com/users/tailrocks/packages/container/velnor-bootstrap-builder/versions
https://api.github.com/users/tailrocks/packages/container/velnor-bootstrap-sandbox/versions
```

## Available image that is explicitly rejected

The public product image has inspectable immutable metadata. This is evidence
that GHCR read-only manifest inspection works, not an approval of this image:

```text
ref:            ghcr.io/tailrocks/velnor-job-ubuntu:0.1.242
index digest:   sha256:c730f40ef5dff56af4eb8917675f8b809f7d90b058550c0c2cc324037c84f5ee
linux/amd64:    sha256:58d42375fd83a7d1c10af634e8667e6bc944f02d3c68af3e114c36fbf4400bf8
config digest:  sha256:e101a9b4361f1febb66ed33e11627db9ebecb76bef1542bc8f79e04d5547736f
```

The exact manifest/config reads were made at:

```text
https://ghcr.io/v2/tailrocks/velnor-job-ubuntu/manifests/0.1.242
https://ghcr.io/v2/tailrocks/velnor-job-ubuntu/manifests/sha256:58d42375fd83a7d1c10af634e8667e6bc944f02d3c68af3e114c36fbf4400bf8
https://ghcr.io/v2/tailrocks/velnor-job-ubuntu/blobs/sha256:e101a9b4361f1febb66ed33e11627db9ebecb76bef1542bc8f79e04d5547736f
```

Its remote config/history identifies a job toolchain, not a sandbox:

```text
Env: PATH=/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
     HOME=/root, MISE_DATA_DIR=/opt/mise, MISE_CACHE_DIR=/opt/mise/cache,
     MISE_CONFIG_DIR=/opt/mise/config, MISE_PYTHON_COMPILE=0,
     CARGO_NET_RETRY=10, CARGO_HTTP_TIMEOUT=120
Cmd:        ["/bin/bash"]
WorkingDir: /__w
User:       root by default
revision:   8fe79477eed0d458f9fe34fc492aa3048c496675 (image label)
```

It fails the producer image admission because the inherited env-name set is
not exactly the seven explicit names required by `ci-pr.yml:185`, and it does
not identify as `org.velnor.sandbox=true`. It also has no identity binding to
source `3ed0023b...`. The pinned Ubuntu base digest in `Dockerfile` is only a
base input, not this final image and not a sandbox approval.

## Builder dependency/source contract

The producer runs exactly:

```text
cargo build --locked --offline -p velnor-workflow
install -m 0555 target/debug/velnor-workflow /output/velnor-workflow
```

inside the image with `/src` as a read-only bind, `CARGO_HOME=/tmp/cargo`,
`CARGO_NET_OFFLINE=true`, `CARGO_TARGET_DIR=/target`, `HOME=/tmp/home`, and
the fixed PATH:

```text
/usr/local/cargo/bin:/usr/local/rustup/toolchain/stable-x86_64-unknown-linux-gnu/bin:/usr/bin:/bin
```

The exact source declares Rust `1.98.1` (`rust-toolchain.toml`) and the Cargo
lock has 438 package entries: 427 crates.io sources, one git source, and ten
workspace/path packages. An offline `cargo tree --locked --offline` over the
exact source, with normal and build edges for `velnor-workflow`, found 115
unique package/version nodes, including the required git dependency:

```text
termrock v0.11.0
git+https://github.com/tailrocks/termrock.git?rev=5283c2acf9154d0cfcd37b1ffe821c00faf90ea2#5283c2acf9154d0cfcd37b1ffe821c00faf90ea2
```

The builder must therefore contain, before the network-less run:

1. A Linux x86_64 Rust/Cargo toolchain that is demonstrably Rust `1.98.1`,
   including host std and linker/runtime support. The workflow PATH bypasses
   `/opt/mise`; an image relying only on the Dockerfile's `/opt/mise` shims is
   not proven usable. The `stable-x86_64-unknown-linux-gnu` path must resolve
   to the pinned `1.98.1`, or the builder must provide an equivalent direct
   toolchain at that path.
2. `/tmp/cargo` populated with the complete locked registry index, crate
   archives/source trees, and the git checkout/database for the exact
   `termrock` revision. No Cargo cache bind is supplied by the workflow.
3. `/bin/sh`, `install`, `git`, and ordinary build utilities. `build.rs` calls
   `git rev-parse HEAD` and `git ls-tree`; the source bind must retain the
   shallow checkout's `.git` metadata. `cargo build` writes only `/target` and
   the output install writes only `/output`.
4. No inherited token/credential env, volumes, host/socket devices, or hidden
   writable source/cache mount. The runtime command supplies only the explicit
   seven env names and uses UID/GID `65532:65532`, `network=none`, read-only
   root, private PID, dropped capabilities, and bounded tmpfs.

The checked-in product `Dockerfile` is not this contract: it uses
`CARGO_HOME=/usr/local/cargo` and cache mounts at `/usr/local/cargo/{registry,git}`
while building, then starts a fresh final Ubuntu stage with product binaries,
Docker CLI tooling, root user, `/work`, and `velnorctl` entrypoint. It neither
publishes a bootstrap builder nor proves a `/tmp/cargo` offline closure.

## Sandbox final-image contract

Before execution, `ci-policy.yml` requires the exact index to resolve to one
non-attestation `linux/amd64` platform manifest and then checks the exact
platform config. A legitimate final sandbox must provide:

```text
architecture: amd64
os:          linux
Config.Env:  ["PATH=/usr/bin:/bin"]
Config.User: ""
Entrypoint:  []
Cmd:         []
WorkingDir:  "/"
Volumes:     null
ExposedPorts: null
Healthcheck: null
Labels[org.velnor.sandbox]: "true"
```

The local pulled image must have the exact config digest advertised by its
platform manifest and the exact platform `RepoDigest`. Runtime then overrides
the process boundary to UID/GID `65532:65532`, `network=none`, read-only root,
private PID/IPC, no capabilities, no-new-privileges, no host/socket/device
mounts, only read-only `/input` and `/candidate` binds, and fixed `/tmp` and
`/output` tmpfs. The only runtime env names are
`SOURCE_HEAD_SHA`, `SOURCE_TREE_SHA`, `SOURCE_REPOSITORY`, `SOURCE_CLOSURE`,
`HOME`, and `PATH`.

No image with these final config fields, exact platform/config digests, source
commit/build recipe identity, SBOM, and attestation was found. Configuration
strings in a Dockerfile or a base-image digest would not be runtime
attestation.

## Required next evidence (not performed here)

An image owner must publish the two separate images from a trusted source
commit and provide, for each image:

- immutable index digest;
- exact `linux/amd64` platform manifest digest;
- exact config digest and all layer digests/sizes;
- OCI config admission output and `RepoDigest` output;
- builder offline-closure proof (`cargo build --locked --offline` under the
  exact producer command, including the 115-node closure and termrock git
  revision);
- source/Dockerfile/build revision, SBOM, and signed provenance/attestation;
- hostile hosted Linux canary evidence before any G1 approval.

Hosted Linux canary is allowed only after source approval and this evidence.
Actual Mac/OrbStack rollout remains prohibited before G3.
