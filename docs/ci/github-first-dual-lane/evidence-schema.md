# GitHub-first dual-lane evidence schema

This is the input contract for `velnor-tools evidence-check`. The checker reads
three independent JSON documents:

```text
velnor-tools evidence-check \
  --stage G0 \
  --manifest config/github-first-dual-lane/manifest.json \
  --snapshot evidence/current-snapshot.json \
  --evidence evidence/records.json
```

The manifest and snapshot are authoritative inputs. An evidence record cannot
declare a repository, branch SHA, PR head, or expected workload on its own.
Missing evidence is an error. `gate_status`, workflow conclusions, and other
claims in a record are never accepted without checking the identities and
child records they reference.

## Manifest (`manifest.json`)

The manifest is external configuration owned by the G0 records workstream. It
must have `schema_version: 1` and exactly 32 unique repositories. The checker
does not contain the repository list or repository-specific branches.

The existing records ledger spelling (`manifest_version: 1` plus
`schema: "velnor.github-first-fleet.v1"`) is accepted as the same schema. Its
nullable preparation rows remain unknown and fail the selected gate until live
values replace them.

```json
{
  "schema_version": 1,
  "manifest_id": "github-first-dual-lane-2026-09-19",
  "repositories": [
    {
      "repository": "owner/name",
      "repository_role": "library",
      "default_branch": "main",
      "expected_workload_ids": ["ci"],
      "required_check_contexts_and_apps": [
        {"context": "ci / required", "app": "github-actions"}
      ],
      "workload_platform_architecture": [
        {"workload_id": "ci", "platform": "linux", "architecture": "amd64"}
      ],
      "provider_eligibility": {
        "github": "eligible",
        "velnor": "eligible"
      },
      "release_applicability": "not-applicable"
    }
  ]
}
```

`provider_eligibility` values are `eligible`, `not-applicable`, or
`excluded`. An `excluded` provider requires a matching, non-empty explanation
in the evidence record and is not a successful execution. A release-ineligible
repository still needs an explicit `release_applicability` value of
`not-applicable` in each record at G2 and later.

## Authoritative snapshot (`snapshot.json`)

The snapshot is captured independently from the evidence records (for example,
from GitHub APIs at one UTC instant). It must list every manifest repository
exactly once. `default_branch_sha` is the observed tip; it is not copied from
an evidence record.

```json
{
  "schema_version": 1,
  "observed_at_utc": "2026-09-19T12:00:00Z",
  "repositories": [
    {
      "repository": "owner/name",
      "default_branch": "main",
      "default_branch_sha": "0123456789abcdef0123456789abcdef01234567",
      "open_prs": [
        {
          "number": 42,
          "head_sha": "abcdef0123456789abcdef0123456789abcdef01",
          "base_sha": "0123456789abcdef0123456789abcdef01234567",
          "merge_sha": "123456789abcdef0123456789abcdef012345678"
        }
      ]
    }
  ]
}
```

An open PR's `merge_sha` may be `null` when GitHub has not produced a merge
candidate. A PR evidence record must then use a non-empty `tested_merge_sha`
only when the run itself proves that candidate; the head and base still must
match the snapshot. A main/push record must check out the snapshot's current
default SHA.

## Evidence envelope (`records.json`)

The envelope has `schema_version: 1`, `manifest_id`, `snapshot_observed_at_utc`,
and a non-empty `records` array. Each record is one repository/provider/event
claim. A valid record contains the following identity and execution fields:

