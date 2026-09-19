# Checker host/release adversarial fixture design

## Status and boundary

Design-only G0 evidence. This is not an approval or rejection of the moving
checker branch. Review source was the clean detached tree
`dual-lane-checker` at `8f976b52e6250deea662cdb23ccd19e7d9ca446c`, with
`docs/ci/github-first-dual-lane/checker-v2-regression-map.md` and
`evidence-schema.md` as the reviewed design authority. No production source
was changed. No host, Docker, OrbStack, package manager, release endpoint, or
live GitHub operation was run.

The fixtures below are executable against a validated passing baseline, but
the current result statements are source-level predictions. They must not be
reported as an exact candidate test result until the corrected implementation
is isolated and reviewed.

## Why these fixtures are necessary

The stage dispatcher makes the boundary explicit (`evidence_check.rs:81-137,
2242-2245, 2274-2292`): G0 checks inventory only; G1+ checks execution; G2+
checks release/install; G4-G7 require both lanes. A G0 pass is therefore not
host, release, or install evidence.

Four current structures create false-green paths:

1. `check_host_binding` (`evidence_check.rs:2619-2688`) accepts a Velnor
   execution when `runner_kind == velnor-managed`, `host_id` is any nonempty
   non-GitHub string, and labels satisfy the reviewed contract. It does not
   verify a runner registration, provisioning record, OS/architecture,
   backend, or payload boundary. For GitHub it likewise checks only the
   `github-hosted` string/class and nonempty host ID. `evidence_live.rs`
   (`:453-473, 585-595`) classifies runner kind from runner name/labels and
   falls back from missing `runner_id` to runner name as `host_id`; those are
   diagnostics, not trust authorities.
2. `check_release_install` (`evidence_check.rs:3033-3094`) blocks a record
   downgrade only when the reviewed manifest says `Required`. A reviewed
   `Applicable` row can submit `NotApplicable`/`Excluded` plus a plausible
   justification and return before canonical release validation.
3. `check_publication_projections` (`:3372-3404`) checks nonempty APT
   repository/suite and Homebrew tap/formula, version equality, and 40-hex
   revision syntax. It does not bind repository/tap, package/formula, release
   asset URL, immutable revision endpoint, or fetched bytes. The canonical
   artifact SHA is checked separately; that does not establish where the
   published bytes came from.
4. `check_install_evidence` (`:3529-3675`) compares operation result,
   release ID, and version strings; predecessor fields need only local
   product/channel/version relations plus syntactically valid SHA/digest.
   Installed binaries need an absolute path, the literal `release-asset`, and
   a digest equal to the submitted canonical artifact. No independently
   fetched publication proof, package-manager ownership, or functional-output
   digest binds those claims to bytes actually installed.

## Harness contract

Use a clean passing baseline that contains the fixed 32-repository scope, a
main Velnor record, both-lane records for G4/G5, and a valid external release
manifest for G2+. The older hostile G2 harness is a mutation pattern, not an
authority; regenerate its baseline for the exact candidate/schema.

For each case:

```sh
set -eu
BASE=/path/to/validated-baseline
CASE=/tmp/checker-case
rm -rf "$CASE"
mkdir -p "$CASE"
cp "$BASE"/manifest.json "$BASE"/snapshot.json "$BASE"/evidence.json \
   "$BASE"/release.json "$CASE"/
# Apply exactly one jq mutation below, into CASE/*.json.
CHECKER=/path/to/exact/detached/velnor-tools
"$CHECKER" evidence-check --stage "$STAGE" \
  --manifest "$CASE/manifest.json" --snapshot "$CASE/snapshot.json" \
  --evidence "$CASE/evidence.json" --release-manifest "$CASE/release.json" \
  --json >"$CASE/report.json"
test "$(jq -r .status "$CASE/report.json")" = fail
```

For G0, omit `--release-manifest`; run only a G0 inventory mutation. Never
run the host or release mutations at G0 and infer a G4/G5/G2 result.

Host mutations must update the selected Velnor/GitHub execution in
`snapshot.repositories[].main_executions[]`, its `jobs[]` runner fields where
applicable, and the matching `evidence.records[]` top-level identity. Keeping
those copies equal prevents an incidental `run-mismatch` from masking the
missing independent proof. Release mutations target the main
`tailrocks/velnor` record only and leave the producer canonical document
unchanged unless the fixture explicitly tests canonical bytes.

