# GitHub-first dual-lane evidence schema v2

`velnor-tools evidence-check` is a fail-closed verifier. Offline mode consumes
three independent machine-readable inputs; trusted live mode receives the
producer's verified raw-store handle in-process:

```text
velnor-tools evidence-check \
  --stage G3 \
  --manifest manifest.json \
  --snapshot snapshot.json \
  --evidence records.json \
  --live \
  --release-manifest application-manifest.json
```

The command shape above is the collector integration contract. In the current
checker revision `--live` intentionally fails closed: no authenticated
collector/current-API closing reconciliation is wired yet, so caller-owned
JSON and CAS files cannot authorize a gate. Offline invocation is
validation-only and also cannot authorize a gate. This is an explicit blocker,
not a successful G0/G7 result.

The reviewed workload manifest is source authority for the fixed 32-repository
scope and generated workload plan. The snapshot is an independently collected
GitHub fact set. Evidence records only bind claims to identities found in that
source. `gate_status`, a badge, an aggregate conclusion, a display name, or a
caller-supplied boolean never authorizes a pass.

Schema v2 is strict: every object has `deny_unknown_fields`; field aliases,
nullable-row normalization, legacy fallbacks, and flat release/install aliases
are not accepted. Parse failure is failure. No credentials, bearer tokens, or
authentication headers may occur in any source/provenance field.

Offline validation recomputes supplied base64 bytes and checks canonical
`sha256://<hex>` references, but does not open a caller-selected CAS path.
Only the producer-owned live store may reopen and rehash safe/original bytes;
its verified handle is passed through the authenticated capture. The checker
never treats a URI or caller digest as proof, follows a caller path, or
fetches arbitrary network URLs.

## Reviewed workload manifest

The manifest is external to the generic workflow generator. It must contain
`schema_version: 2`, a non-empty `manifest_id`, a reviewed source identity, and
exactly these repositories, once each:

```text
tailrocks/velnor
tailrocks/velnor-apt
tailrocks/parallax
tailrocks/tracing-request-level
tailrocks/termrock
tailrocks/termpane
tailrocks/tablerock
tailrocks/schemalane
tailrocks/ruxel
tailrocks/pg-bigdecimal
tailrocks/parallax-telemetry-playground
tailrocks/homebrew-tablerock
tailrocks/homebrew-ruxel
tailrocks/homebrew-parallax
tailrocks/homebrew-holla
tailrocks/holla-apt
tailrocks/holla
tailrocks/homebrew-velnor
tailrocks/tailrocks-typescript-skills
tailrocks/tailrocks-skill-authoring-skills
tailrocks/tailrocks-rust-skills
tailrocks/tailrocks-roadmap-skills
tailrocks/tailrocks-pull-request-skills
tailrocks/tailrocks-open-source-skills
tailrocks/tailrocks-macos-skills
tailrocks/tailrocks-code-quality-skills
jackin-project/jackin
jackin-project/jackin-agent-smith
jackin-project/homebrew-tap
jackin-project/jackin-the-architect
jackin-project/jackin-sentinel
jackin-project/jackin-role-action
```

The source object is `{repository, revision, digest, reviewed_by}`. Revision
is a 40-hex SHA and digest is a SHA-256 content digest. Each repository row
contains the generated workload/platform matrix, non-empty `expected_jobs`,
`generated_plan_digest`, workflow path/revision, generator/runtime pins,
provider eligibility, typed provider host contracts, and explicit
`release_applicability`. `expected_jobs` is the reviewed generated plan; it is
not copied from an evidence record.

Each expected job identifies `job_id`, workload, provider, platform,
architecture, `required: true`, and (when applicable) a typed
`child_workflow` containing repository, workflow path, and `workflow_run`
event. Provider eligibility is one of `eligible`, `not-applicable`, or
`excluded`; excluded providers are not successful execution.

## Authoritative snapshot

The snapshot must contain `schema_version: 2`, `snapshot_id`, the exact
`manifest_id`, an RFC3339 UTC observation time, and source metadata:

```json
{
  "collector": "velnor-tools/evidence-live",
  "collector_revision": "...",
  "api_base": "https://api.github.com",
  "captured_at_utc": "2026-09-20T12:00:00Z",
  "read_only": true,
  "page_count": 123,
  "permission_scopes": ["metadata:read", "actions:read"]
}
```

