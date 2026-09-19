# GitHub-first dual-lane evidence schema v2

`velnor-tools evidence-check` is a fail-closed verifier. It consumes four
independent machine-readable inputs:

```text
velnor-tools evidence-check \
  --stage G3 \
  --manifest manifest.json \
  --snapshot snapshot.json \
  --evidence records.json \
  --live \
  --release-manifest application-manifest.json
```

The reviewed workload manifest is source authority for the fixed 32-repository
scope and generated workload plan. The snapshot is an independently collected
GitHub fact set. Evidence records only bind claims to identities found in that
source. `gate_status`, a badge, an aggregate conclusion, a display name, or a
caller-supplied boolean never authorizes a pass.

Schema v2 is strict: every object has `deny_unknown_fields`; field aliases,
nullable-row normalization, legacy fallbacks, and flat release/install aliases
are not accepted. Parse failure is failure. No credentials, bearer tokens, or
authentication headers may occur in any source/provenance field.

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

Source metadata is provenance, not proof by itself. `--live` performs a fresh
read-only API collection and reconciles current facts; it fails on inaccessible
permissions, rate limits, malformed responses, missing pagination, or a
revision/PR/ruleset/run/job mismatch. G7 always requires `--live`. Offline
validation is a fixture/schema check and can never establish G7.

Every repository row records the GitHub numeric repository ID, actual default
branch and SHA, complete ruleset status-check/app-ID inventory, workflow path
and immutable revision inventory, all current open PRs (including drafts/bots),
and separate execution arrays for the current main tip and each PR.

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
| G7 | fresh live reconciliation plus independent review artifact | exact manifest/snapshot/evidence digest, source tree/diff/run-manifest digests | reread default branch and every PR head; bind reviewer artifact externally | owner/reviewer-only, stale digest, missing artifact binding | `--live` and external attestation required |

The current read-only collector gap is recorded at
`dual-lane-evidence/G0/fleet/collector-contract-gap.md` (SHA-256
`186e6a1ed70bea2588df77f9566e4e56ad9a14b65e3654960aeebb738f45c071`): its v1
REST output lacks raw query/auth/page provenance and complete ruleset,
check-suite, job, artifact/log, workflow-graph, merge-group, and child-lineage
objects. The checker therefore emits `authoritative-collector-required` for
execution stages; this schema is not a claim that live collection is complete.

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