The current schema has `deny_unknown_fields`. A future sidecar or typed proof
field is therefore a **schema-next fixture**: do not add it to a current input
and call a parse error a host/release result. Current-schema fixtures below
use existing fields to demonstrate the false-green claim; schema-next fixtures
specify the independent proof that must be consumed.

## Host-trust fixtures

| ID / stage | One-case mutation | Current source prediction | Required result after repair |
| --- | --- | --- | --- |
| `H0-github-runner-name-spoof` / G1 and G3 | On the GitHub-provider main record, keep `runner_kind: "github-hosted"`, a nonempty non-sentinel `host_id`, and allowed labels, but set `runner_name: "velnor-host-foo"` (also test `"hostfoo"`). Mirror the name in the authoritative snapshot execution/job. | `check_host_binding` ignores `runner_name`; the record can pass despite a name identifying a Velnor/self-hosted runner. Live collection also has a dangerous name/label classifier. | Fail `runner-registration` unless a GitHub API runner/job identity proves a hosted runner ID, group, OS/architecture, run/attempt, and job association. Name/labels remain diagnostics only. Run G1 with no release document; run G3 with the same host mutation plus a valid release document. |
| `H1-velnor-host-id-spoof` / G4 | On the Velnor main record, set `host_id: "claimed-host-not-registered"`, keep `runner_kind: "velnor-managed"`, `runner_name` and all required labels contract-valid. Mirror execution and job fields. No host attestation exists. | The Velnor branch requires only nonempty/not-GitHub `host_id` and contract labels; source predicts no host finding. | Fail `host-attestation`: host ID must resolve to an independent Velnor runner registration/provisioning record bound to run ID, attempt, job ID, source SHA, target, and expiry. |
| `H2-docker-boundary-absent` / G4 and G5 | Keep the H1 record valid but use `host_id: "orbstack-vm-claimed"` and then `"docker-host-claimed"`; keep Velnor runner kind/labels. Do not supply a trusted runtime-boundary object. | Host checks see a nonempty ID and valid labels; Docker/OrbStack execution is not represented or checked. | Fail `runtime-boundary`: independent collector must bind container/VM ID, immutable image digest, runtime (`docker`/`orbstack`), host registration ID, workspace mount/source digest, entrypoint/payload digest, run/attempt/job. A payload-produced string cannot satisfy this. |
| `H3-native-exception-spoof` / G5 | For the native macOS/Homebrew target, keep the Velnor host contract valid and claim `host_id: "macos-native"`; omit native facts in one variant and attach an independent fact of `backend: "docker"`/`"orbstack"` in the mismatch variant. | Current schema has no backend or native attestation; the host branch accepts the claim. | Fail `native-boundary` unless an independent attestation positively proves native macOS, OS/version, architecture, runner registration, and no container/VM backend. “No Docker field” is not proof of native execution. |
| `H4-host-registration-mismatch` / G4 and G5 | Keep record/snapshot host strings coherent, but schema-next attestation names a different runner registration or expired host. | Current checker has no attestation input and cannot detect the mismatch. | Fail closed on registration ID, host ID, run/attempt/job, source SHA, target, or validity-window mismatch. |

The `H0` case is important: changing only `runner_name` must not be treated as
an authoritative host mutation. It proves the exact requested case where a
GitHub-provider record with `velnor`/`hostfoo` naming can currently pass. The
live collector must stop promoting that name to provider/host trust; an API
runner registration or trusted provisioning authority must decide.

### Independently collectable host proof

The host proof should be a separate, immutable, typed object, not a new free
text field in `EvidenceRecord`:

```text
HostAttestation {
  provider, runner_registration_id, host_id, runner_kind,
  run_id, run_attempt, job_id, repository, workflow_revision, source_sha,
  platform, architecture, os_image, backend,
  payload_boundary_digest, issued_at, expires_at,
  source_url, bytes_sha256, signer_or_api_identity
}
```

Required authority differs by provider:

- GitHub: read-only runner/job API identity (numeric runner/job IDs, hosted or
  self-hosted classification, group, OS/architecture, run/attempt, job URL).
  Never derive hosted trust from `runner_name`, display title, labels, or the
  evidence row.
- Velnor: an independently signed/provisioned registration or immutable
  controller record, with host ID, runner registration, platform/architecture,
  OS image, backend, lifecycle validity, and exact run/job/source binding.
  The evidence producer cannot author this record after the job.
- Docker/OrbStack: a trusted wrapper/controller records container or VM ID,
  image **digest** (not tag), runtime backend, host registration, mounts,
  checkout/source digest, entrypoint, and payload digest. The wrapper must be
  outside the untrusted payload and bind the result to run/attempt/job.
