# G2 independent distribution review

Observed: 2026-09-19. This is an independent acceptance review, not an
implementation approval. Source repositories were inspected read-only.

## Baseline and effective model

- Velnor producer: `tailrocks/velnor` at `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- APT consumer: `tailrocks/velnor-apt` at `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`.
- Homebrew consumer: `tailrocks/homebrew-velnor` at
  `7af1249f3d69c9f2e548583cdc9f3e737da41b81`.
- Existing evidence: `../distribution/findings.md`.
- Effective child settings: `gpt-5.6-luna`, reasoning `max`; `rtk` 0.49.0.

## Observed blockers and false-green paths

1. APT stable discovery uses `gh release list ... --limit 1` and can select
   `velnor-workflow-runtime-*` instead of an application release. The current
   code is at `../dual-lane-apt/.github/workflows/release.yml:96-102`.
2. APT preview expects a GitHub `preview` release, but the pinned producer had
   only a `preview` tag and no release. The consumer path is
   `../dual-lane-apt/.github/workflows/release.yml:103-115`.
3. The pinned APT workflow drops hidden `.reprepro-ok` during the
   verify-to-publish artifact handoff (`:141-147`). The producer already has
   the intended fix and regression test at
   `../velnor/crates/velnor-workflow/src/primitives/release.rs:4283-4289`
   and `:8330-8353`, but the consumer pin is stale.
4. APT config schedules both GitHub and Velnor lanes by default, including
   Velnor capacity that may be queued or unavailable:
   `../dual-lane-apt/.github-gen/velnor-workflow.toml:13-20`.
5. Homebrew is a source-built `velnorctl`-only formula. It installs no
   `velnor-runner` or `velnor-workflow`, and its README explicitly disclaims
   the runner: `../dual-lane-homebrew/Formula/velnorctl.rb:1-19` and
   `README.md:3-20`.
6. Component versions are not coherent: `velnor-runner` is `0.1.277`, while
   `velnorctl` and `velnor-workflow` remain `0.1.0`. A formula/tag version or
   regex-only smoke test can therefore pass while product identity is wrong.
7. Preview publication verifies the current release, then deletes the rolling
   release before creating its replacement (`../velnor/.github/workflows/preview.yml:1001-1038`).
   A failed create leaves consumers without a current preview endpoint.
8. Runtime products are a separate `velnor-workflow-runtime-*` namespace and
   currently cover Linux x64/arm64 plus macOS arm64 only:
   `../velnor/crates/velnor-workflow/src/primitives/runtime_products.rs:70-148`.
   Intel Mac support must not be implied.

## Acceptance constraints: APT (`g0_distribution`)

- Regenerate from a reviewed generator pin containing the hidden-file fix, and
  prove byte-identical generation with the pin/check command. Do not hand-edit
  generated workflows.
- Stable discovery must paginate all releases and accept only typed
  application releases: exact stable version grammar, application manifest
  schema/product identity, matching source tag/commit, and complete expected
  amd64+arm64 assets. Reject runtime releases, previews, drafts, invalid or
  incomplete manifests, and API/pagination failures. Add fixtures for each.
- Preview must consume an explicit complete GitHub release/manifest bound to
  `refs/heads/main` and its exact commit. Tag-only, Pages-only, or stale
  pointer state is not a valid source.
- Make routine feed publication GitHub-hosted by default. Velnor can remain an
  explicit recovery/qualification choice only; publication must not depend on
  queued Velnor jobs.
- Preserve `.reprepro-ok` through the artifact handoff and retain a test that
  fails when hidden files are omitted.
- Feed mutation must be verify-before-publish, sign both suites and arches,
  preserve the other suite, retain a rollback, reject version rollback, and
  fail closed unless verify, publish, and deploy all succeed.
- Prove on clean Debian hosts: stable fresh install and upgrade, preview fresh
  install and upgrade, preview-to-stable switch, both arches, signature
  validation, every required binary, package metadata/permissions/config, and
  real service-manager behavior.

## Acceptance constraints: Homebrew (`g2_homebrew_contract`)

- Replace the CLI-only contract. Install the compatible workflow-generation
  and start/manage runtime surface; explicitly decide and test the native vs
  Docker-backed macOS boundary. A missing sibling binary or PATH fallback is a
  failure.
- Define explicit stable and preview formula/channel identities, commands,
  version ordering, upgrade/switch behavior, and uninstall behavior.
- Use immutable release inputs with checksums and source commit/product
  identity. No source checkout, local symlink, or ambient PATH can satisfy a
  clean install.
- Generated PR/main CI must cover formula contents, checksums, architecture,
  all installed binaries, `--version`, source identity, and sibling discovery.
- Test clean hosted macOS arm64 installs. Record Intel Mac unsupported unless
  an actual native artifact and test exist; do not advertise cross-compiled
  support.

## Acceptance constraints: producer (`g2_native_packages`)

- Stable and preview must publish real immutable artifacts with complete
  manifests/release records, exact source ref/commit/version, checksums,
  attestations, signatures, and every declared platform asset.
- Keep application release discovery typed and separate from
  `velnor-workflow-runtime-*`. Runtime tags must never satisfy the app
  consumer contract.
- Stable releases/tags are immutable and idempotent reruns byte-confirm
  existing assets. Preview versions are unique and orderable; publish
  immutable versioned assets, verify them, then advance the pointer. Never
  delete the current preview before replacement is proven.
- Make product/component identity coherent. The Debian package inventory
  currently includes `velnor-runner`, `velnorctl`, `velnor-workflow`, guest
  tools, services, and config (`../velnor/crates/velnor-runner/Cargo.toml:56-65`).
  The consumer contract must test that inventory rather than only one binary.
- Require Linux amd64/arm64 and explicitly declared native macOS targets;
  Intel remains unresolved until built and tested.
- Use one publisher after required hosted/native checks. Recovery publication
  must be explicit and remain acyclic.

## Machine-verifiable installer gate

Every channel/platform case must emit a structured record containing:

```text
product_id
channel
version
source_repository
source_ref
source_commit
platform
architecture
artifact_name
artifact_sha256
manifest_sha256
attestation_verified
signature_verified
installed_binary_inventory
installed_binary_versions
installed_binary_source_identity
upgrade_from
switch_from
service_manager_result
```

Required cases:

- Debian amd64 and arm64: stable fresh, stable upgrade, preview fresh,
  preview upgrade, preview-to-stable switch, signature and service checks.
- Homebrew macOS arm64: stable and preview fresh install, same-channel
  upgrade, cross-channel switch, uninstall; verify all required binaries and
  source identity. Intel is a required negative/unsupported result unless
  native assets exist.
- Every case must prove the downloaded artifact and manifest digests before
  install, and must fail if a sibling binary is absent, a component reports a
  mismatched version/source, or a local checkout/PATH supplies the executable.

## Required hostile fixtures

Mixed paginated releases; runtime release ahead of app release; preview tag
without release; missing/extra asset; wrong source commit/ref; malformed
manifest; checksum mismatch; failed attestation/signature; API failure;
partial upload/retry; preview pointer rollback; hidden sentinel loss; dropped
APT suite; formula missing sibling; unsupported architecture; and rerun with
existing immutable assets.

This report intentionally does not approve source unit tests as proof of the
external product contract. Final approval requires actual producer release,
consumer feed/tap publication, and clean installer/upgrade evidence.

## Manifest arbitration and staged CI design

### One application authority

The producer must publish one canonical application manifest for each immutable
release candidate. It is the only authority for product/channel/version/source
identity and complete artifact inventory. A schema change is a breaking change:
consumers accept exactly the declared current schema and fail closed; they do
not fall back to an older schema that lacks inventory.

The canonical manifest should contain, at minimum:

```text
schema
product_id
channel
version
source_repository
source_ref
source_commit
release_tag
release_id
artifacts[{name,target,kind,sha256,size}]
components[{name,crate,version,binary,targets}]
```

APT and Homebrew select their required artifact projection from this same
manifest. Package-specific records are allowed only as subordinate transport
records. They must carry `parent_manifest_sha256` (or an equivalent immutable
manifest identifier), repeat and cross-check the parent identity, and never
become an alternate product authority. Existing APT `release-record.json` and
Homebrew package/archive records can remain subordinate if their roles are
explicit and their fields are checked against the parent.

Do not put `manifest_sha256` inside the bytes whose digest it names. Canonical
manifest bytes are hashed externally (sidecar, release record, feed/tap
publication record, or formula metadata). A subordinate identity may contain
the parent manifest digest; the parent must not contain a digest of that
identity. This prevents self-hash recursion and makes retries byte-stable.

The current Homebrew proposal must therefore change its three-way identity
model (`config/homebrew-release-contract.json` and
`docs/homebrew-release-contract.md`): `homebrew-manifest.json` should be a
consumer projection or disappear, and archive `manifest.json`/`identity.json`
must be checked as subordinate records against the canonical app manifest.

### Staged installer workflow

Use an acyclic four-stage chain. Each stage emits JSON evidence and immutable
links; later stages consume exact IDs/digests rather than rediscovering a
moving `main` or a mutable preview pointer.

1. **Producer stage:** build the declared target matrix, assemble deterministic
   archives/debs, emit the canonical manifest, compute its external digest,
   sign/attest every artifact and manifest, and upload an immutable release.
   Verify every manifest artifact row against the bytes before publishing the
   rolling channel pointer.
2. **Consumer staging stage:** APT discovery selects the canonical manifest,
   filters Linux amd64/arm64 rows, verifies source/ref/version/product and
   fetches packages. Homebrew updater selects the same manifest, filters
   native macOS rows, verifies sibling inventory/checksums, and renders stable
   or preview formulas. No consumer builds from source in the normal path.
3. **Publication stage:** publish the signed APT suite and tap commit/formulas
   only after the staged inputs pass. Preserve the other APT suite and current
   preview pointer. Record producer run, release ID, manifest digest, feed/tap
   revision, and formula/package rows.
4. **Clean-client stage:** install from the real published endpoint in
   disposable environments, then run upgrade/switch/uninstall cases. G2 hosted
   macOS tests do not invoke OrbStack, nested virtualization, or `host start`;
   actual Docker-backed host execution belongs to G4/G5. They must still prove
   all installed native binaries, sibling discovery, version/source identity,
   and a useful no-network smoke command.

### Exact current platform capability map

The generator's fixed runtime platform map is:

```text
Linux x86_64   -> ubuntu-24.04      -> Linux-X64
Linux aarch64  -> ubuntu-24.04-arm  -> Linux-ARM64
macOS arm64    -> macos-15          -> macOS-ARM64
```

This is encoded in `../velnor/crates/velnor-workflow/src/primitives/runtime_products.rs:70-148`.
The release workflow provisions Rust target names including both
`aarch64-apple-darwin` and `x86_64-apple-darwin`, but a cross target is not
native Intel evidence. Homebrew must advertise Intel only after a real
producer artifact and a matching native clean-client test; otherwise emit an
explicit unsupported result. Native macOS preflight supports Docker execution
but rejects Firecracker/KVM (`../velnor/crates/velnorctl/src/local_diagnostics.rs:1-5,76-152`).

### Evidence fields for `g0_checker`

Use the existing evidence envelope's `release` and `install` objects, with the
following machine-verifiable values. The checker already requires the starred
fields; the remaining fields should be retained in the evidence payload for
independent cross-checking:

```json
{
  "release": {
    "applicability": "required",
    "release_channel": "stable|preview",
    "release_version": "X.Y.Z or preview version",
    "tag_target_sha": "40-hex source commit",
    "release_id": "immutable provider release ID",
    "asset_digests": {"asset-name": "sha256:<64-hex>"},
    "apt_feed_revision_suite_and_candidate": "consumer-sha; suite; version; manifest-sha",
    "homebrew_tap_revision_and_formula": "consumer-sha; formula; version; manifest-sha",
    "manifest": {
      "schema": "exact current schema",
      "product_id": "configured product",
      "source_ref": "exact tag or refs/heads/main",
      "source_commit": "same 40-hex SHA",
      "manifest_sha256": "sha256:<64-hex>"
    }
  },
  "install": {
    "applicability": "required",
    "install_upgrade_test_environment": "OS image; architecture; runner; clean workspace/PATH",
    "installed_binary_identity": {
      "product_id": "configured product",
      "channel": "stable|preview",
      "version": "same release version",
      "source_sha": "same source commit",
      "manifest_sha256": "same canonical digest",
      "binaries": [
        {"name": "velnorctl", "path": "absolute installed path", "sha256": "sha256:<64-hex>"},
        {"name": "velnor-runner", "path": "absolute installed path", "sha256": "sha256:<64-hex>"},
        {"name": "velnor-workflow", "path": "absolute installed path", "sha256": "sha256:<64-hex>"}
      ]
    },
    "upgrade_from": "prior channel/version or null",
    "switch_from": "prior channel/version or null",
    "service_manager_result": "systemd success|not-applicable with reason",
    "functional_result": "success"
  }
}
```

`tag_target_sha` must equal the evidence record's actual checkout/source SHA;
installed `product_id`, version, and source SHA must equal the release. The
checker must reject missing sibling rows, empty/unsupported architecture,
manifest digest mismatch, stale parent records, local checkout/PATH fallback,
and a `functional_result` that is merely a formula or package test. APT
service success requires a genuine systemd-capable environment; Homebrew's
macOS G2 result is explicitly `not-applicable` for systemd and must not invoke
OrbStack.

The current checker implementation only validates the existing scalar release
and install fields (`../dual-lane-checker/crates/velnor-tools/src/evidence_check.rs:374-415,2006-2198`);
unknown nested manifest/environment fields would otherwise be ignored. Before
using this as a gate, `g0_checker` must type and validate the manifest,
artifact, component, environment, operation, and service fields above (or
reject records that omit them). Do not treat the current free-form publication
strings or non-empty binary list as proof of channel/feed/formula/install
coherence.

## Revision reconciliation: preliminary A–H checkpoint (2026-09-20)

This section reconciles claims to exact pushed heads. It is evidence only; it
does not approve G0, G2, G4, G5, G6, or G7.

| Area | Exact pushed head and committed change | Evidence-backed result | Remaining acceptance blocker |
| --- | --- | --- | --- |
| A — fixed scope | Estate `58bd26f905624820042ff944997ea21476fc0127` (parent `7922da75adc2c034f396bd9a384182c776dfc084`) adds `deny_unknown_fields` plus one parser test. The prior fixed-scope/boundary implementation is in `7922da75adc2c034f396bd9a384182c776dfc084`/`6387532f03c230f26795975ba5ff3034f2862eb7`. | Exact 58bd tests: 214 passed. The fixed 32-name authority and hostile scope tests from 6387532f are retained. | This is not live 32-repository evidence. The 58bd delta does not add live source binding, per-row access, or provider/runtime proof. Keep the estate review's authority-boundary and fresh-default requirements. Evidence: `G0/estate-scope-review/report.md`. |
| B — revision freshness | Records `4a9435e7cbeb220f1d2e985ab4d3fac97290dcef` (parent `85382073f3fa77e629e6cdffcbb341c031d08e67`) changes only PLAN/RUNBOOK/SPEC/STATUS wording. | The prior records audit remains the applicable data result; no new collector rows or live refresh are committed. | Audit snapshot was `2026-09-19T17:05:57Z`; `jackin-project/homebrew-tap` was 31/32 source-bound (`f1669391582f92c95da1aa5958de4397dfedca09` committed vs `2281ae9c95dfcb5bf82dfd025fdf58a50658b1d1` observed), and 78 historical PR identities versus 77 later rows remain snapshot-bound. Refresh all exact 32 now; stale/missing rows fail. Evidence: `G0/fleet/records-completeness-review.md`. |
| C — PR/check/workflow inventory | Same records 4a docs explicitly state the Velnor-only `check-contract.json` cannot cover the other 31 repositories. | The canonical docs correctly preserve unknowns and prohibit fleet-wide reuse. | Static `fleet.json` has no per-row PR/check/App/run/workflow inventory, and external PR rows are not a complete 32-repository ledger. Missing required coverage, duplicate IDs, pagination truncation, queued/canceled/failed/child-log absence must reject; no manifest-only pass. |
| D — workload/dependency/access | Checker `d9a277938d54d93f06b85ce6e8d406ebb67468c3` is byte/source-tree identical to hostile-tested `2ba66b116dd5511f0b4f2a6856cfbed6bd290152`; d9 is attribution-only. | Independently tested exact 2ba correctly rejects scope substitution, manual-only main, extra/duplicate actual jobs, empty/malformed logs, missing required child, unknown fields, and flat/legacy aliases. | 2ba/d9 still accepts an internally consistent extra `third_party` policy key, G0 `blocker`/`next_action` due early return, optional/self-declared child graph, and fields without independent collector/model/access binding. Evidence: `G0/checker-adversarial-fixtures/report.md`. |
| E — source/provider provenance | d9 source identity was verified with `git diff --quiet 2ba..d9`; no semantic checker fix landed. | Correct rejection of the old WIP mutations must not be overclaimed as provider proof: manual-only, scope substitution, and extra actual IDs fail in the exact harness. | Run/check/log/child URLs remain opaque and can be changed coherently to evil hosts; required checks remain self-attested; no independently API-bound source/ref/check/run/provider evidence. Missing optional manifest identity/digest fields must be a finding, not a skip. |
| F — execution/host | Lane compare `3ad452acd8004c06cb866f75392bb5749adfe818` is clean and remote-equal. | It fixes full-census, pagination, terminal-state, empty evidence, and selector diagnostics; lane-compare is explicitly auxiliary. | Exact review rejects 3ad: partial nonempty HTML can pass; watch can hide current workload-class drift; watch filters recent failed runs before validation. Separately, checker G4/G5 synthetic `velnor-managed` host + nonempty ID passes without Mac/OrbStack/Docker/arch proof; hosted G4/G5 require qualifying both-lane comparison and actual capability evidence. Evidence: `G0/lane-compare-review/review-3ad452acd.md`; checker adversarial report. |
| G — logs/artifacts/child graph | The exact checker harness proves several independent missing/duplicate checks fail. Lane 3ad adds pagination and artifact completeness checks for its auxiliary comparison. | Correct fail-closed negatives exist for empty logs, malformed log URL syntax, missing child, duplicate/extra actual jobs, and missing artifacts in lane-compare fixtures. | No typed URL binding or independently derived mandatory child graph exists in checker; lane 3ad still has the three failures above. Collector must prove page 2 for >100 artifacts, explicit expected artifacts/logs, no truncation/API error, and source/run/job binding; missing/empty logs fail independently. |
| H — release/install and publication | No exact producer/consumer release or clean-client commit in this reconciliation closes the distribution contract. The records 4a docs correctly keep release/install unknown. | Existing scalar schema rejects flat release/install aliases; product-manifest arbitration and external-digest/no-recursion constraints remain valid. | G2+ only: release and install must be required by authoritative manifest applicability and cannot be downgraded to `N/A`/excluded by a record. Require published stable/preview feed/tap identity, exact asset/manifest digests, source/tag identity, sibling inventory, clean fresh/upgrade/switch results, and service result. No G0 future-execution credit. |

### Exact disposition

- Checker: **not accepted**. d9 is source-identical to the independently hostile-tested 2ba tree; correctly rejected old WIP cases are real, but the false greens above remain.
- Records: **not G0-complete**. 4a is documentation-only relative to the audited records source; the external row audit remains stale/incomplete and explicitly says no G0 pass.
- Lane compare: **reject source** at 3ad until its three fail-closed blockers are fixed; it remains auxiliary even after repair.
- Estate scope: **not gate-approved**. 58bd hardens parser strictness only; it does not replace live identity/default/access/provenance evidence.

Primary exact evidence: `G0/checker-adversarial-fixtures/report.md`,
`G0/lane-compare-review/review-3ad452acd.md`,
`G0/fleet/records-completeness-review.md`,
`G0/estate-scope-review/report.md`, and
`G0/fleet/push-checkpoint-current.json`.

Review boundary: at the d9 review timestamp the shared checker worktree
contained uncommitted owner edits in `crates/velnor-tools/src/evidence_check.rs`
and `crates/velnor-tools/src/main.rs`; those edits were excluded. No dirty-tree
behavior is treated as a result. Findings above target only exact pushed
d9/2ba; b11 is reviewed separately below in a clean detached tree. Later
shared owner edits visible in the checker/records worktrees are likewise
excluded; exact commit objects and remote-equal refs are the only inputs here.

## Historical b3 G2 fixtures versus pushed d9

The nine rows below are the historical exact `b3b6b2ef5239ff3354f504b8aeb638129fd0504b`
review, not current d9 evidence. The historical report recorded all nine as
false green. d9 is source-identical to exact hostile-tested 2ba, but its
canonical release schema changed several fixture shapes; old free-text fixture
results must not be relabeled as d9 runs.

| Historical b3 mutation | Historical result | d9/2ba source result or status | Required re-test / remaining gap |
| --- | --- | --- | --- |
| `manifest-component-missing` | b3 false green; component/install list was evidence-declared. | **Still structurally open:** d9 checks non-empty inventories and local bindings, but no config-derived exact product component set. | Remove one canonical component and matching binary from a d9-schema fixture; must fail. |
| `digest-binding` | b3 false green; digest was syntax/free-text bound. | **Old fixture obsolete; core check repaired:** d9 recomputes external digest over canonical manifest bytes and cross-checks producer/APT/Homebrew digest fields. | Add tampered manifest bytes, named manifest-asset mismatch, and consumer publication digest mutation; all must fail independently. |
| `arch-mismatch` | b3 false green. | **Cross-field fixture repaired:** d9 binds installed target to environment and canonical component/artifact target. | Re-run with d9 canonical fixture; still require actual downloaded artifact/host architecture proof outside checker. |
| `service-unsupported` | b3 false green; arbitrary N/A on Linux. | **Cross-field fixture repaired:** d9 compares target service applicability and requires `systemd-success` when target is required. | Add authoritative target applicability/exclusion mutation; unsupported N/A must not waive an applicable package. |
| `upgrade-paths-absent` | b3 false green. | **Repaired:** d9 requires typed clean-install, same-channel upgrade, and channel-switch operations plus predecessors. | Re-run exact d9 fixture; require actual feed/tap transaction evidence, not submitted operation claims only. |
| `same-version-replacement` | b3 false green. | **Repaired structurally:** predecessor must be distinct and older/different channel as applicable. | Add same-version/different-source publication and package replacement fixture; provider/feed identity remains unbound. |
| `source-tag-identity` | b3 false green; source/ref/tag were shape-only. | **Partially repaired:** d9 binds source commit to record checkout and enforces tag/ref equality, but source repository is only syntax/equality within the submitted document and preview source grammar is not modeled. | Mutate source+producer coherently to another valid repository; test preview `refs/heads/main` + immutable tag/source issuance; reject without independent provider binding. |
| `producer-consumer-chain` | b3 false green; APT/Homebrew strings accepted as free text. | **Partially repaired:** d9 uses typed projections and binds their manifest digest/version/revision shape, but does not fetch or reconcile real feed/tap content, release ID, or formula/package identity. | Replace with structured valid-but-unrelated APT/tap revisions and producer run; must fail against independent provider/package facts. |
| `stale-schema` | b3 false green; schema only nonempty. | **Repaired:** d9 requires `velnor.application-manifest.v1` and canonical release schema version. | Keep stale/unknown schema rejection; no older-schema fallback. |

Additional current d9 gap not represented by the nine b3 rows: when the
authoritative manifest says `release_applicability: applicable` (or another
non-`required` state), `check_release_install` permits record-side
`install.applicability: not-applicable`/`excluded` with a justification and
returns before canonical install verification (`evidence_check.rs:3033-3093`).
Applicability policy must be authoritative for the product/package, and a
release that is applicable cannot receive an install waiver from the record.
Add a d9 exact negative for `Applicable -> N/A`, `Required -> Applicable`, and
install waiver with a free-text reason; G2+ must reject each.

## b11 first-slice follow-up (exact pushed source)

Exact pushed checker head:
`b11b57b73f0bae8b9a7edf530adf6938c83c5fa2`, parent
`d9a277938d54d93f06b85ce6e8d406ebb67468c3`, remote-equal and clean in the
detached review tree. This is a first-slice repair, not a G0 approval.

Independent exact checks: `rtk cargo test -p velnor-tools --no-fail-fast`
reported **224 passed**; clippy with `-D warnings` was clean; `git diff
--check HEAD^..HEAD` was clean. `cargo fmt --all -- --check` **failed** on
three formatting-only test locations in `evidence_check.rs` and `main.rs`;
that hygiene failure is recorded, not repaired here.

Verified b11 repairs: canonical `evidence-check` CLI only; exact provider key
set `{github,velnor}`; fixed 32-name scope substitutions; strict lowercase
`sha256:` digest and target parsing; G0 blocker/next-action validation before
the inventory return; and unconditional `g0-authoritative-proof-missing`
when only summary counts/digests are supplied
(`evidence_check.rs:1711-1721,2253-2257,2327-2345,3898-3928,4178-4205`).
These correctly close the corresponding historical b3/d9 false greens.

The intentional b11 fail-closed result also proves it is not yet an accepted
G0 checker: the current `G0InventoryEvidence` remains scalar summary data and
the checker now rejects it until typed independently collected workflow,
PR/check, dependency-graph, access, and model/session evidence is wired. The
remaining d9 release/provenance/host/child/phase gaps above are unchanged by
the b11 diff. In particular, no current b11 proof exists for required PR plus
resulting-main role coverage, check-run/App IDs and URLs, recursive child
closure, API terminal status, trusted host registration, G4/G5 hosted
comparison, G6 same-candidate/main parity, or external G7 attestation.

Independent historical constraint artifact verified in full (325 lines,
SHA-256
`0df74945c639900f752bd9d7dafa88cac368fdaca53b86ddc2c9ae4b6477ad04`):
`G0/checker-review/report.md`. It remains a b3/df historical review, not a
b11 implementation result; use it for acceptance constraints and line-level
citations only.

The current external dependency/access artifact is SHA-256
`f58da9d4ea2bc32ba8867cbb4897997bc48c6e4228a14ed4ec0710c056f67f60`,
`G0/fleet/dependencies-and-access.json`. It represents all 32 identities and
15 source-bound edges, but declares `status: inventory_only_not_gate` and
reports only 26/32 local exact checkouts, 1 pinned checkout mismatch, 4
report-bound rows, and 1 inventory-only row. Presence of this artifact does
not satisfy the checker: typed edge completeness, source/ref integrity,
access/model evidence, and API-bound workflow/check/run records remain
missing or explicitly unknown.

### Accepted architecture constraints versus b11 implementation

| Constraint | b11 status | Mandatory negative / API limitation |
| --- | --- | --- |
| Fixed 32 and G0 proof | Exact names/provider keys are enforced; summary-only G0 now fails closed. | Missing typed collector/model/access/graph records must fail. A count, digest, or `fleet.json` conversion cannot pass. |
| Live revision freshness | Snapshot/record equality and live mode exist; no fresh default/PR/API proof is supplied by this slice. | Stale default/PR SHA, moved head, offline-only invocation, API page/auth/query/hash failure must fail; G7 must reread current heads after collection. |
| PR candidate versus resulting main | Coverage requires records by `(repository, provider, PR/event)` plus push main, but does not encode tested-merge/merge-group trust roles or all PR check/App objects. | Omit one PR's checks, substitute contributor head for tested merge, or reuse one main row for PR/G3/G6; each must fail. Synthetic merge and check-App identity are not inferable from names. |
| Expected jobs/checks/children | Job names/IDs, required contexts, child count, and source fields are compared to supplied snapshot/manifest. | Expected plan must be independently derived; missing/extra/duplicate job, neutral/skipped status, missing recursive child/job/check/log, or opaque/unbound URL must fail. GitHub APIs may lack parent-run IDs; require verified dispatch/output/workflow-call edges or fail closed. |
| Provider/runner/host trust | Provider keyset is closed; host checks still use submitted kind/host/labels and no trusted registration/capability attestation. | Unknown provider, hosted/Velnor label spoof, wrong source/event, Mac/OrbStack/Docker/image/architecture mismatch must fail. Labels/actors/host strings are not proof. |
| G2 release/install | Typed canonical manifest, external digest, target/component bindings, operations, and service checks are present. Applicability waiver and provider/feed/tap external binding remain open. | Required/applicable release cannot be downgraded to install N/A; stale schema, missing component/asset, wrong source/tag, wrong feed/tap/release ID, same-version replacement, missing upgrade/switch, checkout/PATH fallback must fail. |
| G4/G5/G6 cross-lane | b11 requires two providers for stages that request both and compares some source/workload fields; it does not independently establish qualifying hosted comparison, same candidate/resulting-main, native-only obligations, or one publisher. | Velnor-only, wrong source/workload/target, or cross-lane manifest mismatch must fail; unsupported capability is blocked, not N/A success. |
| G7 independent review | Owner/reviewer strings must differ; b11 does not bind an external reviewer attestation over exact evidence digest in this slice. | Self-review, stale attestation, wrong manifest/snapshot digest, or no independent reviewer must fail. |

No unavailable API fact is invented in this review. Where GitHub cannot expose
the needed parent/check/checkout/provider identity, the acceptance result is
`blocked`/`not proven`, never inferred green from a URL, label, newest run, or
self-authored record.

## 570645 exact coverage-contract review

Exact pushed checker head:
`570645f0355a227a548180a880c1ccb553046b6e`, parent
`b11b57b73f0bae8b9a7edf530adf6938c83c5fa2`, remote-equal. Detached worktree
was clean. This is a bounded source/fixture review, not a gate approval.

Independent checks on the exact tree:

- `rtk cargo test -p velnor-tools --no-fail-fast` — **229 passed**.
- `rtk cargo fmt --all -- --check` — pass.
- `rtk cargo clippy -p velnor-tools --all-targets --all-features --locked -- -D warnings` — pass.
- `git diff --check b11b57b73f0bae8b9a7edf530adf6938c83c5fa2..570645f0355a227a548180a880c1ccb553046b6e` — pass.

Focused committed tests passed: `coverage_requires_default_branch_and_each_pr_subject`,
`immutable_pr_subject_positive_and_head_mutation_negative`,
`lane_pairing_requires_same_immutable_subject`,
`run_attempt_is_part_of_authoritative_lookup`,
`g7_attestation_requires_external_artifact_bindings`, and
`summary_inventory_positive_shape_stays_fail_closed_without_typed_proof`.

### Verified contract repairs

- `EvidenceRole` is explicit and strict: `inventory`, `default_branch`,
  `pull_request`, `merge_group`; missing field/role mismatch fails.
- Main coverage requires the snapshot's current default SHA in the record,
  trigger source, and checkout; PR coverage requires current number/head/base,
  tested merge SHA, event, and exact run attempt; merge-group coverage requires
  the merge-group SHA.
- Coverage is derived from snapshot PR/main subjects, not merely record keys.
  Lane pairing compares role, PR identity, source, checkout, and event.
- Exact run ID + attempt lookup is enforced. G7 reviewer attestations now
  require an external-artifact object shape. Every execution stage emits
  `authoritative-collector-required` until independent collector derivation is
  available.

### Own exact CLI probes

Using the existing typed fixture source, I added only temporary fields in
`/private/tmp` (no repository files changed):

| Probe | Exact command result | Finding |
| --- | --- | --- |
| G0 summary-only records with `evidence_role=inventory` | `evidence-check --stage G0` exited 1, `status=fail` | `g0-authoritative-proof-missing` |
| Same G0 records, but caller rewrote `dependency_graph_digest` to a valid `sha256:<64 lowercase hex>` value | exited 1, `status=fail` | still `g0-authoritative-proof-missing`; self-attested hash cannot green G0 |
| G1 records with `evidence_role=default_branch` but no collector-derived coverage | `evidence-check --stage G1` exited 1, `status=fail` | `authoritative-collector-required` |

The G0 result is the required fail-closed behavior: a valid-shaped count and
caller digest do not substitute for typed workflow/PR/check/dependency/access/
model evidence. The G1 result confirms execution stages cannot accidentally
pass from an offline/self-authored snapshot.

### Remaining exact-source gaps

1. **Collector remains missing.** The unconditional execution blocker is
   correct for this slice, but no live G0/G1/G3/G7 collector can yet provide
   raw request/page/auth/rate-limit/error provenance, complete ruleset/check
   Apps, workflow/action graph, jobs, artifacts/logs, checkout proof, or
   recursive child lineage. The referenced gap artifact is
   `G0/fleet/collector-contract-gap.md`, SHA-256
   `186e6a1ed70bea2588df77f9566e4e56ad9a14b65e3654960aeebb738f45c071`.

2. **Review artifact hashes are shape-checked, not independently verified.**
   `check_review_attestation` accepts any valid `source_tree_digest`,
   `source_diff_digest`, and `run_manifest_digest`; `source_revision` is only
   required to be a valid SHA, and `source_url` only starts with HTTPS. There
   is no external byte/object binding or exact reviewed-revision equality in
   this commit. The positive test only mutates a malformed hash (`"unbound"`),
   not a valid wrong digest. A valid-but-wrong hash/source revision/evil-host
   fixture must fail after the collector/attestation artifact contract lands.

3. **G2 release/install waiver remains.** The 570645 diff does not change
   `check_release_install`: a manifest `release_applicability: applicable`
   can still allow record-side install `not-applicable`/`excluded` with a
   justification. Required/applicable product packages need an authoritative
   install policy; records cannot waive it.

4. **External trust remains unproven.** Run/check/log URLs, provider/host
   identity, checkout contents, child producer edges, and APT/Homebrew
   publication identities remain caller/snapshot claims until independently
   collected and bound. Cross-lane pairing now compares subjects, but a
   self-authored equal source/manifest digest is not provider proof.

5. **G6/G7 semantic obligations remain incomplete.** The code compares some
   lane workload/source fields and producer manifest digest, but does not yet
   establish same qualifying candidate plus resulting-main, native-only
   obligations, a singular publisher from provider facts, or an external
   reviewer artifact over exact evidence bytes.

Disposition: **future-proof contract slice; not accepted and not a G0/G1/G3/
G6/G7 gate result**. No source/publication changes were made by this review.

## 54c9602 exact API-acquisition source review

Reviewed exact detached commit `54c9602bb75c344e21318e7d8097389f15c61e02`
(`codex/g3-api-acquisition`, remote equal, clean). The commit adds
`crates/velnor-tools/src/github_acquisition.rs` and only exposes it from
`main.rs`; it does not wire the seam into `evidence_live` or the checker.

### Verification

- `rtk cargo test -p velnor-tools --no-fail-fast`: **235 passed**.
- `rtk cargo test -p velnor-tools github_acquisition::tests:: -- --nocapture`:
  **11 passed**, including REST page-2 permission failure, full-page
  truncation, GraphQL cursor duplication, GraphQL permission/rate errors, and
  raw digest tamper rejection.
- `rtk cargo check -p velnor-tools`: exit 0, but the newly exposed module has
  unused/dead-code warnings.
- `rtk cargo clippy -p velnor-tools --all-targets --all-features --locked -- -D warnings`:
  **failed (66 errors)**, primarily because the module is exposed but unused
  (`VecDeque` plus dead-code diagnostics).
- `rtk cargo fmt --all -- --check`: **failed** on pre-existing formatting in
  `evidence_check.rs` and `main.rs`; no changed module formatting error was
  reported.
- `rtk proxy git show --check --format=fuller HEAD`: clean; commit contains
  DCO signoff and `Co-authored-by: Codex <codex@openai.com>`.

An external public-API harness at `/private/tmp/g3-api-harness` imported the
exact source without changing the review tree. `rtk cargo run --quiet` produced:

```text
host_drift_followed=true
credential_url_forwarded=true
raw_store_receives_secret_bytes=true
storage_ref_unbound=true
state=Complete
graphql_initial_host_drift_followed=true
```

### Findings

1. **Blocker — URL and Link targets are not host-bound (SSRF/auth leak).**
   `validate_rest_request`/`validate_graphql_request` only reject empty strings
   (`github_acquisition.rs:1523-1554`). `parse_next_link` accepts any nonempty
   URI (`:1727-1754`), and `collect_rest` assigns it directly to the next
   transport request (`:774-850`). The harness supplied an evil absolute Link
   containing `token=ghp_secret`; collection returned `complete=true` and the
   transport received the evil host and credential-looking query. An initial
   GraphQL endpoint on the evil host is likewise accepted. A production
   transport cannot be trusted to repair an untyped URL contract; require a
   fixed GitHub API origin, URI parsing/normalization, no userinfo/credential
   query, and redirect/next-link host binding before sending auth.

2. **Blocker — raw credential bytes and metadata are caller-controlled.**
   `RawObject` derives `Debug` and stores the unfiltered response bytes
   (`:157-166`); `RawObjectStore` receives them directly. The harness proved a
   response body containing `Authorization: ghp_secret` is retained by the
   store. Redaction only covers a small key/token heuristic
   (`:1822-1894`), while `endpoint_or_operation` is copied unredacted into
   `RequestRecord` (`:1668-1677`) and may contain query credentials. The
   accepted `AuthIdentity` is also a caller-supplied string plus heuristic
   secret check, not an independently fetched credential/viewer binding.
   Require an opaque auth handle, strict URL/query rejection, registered-token
   masking, and a raw-object policy that cannot persist secret-bearing bytes.

3. **Blocker — raw digest is not an external object/source binding.**
   `retain_raw` checks digest, length, and copied metadata against the bytes
   handed to the store (`:1605-1632`), but accepts any `storage_ref`; it does
   not verify that an immutable external object exists or binds the bytes to a
   repository, source revision, endpoint, response headers, and API request.
   The harness returned a valid digest with `store://unbound/caller-asserted`
   and the collection still completed. `RawObjectRef` therefore remains
   self-attested until a real immutable object store/collector verifies it.

