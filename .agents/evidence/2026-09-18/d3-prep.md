# D3 PREP — second release + bastion-upgrade procedure (authored, NOT executed)

Date: 2026-09-17. Status: READ-ONLY prep. D3 gate not reached; zero releases cut, zero feed writes, zero bastion writes.
Scope: bastion campaign. Work-plan STEP D3 + checklist item D3: ship D1/D2 code to bastion as a second signed APT
release through the identical locked procedure (B3 → B4 → C1-upgrade → C2-repeat).
Inputs: work-plan STEP D3, `/tmp/b3-prep.md` (bootstrap release), `/tmp/b4-prep.md` (signed feed),
`/tmp/c1-hostsetup.md` (host setup + locked transaction), `/tmp/c2-impl.md` + `/tmp/c2-bench-prep.md` (quota-free baseline).

Notation: `<V1>` = B4-shipped bootstrap version (candidate at D3 start, e.g. `0.1.274`); `<V2>` = new D3 candidate
version; `v<V2>` = new tag; `<C2>` = new release commit. Every `<V1>` value is re-read live at D3 start, never copied
from this file.

## 0. Frozen inputs + entry gates (re-verify read-only at D3 start)

- E1 D1 exit bundle complete (Rust adapter + fixtures + live-canary proof, digest-pinned images, credential/trust
  wiring, private-Docker proof, allocator race-test proof, restart/recovery proof).
- E2 D2 exit bundle complete (no-legacy mechanical proof, routing/fanout proof, strict-result negatives,
  capability-test proof, watchdog + outage-detection proof, trust-denial proof).
- E3 D1/D2 source merged to reviewed `main` commit `<C2>`; generator is the published trusted product; no hand YAML
  in the release path (B3 entry rule, unchanged).
- E4 No live-feed drift unaccounted: re-probe live `publication-record.json` + index versions read-only; base for the
  B4 §5.3 no-rollback guard is the current live state (candidate `<V1>`, previous `<V0>`).
- E5 Bastion is at the C2-qualified state: `velnor-runner=<V1>` installed via the C1 transaction, quota-free proof +
  provisional N recorded. Any drift (version, pins, quota drop-ins) is re-qualified before D3 proceeds.
- E6 B4 §0.4.1-class signer-fingerprint agreement re-asserted: live-sig issuer == publication-record fingerprint ==
  separately trusted reference. Any new discrepancy aborts D3 before any mutation.

## 1. B3 repetition checklist — the Scale Set package release

Same B3 procedure (`/tmp/b3-prep.md` §8), new identity. All through generated hosted CI.

### 1.1 New tag / version identity (B3 §1 repeated)

| Field | Rule for D3 |
|---|---|
| Tag | `v<V2>`, cut from protected `main`; `release verify-tag --branch main --package velnor-runner`; tag-immutability re-check before publish |
| Commit | `<C2>`, 40-hex; checkout pinned to it; publish idempotency check (record commit == `<C2>`) |
| Crate version | `<V2>` = tag minus `v`, portable; Debian version must equal crate version |
| Build kind | `stable` (Scale Set package is a stable release, same as bootstrap) |
| Manifest version | == current `MANIFEST_VERSION` (re-read at D3 start, was 13 at B3) |
| Manifest hash | `sha256(manifest.json)`, compiled once, `OciManifestHash` cross-check |
| Repository anchor | `tailrocks/velnor` only |
| Strict advance | `<V2>` > `<V1>` (SemVer/Debian ordering); otherwise the B4 no-rollback guard will refuse it |

Release record: schema `velnor.release-record/v1`, canonical JSON, digest in `.sha256` sidecar; assembled in
`publish`, independently re-assembled + re-verified by `release assemble`, never trusted as-read.

### 1.2 amd64 + arm64 product set (B3 §2 repeated)

- Required arch set exactly `{amd64, arm64}`; missing/duplicate arch is incoherent.
- Build matrix unchanged: runner tarballs (arm64 cross), metadata single-source x86_64, guest-payload native arm64
  (`ubuntu-24.04-arm`), debian cross with digest-verified binary reuse, OCI legs native per arch.
- Per arch: tarball + `.sha256` + `.bin.sha256`; deb + `.sha256` embedding exactly one
  `/usr/share/velnor/package-record.json` (kind `stable`), `build-identity.json`, `manifest.json`,
  `release/microvm/` with pinned firecracker/jailer/guest-agent/vmlinux/rootfs + non-`UNSET` kernel/rootfs shas.