- Native exception: the same trusted wrapper records positive native facts
  (`sw_vers`/kernel/architecture or provider equivalent) and an explicit
  `backend=native`; absence of a Docker label is insufficient. A Docker or
  OrbStack attestation must contradict, and therefore reject, a native claim.

The checker should compare this proof with snapshot job/run IDs and the
reviewed host contract. Missing, stale, inaccessible, or conflicting proof is
`blocked`/fail, never `NotApplicable` or success. G3 barrier remains in force:
do not collect these facts on the current host during fixture work.

## G2 release/install fixtures

The following mutations are intentionally small and independently executable
with `jq`. `$R` denotes the selected main `tailrocks/velnor` record; use the
same repository/run selector in the local harness rather than assuming a run
number. The projection mutations preserve a valid candidate/version,
revision syntax, and canonical manifest digest so a version-only check cannot
hide the gap.

### Applicability downgrade

`G2-A-applicable-downgrade` (required current-schema false-green fixture):

1. In the reviewed manifest row for `tailrocks/velnor`, set
   `release_applicability` to `"applicable"`.
2. In `$R`, set both `release.applicability` and `install.applicability` to
   `"not-applicable"` and set both justifications to nonempty text such as
   `"distribution deliberately deferred"`.
3. Leave the valid canonical release document present and leave all execution
   facts unchanged.

Equivalent `G2-A-excluded-downgrade` uses evidence `"excluded"`; it must be
tested separately because `Excluded` is a distinct enum value. Current logic
checks only `manifest == Required`, then returns before canonical validation
when the manifest is `Applicable`; source predicts a false green. The repair
must treat reviewed `Required` **and `Applicable`** as requiring typed release
and install proof. `NotApplicable`/`Excluded` is allowed only when that state
comes from the reviewed authority with an authoritative reason; a result row
cannot create or downgrade it.

Illustrative mutation commands:

```sh
jq '.repositories |= map(
      if .repository == "tailrocks/velnor" then
        .release_applicability = "applicable"
      else . end
    )' \
  manifest.json >case-manifest.json
jq '.records |= map(
      if .repository == "tailrocks/velnor" and .pr_number == null then
        .release.applicability = "not-applicable"
        | .release.justification = "distribution deliberately deferred"
        | .install.applicability = "not-applicable"
        | .install.justification = "distribution deliberately deferred"
      else . end
    )' \
  evidence.json >case-evidence.json
```

Both commands assert the intended target with a single `map(if ... then ...
end)` filter; add a `jq -e` count assertion in the harness that exactly one
manifest row and one main record changed.

### Publication identity and immutable asset proof

| ID / stage | Mutation | Current source prediction | Required rejection/proof |
| --- | --- | --- | --- |
| `G2-P-apt-location-spoof` / G2 | Set `release.execution.apt.repository` to `https://attacker.invalid/apt` (or another noncanonical repository), keep suite/candidate/version, canonical manifest digest, and a valid 40-hex revision. | `check_publication_projections` sees nonempty repository, suite, matching version, and valid SHA; expected false green. | Bind the APT feed to reviewed producer identity and immutable revision/URL. Verify signed `InRelease`/Release bytes, package name/architecture/version, `.deb` URL and bytes SHA, parent release ID, and manifest digest. |
| `G2-P-homebrew-location-spoof` / G2 | Set `release.execution.homebrew.tap` to `attacker/homebrew-velnor` or an attacker HTTPS tap, keep formula/version, manifest digest, and valid revision. | Expected false green; tap is only checked for nonempty shape. | Bind tap repository, formula path, immutable commit, archive/bottle URL, target, bytes SHA, parent release ID, and manifest digest. |
| `G2-P-free-text-projection` / G2 | Replace any legacy/free-text projection with `"0.1.1 sha256:<valid>"` while preserving nonempty version/digest tokens. | If accepted by a compatibility path, expected false green; strict v2 should reject parse/legacy shape. | No free-text projection or version/digest substring check. Use typed APT/Homebrew records and independently fetched bytes. |
| `G2-P-asset-url-omitted` / schema-next G2 | Start with a valid typed publication proof, remove the immutable asset URL/bytes proof, retain `asset_digests` and all current nonempty fields. | Current 8f schema cannot express the proof; adding unknown fields causes parse failure, which is not a meaningful current host/release result. | Required future fixture must fail `missing-publication-proof`, not silently interpret the old `asset_digests` map as publication authority. Mutable `latest`, branch, or tag URLs fail even when digest text is valid. |

