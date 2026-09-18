# B1 design: generic typed APT primitives (spec §7)

Read-only audit. No repo edits. Sources: `plans/bastion-three-provider-ci/spec.md`
§7 + work-plan B1, `crates/velnor-workflow/src/primitives/release.rs`,
`crates/velnor-workflow/src/runtime.rs` (`verify-feed`/`update-feed`),
`crates/velnor-runner/src/release.rs`, `../velnor-apt/scripts/verify-release.sh`
(974 lines) + `test-verify-release.sh` (971 lines) + `package-update.sh` +
`publication-previous.jq`.

## 1. Existing-code inventory

### 1a. Generator dispatch (workflow `release.rs`)

- `ReleaseSpec` has `kind/package/packages/binary/targets/image/
  source_repository/consumer_repository/artifact_path/description/
  manifest_schema`. `declared_spec` accepts `kind = "apt"`.
- `release_contract_complete`: apt = `package` + `consumer_repository` non-empty.
- `render_release` dispatches `"apt" → render_apt_release → render_package_feed`,
  a thin workflow: schedule + `workflow_dispatch(runner, channel)` inputs,
  `admit-runner` (rejects velnor-only mutation), `verify` job calling
  `velnor-workflow release verify-feed --kind apt --package … --coordinate …`,
  `mutate` job (default-branch, schedule/dispatch, non-velnor, `package-feed`
  environment) calling `… release update-feed … --channel "$CHANNEL"`.
- `deb_architectures` maps `x86_64/aarch64-unknown-linux-gnu → amd64/arm64`,
  fails closed (`None`) on anything else. `render_pages_release` exists but is
  docs-only (Bun build + Pages deploy), not an APT deployment path.

### 1b. Feed runtime (`runtime.rs`) — the gap

- `verify_feed`: validates package name; for apt requires
  `conf/distributions` or `debian/` to exist. Nothing else.
- `update_feed`: re-runs `verify_feed`, checks `channel ∈ {stable, preview}`,
  then prints `verified; mutation is GitHub-writer only` and exits 0.
  **It mutates nothing.** This is exactly the docs-only feed spec §7 forbids
  (`apt-repository` must produce a real feed). B1's core build-new surface.

### 1c. Runner coherence model (`runner/release.rs`, 2157 lines) — reuse core

Already typed and tested: `ReleaseRecord/PackageRecord/PublicationRecord/
PreviousPointer/ExpectedAptPublicationMetadata/ActualAptPublicationMetadata`,
`ReleaseRecord::verify`, `PackageRecord::verify`,
`verify_apt_publication_metadata` (expected-vs-served claims + publication
binding, self-row rejection, signer binding), `assemble`, `emit_record`,
`CoherenceError` taxonomy (redacted diagnostics), CLI
`emit/assemble/verify-record/verify-installed/activate/rollback/export`.
Hard anchors: `SOURCE_REPOSITORY = tailrocks/velnor`, `REQUIRED_ARCHES =
{amd64, arm64}`, arch→target triples, preview `~preview.N+<7hex>` grammar
(`is_preview_debian_version`), canonical JSON + sidecar digest discipline.
Explicit non-goal stated in-module: it verifies **typed claims**, never raw
APT bytes, served bytes, or GPG. The trusted byte/signature verifier that
produces those claims **does not exist in Rust** — that is the hole the
typed verifier contract ( §3) fills.

### 1d. `verify-release.sh` — behavioral oracle, not the implementation

Four subcommands × `--suite stable|preview` (default stable):
`resolve-commit` (independent tag→commit via `git ls-remote` against
`$SOURCE_GIT`, `^{}` deref first; preview refuses — caller supplies commit),
`download` (coherence inputs ONLY via `gh release download` with exact
`--pattern` allowlists; stable: record+sidecar, manifest+sidecar, 2 debs+
sidecars; preview: release-manifest.json, SHA256SUMS, 2 dotted-name debs+
sidecars — `~`→`.` GitHub rewrite), `verify` (fully offline: checksums,
schema/source/tag/version/commit, manifest binding, record-internal OCI
coherence + optional live `--verify-oci` via buildx imagetools, per-arch
deb sidecar/record/bundled-identity/extracted-binary checks, signer
fingerprint equality; arms `.reprepro-ok` sentinel), `publish` (needs
sentinel + `apt-ftparchive`/`dpkg-deb`/`gpg`; stable wipes and rebuilds
`./public` with deterministic 4-deb pool; preview appends second stanza,
never wipes; `--bootstrap` only for never-published preview, 2-deb pool,
null previous; signs `Release/Release.gpg/InRelease`, emits
`publication-record[.json|-preview.json](.sig)` + `last-publish[-preview]`).
Caller-owned (NOT in script): suite-existence probe (`dists/preview/
InRelease`), `--prev-dir` rollback-pair recovery, `--previous-pointer`
derivation (`publication-previous.jq`), preview attestation gating
(`gh attestation verify --deny-self-hosted-runners`), Pages deploy.
`package-update.sh` (+ `test-package-update.sh`) owns channel state files
(`package-state[-preview].json`, `velnor.apt-package-state.v1`).
**No live caller workflow exists** in `../velnor` or `../velnor-apt`
(only `ci-unit-docs.yml` unit/docs) — the generated workflow B1 builds
*is* the caller. The parenthetical "fetch via gh api" matches the script's
`gh release download` + `git ls-remote` fetch pattern (public-repo reads,
no privileged credential).