- D3 delta inside the debs: D1 Rust Scale Set adapter + D2 three-provider/watchdog code, same embedded-record schema.
  No untracked feature binary: everything bastion runs arrives in these debs.
- Published assets (14, no clobber, record-verified idempotency): `release-record.json` + `.sha256`,
  `manifest.json` + `.sha256`, `release-manifest.json`, `SHA256SUMS` (exactly 2 tarballs + 2 debs), 2 tarballs +
  sidecars, 2 debs + sidecars.

### 1.3 OCI / payload identity incl. native arm64 (B3 §3 repeated)

- Image `ghcr.io/tailrocks/velnor-job-ubuntu:{<V2>}` from per-arch staging tags `release-{<C2>}-{amd64,arm64}` via
  `imagetools create`; index digest is the identity; `oci_image_ref` embeds it; labels pin
  version/revision/source/manifest-hash.
- Admission gate (no mutation on inspect; tag-without-release refused unless exact index digest via verified recovery
  input), pre-publish OCI-tag + git-tag immutability, publish-path `oci_index_digest` equality — all re-run for `<V2>`.
- Payload pins: kernel + per-arch firecracker/jailer URL+sha256 in `microvm/pins.json`, verified at download and
  per-binary; guest seed exact-restore with checksum + agent-byte verification.
- D3 delta: official runner + DinD image pins (D1 action 4, validated digests, never `latest`) are recorded as part
  of the release evidence; runner version recorded separately; tool content verified.

### 1.4 Attestations (B3 §4 repeated)

- Both tarballs: `attest-build-provenance` by `build`; verified at publish via `gh attestation verify --repo`.
- Both debs: callable-only `ci-release-package-signer.yml` (admits only `refs/tags/v*` / `refs/heads/main`, binds
  sha256 pre-attestation); verified via `--signer-workflow`.
- Attestation over fresh-build staged subjects, never post-publish; any miss fails the release before `gh release create`.

### 1.5 Prerequisite sweep (B3 §5 adapted for D3)

The B3 grep-negative sweep (no `CPUQuota`, `NanoCpus`, `per_slot`, `job_resource_options`, `VELNOR_JOB_CPUS`,
`write_host_scaled_jobs_cpu_quota`, `verify_docker_job_cgroup_boundary` in `crates/` + `debian/`) plus grep-positive
(`max_jobs`/global-N ledger, JIT creation) is re-run over tag `v<V2>` and must stay green: D1/D2 must not reintroduce
any quota/budget mechanism. Added D3 assertions over the tag:

- No per-scope/per-engine N (single host-wide `max_jobs=N` ledger still the only capacity authority).
- No new `safe_container_option` quota flags; nested-create resource fields still stripped per the L4 decision.
- postinst still deletes quota drop-ins and never writes one (spec §4.3; upgrade path exercises this — see §3).
- New D1/D2 units carry no `CPUQuota`/`MemoryMax`/`MemoryHigh` (control-plane `velnor-control.slice`
  weights/TasksMax excepted per C2 boundary; workload `velnor-jobs.slice` identity-only).

### 1.6 Coherence negatives (B3 §6 repeated, each must FAIL the release)

Mismatched arch / missing manifest / bad digest / missing attestation / moved tag-or-image / partial-clobber —
same six negatives, executed against the `<V2>` candidate. KVM-separation proof (B3 §7) is re-recorded: release
workflow boots no VM, missing-KVM stays fail-closed without Docker fallback, bastion still has no KVM substrate,
KVM tests still declared-separately qualified.

### 1.7 B3-repeat exit bundle

Tag/commit/version (`v<V2>`/`<C2>`/`<V2>`), per-arch digests, manifests/record, OCI/payload identity incl. native
arm64, trusted attestations, negative-coherence logs, prereq sweep (§1.5), KVM-separation proof, D1/D2 image-pin +
runner-version record.

## 2. B4 repetition checklist — signed feed update with retention

Same B4 procedure (`/tmp/b4-prep.md`), candidate `<V2>`. D3 consumes the B3-repeat release record, never
`releases/latest`.

### 2.1 Gate V0 — independent pre-mutation verification (zero partial state on any failure)