Source metadata is provenance, not proof by itself. The producer-owned live
collector must perform the fresh read-only API collection before writing the
typed capture consumed by `--live`; the checker then rejects inaccessible
permissions, rate limits, malformed responses, missing pagination, or a
revision/PR/ruleset/run/job mismatch in that capture. G7 always requires
`--live`. Offline validation is a fixture/schema check and can never establish
G7.

Every repository row records the GitHub numeric repository ID, actual default
branch and SHA, complete ruleset status-check/app-ID inventory, workflow path
and immutable revision inventory, all current open PRs (including drafts/bots),
an explicit artifact census, and separate execution arrays for the current main
tip and each PR. Each artifact row binds `{artifact_id, run_id, run_attempt,
run_head_sha, name, digest, expired, source_url, raw_object_refs}`. The checker
requires unique IDs and names, a non-expired artifact, a known source/run
identity, the canonical GitHub API artifact URL, and a raw object whose object
kind is `workflow_artifacts`. The row's `digest` is the downloadable archive
digest reported by the API member; it is distinct from the SHA-256 of the
paginated API response bytes in the referenced raw object. The checker binds
the row's ID, name, expiry, archive digest, run/head identity, and download URL
to an actual member of that captured page, then verifies the page-byte digest
separately.

Ruleset checks are `{context, app_id}` with a source URL and complete-page
marker. A PR row is `{number, state: "open", head_sha, base_sha, merge_sha,
source_url, executions}`. The checker requires all current PR identities and
the resulting main revision for execution gates; a PR run cannot substitute for
post-merge main evidence.

An execution observation records run ID/attempt/URL, workflow path/revision,
event, trigger and actual checkout SHAs, status, conclusion, provider, and
typed runner identity (`runner_kind`, `host_id`, labels). It contains the
authoritative job inventory, required check observations, and recursive child
run graph. Jobs and checks must carry source URLs, IDs, status, and conclusion.
Missing, queued, canceled, timed-out, neutral, skipped, or failed work fails.

## Evidence envelope

The envelope has `schema_version: 2`, exact `manifest_id`, exact `snapshot_id`,
the explicit checker stage, and records. A record repeats the section-10
identity fields, but those repeats are compared to manifest/snapshot source:

```text
repository, repository_role, evidence_role, default_branch, default_branch_sha,
observed_at_utc, generator_revision, runtime_product_id,
generator_artifact_digest, configuration_digest, generated_tree_digest,
scan_state_digest, runtime_release_version, runtime_source_sha,
job_image_digest, expected_workload_ids,
required_check_contexts_and_apps, workload_platform_architecture,
provider_eligibility, justified_exclusions,
pr_number, pr_head_sha, pr_base_sha, tested_merge_sha, merge_group_sha,
workflow_path, workflow_revision, event, run_id, run_attempt, run_url,
trigger_source_sha, actual_checkout_sha, provider, runner_name, host_id,
runner_kind, runner_labels, run_status, run_conclusion,
expected_jobs, actual_job_ids, actual_job_conclusions, logs,
child_run_links, required_checks,
release, install, owner, reviewer, gate_status, blocker, next_action
```

`evidence_role` is one of `inventory`, `default_branch`, `pull_request`, or
`merge_group`. It is required; it is not inferred from a nullable PR number.
Execution coverage is derived from the authoritative snapshot, never from the
record list:

| Role | Required immutable subject | Required event | Coverage obligation |
| --- | --- | --- | --- |
| `default_branch` | repository + current default-branch SHA + run ID/attempt | `push` | resulting current main, separately from every PR |
| `pull_request` | PR number + head/base SHA + tested merge SHA + run ID/attempt | `pull_request` | every current open PR, including draft/bot/fork rows |
| `merge_group` | PR number + head/base SHA + merge-group SHA + run ID/attempt | `merge_group` | every required merge-group candidate when captured |

The checker rejects role/subject mismatches, duplicate immutable identities, and
PR records that omit the current head/base/candidate. G6/G7 additionally pair
GitHub and Velnor records only when role, PR subject, source SHA, checkout SHA,
and event all match; two unrelated green rows are not a comparison. The
collector must independently derive these subjects and the expected job/check/
child sets before execution coverage can pass. Until that collector contract is
complete, execution stages return an explicit authoritative-collector blocker.

