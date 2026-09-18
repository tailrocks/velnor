# B4 PREP — signed-feed publication procedure (authored, NOT executed)

Date: 2026-09-17. Status: READ-ONLY prep. B4 gate not reached; zero feed writes.
Scope: bastion campaign. No mutation of `tailrocks/velnor-apt`, no Pages deploy,
no signing operation performed here.
Inputs: `/tmp/apt-feed-probe.md` (live feed state, read-only probe),
spec §7 pipeline, work-plan STEP B4 + checklist item B4,
`/tmp/a0-consumers.md` §1 + `/tmp/b2-prep.md` (velnor-apt layout/baseline),
`/tmp/b3-prep.md` (bootstrap release procedure, B4's upstream).

## 0. Frozen inputs (re-verify read-only at B4 start)

### 0.1 Live feed baseline (from probe, 2026-09-17 ~01:12 UTC)

- Feed root `https://velnor-apt.tailrocks.com/` (GitHub Pages, `server: GitHub.com`).
- Single suite **`stable`**; all other suites 404.
- Live objects: `dists/stable/{InRelease,Release,Release.gpg}`,
  `dists/stable/main/binary-{amd64,arm64}/{Packages,Packages.gz}`.
- Release Date `Mon, 14 Sep 2026 14:52:13 +0000`; Origin/Label `Velnor`;
  Suite/Codename `stable`; Components `main`; Archs `amd64 arm64`.
- Index holds exactly two versions, both archs: previous **`0.1.273`** +
  candidate **`0.1.274`** (per-arch filenames/sizes/SHA256 in probe §5).
- All 4 pool `.deb`s HTTP 200, `Content-Length` == `Size:`; 0.1.274 magic
  `!<arch>`; `0.1.272` and `0.1.275` 404 both archs (retention = exactly
  the two indexed versions).
- `publication-record.json` at feed root: schema
  `velnor.publication-record/v1`, tag `v0.1.274`, crate `0.1.274`,
  `inrelease_sha256` + both `packages[].sha256` MATCH live metadata;
  `previous: {v0.1.273, source_record_sha256 e6497cdd…}`.
- Signatures: InRelease payload byte-identical to Release; both sigs same
  key `857FCD279679A34B` (issuer fp `CD4693750A4BA4F12BC9ABFD857FCD279679A34B`),
  created = Release Date. No public key published on feed (expected per
  procedure; crypto validity unverified from feed alone).

### 0.2 velnor-apt repo baseline (from A0 §1 / B2 prep)

- Live `main` = `d62820d4`; 33-entry tree; `.github/workflows/` =
  `ci-unit-docs.yml` only; omission notice blob `3172bb88…` intact.
- B4 runs only after B2's atomic promotion landed the generated APT
  coverage (publish path, APT CI, single-writer Pages deploy, typed
  verifier contract). If B2 has not landed, B4 aborts.

### 0.3 Upstream release (from B3)

- B3 exit bundle: exact tag `v<VERSION>` / commit / crate version,
  coherent amd64+arm64 products + manifests/record/checksums, OCI/payload
  identity incl. native arm64, trusted attestations, negative-coherence
  logs, prereq sweep, KVM-separation proof.
- B4 consumes the B3 **release record** (never `releases/latest`).

### 0.4 Known flags that MUST clear before B4 executes

1. **Signer-fingerprint discrepancy** (probe §7/§9.1): publication record
   claims `7E66…B801`, live sigs show issuer `CD46…A34B`. Resolve against
   the separately trusted key reference (confirm or rule out
   primary/subkey relation). Do not publish or C1-install until reconciled.
2. Full `.deb` payload hashes were NOT verified in the probe (sizes + magic
   only); B4 §1 re-verifies every payload byte against the B3 source record.
3. No-rollback / same-feed serialization / pre-mutation verification are
   process properties — this procedure defines them; the static probe
   cannot prove them.

## 1. Gate V0 — independent pre-mutation verification (BEFORE any feed mutation)

Runs in the generated publish workflow, on hosted runners, against the B3
release artifacts fetched by exact identity. ANY failure blocks publication
with **zero partial state**: no pool upload, no index change, no signature,
no record write, no Pages deploy. Independence = re-derived from B3 source
artifacts + separately trusted references, never trusting B4's own staged
outputs or the live feed's claims about the candidate.

V0.1 Source/tag identity:
- tag `v<VERSION>` resolves to the B3 commit; crate version == Debian
  version == tag-minus-`v`; repository anchor `tailrocks/velnor`.
V0.2 Digest verification:
- recompute SHA256 of every fetched subject (2 tarballs, 2 debs,
  `manifest.json`, `release-record.json`); each matches its sidecar and
  the `SHA256SUMS` strict check; record is canonical bytes matching its
  `.sha256` sidecar.
V0.3 Manifest verification:
- `manifest_version` == current `MANIFEST_VERSION`; `sha256(manifest.json)`
  == `record.build.manifest_sha256`; embedded `source_sha` == record commit.
V0.4 OCI verification:
- image ref `ghcr.io/tailrocks/velnor-job-ubuntu:{VERSION}` resolves to the
  record's `oci_index_digest`; per-arch platform digests pinned; labels
  (version/revision/source/manifest-hash) match the record.
V0.5 Provenance verification:
- `gh attestation verify` green for both tarballs (build signer) and both
  debs (package-signer workflow signer, admitted refs only). APT-chain and
  attestation are recorded as DISTINCT checks (spec §7 ¶"distinct checks").
V0.6 Deb-content verification:
- per arch: `dpkg-deb Architecture` == matrix arch; exactly one embedded
  `/usr/share/velnor/package-record.json` (schema
  `velnor.package-record/v1`, kind `stable`); packaged
  `/usr/bin/velnor-runner` digest == record `binary_sha256`; microvm stage
  pins + non-`UNSET` kernel/rootfs shas hold.
V0.7 Signer-reference check:
- the trusted signing key for the feed is loaded from its opaque secret
  ref; its fingerprint is authenticated against the separately trusted
  project reference (same rule as C1's key authentication). The §0.4.1
  discrepancy must already be reconciled — V0.7 re-asserts
  record-fingerprint == signing-key fingerprint == trusted reference.

V0 evidence: single pre-mutation verification log (digests recomputed,
attestation outputs, deb-guard outputs, fingerprint comparison). The log is
a B4 exit artifact; without it B4 is not done.

## 2. Staged APT index assembly (still pre-publication)

Staging area: ephemeral workflow workspace (or a PR-scoped preview prefix),
never the live `dists/stable` tree. Steps:

1. Fetch the previous live state read-only: current `Release`, per-arch
   `Packages`, pool presence for the retained version(s).
2. Copy the verified candidate debs into the staging `pool/main/v/velnor-runner/`
   layout: `velnor-runner_<VERSION>_<arch>.deb` per arch (exact B3 bytes).
3. Generate per-arch `Packages` stanzas for the candidate (Section `devel`,
   Maintainer, Depends, Recommends per B3 deb control — no hand edits) and
   merge with the retained-version stanzas (§3) into staged
   `dists/stable/main/binary-{amd64,arm64}/Packages` (+ `.gz`).
4. Generate staged `Release` (Origin `Velnor`, Label `Velnor`, Suite/Codename
   `stable`, Components `main`, Architectures `amd64 arm64`) with
   MD5/SHA1/SHA256/SHA512 blocks over exactly the shipped index objects.
5. Self-check the staged tree WITHOUT signing: recompute every checksum
   block, gzip-roundtrip both `Packages.gz`, assert stanza fields
   (Filename/Size/SHA256) match the staged pool bytes, assert exactly the
   intended version set is indexed (candidate + retained previous, §3).

## 3. Previous-version retention scheme

- Retain exactly **N=2 versions** in index + pool: the new candidate plus
  the previous coherent version (today: `0.1.274` + `0.1.273`; after B4:
  `<VERSION>` + `0.1.274`). Matches observed live retention (probe §6).
- Retention is a typed function over (candidate, previous-live), not a
  hand-maintained list; legacy retention strings are gone (B1 bar).
- Retained version keeps: index stanzas both archs + both pool debs +
  its source-record reference carried in the new publication record's
  `previous: {tag, source_record_sha256}` field.
- Evicted versions (older than previous) are removed from index AND pool
  in the same publication; post-publish probe asserts evicted debs 404
  (as `0.1.272` 404s today).
- Recovery-artifact rule (spec §7 ¶"Breaking schema changes"): a retained
  previous version counts as a recovery artifact ONLY with a proven
  matching config/state recovery snapshot; otherwise the tested
  forward-recovery path is documented. A package downgrade alone is never
  claimed to read a newer state schema. All recovery stays APT-only.
- B4 exit artifact: previous-version recovery statement (retained version
  identity + snapshot proof or forward-recovery doc pointer).

## 4. InRelease/Release signing + publication record

1. Sign the staged `Release` with the trusted feed key (one owner — the
   generated publish workflow; secret via opaque ref, never in config):
   - `InRelease`: clearsigned `Release` (payload MUST be byte-identical
     to `Release`; assert by extracting clearsign body and diffing).
   - `Release.gpg`: detached signature of `Release`.
   - Both signatures same key, creation time == Release `Date`.
2. Write `publication-record.json` (schema `velnor.publication-record/v1`):
   - `source_record_sha256`: digest of the B3 release record consumed.
   - `tag`, `crate_version` for the candidate.
   - `inrelease_sha256`: digest of the signed `InRelease`.
   - `packages[]`: per-arch SHA256 of the staged `Packages` files.
   - `signer_fingerprint`: fingerprint of the key that made the §4.1
     signatures, authenticated per V0.7 (this field is what §0.4.1 caught
     disagreeing — B4 asserts it equals the live-sig issuer at §6).
   - `previous: {tag, source_record_sha256}` chaining to the prior record.
3. Pre-deploy consistency assert: record hashes == staged bytes; signer
   fingerprint == signing-key fingerprint; `previous` matches the live
   feed's current record (no-rollback input, §5.3).

## 5. Generated hosted single-writer Pages deployment

### 5.1 Single writer

- Exactly one workflow path may mutate the feed: the generated publish
  workflow in the B2-promoted velnor-apt tree, running on hosted runners
  (recovery/publisher path — broken local Velnor must not block the
  package needed to repair it, spec §7). No hand workflow, no local
  `gh-pages` push, no second publisher.
- Signing and deployment have one owner each (spec §7: "Verification may
  run multiple ways; signing/deployment has one owner").

### 5.2 Same-feed serialization (not all-repo CI)

- Feed/tree mutation is serialized by a concurrency group scoped to the
  feed deployment (e.g. `concurrency: group: apt-feed-stable`,
  `cancel-in-progress: false`), so two publications can never interleave.
- The group covers ONLY the mutate-feed critical section (pool upload +
  index swap + record write + Pages deploy), never the whole repo CI:
  docs/unit CI keeps running concurrently.

### 5.3 No-rollback guard + proof

- Guard: the publish job reads the live `publication-record.json` at the
  start of the critical section and asserts the candidate strictly
  advances it (candidate version > live `crate_version` AND candidate
  `previous.tag` == live `tag`). An older publication (stale run, replay,
  or misordered queue) fails the guard instead of overwriting a newer
  candidate.
- Publication is last-publish-wins ONLY in the forward direction: a run
  whose base record is not the live record aborts (rebase onto the new
  live state, re-run V0 against the still-current candidate).
- Proof artifact: the deploy log showing live-record-before, guard
  comparison, and live-record-after; plus a negative test (stale-base run
  against a fixture feed refuses to publish, trusted fixture state
  intact — B1 negative-suite class, re-run at B4).

## 6. LIVE candidate verification procedure (independent, post-deploy)

Runs AFTER the Pages deploy has propagated, from a host/network position
independent of the publisher (separate job/step that trusts nothing but
the separately trusted key reference + the B3 record). Concretely:

```sh
# 0. Key authentication FIRST (spec §7): fingerprint from a separately
#    trusted project reference, never blind-trust of a download-URL key.
#    Configure repository-scoped Signed-By (APT source scoped to the feed).
#    <FPR> = trusted fingerprint; <KEYFILE> = keyring holding ONLY that key.

# 1. Refresh metadata through APT's own chain.
apt-get update
#    expect: stable InRelease fetched, signature accepted under Signed-By,
#    no warnings about weak digests or unsigned index.

# 2. Candidate visibility + exact version, both archs.
apt-cache policy velnor-runner
#    expect: candidate == <VERSION> (exact, no suffix drift);
#    installed/previous version still offered (retention, §3).
#    Repeat for the foreign arch, e.g.:
#      dpkg --print-foreign-architectures  # must include arm64 on the probe host
#      apt-cache policy velnor-runner:arm64

# 3. Index hash chain: APT-fetched Packages digests == Release block entries.
#    (apt-get indextargets → files under /var/lib/apt/lists/; sha256sum each.)

# 4. Record comparison: fetch live publication-record.json; assert
#      sha256(live InRelease)        == record.inrelease_sha256
#      sha256(live Packages.<arch>)  == record.packages[<arch>].sha256
#      record.tag / crate_version    == v<VERSION> / <VERSION>
#      record.source_record_sha256   == B3 release-record digest (V0.2 input)
#      record.signer_fingerprint     == trusted <FPR> == live-sig issuer fp
#      record.previous               == pre-publish live record (chain link)

# 5. Pool hash chain: for each arch, download the candidate .deb URL from the
#    live Packages stanza; assert size == Size: and
#    sha256(downloaded .deb) == stanza SHA256 == B3 record deb digest.
#    (Full-payload hash — closes probe gap §0.4.2.)

# 6. Origin check: Release Origin/Label == Velnor; Suite stable; the APT
#    source entry pins origin + Signed-By (no globally-trusted key).

# 7. Retention check: previous version still in live index + pool both
#    archs; evicted versions (older than previous) 404.
```

Every comparison is an exact-equality assert; the first mismatch fails B4.
APT's metadata signature/checksum chain (§6 steps 1–3, 6) and the artifact
attestation re-check (re-run `gh attestation verify` on the B3 subjects as
a distinct post-publish confirmation) are recorded as separate checks.

## 7. B4 gates

Entry gates (ALL green before the publish workflow is dispatched):
- E1 B3 exit bundle complete (exact tag/commit/version, coherent products,
  OCI/payload identity, attestations, negatives, prereq sweep, KVM proof).
- E2 B2 promotion landed: feed mutations flow only through the generated
  single-writer publish workflow; omission notice gone per B2 §3.
- E3 §0.4.1 signer-fingerprint discrepancy reconciled against the trusted
  reference; trusted fingerprint value recorded in the ledger.
- E4 No live-feed drift unaccounted: re-probe live record + index versions
  read-only; base for §5.3 guard is the current live state.

Exit gates (B4 done per checklist):
- X1 V0 pre-mutation verification log green (§1), zero partial state on
  any failure path (negatives: bad digest / bad source / bad manifest /
  bad OCI / missing attestation each block pre-mutation).
- X2 Staged index + retained previous version + signed InRelease/Release
  + publication record, all mutually consistent (§2–§4).
- X3 Single-writer Pages deployment with same-feed serialization (§5.1–5.2).
- X4 No-rollback proof: guard comparison log + stale-base negative test (§5.3).
- X5 LIVE candidate independently verified per §6 (signed chain, exact
  version, both archs, hashes, source/record, origin) + attestation as a
  distinct check.
- X6 Signer fingerprint + exact candidate identity recorded in the ledger:
  `velnor-runner=<VERSION>`, per-arch deb SHA256, tag/commit, B3 record
  digest, publication-record digest, trusted signer fingerprint.

Evidence bundle (verifier: APT supply-chain verifier): V0 log (§1),
staged-tree self-check log (§2), retention/recovery statement (§3),
signing + record-consistency log (§4), deploy log with guard proof (§5),
live-verification transcript (§6: `apt-get update`, `apt-cache policy`
both archs, all hash/record comparisons), exact candidate identity + signer
fingerprint ledger entry (§7/X6).

Abort conditions: any V0 failure; any §6 mismatch; stale-base guard trip;
signer-fingerprint mismatch anywhere; B2-promoted generated path absent
(any hand YAML in the publish path fails B4 outright); partial-state
detection (pool/index/record/Pages out of agreement post-run).
