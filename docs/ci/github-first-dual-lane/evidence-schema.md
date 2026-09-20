# GitHub-first dual-lane evidence schema v2

`velnor-tools evidence-check` is a fail-closed verifier. Offline mode consumes
three core machine-readable inputs: manifest, snapshot, and evidence. Every G2+
offline invocation also requires a release manifest. The following is an
offline validation invocation:

```text
velnor-tools evidence-check \
  --stage G3 \
  --manifest manifest.json \
  --snapshot snapshot.json \
  --evidence records.json \
  --release-manifest application-manifest.json
```

Offline invocation reads the supplied files, validates their shape and internal
consistency, then always fails with `offline-validation-only`; it cannot
authorize a gate. The `--live` path is separate: it forwards only the stage and
manifest path as a reviewed-source selector, and expects the producer to return
the snapshot, evidence, release document, and verified raw-store handle. The
current checker has no installed authenticated collector or closing API
reconciliation, so `--live` fails before reading caller files. This is an
explicit blocker, not a successful G0/G7 result.

The reviewed workload manifest is intended authority for the fixed
32-repository scope and generated workload plan. The current checker does not
load the enriched projection from a pinned reviewed source or recompute its
exact digest, so it emits `manifest-projection-unverified` and cannot authorize
a pass. The snapshot and typed G0 rows are consistency inputs until a registered
producer collection binds them to provider responses. `gate_status`, a badge,
an aggregate conclusion, a display name, or a caller-supplied boolean never
authorizes a pass.

Schema v2 is strict: every object has `deny_unknown_fields`; field aliases,
nullable-row normalization, legacy fallbacks, and flat release/install aliases
are not accepted. Parse failure is failure. The producer contract forbids
credentials, bearer tokens, and authentication headers in source/provenance
fields. The checker does not verify `secret_excluded`, scan GraphQL variable
values, or search the supplied safe/original raw bytes for credentials; those
producer claims remain unauthenticated until a trusted redaction path is wired.

Offline validation recomputes supplied base64 bytes and checks canonical
`sha256://<hex>` references, but does not open a caller-selected CAS path.
No producer-owned live store is wired into this checker revision. A future
authenticated integration must reopen and rehash safe/original bytes through
the producer's verified handle. The checker never treats a URI or caller
digest as proof, opens a filesystem path named by a CAS/storage reference, or
fetches arbitrary network URLs. This does not prevent offline `check_paths`
from reading the local manifest, snapshot, and evidence files its caller
supplies. Every G2+ offline invocation also requires
and reads a caller-supplied release manifest. In live mode, the
`--release-manifest` path is not passed to the collector; the producer supplies
the release document.

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

Source metadata is provenance, not proof by itself. The collector described by
this contract is not integrated: `--live` currently fails before reading
caller files. When an authenticated collector is integrated, it must perform
fresh read-only API collection and closing reconciliation before creating the
typed capture. G7 always requires `--live`; offline validation is a fixture
and schema check and can never establish G7.

Each repository row claims the GitHub numeric repository ID, default branch
and SHA, ruleset status-check/app-ID inventory, workflow revisions, open PRs,
artifact census, and separate execution arrays for the main tip and PRs. The
current checker checks typed shape and cross-input consistency; it does not
parse and join all row claims to subject-specific provider responses. In
particular, positive check-suite/check-run/job IDs are not provider evidence.

An artifact row carries `{artifact_id, run_id, run_attempt, run_head_sha, name,
digest, expired, source_url, raw_object_refs}`. The list endpoint response and
the artifact archive are different objects: the row's `digest` is the
provider-reported archive checksum, not the hash of the JSON listing response.
The current checker does not parse listing rows, prove the listed run attempt,
download the ZIP bytes, or join `source_digest` to source bytes. Trusted live G0
therefore emits `g0-artifact-row-unverified`,
`g0-artifact-attempt-unverified`, `g0-artifact-archive-unverified`, and
`g0-artifact-source-digest-unverified`.