Coverage authority map (implementation boundary):

| Gate | Authoritative source | Typed identity/reference | Collector obligation | Validator/negative fixture | Applicability |
| --- | --- | --- | --- | --- | --- |
| G0 | reviewed 32-row manifest + fresh GitHub inventory | repository ID, default SHA, every PR head/base, ruleset context/app, workflow/content SHA, graph/model/access artifact digests | paginate and retain raw query/page/provenance; no count-only summary | exact scope, missing PR/check/workflow/graph/model/access, stale snapshot | inventory only; no execution pass |
| G1/G3 | fresh snapshot plus independently parsed workflow/run graph | `default_branch` and `pull_request`/`merge_group` subjects, run ID/attempt, source/event/checkout | derive expected jobs/checks/children from workflow revision and current ruleset | PR/main substitution, wrong head/base/merge, duplicate subject, queued/manual/unbound run | blocked until collector derivation is complete |
| G6 | the same qualifying PR candidate and resulting main in both lanes | paired role + PR identity/source/workload/target contract; one publisher digest | independently associate both provider runs and native-only obligations | unrelated green rows, source/workload mismatch, publisher rebinding | blocked until cross-lane association is collected |
| G7 | producer fresh-live capture plus independent review artifact | exact manifest/snapshot/evidence digest, source tree/diff/run-manifest digests | producer rereads default branch and every PR head; checker binds the typed capture and reviewer artifact externally | owner/reviewer-only, stale digest, missing artifact binding | `--live` and external attestation required |

The current read-only collector gap is recorded at
`dual-lane-evidence/G0/fleet/collector-contract-gap.md` (SHA-256
`186e6a1ed70bea2588df77f9566e4e56ad9a14b65e3654960aeebb738f45c071`): its v1
REST output lacks raw query/auth/page provenance and complete ruleset,
check-suite, job, artifact/log, workflow-graph, merge-group, and child-lineage
objects. The checker therefore emits `authoritative-collector-required` for
execution stages; this schema is not a claim that live collection is complete.

### Collector/store integration checkpoint

The checker and producer are separate owners. Current checkpoints are recorded
here so an integration cannot silently combine incompatible contracts:

| Boundary | Current producer checkpoint | Checker requirement | Status |
| --- | --- | --- | --- |
| API acquisition and collection | `origin/codex/g3-live-collector` `188fed1c` | authenticated read-only transport, complete request/page ledger, raw response references including original bytes, opening and closing default/PR rereads, and source-derived workflow/run/job/check/child graph | mapper now emits the current nested source/dependency/check/graph fields; it still does not register an in-process checker adapter or prove a full live gate |
| Raw-byte publication | `origin/codex/g3-raw-store` `7bfe06f9` | one production `RawObjectStore` using `sha256://<lowercase-64-hex>`, measured original response bytes, descriptor-relative reopen/rehash, and immutable sidecar/reference binding | combined integration still needs the canonical storage-reference helper visibility and checker-side consumption of the producer store; independent review remains pending |
| Checker validation | `4903c337bae2b2d4f8a2f0ccaeb25a3d50d95a1e` | strict `g0_contract.rs` parsing and deterministic comparison against reviewed scope/source; `--live` must receive a producer-authorized capture | recursive child matrix collapse is rejected; live path intentionally fails closed until producer adapter integration and independent review |

The integration target is one in-process path, not a second JSON normalizer:

1. The collector owner keeps `github_acquisition.rs`, `github_transport.rs`,
   `github_live_collector.rs`, `g0_live_mapping.rs`, and `github_live_cli.rs`.
   The collector must return a typed capture whose source-derived plan contains
   all non-empty expected jobs, recursive child edges, required check/app
   identities, and every raw request/page reference. It must reread the default
   branch and every open PR at close, then reject any changed identity.
2. The raw-store owner keeps the producer `github_raw_store.rs` and its
   acquisition integration. `RawObjectStore::store(RawObject)` must measure
   bytes supplied by the authenticated transport; `verify(&RawObjectRef)` must
   reopen the same descriptor-relative object and sidecar. A caller-supplied
   `original_sha256`, URI, or path is metadata until verification succeeds. No
   checker-local CAS implementation may substitute for this store. The
   checker no longer carries a second path-based raw-store implementation.