4. **Blocker — revision reconciliation is not a full PR identity.**
   `RevisionIdentity` contains only repository, free-form `subject`, and
   optional head/base/tested-merge/merge-group SHAs (`:1425-1435`). It lacks
   typed PR number, head repository/fork, base branch, draft/bot/author/event,
   and independent source URLs/API object references; SHA strings are not
   validated. `key()` is only `repository:subject` (`:1437-1440`). Retaining
   opening/closing vectors does not independently acquire or reconcile full
   PR identities, so a producer can relabel a subject or omit identity fields.

5. **Major — generic pagination is not a complete collector contract.**
   REST correctly fails closed for a full page without Link, malformed Link,
   page cap, page-size overflow, and page-2 permission failure. GraphQL
   correctly requires `pageInfo`, cursor for `hasNextPage`, detects repeated
   cursors, and fails closed on page cap/errors. However, no hostile REST
   duplicate-with-different-query-order fixture exists; Link loop detection is
   string-based and host/query normalization is absent. The workflow-jobs
   request does not force an all-attempts filter (`:470-482`), unlike
   check-runs (`:412-427`), so run-attempt completeness remains a collector
   obligation. `RequestRecord` also has no typed repository/source identity.

6. **Major — no actual I/O/provenance integration.** The module defines only
   `AcquisitionTransport` (`:148-155`) and `RawObjectStore` (`:189-193`); no
   production implementation records raw request/page/auth/rate-limit/error
   provenance. `main.rs:6` merely exposes the module. `evidence_live.rs`
   continues using the older `FleetHttp`/`paginate` path and cannot consume
   `CollectionResult`, `RequestRecord`, or `RawObjectRef`. The generic REST and
   GraphQL helpers also do not traverse the required workflow/reusable-action,
   check-suite/job/artifact/log, child-run, checkout, or ruleset graph. This
   is a useful seam, not a live collector and not checker-570 integration.

