# Independent G0 skills-fleet raw inventory review

- Capture partition: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/fleet/skills-raw-20260920T061548Z`
- Review time: `2026-09-20T06:26:05Z` UTC
- Capture window: `2026-09-20T06:15:53Z`–`2026-09-20T06:17:05Z` UTC
- Scope: exact eight skills repositories; raw-byte integrity; authenticated
  identity; repository/default-branch SHA reconciliation; open-PR pagination;
  recursive-tree workflow filtering; rulesets, protection, checks, statuses,
  and aggregate-status interpretation
- Method: read-only local rehash and independent JSON/body derivation
- Safety: no source writes, GitHub requests, remote mutation, dispatch, rerun,
  or live-gate action

## Verdict

The partition is usable as an **independent raw inventory observation only**.
It is not a G0 gate result. `manifest.json` correctly remains
`complete: false`, `claimable: false`, with 16 explicit protection-related
404s. Missing workflow files and empty/pending status evidence remain
incomplete evidence, never an N/A success.

## Corpus and byte integrity

Independent rehash found 226 raw files: 105 `.body`, 105 `.response`, and 16
`.stderr`. Request IDs are unique and contiguous from `00001` through `00105`.
Every request's recorded body byte count, body SHA-256, and response SHA-256
matches the corresponding raw file; mismatches: **0**. Request file bindings
also match their request IDs; body/response pairs: 105.

| Artifact | SHA-256 |
| --- | --- |
| `manifest.json` | `a3239feb9227a46433eba29a8e5e301353c165bbd60bb0c736e40c4b1110ceef` |
| `auth-identity.json` | `bb1ab3ab924c4cecf09b88c7e2261235059edd65f1a74b9757f557e3c8ef4fb9` |
| `capture-summary.md` | `c1e4fda0e071735cf96e48677c5fe400c386de93b781c3e9542614a531050e1e` |
| `snapshots/before.json` | `1c66302c3dc672c21d01fef1bb9d9663e76d146aa3153b08436112771f145bed` |
| `snapshots/after.json` | `58e173e2f79846460c5afece377cf56ec22791ffdd4691970fd1332be78c8793` |
| `normalized/requests.jsonl` | `ebea146dbe4796034a2ed3038da6bf5eefc90129c3e4e96bb7ea27268e021311` |
| deterministic sorted raw-file hash manifest | `cef6b1a6b88965797c2523f95301a1d358a7501f810bbacd1a10f60a47e1fbfd` |
| deterministic sorted all-partition-file hash manifest | `093a408c5a0b60483e29cba497fa1cc8f76748e3145705190e16f95a1c185de9` |

The deterministic raw hash manifest covers relative path, SHA-256, and byte
size for every raw file. The all-partition hash manifest covers the same tuple
for all 281 files under the capture partition.

Normalized-file hashes:

| File | SHA-256 |
| --- | --- |
| `normalized/aggregate-status.jsonl` | `6bb4a6dda1615897013d000367930abbab5ffbef1a86b7f4b25c3a9853decca9` |
| `normalized/branch-protection.jsonl` | `7cdd141524792a823b564e0d719b88c08db663c1137ec978eb99c59755d7a5cf` |
| `normalized/check-runs.jsonl` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `normalized/open-prs.jsonl` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `normalized/repositories.jsonl` | `c329ff01074dc55b7070c2c60f62b78544d3a8ada3686d4b57093801861ec8a9` |
| `normalized/rulesets.jsonl` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `normalized/sha-targets.jsonl` | `8363d3418c3cee8960a1d3bc147a8bff13d7f5bfaf1e9e7840c2d96d08483180` |
| `normalized/statuses.jsonl` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `normalized/workflow-files.jsonl` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |

## Authenticated identity and request results

The auth endpoint is HTTP 200 and identifies `donbeave`, GitHub user ID
`139017`, type `User`. All 105 request records carry the same identity tuple.
No long credential pattern (`gho_`, `ghp_`, `github_pat_`, or bearer token)
was found in the partition. The 16 `.stderr` files all contain only:
`gh: Branch not protected (HTTP 404)`.

Independent response counts:

- HTTP 200: **89**
- HTTP 404: **16**
- Other status, permission error, rate-limit error, or command error: **0**

Every 404 is exactly one of these two endpoints for each of the eight repos:

- `branches/main/protection`
- `branches/main/protection/required_status_checks`

Every 404 body and stderr has the exact semantic message `Branch not
protected`. This is explicit unprotected/absent protection evidence, not a
permission or authentication success and not a required-check pass.

## Canonical repository set and default SHAs

The manifest has exactly eight unique repository names. Response bodies bind
all 16 repository observations to `tailrocks/<name>`, owner `tailrocks`,
default branch `main`, public, non-archived repositories. The repository-set
digest over sorted bare names is
`d7dbc3878f9174b879819dc55ab25ee31704b7c968ceaff0a7b1bc0b6e731fa6`.

| Repository | Before `main` SHA | After `main` SHA |
| --- | --- | --- |
| `tailrocks/tailrocks-code-quality-skills` | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` |
| `tailrocks/tailrocks-macos-skills` | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` |
| `tailrocks/tailrocks-open-source-skills` | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` |
| `tailrocks/tailrocks-pull-request-skills` | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` |
| `tailrocks/tailrocks-roadmap-skills` | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` |
| `tailrocks/tailrocks-rust-skills` | `bcc31b1d935dac4de191a6b71b3091f628d204c0` | `bcc31b1d935dac4de191a6b71b3091f628d204c0` |
| `tailrocks/tailrocks-skill-authoring-skills` | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` |
| `tailrocks/tailrocks-typescript-skills` | `f434715f7c664af431f0be62982aa379400102de` | `f434715f7c664af431f0be62982aa379400102de` |

The deterministic sorted SHA table digest is
`f3b96c5657421e68068e48daa272d11fb00d692f870458508ebe120a270f6cca`.
All eight before/after pairs are equal.

## Open-PR pagination

There are 16 successful open-PR requests: one `page=1`, `per_page=100` request
per repository per snapshot. Every response body is `[]`, and every response
has no `Link`/next-page header. Before and after open-PR identity sets are
therefore empty and equal for these captured snapshots. The deterministic PR
table digest is
`1627539607bf6869492eb95d57b49e5f29397377491e12fa6d27a3b4b90c5608`.

This does not claim that no PR can exist outside the capture window.

## Recursive tree and workflow filtering

There are eight unique recursive tree requests, one per stable default SHA.
All return the requested SHA and `truncated: false`; entry counts are:

| Repository | Tree entries | `.github/workflows/*.yml|yaml` paths |
| --- | ---: | ---: |
| `tailrocks/tailrocks-code-quality-skills` | 270 | 0 |
| `tailrocks/tailrocks-macos-skills` | 383 | 0 |
| `tailrocks/tailrocks-open-source-skills` | 197 | 0 |
| `tailrocks/tailrocks-pull-request-skills` | 195 | 0 |
| `tailrocks/tailrocks-roadmap-skills` | 308 | 0 |
| `tailrocks/tailrocks-rust-skills` | 357 | 0 |
| `tailrocks/tailrocks-skill-authoring-skills` | 201 | 0 |
| `tailrocks/tailrocks-typescript-skills` | 315 | 0 |

The workflow-filter table digest is
`0e564f40547fa97c0b6fd4e7c92cf68d00c1e9ead202194d9c1fe0a770d95f63`.

The recursive trees themselves are complete, but no workflow path exists, so
no workflow bytes were fetched. Workflow coverage is **incomplete**, not
`N/A` success; no generated-workflow or expected-job claim may be derived from
this absence.

## Rulesets, protection, checks, and statuses

For each of the eight default SHAs:

- rulesets: HTTP 200, empty list;
- branch protection: HTTP 404, `Branch not protected`;
- required status checks: HTTP 404, `Branch not protected`;
- check-runs: HTTP 200 with `filter=all`, `total_count=0`, `check_runs=[]`;
- commit statuses: HTTP 200, empty list;
- aggregate status: HTTP 200, `state=pending`, `total_count=0`, `statuses=[]`.

The deterministic status table digest is
`90a5847a74a59a07865e0d8897eb03e794f4fbba484919b46dea2e41101a4bed`.
Empty checks/statuses and `pending` aggregate state are retained as
incomplete/undetermined evidence. They are not green, not required-check
proof, and not a gate success.

## Earlier parser attempt

`skills-raw-20260920T060520Z` is retained but excluded from authority. Its
manifest has only 33 requests, reports `complete: true` despite a null auth
identity, and lacks the corrected 105-request raw partition. This review uses
only `skills-raw-20260920T061548Z`.

## Disposition

Use this partition for the exact eight-repository identity/default-SHA and
empty-open-PR inventory at its capture time. Keep workflow coverage,
protection/required-check coverage, and status/check conclusions incomplete;
do not convert them to N/A or success. No live G0 gate or approval was issued.
