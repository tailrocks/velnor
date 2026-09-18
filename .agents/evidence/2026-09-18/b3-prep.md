# B3 PREP — coherent bootstrap Velnor release procedure

Campaign: bastion three-provider CI. Scope: **read-only design** — no releases cut here.
Sources: `plans/bastion-three-provider-ci/spec.md` §7, `.github/workflows/release.yml`
(4309 lines), `.github/workflows/preview.yml` (1065 lines),
`crates/velnor-workflow/src/primitives/release.rs` (generator),
`crates/velnor-runner/src/release.rs` (2157 lines, identity model),
`.github/workflows/ci-release-package-signer.yml`, `/tmp/c2-prereq-map.md`.
Work-plan: STEP B3 + checklist item B3; feeds B4 (signed APT) → C1 (locked install).

## 1. Release identity (exact)

Stable lane only (preview builds no release record and no OCI image —
`release.rs:30-44`; bootstrap for bastion is a **stable** release):

| Field | Value / rule | Enforced by |
|---|---|---|
| Tag | `v<VERSION>`, cut from protected `main` | `release.yml:73` `release verify-tag --branch main --package velnor-runner`; tag-immutability re-check before publish `:4156-4178` |
| Commit | `github.sha`, 40-hex; checkout pinned to it | publish idempotency check `:4245` (record commit == `$COMMIT`) |
| Crate version | `VERSION` = tag minus `v`, portable (`v[0-9]*`, no `/`/space) | `release.yml:74-88` |
| Debian version | must equal crate version | `CoherenceError::DebianVersion` (`release.rs:618`) |
| Build kind | `stable` | package-record candidate `:3807-3812`; `PACKAGE_KIND_STABLE` |
| Manifest version | == `MANIFEST_VERSION` (currently 13) | `CoherenceError::ManifestVersion`; `manifest.rs:20` |
| Manifest hash | `sha256(manifest.json)`, compiled once | `metadata` job `:3368-3382`; `OciManifestHash` cross-check |
| Repository anchor | `tailrocks/velnor` only | `CoherenceError::Repository`; `SOURCE_REPOSITORY` |

Release record: schema `velnor.release-record/v1` (`release.rs:64`), canonical
JSON (arches sorted, 2-space pretty, trailing newline), digest stored **outside**
the record (`.sha256` sidecar → publication record → deployed pointer; acyclic
DAG, `release.rs:15-21`). Assembled in `publish` (`:4002-4079`) and
independently re-assembled + re-verified by `release assemble`
(`release.rs:assemble_command`), never trusted as-read.

## 2. Product set: amd64 + arm64, manifests, record, checksums

Required arch set is exactly `{amd64, arm64}` — a record missing/duplicating one
is incoherent (`REQUIRED_ARCHES`, `CoherenceError::ArchitectureSet`,
`release.rs:84-86,724-764`).

### 2.1 Build matrix and native-vs-cross (exact)

| Job (`release.yml`) | amd64 | arm64 | Native? |
|---|---|---|---|
| `build` `:2862` (runner tarballs) | `x86_64-unknown-linux-gnu` | `aarch64-unknown-linux-gnu`, both on `ubuntu-24.04` (`:2868-2875`) | arm64 **cross-compiled** |
| `metadata` `:3276` | one `x86_64` release-build binary exports `build-identity.json` + `manifest.json` (`:3368-3382`) | — | single-source metadata |
| `guest-payload` `:3394` | `ubuntu-24.04` | **`ubuntu-24.04-arm`** (`:3405-3407`) | **native arm64** payload |
| `debian` `:3616` | `ubuntu-24.04` | `ubuntu-24.04` (`:3624-3632`) | arm64 **cross** (`--target`), reuses `build` binary digest-verified (`:3748-3770`) |
| `image-platform` `:3003` | `ubuntu-24.04` | **`ubuntu-24.04-arm`** (`:3015-3017`) | **native arm64** OCI leg |

B3 "native arm64 payloads" = guest payload (vmlinux/rootfs/guest-agent built on
the arm64 runner) + OCI arm64 platform image (built natively, digest-pinned into
the index). Binaries/debs are cross-built but digest-bound per arch at every
handoff (`velnor-runner-{arch}.bin.sha256`, deb guards `:3893-3916`, publish
re-verify `:4116-4134`).

### 2.2 Per-arch payloads

