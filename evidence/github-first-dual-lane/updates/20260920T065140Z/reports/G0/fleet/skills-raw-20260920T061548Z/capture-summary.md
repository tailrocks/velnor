# Independent Skills fleet raw capture

Capture window: `2026-09-20T06:15:53Z`–`2026-09-20T06:17:05Z` UTC.

Command: `rtk proxy python3 /tmp/capture_skills_raw.py /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/fleet/skills-raw-20260920T061548Z`

Authenticated identity: `donbeave` / GitHub user ID `139017`. Tokens were not persisted. The raw response headers preserve request IDs, scopes, rate-limit fields, HTTP status, and dates.

## Result

- Eight exact repositories captured before and after.
- 105 GET requests: 89 HTTP 200 and 16 HTTP 404.
- All 16 failures are explicit branch-protection/required-status-checks 404s (`Branch not protected`), eight repositories × two endpoints. No permission, rate-limit, or command errors occurred.
- `manifest.json` is deliberately `complete: false` and `claimable: false`; the 404 distinction is preserved and no gate/expected-job claim is made.
- Before/after default SHAs and open-PR sets are identical. Both open-PR sets are empty for all eight repositories.

| Repository | default `main` SHA |
| --- | --- |
| `tailrocks-typescript-skills` | `f434715f7c664af431f0be62982aa379400102de` |
| `tailrocks-skill-authoring-skills` | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` |
| `tailrocks-rust-skills` | `bcc31b1d935dac4de191a6b71b3091f628d204c0` |
| `tailrocks-roadmap-skills` | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` |
| `tailrocks-pull-request-skills` | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` |
| `tailrocks-open-source-skills` | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` |
| `tailrocks-macos-skills` | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` |
| `tailrocks-code-quality-skills` | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` |

For each exact SHA, the recursive tree, rulesets, branch protection, required-status-checks, check-runs with `filter=all`, commit statuses, and aggregate status were captured. Trees were not truncated; all eight trees contain zero `.github/workflows/*.yml`/`.yaml` paths, so no workflow-byte endpoint was applicable. Ruleset lists are empty. Check-runs and commit-status rows are empty; aggregate status responses are preserved as returned (`pending`), without interpreting them as a gate.

## Evidence layout and validation

- `raw/*.response`: original response headers plus body.
- `raw/*.body`: body-only bytes; `raw/*.stderr` exists only where GitHub CLI emitted an error.
- `normalized/requests.jsonl`: endpoint, context/page, UTC, HTTP/error, response headers, auth identity, paths, and SHA-256 hashes.
- `normalized/*.jsonl`: repository snapshots, PR identities, SHA roles, workflow rows, rulesets, branch protection, check runs, statuses, and aggregate status.
- `normalized/<repo>/<sha>/`: per-SHA workflow inventory, rules, and checks.
- `snapshots/before.json` and `snapshots/after.json`: complete normalized before/after snapshots.

Read-only validation found all 105 raw/body hashes match their request metadata. No `gho_`, `ghp_`, `github_pat_`, or bearer-token strings occur in the partition.

An earlier attempt is retained separately at `skills-raw-20260920T060520Z`; its parser was corrected before this dataset. This partition is the reviewable capture.