Ruleset checks are `{context, app_id}` with a source URL and complete-page
claim; the current checker does not parse each check from the ruleset response
or prove the rule's target, enforcement, or conditions make it active for the
branch. The capture has no classic branch-protection required-status-check
response, so listing rulesets cannot establish the effective required-check
set. The checker does not yet emit a dedicated
`required-check-inventory-unverified` finding; G0 must retain that blocker
until active/applicable ruleset checks and classic branch-protection contexts
with app IDs are collected and joined. The general
`g0-api-row-response-unverified` finding also blocks provider-row authority.
A PR row is `{number, state: "open", head_sha, base_sha, optional tested_merge_sha,
source_url, executions}`. G0 cross-checks claimed PR workflow/check source SHAs
against the current PR head and rejects duplicate run/attempt identities in
the regular snapshot. It does not authenticate the PR/check rows or infer a
checkout from a merge candidate. Execution-stage requirements are separate; a
PR run cannot substitute for post-merge main evidence.

An execution observation claims run ID/attempt/URL, workflow path/revision,
event, trigger and actual checkout SHAs, status, conclusion, provider, and
typed runner identity (`runner_kind`, `host_id`, labels). It contains the
claimed job inventory, required check observations, and recursive child run
graph. Jobs and checks must carry source URLs, IDs, status, and conclusion.
The current checker can compare these claims structurally but has no
authenticated execution collector; `--live` fails closed before reading them.

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
The rows below describe execution evidence (G1+). G0 inventory requires each
current PR's head and base SHA; a missing tested-merge candidate is valid and
cannot prove a run's checkout. Execution coverage is derived from the
authoritative snapshot, never from the record list:

| Role | Required immutable subject | Required event | Coverage obligation |
| --- | --- | --- | --- |
| `default_branch` | repository + current default-branch SHA + run ID/attempt | `push` | resulting current main, separately from every PR |
| `pull_request` | PR number + head/base SHA + tested merge SHA + run ID/attempt | `pull_request` | every current open PR, including draft/bot/fork rows |
| `merge_group` | PR number + head/base SHA + merge-group SHA + run ID/attempt | `merge_group` | every required merge-group candidate when captured |

The checker rejects role/subject mismatches, duplicate immutable identities, and
PR records that omit the current head/base or a candidate required by that
execution stage. G6/G7 additionally pair
GitHub and Velnor records only when role, PR subject, source SHA, checkout SHA,
and event all match; two unrelated green rows are not a comparison. The
collector must independently derive these subjects and the expected job/check/
child sets before execution coverage can pass. Until that collector contract is
complete, execution stages return an explicit authoritative-collector blocker.

Coverage authority map (implementation boundary):

| Gate | Authoritative source | Typed identity/reference | Collector obligation | Validator/negative fixture | Applicability |
| --- | --- | --- | --- | --- | --- |
| G0 | fixed repository allowlist; manifest projection and live API inventory remain unverified | repository ID, default SHA, every PR head/base, ruleset context/app, workflow/content SHA, graph/model/access artifact digests | authenticated response parsing, exact source joins, complete pagination, and reviewed manifest binding | structural consistency only; explicit blockers prevent certification | blocked |
| G1/G3 | fresh snapshot plus independently parsed workflow/run graph | `default_branch` and `pull_request`/`merge_group` subjects, run ID/attempt, source/event/checkout | derive expected jobs/checks/children from workflow revision and current ruleset | PR/main substitution, wrong head/base/merge, duplicate subject, queued/manual/unbound run | blocked until collector derivation is complete |
| G6 | the same qualifying PR candidate and resulting main in both lanes | paired role + PR identity/source/workload/target contract; one publisher digest | independently associate both provider runs and native-only obligations | unrelated green rows, source/workload mismatch, publisher rebinding | blocked until cross-lane association is collected |
| G7 | producer fresh-live capture plus schema-required reviewer-artifact claim | exact manifest/snapshot/evidence digest, source tree/diff/run-manifest digests | producer rereads default branch and every PR head; checker checks claim formats, fixed source repository, and supplied manifest/snapshot IDs only | API capture, artifact bytes/signature, reviewer authorization, and source binding are unauthenticated | blocked |

The historical read-only collector gap is recorded in reviewed evidence
commit `eda33aee9d7185052ea3f359b5b3755d7115a0a6` at
`evidence/github-first-dual-lane/updates/20260919T211933Z/reports/G0/fleet/collector-contract-gap.md`
(SHA-256 `186e6a1ed70bea2588df77f9566e4e56ad9a14b65e3654960aeebb738f45c071`):
its v1 REST output lacks raw query/auth/page provenance and complete ruleset,
check-suite, job, artifact/log, workflow-graph, merge-group, and child-lineage
objects. The checker therefore emits `authoritative-collector-required` for
execution stages; this schema is not a claim that live collection is complete.