- Tarballs: `velnor-runner-{VERSION}-{x86_64,aarch64}-unknown-linux-gnu.tar.gz`
  + `.sha256` sidecars + `velnor-runner-{amd64,arm64}.bin.sha256` (raw binary
  digests, `:2907-2928`).
- Debs: `velnor-runner-{VERSION}-{amd64,arm64}.deb` + `.sha256`, each embedding
  exactly one `/usr/share/velnor/package-record.json` (schema
  `velnor.package-record/v1`, kind `stable`, informational —
  `release.rs:72-78`), `build-identity.json`, `manifest.json`, and
  `release/microvm/` (pinned firecracker + jailer + guest-agent + vmlinux +
  rootfs.ext4 + `manifest.json` with non-`UNSET` kernel/rootfs shas, `:3835-3888`).
- Guest payload artifacts: `vmlinux`, `rootfs.ext4`, `rootfs.sha256`,
  `velnor-guest-agent`, `guest-agent.sha256` per toolchain arch (`:3604-3614`).

### 2.3 Manifests / record / checksums published on the release

Full asset list (`:4195-4208`, `:4282-4296` — 14 assets, no clobber,
record-verified idempotency):

- `release-record.json` + `.sha256` (canonical record; sidecar holds digest)
- `manifest.json` + `.sha256` (compiled capabilities; `sha256 == record.build.manifest_sha256`
  and embedded `source_sha == record.commit`)
- `release-manifest.json` (schema `velnor.package-release.v1`: source repo/ref/commit/version + asset digests)
- `SHA256SUMS` (exactly 4 subjects: 2 tarballs + 2 debs; each sidecar must match payload, `:4080-4098`)
- 2 tarballs + sidecars, 2 debs + sidecars

## 3. OCI / payload identity incl. native arm64

- Image: `ghcr.io/tailrocks/velnor-job-ubuntu:{VERSION}`, assembled from
  per-arch staging tags `release-{COMMIT}-{amd64,arm64}` via
  `imagetools create` (`image` job `:3179-3274`).
- Index digest `sha256:…` is the identity; `oci_image_ref` must embed it
  (`CoherenceError::OciRef`). OCI labels pin version/revision/source/manifest
  hash (`OciVersion/OciRevision/OciSource/OciManifestHash`).
- Admission gate (`image-admission` `:2941-3001`): version tag inspected **without
  mutation**; a tag existing without a matching GitHub release is refused unless
  the exact index digest is supplied via explicitly verified recovery input
  (`existing-image-digest`); fail-open inspection failure aborts.
- Pre-publish immutability: OCI tag must not have moved (`:4145-4155`) and git
  tag must still resolve to `$COMMIT` (`:4156-4178`).
- Publish-path OCI check: published record's `oci_index_digest` must equal the
  current image job output (`:4253-4257`).
- Payload identity (microvm): kernel tarball + per-arch firecracker/jailer
  tarballs pinned by URL+sha256 in `microvm/pins.json`, verified at download
  (`:3841-3849`) and per-binary sha (`:3872-3877`); guest seed cache is
  exact-restore with checksum + agent-byte verification (`:3532-3560`).

## 4. Attestation set

| Subject | Attested by | Verified at |
|---|---|---|
| `dist/*.tar.gz` (both arches) | `build` job, `attest-build-provenance` (`:2929-2932`) | `publish`: `gh attestation verify $artifact --repo $GITHUB_REPOSITORY` (`:3990-3995`) |
| Each `.deb` | `sign-deb` → callable-only `ci-release-package-signer.yml` (admits only `refs/tags/v*` or `refs/heads/main`; binds sha256 pre-attestation) | `publish`: `gh attestation verify … --signer-workflow …/ci-release-package-signer.yml` (`:3996-4001`) |

Attestation happens over the fresh-build subjects staged in `signer-input`
(`:4111-4115`, `:4301-4307`) — never post-publish. Missing/failed attestation
fails the release before `gh release create`.

## 5. Unbounded / global-native prerequisite check (from `/tmp/c2-prereq-map.md`)