### 1e. `test-verify-release.sh` — ~50-case negative-test oracle

Stable: tampered record/deb, commit disagreement, missing record, extra deb,
manifest-hash mismatch, signer mismatch, OCI ref/label mismatch, packaged
identity disagreement, extracted-binary mismatch, publish-without-passphrase.
Preview: missing commit, suffix≠commit, grammar violations, manifest
version/source_ref/repo/commit mismatches, sidecar tamper/misname, extra
deb, build-identity sha/crate mismatches, control-arch mismatch, SHA256SUMS
line count, candidate ≤ rollback (dpkg order), bad previous pointer,
missing sentinel, bootstrap-over-existing-pool, bootstrap+prev-dir,
bootstrap+stable, non-null bootstrap pointer. Plus idempotency/retry
preservation positives. All portable to typed-verifier tests.

## 2. Per-capability verdict

| # | Spec §7 capability | Verdict | Basis |
|---|---|---|---|
| 1 | Source repo + exact release identity | **extend** | `ReleaseSpec.source_repository` reused; build new typed `ReleaseIdentity` (stable: `vX.Y.Z` tag + independently resolved 40-hex commit; preview: caller-supplied 40-hex commit + `preview` tag const). `resolve-commit` logic becomes a typed fetch, never config-controlled. |
| 2 | Package | **reuse-as-is** | `ReleaseSpec.package` + `valid_package` + `release_contract_complete`. |
| 3 | Architecture set | **reuse-as-is** (runner) / **extend** (workflow) | Runner `Arch/REQUIRED_ARCHES`/triples reused. Workflow needs a typed `arches` field defaulting to both; `deb_architectures` fail-closed mapping reused. |
| 4 | Stable/preview suites | **extend** | `channel ∈ {stable,preview}` input + dispatch choice reused. Build new: per-suite grammars (stable `vX.Y.Z` + record flow; preview `X.Y.Z~preview.N+<7hex>` + manifest flow, dotted-asset normalization), suite-scoped paths (`dists/<suite>`, `pool/<suite>`), bootstrap-vs-strict mode selection. One final implementation, no compat wrappers. |
| 5 | Signer fingerprint + secret refs | **build-new** | Nothing exists. New typed `signer_fingerprint` (full-fpr validation, cf. runner `is_full_fingerprint`) + opaque `secret_ref` type (passphrase/key names resolved only from the `package-feed` environment, never from config values). `Signed-By` wiring + pre-install key authentication per §7 pipeline. |
| 6 | Release-coherence verification | **reuse-as-is** (claim checks) / **build-new** (byte/signature boundary) | Runner `verify*` + `CoherenceError` reused untouched. Build new the trusted boundary the runner explicitly delegates: fetch → hash → GPG-verify → construct `Expected/ActualAptPublicationMetadata` claims. Script `verify` logic is the line-by-line spec; port to Rust in `velnor-workflow` runtime (or a shared crate), script retained as oracle until parity, then removed (no shims). |
| 7 | Repository assembly | **build-new** (typed publisher) | Runner `assemble` (record assembly) reused. New: typed repo assembly — deterministic pool staging (canonical `pool/…/<pkg>_<ver>_<arch>.deb` naming, collision-bytes check), per-arch `Packages` via `apt-ftparchive`, `Release` generation with pinned Origin/Label/Suite/Codename/Architectures/Components, `Release.gpg`+`InRelease` signing. Writes only into a staging dir; live tree untouched. |
| 8 | Publication records | **reuse-as-is** / **extend** | `PublicationRecord/PreviousPointer` schemas + `verify_publication_binds` reused. Extend with the preview record shape (`suite:"preview"`, `tag:"preview"`, manifest-pin `source_record_sha256`, `"preview"`-or-null `previous`) as a final typed variant, not a compat branch. |
| 9 | Previous-version retention | **build-new** | `publication-previous.jq` rules become a typed function: candidate+rollback exactly-2-versions-per-arch index check, cross-arch rollback equality, pointer↔retained-version agreement, legacy `v0.1.121`-string elimination (final schema only — breaking change allowed). Retention count is a typed int, default 1. |
| 10 | GitHub Pages artifact/deployment | **extend** | `render_pages_release` job shapes (artifact upload, `github-pages` environment, `pages:write`+`id-token:write`) reused; build new the APT single-writer deploy job (serialized feed/tree mutation, staged-tree upload only, no-rollback guard: older publication must not clobber newer — check `last-publish` before deploy). |
| 11 | Channel-update tasks | **extend**, only where required | `package-update.sh` state-file logic becomes one typed `channel-update` task emitting `velnor.apt-package-state.v1`; keep only if the feed workflow needs it (state file vs. derived-from-index decision is B1's to make; if the index is the state, delete the task — no shim). |

Cross-cutting: renamed-fixture proof (renamed package/repo/origin must flow
through with zero name-keyed branches — audit: `velnor-runner` literals in
runner `apt_deb_path` and script asset names must become parameters);
`apt-repository` always renders the real feed workflow, never the omission
comment; config surface gains typed fields only ( §3).

## 3. Narrow typed verifier contract (reusing `verify-release.sh` logic)

No shell/YAML executes from config. The generator renders **fixed command
lines**; config contributes only validated scalars passed as flags.

```text
apt.verify:   # pure offline check, mirrors script `verify`
  inputs:  { source_repo: "owner/name" (anchored const per product),
             suite: stable|preview,
             version: <suite grammar>,
             commit: 40-hex (resolved upstream for stable; caller-supplied preview),
             incoming_dir: <fetched coherence inputs>,
             live_signer_fpr: <from keyring>, pinned_signer_fpr: <typed config> }
  effects: read-only + write `<incoming>/.reprepro-ok` sentinel on success
  rejects (before any mutation): every case in §4a; exit non-zero, no sentinel

apt.fetch:    # mirrors `resolve-commit` + `download`; fixed argv, no config exec
  stable:  git ls-remote $SOURCE_GIT refs/tags/<vX.Y.Z>{,^{}} → commit
           gh release download <tag> --repo <src> --dir <d> --pattern <allowlist>
  preview: gh release download preview … (dotted-asset allowlist); commit from caller
  rule: asset patterns are code constants parameterized ONLY by (package, version,
        arch); config can never add a pattern, URL, ref, or command.

apt.publish:  # mirrors `publish`; needs sentinel + secrets from environment only
  inputs:  { …, prev_dir | bootstrap, previous_pointer (typed fn, §2.9),
             signer_fpr (typed), passphrase: env secret_ref }
  effects: write staging dir only (stable: fresh tree; preview: suite paths);
           sign Release/InRelease + publication record + last-publish
  refuses: no sentinel; bootstrap∩prev-dir; bootstrap∩stable; pool-count ≠
           {4 stable, 4 preview, 2 bootstrap}; versions-per-arch ≠ {2, 2, 1};
           preview candidate ¬gt rollback; pointer mismatch; missing tools

apt.pages-deploy:  # single writer; staged tree → Pages; last-publish guard
```

Implementation recommendation: port `verify`+`publish` to Rust inside the
`velnor-workflow release` runtime (`verify-feed`/`update-feed` grow into the
real verify/publish behind these typed flags), reusing runner claim checks;
prove byte-behavior parity against `test-verify-release.sh` fixtures, then
delete the script. One implementation at the end, never two.

## 4. Negative-test list (all must reject pre-mutation, trusted state intact)

### 4a. Port from `test-verify-release.sh` (typed-verifier equivalents)
Bad source identity (record/manifest repo, tag, source_ref, commit);
tampered record/manifest/deb/SHA256SUMS vs sidecars; missing record;
extra (3rd) deb; manifest-hash ≠ record hash; signer live≠pinned;
OCI ref∌index digest; OCI/platform label mismatches (version, revision,
source, manifest-sha); packaged build-identity sha/crate mismatch;
extracted `velnor-runner` binary hash mismatch; preview: missing commit,
suffix≠commit[0:7], grammar violations (`v`-prefix, separators, short
commit), asset-count≠2, sidecar misname/multi-line/non-hex, control
Package/Version/Architecture mismatch, SHA256SUMS line-count≠2;
publish-side: no sentinel, no passphrase, pool-count wrong,
versions-per-arch wrong, candidate missing from index, cross-arch
rollback divergence, preview candidate ≤ rollback, pointer≠retained,
bootstrap-over-existing, bootstrap∩prev-dir, bootstrap∩stable,
non-null bootstrap pointer.

### 4b. New typed-config / generic-behavior rejections
Unknown suite/channel; `kind` outside the locked set; malformed
`source_repository`/`consumer_repository` (not `owner/name`); empty
package; arch set ≠ exactly {amd64, arm64} (missing, duplicate, `i386`);
short/non-hex commit; non-full signer fingerprint; `secret_ref` naming
a config value instead of an environment secret; `Signed-By` missing
or pointing at untrusted key URL; Pages deploy with older `last-publish`
than live (rollback attempt); concurrent second writer (serialization);
renamed-fixture: renamed package/repo/origin/verifier must behave
identically (any name-keyed branch = fail); config-injected command/
pattern/URL/ref in any apt field = usage error (no-exec proof).