3. The checker owner keeps `g0_contract.rs`, `g0_workflow.rs`, and
   `evidence_check.rs`. The eventual adapter must pass the producer's typed
   capture directly to these types, verify measured CAS bytes, and call the
   existing deterministic document checks. It must not reconstruct expectations
   from result records or accept a capture merely because its timestamp, digest,
   or `--live` flag looks fresh.

The checker now exposes the producer seam in
`crates/velnor-tools/src/live_authority.rs`. The minimum handoff API is:

```text
AuthenticatedClosingCollector::collect_closing(
    ClosingCaptureRequest { stage, reviewed_manifest }
) -> AuthenticatedClosingCapture {
    manifest: ManifestDocument,
    snapshot: SnapshotDocument,
    evidence: EvidenceDocument,
    release: Option<CanonicalReleaseDocument>,
    raw_store: VerifiedRawStoreHandle,
}
```

`AuthenticatedClosingCapture` is producer-owned and must not be constructible
from deserialized caller JSON. Its producer must bind the authenticated
viewer/scopes, API request identities, complete pagination, opening/closing
revision sets, source-derived expectations, and measured raw objects before
constructing it. `VerifiedRawStoreHandle` must call the production
descriptor-relative store's reopen/rehash verification. A future checker
integration may accept only this in-process value; offline files continue to use
`check_paths` for parser/rejection tests and cannot authorize G0 or G7.

The producer installs its adapter once with
`live_authority::install_collector(Box<dyn AuthenticatedClosingCollector>)`
during process setup. Registration is immutable and process-local; no command
argument, JSON field, timestamp, digest, or caller path can install or
replace the authority. If setup does not register the authenticated collector,
`--live` returns the explicit capability error.

Do not cherry-pick either checkpoint wholesale into the checker branch. Their
collector and checker contracts diverged from the strict c032 types. The next
producer commits must publish the adapter and store guarantees above, with
hostile tests for stale closing heads, missing PRs/pages/jobs, wrong
request/object bindings, raw-store replacement, and unauthenticated or
caller-authored `--live` input. Until then, `check_paths_live` remains an
explicit fail-closed boundary.

### Typed G0 collector handoff

`evidence.g0_inventory` accepts one strict `G0InventoryEvidence` object from
`crates/velnor-tools/src/g0_contract.rs`. This is a collector handoff, not a
result-record summary. Every contract type denies unknown fields; legacy
scalar inventory fields, aliases, and opaque success booleans are rejected.
`collector_snapshot_bytes_base64` must decode to the canonical JSON bytes of
`collector_snapshot` (sorted object keys), and
`collector_snapshot_sha256` is recomputed from those supplied bytes. The
`collector_snapshot_storage_ref` must be an immutable content-addressed
`sha256://<hex>` reference to those exact
bytes. The digest stays outside the object to avoid a hash cycle; a digest or
storage path over a parsed caller object, without the supplied bytes, is not
accepted.

The snapshot carries collector revision, read-only GitHub API identity, safe
viewer scopes, rate-limit observation, complete request/page state, and
request-bound raw-object hashes. It then carries exact fixed-32 repository
rows with independently observed default branch/SHA, complete ruleset
contexts/apps, workflow/reusable-action/scanner inventory, generated-state
artifact reference, every current open PR (including draft/fork/bot rows),
workflow bindings, required-check producers, and immutable raw references.
It also carries unchanged pre/post branch/PR reconciliation, a source-bound
workload/child/release/package/check dependency graph, effective Astra/low and
Luna/max model-session settings, complete gap-free repository access
observations, and a hashed workload artifact reference.

