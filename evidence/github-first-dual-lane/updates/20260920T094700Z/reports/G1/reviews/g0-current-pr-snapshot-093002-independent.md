# Independent G1 review: current PR snapshot

Date: 2026-09-20
Reviewer: G1 independent evidence review

## Verdict

**APPROVE this exact artifact for bounded current PR identity/freshness evidence only.** It proves neither current checks nor execution, merge validity, release state, or any gate. The artifact correctly records observed churn instead of treating initial identities as current.

## Frozen identity and capture window

Target: `G0/fleet/20260920T093002Z/`

```text
manifest.sha256             27325132cc58d532fb597df659327f35dc97df798c4647781456135a950447b5
capture-metadata.json        99d454bdf0d7176bc8f0974bc24833d4cb897f77ad9ad55748de5114f4f39cb5
report.md                   b9d678adf6b83c629c520766aee347094e01d1765377187c75739a9206103ff7
initial canonical           4c658ab84ec705bfb0f3261ae091930ec8ee1425b73346295d9869c85317b25d
reread canonical            7f7e35f2894ff7a9a73b945e29edb8b3143eb0608a83ec0902d013c173591b01
initial identity keys       b03528c01c8a3b4d054755ea9c287c7b9a1f7df319b399a7690d5c12ead8f362
reread identity keys        400fc8beadfe1817a6cbacf70b55688a85595ee70ad7a3304806cdbb0ac2d390
```

Capture metadata records:

```text
initial start       2026-09-20T09:31:09Z
initial API reads   2026-09-20T09:31:18Z
reread start        2026-09-20T09:33:50Z
reread API reads    2026-09-20T09:33:57Z–09:33:58Z
finalized           2026-09-20T09:37:22Z
principal           donbeave (login only)
```

The current `main` ref is `9307861d2668424d27012c3c736d48ab47f9012e` on both reads. Initial and reread `main-ref.body.json` objects are valid `refs/heads/main` commit refs with this same 40-hex SHA.

## Independent verification

1. Raw manifest, API response, and pagination integrity

   - `manifest.sha256` has 40 entries. `shasum -a 256 -c manifest.sha256` returned `OK` for all 40; the directory contains 41 files including the manifest itself.
   - Initial and reread PR HTTP responses are HTTP 200, have empty stderr files, and have no `Link` header. Their recorded page counts are both `1`; `per_page=100` therefore covers all returned open PRs.
   - Each `pulls.json` is one slurped body envelope containing 9 PR records; `pulls-flat.json` is an exact sorted projection of that inner array for both reads. No page or PR is omitted.
   - Raw credential scan found no `Authorization:`, bearer token, `ghp_`, or `github_pat_` value. Metadata states `credentials_saved=false`, `github_writes_performed=false`, and `source_edits_performed=false`.

2. PR census and field preservation

   Both reads contain exactly PRs `948, 952, 957, 960, 961, 962, 963, 968, 971`. Independent field census for each read:

   ```text
   state=open              9/9
   draft=false             7/9
   draft=true              2/9
   user.type=Bot           0/9
   head.repo.fork=true     0/9
   base.repo.fork=true     0/9
   ```

   Identity projections preserve number, state, draft, user, head/base ref/repository/fork/SHA, merge SHA, URLs, IDs, and node IDs. Reread report values match the raw PR records, including `PR #971` head/base/merge updates.

3. Normalization and identity comparison

   - Canonical initial versus reread comparison differs only in PR `#971`: base SHA `e717de39…` → `9307861d…`, head SHA `c541b951…` → `311a3d265…`, merge SHA `a3c10a11…` → `502845e3…`, and `updated_at` `09:25:36Z` → `09:31:57Z`.
   - No PR addition/removal, draft change, bot change, or fork change appears in the canonical diff. The identity-key projection has the same single-PR diff.
   - The report's reread table contains all nine rows and exact head/base/merge fields; no current check/run claim is attached to any row.

## Limits and disposition

This is a point-in-time REST identity capture for `tailrocks/velnor`, default branch `main`. It intentionally fetched no checks, logs, commit history, dispatches, merges, cancellations, or write operations. PR #971 churn is evidence that heads can move during capture; it does not establish why the change occurred. Preserve the initial/reread distinction and both canonical hashes when consuming this artifact. Do not treat it as current gate or execution authority.