7. **Major — request digest/source binding is incomplete.** REST hashes only
   canonical query pairs and GraphQL hashes query text/variables
   (`:1653-1666`); endpoint, GraphQL operation name, target repository, source
   SHA, and request headers are not part of the request digest. `request_id` is
   caller-derived from `collection_id` and page number (`:546-547`), so IDs and
   collection labels are not independently unique or source-bound. A collector
   must bind canonical request bytes plus target/source identity to the raw
   response object.

### Compatibility/disposition

Permission/rate-limit and pagination paths are directionally fail-closed in
the 11 focused tests, but that does not establish actual API trust. The seam
is not compatible with checker-570/b11 as a gate input because it is not wired
to `evidence_live`, has no concrete host-bound transport or immutable raw store,
and cannot derive the full nested evidence graph. **Source-only review:
reject for integration; no G0/G1/G3/G6/G7 approval and no publication claim.**
No source or publication files were changed by this review.

## cedd00c exact API-acquisition correction review

Reviewed exact detached commit `cedd00cb63c9f3a7fbe29b6ae6466458163fb01a`
(`codex/g3-api-acquisition`, remote equal, clean), parent
`54c9602bb75c344e21318e7d8097389f15c61e02`. This is a bounded seam review,
not checker or G0/G2/G7 approval.