Each request must be a successful complete page with the read-only HTTP
method, repository-bound endpoint or GraphQL operation, base64-encoded
canonical query/variables, recomputable query/variables digests, API request
identity, the canonical `collector.auth` and `collector.rate_limit`
references, page metadata, and a raw response object. Raw object safe bytes
are supplied in base64 and their byte length and digest are recomputed; every
raw object storage reference is digest-addressed (`sha256://<hex>`) and must
match the measured bytes. A raw object also carries
`original_sha256`, `original_byte_length`, and `original_storage_ref` for the
exact authenticated provider response before credential masking. The
producer's canonical CAS keeps that original response under its typed
`original` namespace; the producer's verified live handle reopens and rehashes
it before authority reaches the checker. Offline validation checks the typed
bytes and references only; it cannot authorize a gate. A caller-supplied
original digest or URI is never proof. `has_next_page`
requires the next captured page in the same canonical stream, with contiguous
page number/cursor and an exact `https://api.github.com` URL/path/query link;
forbidden, rate-limited, malformed, truncated, unknown, or missing pages fail
closed. Every nested reference must resolve to a raw object from the same
snapshot.
PR check producers bind positive check-suite/check-run/workflow-run/job IDs,
attempt, source SHA, actual checkout SHA, exact `pull_request` event,
completed-success status/conclusion, required check app identity, and a
repository-bound GitHub URL. The checker independently compares collector
default branches, all current PR heads/bases/tested merges, workflow/run
identities, and required check tuples with the regular snapshot and reviewed
manifest; collector claims cannot create an expected job or check.

Each workflow row carries one immutable `source` blob with repository/path,
blob revision, commit SHA, repository-bound URL, media type, byte length,
base64 bytes, recomputed byte digest, an external `storage_ref` for the
descriptor-relative CAS object, and raw-object references. The checker
parses those bytes with the existing YAML parser. It derives non-empty job
IDs, provider/platform/architecture targets, trigger events, and recursive
`uses` dependencies; every referenced reusable workflow/action source must be
present as another immutable dependency blob. `workflow_run` and
`workflow_dispatch` triggers become explicit child obligations. The derived
job/workload set and targets must equal the reviewed manifest's expected plan,
and every reviewed child obligation must be present in the source-derived
edges. Recursive edges retain root workload, child source SHA, and parent
repository/workflow/source identity. Runtime child rows must bind their
`parent_run_id` to the root run or an independently observed intermediate
child run; matching a SHA alone is not a parent association. Jobs, child runs,
or expected checks observed only in result records cannot create an
expectation.

The `source_jobs` array is mandatory for every workflow. Each row binds
`{job_id, workload_id, provider, platform, architecture, required,
raw_object_refs}` and must exactly equal both the reviewed expected-job plan
and the jobs derived from the immutable source bytes. An empty, duplicated,
target-mismatched, or unbound source-job row fails G0; observed run jobs cannot
fill a missing source obligation.

The current v2 row has no concrete matrix-assignment identity. A finite matrix
that expands one logical job into multiple instances therefore fails closed
until the reviewed plan and source-job contract enumerate those instances; the
checker never collapses them into one self-attested row.
Source derivation accepts only literal `if` conditions and unconditional event
maps (plus input declarations for `workflow_call`/`workflow_dispatch`).
Branch/path/type/workflow filters, dynamic conditions, and
`continue-on-error` are explicit unsupported blockers; they are never treated
as unconditional coverage.

The dependency graph is also typed source evidence, not a summary digest.
Every node and edge has an immutable source SHA/ref, raw-object references,
known node/relation kinds, and no dangling or duplicate identities. Required
workloads must have required check edges; reviewed child/release/package
obligations require their corresponding edges. Graph source rows must match
the reviewed workload revisions or the observed default-branch SHA. Missing
edges, illegal cycles, or a source/relation mismatch fail closed.
Edges carry separate `source_sha/source_ref` and
`target_source_sha/target_source_ref` bindings. An edge source digest alone
cannot authorize an unrelated target revision.

The same external-storage rule applies to generated-state and workload
artifacts: `storage_ref` must address the exact measured `sha256` bytes of a
raw object referenced by that artifact. The producer's verified live handle
reopens each source and artifact object before authority reaches the checker;
a caller-provided base64 field, digest, URI, or parsed-object reserialization
cannot substitute for that read. Typed workload-to-check/package/release
edges must also preserve the source repository and workload identity; a node
from another repository is not a valid endpoint merely because its digest and
ID are present.

`--live` will have one canonical input path once collector integration lands:
a producer-owned authenticated capture in `evidence.g0_inventory`, plus its
verified raw-store handle. The checker must revalidate the capture
identity, request/page chain, raw response bindings, source-derived workflow
graph, current repository/PR/check inventory, and measured CAS bytes against
the collector's authority binding. A typed capture, fresh timestamp, or
self-consistent caller file is not that binding. Until the authenticated
read-only API collector and closing-head reconciliation are wired, `--live`
rejects before reading caller files. Offline fixtures prove only parser and
rejection behavior and cannot declare G0 or G7 completion. Execution stages
remain explicitly blocked until the producer supplies the separately derived
run/job/check/child coverage contract.