The canonical release document's external `manifest_sha256` is useful and must
remain outside canonical bytes to avoid self-hash recursion. It is not enough
to require the same digest in APT/Homebrew fields: those fields need an
independent URL/revision and fetched-byte digest. Expected repository/tap
identity must come from reviewed source/config, not a caller-provided string.

Recommended typed projection shape:

```text
PublishedAsset {
  name, target, kind, size, bytes_sha256,
  immutable_url, immutable_ref, provider, release_id, manifest_sha256
}
AptPublicationProof {
  repository_id, repository_url, revision_sha, suite, package, architecture,
  version, release_metadata_url, release_metadata_sha256,
  package_url, package_sha256
}
HomebrewPublicationProof {
  tap_repository, tap_revision_sha, formula_path, formula_sha256,
  bottle_or_archive_url, bottle_or_archive_sha256, target, version
}
```

The independent collector must fetch/hash the bytes or consume a trusted
provider API with immutable object identity. URL strings alone are not proof.

### Install identity and functional proof

| ID / stage | Mutation | Current source prediction | Required rejection/proof |
| --- | --- | --- | --- |
| `G2-I-unpublished-predecessors` / G2 | Give same-channel predecessor a valid-looking older version, distinct release ID, arbitrary valid 40-hex source SHA and valid `sha256:` manifest digest. Give channel-switch predecessor a valid-looking other channel/version/release ID with arbitrary valid source/digest. Keep each operation's observed current release/version successful. | Current checks only local shape/relations; expected false green. | Resolve each predecessor release ID to an independent immutable canonical manifest and publication asset proof. Require source/tag/channel/version/manifest digest and package bytes to match that released identity. A syntactically valid digest is not an existence proof. |
| `G2-I-checkout-path` / G2 | Set each installed binary path to an absolute checkout-like path such as `/tmp/checkout/target/release/velnorctl`; keep `source: "release-asset"` and the canonical artifact digest. | Current path test is only `starts_with('/')`; expected false green. | Independently collect package-manager ownership/path and bytes hash. Reject checkout, source-tree, symlink, ambient-PATH, or unowned paths. |
| `G2-I-claimed-binary` / schema-next G2 | Retain component/artifact/target and claimed digest but omit or mismatch independent installed-byte/package transaction proof. Keep `functional_result: "success"`. | Current schema has no such proof field; absence is accepted. | Functional output must be an immutable, independently collected record bound to environment, installed package identity, binary bytes SHA, target, and release/publication asset. A result string cannot authorize success. |
| `G2-I-operation-source` / schema-next G2 | Keep operation `observed_release_id`, `observed_version`, and `result` equal to canonical values while remove source URL/manifest/asset digest for the transaction. | Current operation fields are exactly those strings; expected false green. | Every clean install, same-channel upgrade, and channel switch must record package manager transaction/output digest and immutable source release/asset identity. |

Recommended independent install object:

```text
InstallAttestation {
  environment_id, provider, host_attestation_digest,
  operation, package_manager, clean_state, command_log_url, command_log_sha256,
  source_release_id, source_manifest_url, source_manifest_sha256,
  source_asset_url, source_asset_sha256,
  installed_product_id, channel, version, target,
  package_db_identity, installed_paths, installed_bytes_sha256,
  functional_result_url, functional_result_sha256
}
```

For an upgrade or channel switch, the predecessor must reference a separately
verified immutable release record; do not accept a caller-invented old
identity merely because its version sorts lower. Installed binary bytes must
be hashed after installation and tied to package ownership and the published
asset. The functional test log must be independently retained and bound to
that identity.

## G0 and evidence interpretation

G0 is intentionally excluded from all host/release/install claims. The exact
source returns from `check_record` after `check_g0_record`, before provider,
execution, host, release, or install checks. A G0 fixture may only mutate the
typed inventory (missing dependency/access/model/source row, empty required
class, or scalar `gate_status`) and verify that the inventory gate fails.
`gate_status=pass` never authorizes a later stage. Offline fixture success is
also not G7/live proof.

Acceptance for the eventual corrected candidate:

- the untouched exact baseline passes its declared stage;
- every current-schema hostile mutation above exits nonzero with a specific
  finding, while unrelated fields remain valid;
- schema-next inputs are consumed by the new typed proof path, not silently
  ignored or downgraded to N/A;
- missing, stale, expired, inaccessible, or conflicting host/publication/
  install proof fails closed;
- no fixture requires a host operation, Docker/OrbStack startup, package
  installation, network publication, or source-tree edit.