B3 action 2: the bootstrap source must already contain the unbounded-policy and
global-native-admission work (§0.7 parallel track) so C2 can **activate** it
before the first bastion job. No bounded jobs "until later" (spec §7 ¶"A
bootstrap package…"). The B3 author verifies the tagged source, not just the
built bytes:

**Must be ABSENT from the tagged source (REMOVE items):**

- Slot-division budget model: `container/host_budget.rs` (`HostBudget`/`SlotBudget`,
  `per_slot`, `docker_cpu/memory_option`, `job_env` build-flag injection,
  buildkit entitlements) — Q1–Q6.
- Budget enforcement in `container.rs` (`append_flags_without_limits`,
  `append_resource_budget`, `declared_container_cpus/memory`, `slot_budget`) — Q7–Q10.
- Daemon policy flags `--job-cpus/--job-memory` (`service.rs:190-198`),
  `job_resource_options` threading in `runner.rs`, `velnor.env`/unit defaults
  `VELNOR_JOB_CPUS/MEMORY` — Q11–Q15 (incl. pinning tests).
- Quota drop-in writer `write_host_scaled_jobs_cpu_quota` (`postinst:23-56`) and its
  verification — Q17; `velnor-jobs.slice` must carry **no** `MemoryHigh/Max`,
  `MemorySwapMax`, `TasksMax`, quota drop-in pin (identity/cleanup only) — Q16.
- Runtime quota proofs: `verify_docker_job_cgroup_boundary`,
  `DockerResourceCapabilities`/`validate_docker_resource_projection`,
  macOS cgroup probe in `local_diagnostics.rs` — Q21–Q23.
- `resize_builder_daemon` (`docker update --cpus/--memory`) and
  `buildx_driver_resource_options` sizing — Q26–Q27.
- Workflow quota flags (`--cpus/--cpu-quota/--cpuset-*/--memory/…`) must be
  stripped from the `safe_container_option` allowlist (`github_adapter.rs:850-875`)
  — Q30 (REFACTOR).

**Must be PRESENT (REFACTOR-complete):**

- N1/N2/N7: host-wide global-N ledger (journal permit authority extended to
  reserved/acquiring/provisioning/assignable/running/cleaning/uncertain),
  **JIT** assignable-capacity creation (no pre-registered N at boot), global-N
  config knob.
- N3/N4: admission gates + existing native protocol reused with admission
  integrated (read-only demand adapter where signals are missing).
- L4 decision: nested-create resource fields in
  `reject_unsafe_nested_host_controls` stripped (default) or explicitly allowed
  with §C2 proof covering nested descendants.
- Q24/Q25: quota fixtures/docs inverted to assert absence.

**Mechanical gate:** grep-negative sweep over the tag (no `CPUQuota`,
`NanoCpus`, `per_slot`, `job_resource_options`, `VELNOR_JOB_CPUS`,
`write_host_scaled_jobs_cpu_quota`, `verify_docker_job_cgroup_boundary` in
`crates/` + `debian/`) plus grep-positive (`max_jobs`/global-N ledger,
JIT creation), recorded in the B3 evidence. Postinst must provably not recreate
quota drop-ins on upgrade (spec §4.3).

## 6. Coherence negatives (each must FAIL the release)

Executable via `release` verbs (`release.rs:run`) and workflow guards; wire as
negative tests against the B3 candidate:

1. **Mismatched arch** — record with a missing/extra arch → `ArchitectureSet`;
   duplicate arch → `DuplicateArch`; wrong triple for arch
   (e.g. arm64→x86_64 triple) → `ArchTarget`. Deb whose `dpkg-deb Architecture`
   ≠ matrix arch, or packaged `/usr/bin/velnor-runner` digest ≠ record
   `binary_sha256`, fails the deb guard (`:3893-3916`) and publish re-verify
   (`:4116-4134`).
2. **Missing manifest** — `manifest_version` ≠ 13 → `ManifestVersion`;
   `sha256(manifest.json)` ≠ `record.build.manifest_sha256` →
   incoherent (`:4249-4252`); OCI label manifest hash mismatch →
   `OciManifestHash`. Deb missing its package record, or >1 record, fails
   (`:3905-3907`).
3. **Bad digest** — record bytes failing sidecar → `RecordChecksum`; non-canonical
   bytes → `NonCanonical`; `assemble` recomputes per-arch binary + deb digests
   from artifacts and refuses disagreement (`assemble_command`);
   `SHA256SUMS --check --strict` failure fails publish (`:4098`, `:4259`).
4. **Missing attestation** — `gh attestation verify` failure (tarballs) or
   wrong-signer failure (debs) aborts before release creation (`:3990-4001`);
   signer rejects non-`v*`/non-`main` refs before download.
5. **Moved tag / moved image** — git tag no longer resolving to `$COMMIT`, or OCI
   version tag moved, aborts publish (`:4145-4178`).
6. **Partial/clobber** — existing release missing any of the 14 assets, or with
   record tag/commit/version/manifest/OCI not matching this run, refuses
   overwrite (`:4187-4280`); never mint at main HEAD with no record (`:4186`).

## 7. How the release proves KVM-tests-stay-separate

Spec §7 contract: "Building/staging a kernel/rootfs is not running a VM.
Genuine KVM-runtime tests stay a declared separately qualified platform
capability; VMs are never provisioned on bastion and a Docker substitute is
never counted as passing them." Proof structure for B3:

1. **Release workflow boots no VM.** `guest-payload` only builds bytes
   (`velnor-guest-image build`, checksums; `:3532-3586`); `debian` only stages
   bytes (`velnor-guest-image stage`, pin/sha checks; `:3835-3888`). No
   firecracker/jailer execution step exists in `release.yml` — firecracker and
   jailer binaries are checksummed artifacts, never invoked. Audit: grep the
   generated workflow for VM-boot invocations → none.
2. **KVM need is fail-closed, not substitutable.** `execution/firecracker.rs:188-191`
   refuses when `/dev/kvm` is missing/unusable; unit test
   `microvm_missing_kvm_does_not_invoke_host_docker` (`execution/tests.rs:201`)
   proves the missing-KVM path does **not** fall back to Docker. Preflight's
   microvm check is a synthetic probe (`preflight.rs:40-72`), not a job VM.
3. **Bastion has no KVM substrate.** Spec §6: "No libvirt/KVM/QEMU" on bastion;
   C1 checklist requires proving it. Therefore no KVM-runtime test can execute
   there — any KVM-capable suite result claimed from bastion is definitionally
   invalid.
4. **Declared-separate qualification.** KVM stays a typed platform capability
   (spec §2: capabilities fail explicitly, never silently default); genuine
   KVM tests are qualified on a KVM-bearing host, recorded with their own
   platform identity — never satisfied by a Docker-lane pass. B3 evidence must
   list which suites are KVM-gated and show the release's test plan excludes
   them from bastion's expected result set rather than reclassifying them.

## 8. B3 procedure (ordered) + gates

```
G0  Source ready: reviewed main commit; unbounded/global-native prereqs merged (§5 sweep green).
G1  Tag v<VERSION> cut on main; verify-tag + version-portability green (release.yml:verify).
G2  Full unit matrix green on github + velnor lanes (build needs: all release-github-*,
    release-velnor-*) — broken tree never reaches build.
G3  Metadata compiled once (build-identity.json + manifest.json + release tool);
    manifest_sha256 exported.
G4  Per-arch products built: tarballs (+bin digests), native guest payloads (arm64 on
    arm64 runner), debs (binary-reuse + guards + embedded records), native OCI legs.
G5  OCI index assembled (both platform digests pinned); admission + immutability hold.
G6  Attestations exist: tarballs (build) + debs (signer, admitted refs only).
G7  Publish: provenance verified → record assembled + re-verified → SHA256SUMS →
    consumer manifest → packaged-identity re-verify → OCI/tag immutability →
    create-once (no clobber) → package subjects staged for signer evidence.
G8  B3 outputs recorded: tag/commit/version, per-arch digests, manifests/record,
    OCI/payload identity, attestations, negative-coherence logs (§6), prereq sweep (§5),
    KVM-separation proof (§7).
```

**Entry gates (block cutting the tag):** G0 prereq sweep green; generator is the
published trusted product (B2 publisher path proven); no hand YAML (workflows
are `velnor-workflow`-generated).

**Exit gates (B3 done per checklist):** exact tag/commit/version; coherent
amd64+arm64 products + manifests/record/checksums; OCI/payload identity incl.
native arm64; trusted attestations — all via generated hosted CI; §6 negatives
demonstrated failing; §5 prerequisites in-tree; §7 KVM separation evidenced.
Verifier: release/package verifier. Next: B4 consumes the record (never
`releases/latest` for the daemon package — spec §3.2) through the generated
single-writer APT flow.