### Verification

- `rtk cargo test -p velnor-tools --no-fail-fast`: **242 passed**.
- `rtk cargo test -p velnor-tools github_acquisition::tests:: -- --nocapture`:
  **18 passed**.
- `rtk cargo check -p velnor-tools`: exit 0, one unused `VecDeque` warning.
- `rtk cargo clippy -p velnor-tools --all-targets --all-features --locked -- -D warnings`:
  **failed (1 error)**: unused `VecDeque` import at
  `github_acquisition.rs:17`.
- `rtk cargo fmt --all -- --check`: **failed** on pre-existing formatting in
  `evidence_check.rs` and `main.rs`.
- Exact detached tree is clean and remote-equal; commit has DCO signoff and
  `Co-authored-by: Codex <codex@openai.com>`.

External public-API harness `/private/tmp/g3-api-harness`, importing the exact
source without changing the review tree, produced:

```text
evil_next_link_rejected=true
duplicate_link_rejected=true
evil_effective_redirect_rejected=true
raw_secret_masked=true
stored_digest_is_transformed=true
original_digest_exposed=false
unrecognized_ghs_credential_persisted=true
effective_url_debug_exposes_secret=true
unbound_storage_rejected=true
evil_graphql_endpoint_rejected=true
```

### Corrected behavior confirmed