The envelope also has optional `reviewer_attestation`; G7 requires
`{reviewer, report_digest, manifest_id, snapshot_id, attested_at_utc, artifact}`
with an external artifact binding source repository/revision, source tree and
diff digests, run-manifest digest, and immutable source URL. Distinct owner and
reviewer strings are not an attestation. The attestation is an external review
artifact, not a `gate_status` claim from a result row.

Records must bind to an authoritative run by exact run ID and attempt. Main
records check out the current default-branch SHA. PR records distinguish
contributor head/base from the current synthetic merge candidate and must use
that candidate as the checkout. Merge-group records bind one immutable merge
group SHA. Workflow path/revision, event, source URL, status, conclusion, job
IDs, check app IDs, status, conclusion, event, and child-run identities are independently reconciled.

Expected jobs are compared to the reviewed plan, actual job IDs and target
platform/architecture come from the snapshot run, and required check contexts
come from the current ruleset. A record cannot replace omitted work with an
empty matrix or self-declared expected IDs. GitHub-hosted execution requires a
concrete hosted runner binding; Velnor execution requires `velnor-managed` and
rejects `github-hosted`. Provider labels and host identity are checked against
the reviewed typed host contract.

## Canonical release and install evidence

G2+ requires `--release-manifest` whenever the reviewed repository's
`release_applicability` is `required`. It is a producer-owned document:

```json
{
  "schema_version": 1,
  "manifest": {
    "schema": "velnor.application-manifest.v1",
    "product_id": "velnor",
    "channel": "stable",
    "version": "0.1.1",
    "source_repository": "tailrocks/velnor",
    "source_ref": "refs/tags/v0.1.1",
    "source_commit": "...",
    "release_tag": "v0.1.1",
    "release_id": "...",
    "producer": {
      "repository": "tailrocks/velnor",
      "workflow_path": ".github/workflows/release.yml",
      "run_id": 123,
      "run_url": "https://github.com/tailrocks/velnor/actions/runs/123",
      "source_commit": "..."
    },
    "artifacts": [],
    "components": [],
    "targets": []
  },
  "manifest_sha256": "sha256:..."
}
```

The checker computes canonical JSON bytes with sorted object keys and verifies
the external SHA-256. The digest is outside the canonical manifest, avoiding a
hash cycle. Artifacts, components, target/platform/architecture/service
inventory, producer run, source/tag/version/channel, and asset digests must
match. A missing component, rebound digest, unsupported target, source/tag
mismatch, or stale schema fails.

The evidence `release` object is typed: producer release identity, named asset
digests, APT repository/revision/suite/candidate projection, and Homebrew
tap/revision/formula/version projection all bind the canonical manifest
digest. Free-form strings cannot stand in for publication identity.

The evidence `install` object is typed. It requires a clean target environment,
successful clean install, successful same-channel upgrade from a distinct older
release identity, successful channel switch from a distinct channel identity,
installed product identity, component/artifact/target-bound binary digests and
absolute installed paths, functional success, and service applicability/result.
Checkout/PATH fallback is rejected. A target whose canonical manifest says a
service is required must report real `systemd-success`; `not-applicable` needs
an authoritative target applicability and justification. Required release or
install evidence cannot be downgraded to N/A by the record.

The producer owns one canonical product manifest; APT and Homebrew are
subordinate projections linked by parent digest. No self-hash or stale fallback
is accepted.

## Gate and failure semantics

The checker sorts findings deterministically and exits non-zero for any
finding. Required negative fixtures include stale SHA, skipped job, missing
repository, wrong provider/host, failed child run, mismatched artifact,
component/target omission, digest rebinding, architecture mismatch, unsupported
service, absent/same-version upgrade, source/tag mismatch, malformed
producer/consumer provenance, checkout/PATH binary fallback, missing current PR
coverage, and stale snapshot schema. A valid fixture only proves checker
coverage; it is never a claim that G7 is achievable or complete.