```json
{
  "schema_version": 1,
  "manifest_id": "github-first-dual-lane-2026-09-19",
  "snapshot_observed_at_utc": "2026-09-19T12:00:00Z",
  "stage": "G0",
  "records": [
    {
      "repository": "owner/name",
      "repository_role": "library",
      "default_branch": "main",
      "default_branch_sha": "0123456789abcdef0123456789abcdef01234567",
      "observed_at_utc": "2026-09-19T12:05:00Z",
      "generator_revision": "123456789abcdef0123456789abcdef01234567",
      "runtime_product_id": "velnor",
      "generator_artifact_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "configuration_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "generated_tree_digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "scan_state_digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      "runtime_release_version": "0.1.1",
      "runtime_source_sha": "23456789abcdef0123456789abcdef012345678",
      "job_image_digest": "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
      "expected_workload_ids": ["ci"],
      "required_check_contexts_and_apps": [
        {"context": "ci / required", "app": "github-actions"}
      ],
      "workload_platform_architecture": [
        {"workload_id": "ci", "platform": "linux", "architecture": "amd64"}
      ],
      "provider_eligibility": {"github": "eligible", "velnor": "eligible"},
      "justified_exclusions": [],
      "PR_number": null,
      "PR_head_sha": null,
      "PR_base_sha": null,
      "tested_merge_sha": null,
      "merge_group_sha": null,
      "workflow_path": ".github/workflows/ci.yml",
      "workflow_revision": "3456789abcdef0123456789abcdef01234567",
      "event": "push",
      "run_id": 100,
      "run_attempt": 1,
      "run_url": "https://github.com/owner/name/actions/runs/100",
      "trigger_source_sha": "0123456789abcdef0123456789abcdef01234567",
      "actual_checkout_sha": "0123456789abcdef0123456789abcdef01234567",
      "provider": "github",
      "runner_name": "ubuntu-24.04",
      "host_id": "github-hosted",
      "expected_jobs": [
        {
          "job_id": "ci",
          "workload_id": "ci",
          "provider": "github",
          "platform": "linux",
          "architecture": "amd64"
        }
      ],
      "actual_job_ids": ["ci"],
      "actual_job_conclusions": {"ci": "success"},
      "logs": ["https://github.com/owner/name/actions/runs/100"],
      "child_run_links": [],
      "required_checks": [
        {
          "context": "ci / required",
          "app": "github-actions",
          "job_id": "ci",
          "conclusion": "success"
        }
      ],
      "release": {
        "applicability": "not-applicable",
        "justification": "library has no release surface"
      },
      "install": {
        "applicability": "not-applicable",
        "justification": "library has no installed product"
      },
      "owner": "operator",
      "reviewer": "independent-reviewer",
      "gate_status": "pass",
      "blocker": null,
      "next_action": null
    }
  ]
}
```

`event` is one of `push`, `pull_request`, `merge_group`, or
`workflow_dispatch`. PR fields are required for `pull_request`; merge group
fields are required for `merge_group`. `workflow_dispatch` is diagnostic unless
its source and required-check association satisfy the same identity rules.

`expected_jobs` must be non-empty. Every expected job must occur in
`actual_job_ids` and have conclusion `success`. Every `child_run_link` must
have a run identity, source SHA, provider, and terminal `success` conclusion.
Required checks must name the supplying app and job; skipped, queued, canceled,
timed-out, neutral, or missing checks fail the gate. A display name, aggregate
green conclusion, or user-provided boolean is not evidence.

At G2 and later, `release` and `install` are required objects. An explicit
`not-applicable` object needs a human-readable justification. A required
release must include a channel, version, tag target SHA, release ID, non-empty
asset digest map, APT feed candidate, and Homebrew tap/formula identity. It must
also carry the canonical nested application manifest with a schema, product
identity, immutable source ref/commit, manifest digest, non-empty artifact
inventory, and non-empty component inventory. Every manifest artifact is
cross-checked against the named asset digest map; every artifact/component
target must be a supported platform/architecture.

A required install must include an environment (a description that explicitly
identifies a clean workspace and PATH, or the typed image/platform/architecture/
runner/workspace/PATH object), an installed binary identity, operation fields,
service-manager result, and `functional_result: "success"`. The binary identity
must bind product, channel, version, source commit, and canonical manifest
digest, and list every canonical component binary with an absolute path and
digest. `systemd success` or a reasoned `not-applicable` service result is
required. Feed/tap records must repeat the canonical manifest digest; free-form
version strings or a non-empty binary list cannot stand in for these checks.

The checker returns a deterministic, sorted list of findings. It exits non-zero
for any error. Fixture tests exercise stale SHA, skipped job, missing
repository, wrong provider, failed child run, and mismatched artifact cases;
fixture success is only checker coverage and cannot establish G7.

Digest fields accept the canonical `sha256:<64 hex>` form and the equivalent
bare 64-hex form emitted by the existing `fleet.json`; both are validated as
content digests, never as arbitrary non-empty strings.

## Stage gates

The command's `--stage` is the gate being evaluated. It is never inferred from
the evidence. The optional envelope `stage` must match it when present.

| Gate | Additional required evidence |
| --- | --- |
| G0 | All 32 manifest rows, current snapshot correspondence, pins/digests, and non-empty workload inventory. |
| G1 | GitHub-hosted execution, jobs, required checks, logs, and child-run completion. |
| G2 | G1 evidence plus explicit release and clean install/upgrade evidence, or justified non-applicability. |
| G3 | Full-fleet current GitHub-hosted migration evidence. |
| G4 | Velnor provider execution and host identity for the pilot records. |
| G5 | G4 evidence plus released package and installation evidence. |
| G6 | Both eligible providers for every manifest row, with singular release/install applicability. |
| G7 | G6 evidence plus independent owner/reviewer identities. |

The checker only proves that supplied records satisfy the selected contract. A
passing synthetic fixture is a validator test, never G7 evidence.