- `GithubApiOrigin` rejects non-GitHub origins, userinfo, fragments, ports,
  and credential-looking query pairs; REST next Links are rebound to that
  origin before transport.
- `TransportResponse.effective_endpoint` is checked against the same origin;
  evil effective redirects are rejected before raw storage.
- REST duplicate Links with reordered query pairs are detected as one page.
- Storage references must be `sha256://<stored-digest>` and the store's
  `verify` hook must accept the returned reference; the prior arbitrary URI
  false-green is rejected.
- Registered and known `ghp_`/`github_pat_` material is masked before storage;
  `RawObject` debug output no longer prints body bytes.

### Remaining exact-source blockers

1. **Blocker — transformed digest is not the original API-response digest.**
   `retain_raw` replaces `object.bytes` with masked/JSON-re-serialized bytes
   and then hashes those bytes (`github_acquisition.rs:1817-1845,
   :1949-1991`). A changed object is labeled
   `redacted-raw-bytes-v1`, but `RawObjectRef` carries no original response
   digest, original length, or separate immutable raw-object reference. The
   harness proved `stored_digest_is_transformed=true` and
   `original_digest_exposed=false`. Downstream must not treat
   `RawObjectRef.sha256` as the exact GitHub response hash. Require an explicit
   pair: independently captured original digest/length and a separately typed
   safe redacted-object digest, with source/request binding for both.