V0.1 source/tag identity (`v<V2>` → `<C2>`, crate == Debian == tag-minus-`v`, anchor `tailrocks/velnor`) →
V0.2 digest recompute (2 tarballs, 2 debs, `manifest.json`, `release-record.json`; sidecars + `SHA256SUMS --check
--strict`; canonical record bytes) → V0.3 manifest (`MANIFEST_VERSION`, `manifest_sha256`, `source_sha`) →
V0.4 OCI (image ref resolves to record `oci_index_digest`, platform digests pinned, labels match) →
V0.5 provenance (`gh attestation verify` tarballs + debs, distinct from the APT chain) →
V0.6 deb content (`dpkg-deb Architecture`, exactly one package record kind `stable`, binary digest, microvm pins +
non-`UNSET` shas) → V0.7 signer reference (signing-key fingerprint == trusted reference; §0/E6 agreement re-asserted).
Single pre-mutation verification log is a D3 exit artifact.

### 2.2 Staged index assembly (pre-publication, never live `dists/stable`)

Fetch previous live state read-only → copy verified `<V2>` debs to staging `pool/main/v/velnor-runner/` →
generate candidate `Packages` stanzas (Section `devel`, control fields from the debs, no hand edits) → merge with
retained-version stanzas (§2.3) → staged `Release` (Origin/Label `Velnor`, Suite/Codename `stable`, Components
`main`, Architectures `amd64 arm64`, MD5/SHA1/SHA256/SHA512 blocks over exactly the shipped objects) → self-check
unsigned (recompute blocks, gzip-roundtrip, stanza-vs-pool assert, exact intended version set).

### 2.3 Retention: exactly N=2, previous = `<V1>`

- Indexed + pooled versions after D3: `<V2>` (candidate) + `<V1>` (previous coherent). `<V0>` (the version `<V1>`
  retained) is evicted from index AND pool in the same publication; post-publish probe asserts its debs 404 both archs.
- `<V1>` keeps: index stanzas both archs + both pool debs + its source-record reference carried in the new
  publication record's `previous: {tag, source_record_sha256}` field (chain link to the pre-D3 live record).
- Retention is the same typed function over (candidate, previous-live); no hand list.
- Recovery-artifact rule restated: `<V1>` counts as recovery ONLY with a proven matching config/state recovery
  snapshot; otherwise the tested forward-recovery path is documented. A package downgrade alone is never claimed to
  read a newer state schema. All recovery stays APT-only.
- D3 exit artifact: previous-version recovery statement (`<V1>` identity + snapshot proof or forward-recovery pointer).

### 2.4 Signing + publication record

Sign staged `Release` with the trusted feed key (single owner, opaque secret ref): `InRelease` clearsigned
(payload byte-identical to `Release`, asserted by diff), `Release.gpg` detached, same key, creation time ==
Release `Date`. Write `publication-record.json` (`velnor.publication-record/v1`): `source_record_sha256` (B3-repeat
record digest), `tag v<V2>`, `crate_version <V2>`, `inrelease_sha256`, per-arch `packages[].sha256`,
`signer_fingerprint` (V0.7-authenticated; asserted equal to the live-sig issuer at §2.6), `previous` (pre-D3 live
record). Pre-deploy assert: record hashes == staged bytes; fingerprint == signing key; `previous` == live record.

### 2.5 Single-writer Pages deployment (unchanged shape)

Exactly one workflow path mutates the feed (generated publish workflow, hosted runners); signing/deployment one
owner each. Same-feed serialization via the feed-scoped concurrency group (`cancel-in-progress: false`) covering
only the mutate-feed critical section. No-rollback guard: candidate strictly advances the live record read at the
critical-section start (`<V2>` > live `crate_version` AND `previous.tag` == live `tag`); stale-base runs abort and
rebase. Proof artifact: deploy log (live-record-before, guard comparison, live-record-after) + stale-base negative
test (fixture feed, trusted state intact).

### 2.6 LIVE candidate verification (independent, post-deploy)

Same B4 §6 transcript against `<V2>`, from a position independent of the publisher, trusting only the separately
trusted key reference + the B3-repeat record:

```sh
# 0. Key authentication FIRST; repository-scoped Signed-By.
# 1. apt-get update — stable InRelease fetched, signature accepted, no weak/unsigned warnings.
apt-cache policy velnor-runner            # candidate == <V2> exact; <V1> still offered (retention)
apt-cache policy velnor-runner:arm64      # foreign-arch repeat (arm64 in foreign archs on probe host)
# 2. Index hash chain: APT-fetched Packages digests == Release block entries (indextargets → /var/lib/apt/lists/).
# 3. Record comparison: sha256(InRelease)==record.inrelease_sha256;
#    sha256(Packages.<arch>)==record.packages[<arch>].sha256; tag/crate == v<V2>/<V2>;
#    source_record_sha256 == B3-repeat digest; signer_fingerprint == trusted FPR == live-sig issuer;
#    previous == pre-D3 live record.
# 4. Pool hash chain per arch: download candidate .deb from live stanza; size == Size: and
#    sha256 == stanza SHA256 == B3-repeat deb digest (full-payload hash).
# 5. Origin: Release Origin/Label == Velnor, Suite stable, source pins origin + Signed-By.
# 6. Retention: <V1> in live index + pool both archs; <V0> debs 404 both archs.
```

APT metadata chain and the re-run `gh attestation verify` (distinct post-publish confirmation) recorded as
separate checks. First mismatch fails D3. Ledger entry: `velnor-runner=<V2>`, per-arch deb SHA256, tag/commit,
B3-repeat record digest, publication-record digest, trusted signer fingerprint.

## 3. C1 locked-path upgrade transaction