### Collector/store integration checkpoint

The checker and producer are separate owners. The commit IDs below name reviewed
snapshots, not live branch tips, so an integration cannot silently combine
incompatible contracts:

| Boundary | Reviewed snapshot | Checker requirement | Status |
| --- | --- | --- | --- |
| API acquisition and collection | `b6d813a696e2b6054e2880c3eaa87e0a6ab14fbf` | authenticated read-only transport, complete request/page ledger, raw response references including original bytes, opening and closing default/PR rereads, and source-derived workflow/run/job/check/child graph | this reviewed snapshot maps nested source/dependency/check/graph fields; it does not register an in-process checker adapter or prove a full live gate |
| Raw-byte publication | `024c5f612874c5ffa2dfd1520be4f0aa6edb9cd7` | one production `RawObjectStore` using `sha256://<lowercase-64-hex>`, measured original response bytes, descriptor-relative reopen/rehash, and immutable sidecar/reference binding | this reviewed snapshot hardens restart-safe retention; checker-side consumption of the producer store and independent review remain incomplete |
| Checker validation | `a1bd6af650054b587241deed6ad8a38ff19498a4` | strict `g0_contract.rs` parsing and deterministic comparison against reviewed scope/source; `--live` must receive a producer-authorized capture | check-run and job URLs are structurally bound to their IDs; recursive child matrix collapse is rejected; live path intentionally fails closed until producer adapter integration |

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
collector and checker contracts diverged from the current checker types. The next
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

The snapshot claims collector revision, read-only GitHub API identity, safe
viewer scopes, rate-limit observation, complete request/page state, and
request-bound raw-object hashes. It then carries fixed-32 repository rows with
claimed default branch/SHA, ruleset contexts/apps, workflow/reusable-action/
scanner inventory, generated-state artifact reference, claimed current open
PRs (including draft/fork/bot rows), workflow bindings, required-check
producers, and raw references. It also claims pre/post branch/PR reconciliation,
a workload/child/release/package/check dependency graph, model-session
settings, repository access observations, and a workload artifact reference.

Request and raw-object rows are checked for schema, recomputed byte/query
digests, and internally consistent IDs, timestamps, page numbers, and claimed
pagination links. REST variables must be empty. GraphQL variables are checked
only as duplicate-free JSON objects; the query receives simple read-only text
checks, not GraphQL AST/schema validation or field-to-variable validation.
These checks validate caller-supplied claims, not provider
responses: response headers and JSON rows are not parsed, item counts/next
links are not independently confirmed, and no producer implementation is
installed to reopen and rehash original-response CAS objects. If the typed
checker path receives a TrustedLive G0 capture, it adds
`manifest-projection-unverified`, `check-id-response-unverified`,
`g0-api-row-response-unverified`, `g0-check-id-response-unverified`,
`g0-artifact-row-unverified`, `g0-artifact-attempt-unverified`,
`g0-artifact-archive-unverified`,
`g0-artifact-source-digest-unverified`, and
`g0-pagination-response-unverified`. A parsed workflow with action steps also
emits `g0-action-semantics-unverified` because action execution and transitive
composite dependencies are not derived. There is no dedicated
`g0-child-target-binding-unverified` finding in this checker yet; runtime
child-parent and target associations remain claims and need an explicit
authority check before G0 can certify them. The CLI's `--live` path currently
fails earlier because no authenticated collector is installed.

Workflow source has a narrower structural binding: its request must be a
successful `GET /repos/{owner}/{repo}/contents/{path}` with
`?ref=<source_sha>`, raw-content `Accept`, and `response_raw_ref` equal to the
referenced raw object's ID. The repository, direct `.github/workflows/<file>`
path, source revision, raw response bytes, and recomputed digest must agree.
This detects mismatched source/request/response projections, but the request
and response are still caller supplied and do not prove an authenticated live
collection.

PR check producer rows claim positive check-suite/check-run/workflow-run/job
IDs, attempt, source SHA, actual checkout SHA, `pull_request` event, successful
status/conclusion, app identity, and repository URL. The checker verifies
current-head equality and consistency with the snapshot's run/attempt rows,
but does not bind suite/run/job IDs to parsed provider response rows. Every
nested raw reference must resolve by ID within the same supplied snapshot;
that ID membership does not bind the referenced response to the claim's
endpoint, subject, or payload.