2. **Blocker — credential masking is incomplete and response URL debug leaks.**
   Marker masking recognizes only `ghp_` and `github_pat_`
   (`:1869-1903`); `ghs_`, `gho_`, `ghu_`, and `ghr_` material in an ordinary
   JSON field passes through. The harness stored a `ghs_` token unchanged.
   `TransportResponse` Debug renders `effective_endpoint` verbatim
   (`:277-285`), and the harness showed a credential-bearing URL appears in
   its debug output even though collection later rejects it. Use the shared
   complete GitHub token marker set/registered-token masker and redact or
   structure URLs before any debug/error path.

3. **Major — storage verification remains a caller contract, not actual
   external proof.** The exact `sha256://` URI and `verify` call
   (`:1980-1990`) prevent the prior obvious false-green, but a caller-owned
   store can return the right URI/digest and self-approve without proving an
   immutable object exists. No production store or transport is present; the
   seam still cannot establish actual bytes, source, endpoint, response
   headers, or request identity independently.

4. **Major — source identity/digest remains incomplete.** The request digest
   now includes endpoint plus query/document (`:2012-2025`, `:2099-2105`), but
   GraphQL operation name, repository/source SHA, request headers/auth scope,
   and API response identity are not bound into it. `RequestRecord` still has
   only free-form endpoint and caller-derived request IDs. A collector must
   bind canonical request bytes and target/source identity to each external
   object.

5. **Major — actual I/O, full PR identity, nested graph traversal, and all
   attempt acquisition remain absent.** The seam is still not wired into
   `evidence_live`/checker-570; no concrete authenticated/no-redirect
   transport or external raw store exists. `RevisionIdentity` remains the
   previous free-form subject plus optional SHAs, not a typed full PR identity.
   Generic helpers do not derive workflow/reusable-action, ruleset/check-app,
   jobs/logs/artifacts, checkout, recursive child-run, or provider graph. The
   workflow-jobs request still does not force all attempts, unlike check-runs.

Disposition: **endpoint/redirect/query and duplicate-link hardening passes the
bounded hostile fixtures, but the corrected acquisition module is not accepted
as an authoritative raw/provenance source** until original-vs-redacted digest
semantics and complete credential masking/log safety are fixed. No gate,
publication, live-I/O, PR-identity, or nested-collector claim. No source or
publication files were changed by this review.