The infrastructure subagent owns bastion writes in this window. D3 runs the C1 transaction with `VERSION=<V2>`
— an upgrade, not a first install — so drain and state preservation are non-trivial (unlike C1's idle no-op).

### 3.1 Pre-upgrade checks (fingerprint → origin, same order as C1/B4)

1. **Fingerprint FIRST:** authenticate the feed public-key fingerprint against the separately trusted project
   reference (record value; must equal the §2.6 ledger fingerprint); repository-scoped `Signed-By` for
   `https://velnor-apt.tailrocks.com/` already configured from C1 — verify the keyring still holds ONLY that key
   and the source entry still pins origin + `Signed-By` (no globally-trusted key, no drift).
2. **Metadata:** `apt-get update` — signed InRelease/Release chain accepted under `Signed-By`, no weak/unsigned warnings.
3. **Version:** `apt-cache policy velnor-runner` — candidate == `<V2>` exact (no suffix drift); installed == `<V1>`.
4. **Arch:** candidate arch matches host (`dpkg --print-architecture`); foreign-arch stanza present for the record.
5. **Hash:** APT-fetched `Packages` digests == Release block entries; candidate stanza SHA256 == B3-repeat deb digest.
6. **Source/record:** live publication record asserts `tag v<V2>`, `source_record_sha256` == B3-repeat digest,
   `previous.tag` == `v<V1>`; APT chain and attestation recorded as DISTINCT proofs (re-run
   `gh attestation verify` on the B3-repeat subjects as the attestation leg).
7. **Origin:** Release Origin/Label `Velnor`, Suite `stable`.

### 3.2 Drain + config/secret preservation (general form — host is provisioned now)

1. Stop new admission controller-side (no new reservations on either lane).
2. Let running jobs finish within deadlines; cancel only per queue contract (same-PR supersession may cancel
   same-PR older attempts; never siblings/unrelated). Native lane + any Scale Set lane both drain.
3. Export job logs/artifacts OUT of disposable containers first.
4. Remove ONLY owned inactive resources (recorded ownership identity; active leases protected; never host-wide prune,
   never `docker system prune`).
5. Verify idle: `docker ps` empty of job containers; permits released exactly once (ledger converges to 0 occupied);
   `df`/`df -i` healthy.
6. Snapshot/export: `/etc/velnor` config, `/var/lib/velnor` durable state + `permit-ledger.db`, secrets inventory
   (presence + permissions, never values). Config/secrets are preserved across the upgrade, never reset to defaults.
7. Re-run the C1 P0 read-first spot checks that guard destructive ops (SSH session held throughout; `nvme1n1` still
   pristine: no mount, `blkid` empty; no reboot without an operator window).

### 3.3 The locked transaction (exact, `VERSION=<V2>`)

```sh
VERSION=<V2>   # the §2.6-verified candidate, exact
install -d -m 0750 /run/velnor
apt-get update
apt-cache policy velnor-runner
/usr/bin/flock --exclusive --nonblock --no-fork /run/velnor/package-transaction.lock \
  apt-get install "velnor-runner=${VERSION}"
dpkg-query -W velnor-runner   # expect: velnor-runner <V2> exact
```

Never `dpkg -i`, `apt install ./file.deb`, copied executables, disabled signature verification, or altered packaged
files. `flock --nonblock` failing means another transaction holds the lock — abort and re-qualify, never force.
postinst behavior under upgrade is load-bearing: it must delete the stale `10-host-cpu.conf` if present and prove
effective quota absence, and must NOT write any quota drop-in (spec §4.3; asserted again at §5).

### 3.4 Verify-installed + package-derived activation

1. Installed identity: compatible package/manifest/binary record — packaged `/usr/bin/velnor-runner` digest ==
   B3-repeat `binary_sha256`; embedded package record kind `stable`, version `<V2>`; `manifest.json` hash ==
   record `manifest_sha256`.
2. `release verify-installed` BEFORE starting services. Never assume install starts the fleet.
3. Derive activation, drain, and health commands from the implemented package/help (`velnor-runner --help`,
   shipped units, package docs) and verify each; never invent `velnorctl` mutation verbs. D1's new Scale Set /
   allocator surface is activated only through these package-derived commands.
4. Start services per the derived activation; prove post-install health (§4) + idempotent re-run (repeat the
   transaction: already-`<V2>` converges with no changes, no error).

## 4. Post-upgrade checks: identity + image pins + restart health

1. **Package/config/state identity:** `dpkg-query -W` == `<V2>`; config in `/etc/velnor` preserved (diff against
   the §3.2 snapshot — only intended migration deltas); `/var/lib/velnor` state + `permit-ledger.db` intact and
   readable by the new binary (state-schema advance is forward-only; a downgrade claim would need the §2.3 snapshot
   proof, which is not exercised here); no packaged file altered (debsums/verify-installed re-run green).
2. **Image pins:** job/toolchain image digests still the pinned values (no `latest` anywhere); official runner image
   + per-worker DinD pins resolve to the D1 recorded digests; runner version separately recorded; actual tool
   content spot-verified. Any pin move vs the D1 record is a finding, not a silent accept.
3. **Restart/activation health:** services active (`systemctl is-active` on the shipped units); authenticated fresh
   outbound health records bound to repo/source/run/attempt/provider/sequence/freshness/permits/progress (spec §8);
   `max_jobs=N` ledger reconciled before capacity is re-advertised (reconcile-before-advertise; generation-fenced;
   no permits lost or double-counted across the restart); Docker/cgroup re-verify per C1 §5
   (`cgroup2fs`, `systemd 2 overlayfs`, data-root `/var/lib/docker`, Buildx/Compose versions, holds still in place).
4. **Managed paths + NVMe:** `/etc/velnor`, `/var/lib/velnor`, `/run/velnor` (lock file present, no stale holder),
   `/var/cache/velnor` exist with modes; `nvme1n1` untouched assertions pass post-upgrade.
5. **One canary real job** (native lane) before reopening admission: record URL, engine identity, occupancy ledger
   entries, cleanup receipt — then reopen admission.

## 5. FULL C2 quota-free re-inspection bundle (after the upgrade)

Work-plan D3 action 3 + C2 action 6: quota-free proof is repeated for EVERY package upgrade. The whole C2 bundle
(C2 actions 2–4), not a spot check, executed against real post-upgrade job containers:

### 5.1 HostConfig inspection (real job containers)

- `docker inspect --format '{{.HostConfig}}'` on every real job container in the inspection wave: no `NanoCpus`,
  no cpu quotas/periods, no cpuset CPUs/Mems ceilings, no memory ceilings (`Memory`, `MemoryReservation`,
  `MemorySwap`), no PIDs/Blkio ceilings. Inspect inherited limits, not only emitted flags.
- Cover both lanes where jobs ran: native containers AND official-worker inner containers (Scale Set lane);
  nested-create descendants included (L4 decision: resource fields stripped at the lease gate).
- Effective build env of the same jobs: no injected `CARGO_BUILD_JOBS`/`MAKEFLAGS`/`MBX_SCHEDULER_*`/
  `VELNOR_JOB_BUDGET`, no BuildKit sizing flags, no Gradle/heap partitions. Workflow's own spellings pass through.

### 5.2 Effective cgroup ancestry

- For each inspected container, walk the cgroup ancestry to the host: `cpu.max`, `memory.max`, `memory.high`,
  `memory.swap.max`, `cpuset.cpus(.effective)`, `pids.max` — all effectively `max`/unset on the workload ancestry.
- Hidden-ancestor rule: any ceiling found above the container and below the host root fails the bundle (E2 fault row
  "hidden ancestor limits" applies here too). Slice check: `velnor-jobs.slice` loaded, identity-only —
  `CPUQuotaPerSecUSec`/`MemoryMax`/`MemoryHigh` all `infinity`; no `10-host-cpu.conf` or any quota drop-in present.

### 5.3 Units / drop-ins

- `systemctl cat` over every shipped Velnor unit (daemon, workers, Scale Set provider, socket/path units incl. all
  D1/D2-new units): no `CPUQuota*`, `MemoryMax`, `MemoryHigh`, `MemorySwapMax`, `TasksMax`, `AllowedCPUs`/
  `AllowedMemoryNodes` on workload units; no quota drop-ins in `/etc/systemd/system/*.d/` or `/run/systemd/system/*.d/`.
- `velnor-control.slice` weights/TasksMax remain the only allowed control-plane tuning (C2 boundary, unchanged).
- postinst/postrm re-audit on the installed `<V2>` package: postinst provably cannot recreate quota drop-ins on
  upgrade (spec §4.3); `jobs_slice`/`node_arch` absence tests green on the shipped tree.

### 5.4 No-raw-socket proof

- `docker inspect` mount lists of every job container: no bastion `/var/run/docker.sock`, no alternate host socket,
  no unrestricted host Docker proxy bind. Native jobs use only the mediated lease API; Scale Set workers use only
  their private per-worker DinD socket (no public TCP API — §5.5 of D1 restated at the host level).
- No `docker` group membership for job identities; Velnor control sockets under `/run/velnor` owner-only.

### 5.5 Health + smoke + ledger (closes the bundle)

- Ready health: authenticated outbound records bound to repo/source/run/attempt/provider/sequence/freshness/permits/
  progress (spec §8) green after the upgrade.
- Real native smoke job post-upgrade (may be the §4 canary if it ran the full inspection wave): URL, engine
  identity, occupancy ledger entries (reserved → running → released exactly once), cleanup receipt.
- Occupancy/cleanup ledger extract covering the inspection wave: total occupied ≤ N monotonically, no leaked permits.

## 6. D3 gates + evidence + aborts

Entry gates: §0 E1–E6 all green. Exit gates (D3 done per checklist):

- X1 New coherent release: §1.7 bundle complete via generated hosted CI (exact tag/commit/version, amd64+arm64 +
  manifests/record/checksums, OCI/payload identity incl. native arm64, attestations, negatives, sweep, KVM proof).
- X2 Signed feed update: V0 log, staged-tree self-check, retention/recovery statement (`<V2>`+`<V1>`, `<V0>` evicted),
  signing + record-consistency log, deploy log with no-rollback guard proof, LIVE verification transcript (§2.6),
  ledger identity entry — APT chain and attestation as distinct checks.
- X3 Locked upgrade: §3 transaction log, `dpkg-query` identity `<V2>`, binary/record/manifest proof,
  verify-installed proof, package-derived activation + idempotent re-run proof, drain + preservation receipts.
- X4 Post-upgrade: §4 identity/pins/restart-health proofs + canary job URL.
- X5 Repeated quota-free: FULL §5 bundle (HostConfig, cgroup ancestry, units/drop-ins, build env, no-raw-socket,
  health + smoke + ledger). A spot check does not satisfy X5.

Evidence bundle (verifier: release/package verifier + APT supply-chain verifier + C-phase host verifier): §1.7
release bundle, §2 V0/staging/retention/signing/deploy/live logs, §3 transaction + preservation logs, §4 health
transcript, §5 inspection bundle, ledger entries (new release identity, feed identity, installed identity,
signer fingerprint). Every step requires evidence reviewed by a verifier other than its author.

Abort conditions: any V0 failure; any §2.6 mismatch; stale-base guard trip; signer-fingerprint mismatch anywhere;
hand YAML anywhere in the release/publish path; `flock` contention (never force); `verify-installed` failure
(services stay down, forward-recovery per §2.3 — never a blind downgrade); any §5 quota/socket/ceiling finding
(blocks admission until root-caused); partial-state detection at any boundary (pool/index/record/Pages, or
package/config/state on the host, out of agreement post-run).