Each workflow row claims one immutable `source` blob with repository/path,
blob revision, commit SHA, repository-bound URL, media type, byte length,
base64 bytes, recomputed byte digest, a claimed external `storage_ref`, and
raw-object references. The checker parses those supplied bytes with the
existing YAML parser. Root, job, strategy, and step mappings use a deliberately
narrow checker field subset derived from [Runner v2.337.0 at immutable commit
`397b032cbf865e9c3ddfab89d533ec19325e1273`](https://github.com/actions/runner/tree/397b032cbf865e9c3ddfab89d533ec19325e1273);
schema-valid fields
outside that subset fail closed. Retained display fields, finite timeouts,
step/action `with`, and environment maps are shape-checked. Environment values
must be strings; keys must be statically known, nonempty strings. The checker
supports, for every YAML mapping, ASCII keys and the documented [`Á`/`á`](https://learn.microsoft.com/en-us/dotnet/standard/base-types/best-practices-strings#ordinal-string-operations)
one-character case pair for Runner's `OrdinalIgnoreCase` duplicate check.
Other non-ASCII key scalars are outside this checker subset; that restriction
does not describe what Runner accepts. Exact duplicate keys, expression-valued
mapping keys, and recursively
detected collisions under the supported case comparison fail parsing. The root
subset is `on`, `name`, `description`, `run-name`, `env`, and `jobs`. Regular
jobs support empty `needs`, `if`, matrix-only `strategy`, `name`, `runs-on`, static
timeouts, `continue-on-error: false`, `env`, and `steps`; `container` and
`services` are explicit blockers. Reusable jobs support `name`, pinned `uses`,
scalar `with`, `secrets`, empty `needs`, literal `if`, and matrix-only
`strategy`. Run steps support their command, display/ID, condition, static
timeout, `continue-on-error: false`, shell/working-directory strings, and
environment map; action steps support pinned `uses`, string-valued `with`,
display/ID, condition, static timeout, `continue-on-error: false`, and
environment map. Permissions, defaults, concurrency, outputs,
environment protection, snapshot, and other behavior fields fail as outside
the parser subset.

The parser derives only supported event names, Runner-valid ASCII job IDs
(under 100 characters and without the reserved `__` prefix), runner targets,
finite matrix assignments, and immutable `uses` edges. It accepts only the G0
event subset `push`, `pull_request`, `pull_request_target`, `merge_group`,
`workflow_run`, `workflow_dispatch`, and `workflow_call`; this is checker
policy, not a claim that Runner's default event schema rejects other names.
The plan does not establish effective permissions or command behavior. Every
referenced reusable workflow/action source must be present as another immutable
dependency blob. This is not yet full Runner parser parity: YAML merge keys can
be applied by the YAML library before unknown-field checks, and false-
conditioned reusable jobs or action steps can be skipped before their `uses`
references receive all static validation. These gaps are not separately
represented by a checker blocker. The parser also shape-checks reusable-call
`with` and `secrets` but does not compare them with the referenced workflow's
`workflow_call` declarations. Do not treat these checks as complete workflow-
source authority. `workflow_run` and `workflow_dispatch` triggers become
explicit child obligations. The derived job/workload set and targets are
compared with the supplied manifest's expected plan, and every supplied child
obligation must be present in the source-derived edges. That manifest
projection is not trusted until its reviewed source bytes and digest are
bound. Recursive edges retain root workload, child source SHA, and parent
repository/workflow/source identity. Runtime child rows must bind their
`parent_run_id` to the root run or an independently observed intermediate
child run; matching a SHA alone is not a parent association. Jobs, child runs,
or expected checks observed only in result records cannot create an
expectation.

The `source_jobs` array is mandatory for every workflow. Each row claims
`{job_id, workload_id, provider, platform, architecture, required,
raw_object_refs}` and must exactly equal both the supplied expected-job plan
and the jobs derived from the source bytes. An empty, duplicated,
target-mismatched, or unbound source-job row fails structural validation;
observed run jobs cannot fill a missing source obligation. The expected-job
plan remains untrusted until the manifest-projection blocker is resolved.

The current v2 row has no concrete matrix-assignment identity. A finite matrix
that expands one logical job into multiple instances therefore fails closed
until the reviewed plan and source-job contract enumerate those instances; the
checker never collapses them into one self-attested row. A reusable-workflow
caller with more than one matrix assignment also fails closed because its
current child-edge contract has no caller-assignment field.
Source derivation accepts only literal `if` conditions and unconditional event
maps. Schema-checked input/output/secret declarations are supported only for
`workflow_call`; any nonempty `workflow_dispatch` configuration, including
input declarations, is rejected until that schema is validated. Branch/path/
type/workflow filters, dynamic conditions, and true or dynamic
`continue-on-error` values fail closed; literal false is accepted and never
treated as conditional coverage.

The dependency graph is also typed structural input, not authority by itself.
Every node and edge claims an immutable source SHA/ref, raw-object references,
known node/relation kinds, and no dangling or duplicate identities. Required
workloads must have required check edges; reviewed child/release/package
obligations require their corresponding edges. Graph source rows must match
the reviewed workload revisions or the observed default-branch SHA. Missing
edges, illegal cycles, or a source/relation mismatch fail closed.
Edges carry separate `source_sha/source_ref` and
`target_source_sha/target_source_ref` bindings. An edge source digest alone
cannot authorize an unrelated target revision.

The same external-storage rule is required for generated-state and workload
artifacts: `storage_ref` must address the exact measured `sha256` bytes of a
raw object referenced by that artifact. The producer's verified live handle
that would reopen each source and artifact object is not currently connected
to the checker. Typed workload-to-check/package/release edges must also
preserve the source repository and workload identity; a node from another
repository is not a valid endpoint merely because its digest and ID are
present.

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

For G7, the schema requires a `reviewer_attestation` object with
`{reviewer, report_digest, manifest_id, snapshot_id, attested_at_utc, artifact}`
and artifact fields for source repository/revision, source tree and diff digests,
run-manifest digest, and immutable source URL. This is a required schema
contract, not implemented reviewer authentication. The checker shape-checks
these input claims, requires the fixed source-repository value, and matches the
claimed manifest/snapshot IDs against the supplied documents only. It does not
load or hash artifact bytes, verify a signature, authenticate the reviewer, or
verify the claimed source revision, tree/diff/run digests, or URL against an
external artifact. Distinct owner and reviewer strings or a populated artifact
object do not establish an attestation. G7 remains blocked until a trusted
producer and reviewer-verification path is implemented.

For eventual certification, records must bind to an authoritative run by exact
run ID and attempt. Main records must check out the current default-branch SHA.
PR records must distinguish contributor head/base from the current synthetic
merge candidate and use that candidate as the checkout. Merge-group records
must bind one immutable merge-group SHA. Workflow path/revision, event, source
URL, status, conclusion, job IDs, check app IDs, and child-run identities must
be independently reconciled. The current snapshot fields are caller claims;
they are not authenticated run evidence.

The checker compares expected jobs with the supplied manifest and compares
actual job IDs/targets and required checks with the supplied snapshot. These
comparisons can reject inconsistent claims but cannot establish their source.
A record cannot replace omitted work with an empty matrix or self-declared
expected IDs. The current structural `check_host_binding` compares claimed
`runner_kind` and labels with the supplied host contract; `host_id` is only
required to be nonempty and not `github-hosted`. These caller claims do not
authenticate runner registration, provider, Velnor ownership, or host identity.
Future GitHub-hosted execution requires a trusted hosted-runner binding;
Velnor execution requires independently verified `velnor-managed` ownership.

## Canonical release and install evidence

Every G2+ offline invocation requires and reads `--release-manifest`, regardless
of `release_applicability`; that applicability field controls whether release
and install records are required. The schema describes this as a producer-owned
document, but offline mode reads caller-supplied bytes:

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
the supplied external SHA-256. This proves only that the caller's digest
matches the caller's manifest bytes; it does not authenticate a producer or
bind the manifest to a GitHub release response. The checker has no dedicated
G2+ release-producer-authenticity blocker today. Live mode currently fails
before reading caller files, and offline mode cannot authorize a gate. The
digest is outside the canonical manifest, avoiding a hash cycle. Artifacts,
components, target/platform/architecture/service inventory, producer run,
source/tag/version/channel, and asset digests are checked for internal
consistency. A missing component, rebound digest, unsupported target,
source/tag mismatch, or stale schema fails structural validation.

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
