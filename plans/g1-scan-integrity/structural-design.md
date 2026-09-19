# G1 scan-integrity structural design

Status: **design only; no source or generated-file changes, no G1 approval**.

Scope is the ownership class and scan/write ordering needed after the rejected
candidate review. Implementation must be a new immutable source candidate,
reviewed independently before it is considered further.

## Evidence reconciled

- Rejected candidate: `2c328e1d471546a2c9b4b1f7e790d1e3562c66d3`.
- Exact review: `G1/scan-integrity/review2.md`, SHA-256
  `f1d32be2d95063476a5402bc95abca2f3c6cc1c93e8a655d5b5ffae1d1d7a1fb`.
- Earlier report: `G1/scan-integrity/REPORT.md`, SHA-256
  `70d8d8c5cb7eb95470171e0c9048267ed489a2e7803066267423da37ef704ce1`.
- Pre-code constraints: `G0/fleet/scan-ownership-design-review.md`, SHA-256
  `25940dcce1e2d7dc9680c2f3417cea937de0b733286523f66c3b57e27ad662ca`.
- Conditional review being addressed: `G1/scan-integrity/structural-design-review.md`,
  SHA-256
  `2c148b245fe71489b71e51f53d40036048f7af2dc4f94ae4179376f69a9cb067`.

The candidate's `verified_recorded_output_paths` and
`verify_generated_ownership` still accept repository-writable sidecar digest
claims. `content_digest_bytes` is public deterministic FNV-1a, not an
authenticator. A user can edit a current-renderer workflow or action and set
the sidecar to the edited bytes' digest; the file then disappears from scan
provenance and can be overwritten. The same candidate rejects a recorded path
which is absent from the current renderer before the later stale-delete plan,
so a legitimate removed output cannot be regenerated safely. Its config-drift
check also regresses to an unrelated renderer error, its release snapshot
check fails on `release.yml`, and clippy reports `needless_continue`.

## Trust boundary

The sidecar, generated header, path spelling, and any digest contained in the
worktree are **claims only**. They may cache diagnostics, but none may grant
scan invisibility, overwrite permission, or deletion permission. Do not add a
new sidecar trust scheme.

Ownership proof must come from an immutable baseline selected independently of
those claims:

```text
TrustedGenerationBaseline {
    immutable_base_commit: full object id,
    generator_revision: full pinned source/release revision,
    generator_closure: source-closure/release identity,
    render_schema: exact renderer schema/revision,
    typed_outputs: canonical repo-relative path -> immutable content identity,
}
```

The bounded implementation has exactly two baseline acquisition modes plus an
explicit local no-baseline mode. A mutable checkout, current sidecar, current
header, current config text, or current renderer path list cannot select any
mode merely by naming itself.

### `protected-base-api` acquisition

Only the trusted policy caller may use this mode. Before reading the target
tree, the caller supplies an immutable `ProtectedPolicyContract` from the
base-owned policy launcher (never from the PR/worktree):

```text
ProtectedPolicyContract {
    repository_id, repository_full_name,
    base_ref, base_branch,
    validator_workflow_path, validator_workflow_blob,
    validator_revision, validator_closure,
    caller_app_id, caller_installation_id,
    required_checks: {(context, app_id)},
    protected_ruleset_ids, required_enforcement,
    accepted_bypass_actors,
    generator_repository_id, generator_repository_full_name,
}
```

The caller proves its own App/installation identity against fixed values in
the launcher (including the API `/app` and `/app/installations/{id}` identity
records or their equivalent trusted runtime attestation), and binds this
contract to the target repository/base ref. The implementation compares all
returned repository, protection, ruleset, check `context`/`app_id`, validator,
and bypass-actor fields to this contract. It must reject missing, duplicated,
or ambiguous identity; a check title without the expected App ID is not proof.
No target config, branch name, PR check title, or current renderer may choose
the App, installation, validator revision/closure, accepted check set, or
bypass actors. The contract must require the base policy workflow itself to
be the protected producer, with `enforce_admins`/ruleset enforcement and
exact bypass actors matching the immutable accepted set.

The caller supplies repository identity and the protected base branch/ref from
policy context; neither value comes from PR config, the sidecar, the PR branch,
or the worktree. The acquisition contract is:

1. `GET /repos/{owner}/{repo}/git/ref/heads/{base}`. Retain the raw response,
   viewer identity, request parameters, and returned base commit SHA.
2. `GET /repos/{owner}/{repo}/git/commits/{base_commit}` and
   `GET /repos/{owner}/{repo}/git/trees/{base_tree}?recursive=1`. Retain the
   raw commit/tree responses, complete tree ID, pagination/terminal marker,
   and every path/blob ID used below.
3. `GET /repos/{owner}/{repo}/git/blobs/{blob_sha}` for the base generation
   config, pinned generator/source-closure inputs, and every typed generated
   output. Decode the returned bytes and verify the returned blob SHA against
   the requested object before use.
4. Independently call
   `GET /repos/{owner}/{repo}/branches/{base}/protection`, then
   `GET /repos/{owner}/{repo}/rulesets` and each applicable
   `GET /repos/{owner}/{repo}/rulesets/{ruleset_id}`. Retain every raw page,
   cursor, terminal page, viewer identity, and response hash. Prove that the
   contract's validator workflow path/blob is the required protected producer;
   verify branch-protection required checks and ruleset required checks as
   exact `(context, app_id)` pairs, active enforcement, and exact bypass actor
   set. A branch name, title-only check, or config assertion is not enough.
5. Require the target base config to contain a full `[generator].revision`.
   Missing revision is `baseline_generator_unproven`; do not fall back to the
   protected entrypoint pin or environment variable. Require its repository
   coordinate to equal the contract's immutable generator repository identity.
   Resolve that separate generator repository's commit/tree/source-closure
   objects through the corresponding `/repos/{generator_owner}/{generator_repo}`
   `/git/commits/{pin}`, `/git/trees/{tree}?recursive=1`, and `/git/blobs/{sha}`
   calls. Verify repository ID/full name, full pin, object types, closure, and
   byte identity; an unavailable/mismatched external generator is
   `baseline_generator_unproven`, never local HEAD.
6. Compute the existing closure identity from the verified generator object
   set, render the protected base using that revision, and require exact byte
   identity for every typed output and the recorded path contract. The
   `ProtectedPolicyContract`'s validator revision/closure and workflow blob
   must match the corresponding protected objects.
7. Re-run step 1 after all reads and rendering. Any base-ref movement, changed
   returned commit/tree ID, incomplete pagination, blob mismatch, protection
   404, missing/unusable ruleset, 401/403, API transport error, or unavailable
   required App is `baseline_unavailable` (or the more specific
   `baseline_ref_moved`, `baseline_policy_unproven`,
   `baseline_generator_unproven`, `baseline_tree_incomplete`,
   `baseline_blob_unavailable`, or `baseline_render_mismatch`). It is never
   treated as protected and never falls back to `HEAD`, a branch checkout, or
   sidecar data.

### `operator-commit` acquisition

Local generation accepts one explicit full-object CLI value,
`--baseline-revision <40-hex-commit-sha>`, or the mutually exclusive explicit
`--local-no-baseline` flag on generation/check commands. The
CLI resolves no symbolic names: a test may obtain `HEAD`'s full SHA with
`git rev-parse HEAD`, but passes that SHA. The value is never read from config
or sidecar. Validate `git cat-file -e <sha>^{commit}`, but perform all object
reads with replace/graft/alternate processing disabled. Before use, inspect
and reject any replace refs, graft files, or `objects/info/alternates`; an
object available only through an alternate is unavailable. Verify raw Git
object type, commit/tree/blob IDs and Git object framing against the supplied
full SHA, then read that commit's tree/blobs from the object database (not the
mutable worktree), verify the generator revision/source closure and
byte-identical typed outputs, and report
`baseline_source=operator-commit`, the full baseline commit/tree IDs, and the
generator closure in every result. Missing objects, incomplete source closure,
invalid path contract, replace/graft/alternate state, or render mismatch is
`baseline_unavailable` with no write. Protected policy mode rejects this local
mode; it cannot be used to weaken the protected API contract.

The CLI does not persist this authority in a new file. The operator supplies
the full SHA again for a later config/source change or stale removal; the
sidecar remains only an advisory projection. `baseline_source=none` is legal
only when the operator explicitly selected `--local-no-baseline`; protected
policy mode cannot select it through a flag, environment, config, or sidecar.
An explicit local first generation may create missing current-renderer outputs
and may repeat unchanged current-exact generation. Existing foreign files
remain visible/conflicting. If a changed config/source/generator needs a prior
preimage, or stale output needs removal, `none` returns the bounded error
`baseline unavailable; supply --baseline-revision <full-sha>` and performs no
mutation. It never refreshes a sidecar as proof.

The old broad “replace unowned workflows” behavior is intentionally removed.
`--force` is not implicit adoption and is not a baseline selector. No
compatibility alias or hidden fallback is allowed. A published-release
baseline is not selected here: no immutable published baseline/path artifact
was supplied by the review evidence, so implementation must not infer one
from a generic product or attestation mention.

The baseline's content identity is trusted only because it is bound to the
protected Git object set or the explicitly operator-selected immutable commit
and its verified source closure. A new secret, signature, or cryptographic
protocol is out of scope.

## Independent classifications

Classify each canonical repository-relative regular file using the trusted
baseline, the current renderer's desired bytes, and a preimage captured from
the file. Never classify from sidecar equality alone.

The current renderer is not automatically an authority. It must receive a
mode-bound `CurrentRenderContract`:

- In `protected-base-api` mode, the trusted policy caller supplies the
  validator/generator revision and closure plus the complete typed current
  path/content contract. The path and desired bytes must match that contract;
  a PR-controlled renderer/config cannot add an accepted path by rendering it
  successfully. A new workflow/action emitted only by the candidate is
  `CurrentOnlyUnbound`, not owned.
- In `operator-commit` mode, the operator-selected full-SHA baseline and the
  explicitly selected local generator binary are the declared authority for
  this invocation. The result reports that operator trust mode.
- In explicit `--local-no-baseline` mode, the renderer is a preview only:
  it may prove a current-exact no-op for an existing path or create a missing
  current output in this explicit local invocation, but it cannot prove a
  prior preimage, replace an existing differing path, or delete stale output.
  No current-only claim survives as authority for a later invocation.

Protected mode therefore needs an authenticated current render contract in
addition to the prior baseline. If the trusted caller cannot provide it,
protected generation fails `baseline_generator_unproven`; it must not promote
the candidate's current path list or desired bytes.

| Class | Required proof | Scan treatment | Mutation treatment |
| --- | --- | --- | --- |
| `GeneratedCurrentExact` | Authenticated current render contract contains the typed path and desired bytes; existing bytes equal those bytes. Local no-baseline mode may use the exact current bytes only for its same-invocation no-op/exclusion; operator mode uses its explicitly selected binary contract. | May be excluded only in the later generated-output pass. | Replace/update only after preimage revalidation; local no-baseline cannot replace an existing differing path. |
| `GeneratedCurrentPriorExact` | Authenticated current render contract contains the path; existing bytes equal the immutable prior baseline bytes while current desired bytes changed. | Remains visible in the raw audit; never sidecar-hidden. | May update to authenticated current bytes after explicit generation and preimage revalidation. |
| `GeneratedStaleExact` | Authenticated current contract omits the path; prior contract contains it; existing bytes equal immutable prior bytes. | Remains visible to the raw audit; it is not put into the current `owned_paths` exclusion set. | Delete only as an explicit stale plan with `--force`; exact preimage required. |
| `CurrentOnlyUnbound` | Current candidate renderer/config claims a path and exact bytes, but protected caller has not authenticated that path/content contract. | Visible input and ownership conflict. | No write or delete, including `--force`. |
| `ModifiedGenerated` | Typed current/prior path exists, but bytes match neither trusted current nor trusted prior content. | Visible input and ownership conflict. | No write or delete, including `--force`; require manual reconciliation. |
| `ForeignOrUnknown` | No trusted current/prior path-and-content proof. | Visible input and ownership conflict. | Never overwrite/delete from generator flags or sidecar claims. |

The implementation must keep two named views, with no ambiguity between
audit evidence and detector input:

```text
raw_inventory   = every repository file and identity, including workflows,
                  actions, sidecar, current outputs, prior outputs, and stale
                  outputs; only the repository's fixed control exclusions apply
detector_inputs = raw_inventory after immutable-baseline classification removes
                  exact generated current/prior/stale outputs and fixed
                  controls; modified/unknown .github files remain
```

`raw_inventory` is retained for reports and is never sidecar-filtered.
`detector_inputs` is what capability inference consumes. Before the current
renderer is known, remove only exact `GeneratedCurrentPriorExact` and
`GeneratedStaleExact` paths proven by the immutable prior baseline. Render the
current typed surface from those detector inputs, then run a mandatory bounded
second detector pass that removes only authenticated `GeneratedCurrentExact`
paths and checks for a stable class set. Every removed detector path remains in
`raw_inventory` with its class and immutable proof. Thus old generated
workflow/action bytes cannot influence capability inference, while a modified
or foreign file remains both visible and conflict-producing. Fixed generator
control files are exactly the code-owned literals
`.github/ci/.github-actions-generator-state` and
`config/fleet/velnor-host.env`; no config, sidecar, or renderer may extend this
set. A generated workflow/action or other `.github` file is not a control file
merely because of its name.

The fixed-point protocol is finite and mandatory:

```text
MAX_DETECTOR_PASSES = 3
D0 = raw_inventory - FIXED_CONTROLS - exact prior-baseline outputs
for pass in 0..MAX_DETECTOR_PASSES:
    scan(Dpass), render authenticated current contract, classify raw_inventory
    Dnext = D0 - exact GeneratedCurrentExact outputs
    fingerprint = (Dpass entries + rendered path/content IDs + class set)
    if pass > 0 and fingerprint == previous fingerprint:
        return stable detector_inputs = Dpass
    Dpass = Dnext
error detector_fixed_point_unreachable; no output or sidecar mutation
```

`Dpass` entries include canonical path, regular-file kind, immutable content
identity, and preimage identity; rendered maps include sorted canonical paths
and desired content identities. A changed/cyclic renderer, changing class set,
or third-pass disagreement returns the exact bounded
`detector_fixed_point_unreachable` error. There is no “best effort” or
unbounded retry. The current-only hostile path never enters `Dnext` because it
is `CurrentOnlyUnbound`, not `GeneratedCurrentExact`.

This deliberately separates two decisions:

1. **Input scan visibility**: whether a file may be omitted from
   `detector_inputs` after it remains in `raw_inventory`. A digest in the
   sidecar never decides this.
2. **Mutation authorization**: whether a planned replacement/deletion is
   allowed. It independently checks typed path, trusted exact preimage,
   regular-file identity, symlink/ancestor safety, and unchanged bytes after
   planning.

Scan exclusion never implies overwrite permission. A stale exact file may be
known generated while remaining visible in the audit; an edited generated
file is never safe merely because its path is typed.

## Required ordering

1. Resolve the independent baseline through `protected-base-api`,
   `operator-commit`, or explicit local `--local-no-baseline`. Re-read the
   protected base ref after acquisition and before plan use. A protected API,
   caller-identity, ruleset, external-generator, or closure failure is a
   terminal `baseline_unavailable`/`baseline_*` result before rendering, with
   no output or sidecar mutation; it cannot downgrade to local `none`.
   `none` is fail-closed for prior/stale ownership and is legal only after an
   explicit local operator flag.
2. Read and canonicalize generation config and scan-input identity before
   renderer validation. Compare advisory recorded inputs for diagnostics, so a
   supported config-drift check reports `config input changed` before an
   obsolete fixture reaches an unrelated renderer error. The advisory input
   record still grants no ownership.
3. Build `raw_inventory` with no sidecar output exclusions. Capture path kind,
   canonical path, file identity, bytes, and immutable baseline relation for
   candidate generated files. Derive `detector_inputs` by removing only exact
   prior-baseline generated paths; retain all other files.
4. Render the current typed surface from `detector_inputs`. The renderer
   supplies desired bytes and a typed path contract; it does not authenticate
   existing bytes.
5. Classify every existing candidate using the table above, then perform the
   mandatory bounded three-pass detector protocol and require its exact stable
   fingerprint. Any
   `ModifiedGenerated`, `ForeignOrUnknown`, invalid path, symlink, baseline
   mismatch, or missing required baseline produces a conflict/no-write result.
   Diagnostics must name the path, class, and proof/preimage reason.
6. Build one plan. Current exact/prior-exact typed outputs may be created or
   refreshed; stale exact outputs may be deleted only with explicit `--force`.
   `--force` is typed current replacement/stale-plan authorization only; it is
   not provenance, scan suppression, adoption of unknown bytes, or permission
   to delete an unproven stale path. Modified/unknown files remain untouched.
7. Re-read the protected base ref and revalidate every preimage (regular
   identity and exact bytes),
   reject concurrent replacement or symlinked ancestors, then apply atomically.
   Write the sidecar last as an advisory projection of the trusted plan. Never
   use a new sidecar entry to authorize the same operation being planned.

This ordering permits a config/source change to replace an exact old generated
preimage and permits exact stale removal, while preventing a forged current
digest from shaping the scan or authorizing a write.

### Exact force contract

- `GeneratedCurrentExact` is already current; no force is needed.
- `GeneratedCurrentPriorExact` may be refreshed by normal generation when the
  operator supplied a valid baseline. The plan prints
  `update GeneratedCurrentPriorExact <path>` and proves the old immutable
  baseline bytes, not the sidecar digest. `--force` may acknowledge this typed
  current replacement but does not broaden its proof.
- `GeneratedStaleExact` is planned as
  `delete GeneratedStaleExact <path>` only with `--force`, a valid baseline,
  and an exact old baseline preimage. The stale path is never deleted merely
  because the sidecar lists it.
- `ModifiedGenerated` and `ForeignOrUnknown` always stop the plan. Plain
  `--force` cannot adopt, overwrite, hide, or delete either class. There is no
  compatibility mode for the old broad unowned-workflow replacement behavior.

Every result prints `baseline_source` (`protected-base-api`,
`operator-commit`, or `none`), the full baseline revision/tree when present,
source closure, and each path's class/action/proof. A missing baseline error
must identify the exact operation that requires it rather than making an
unchanged current-exact first/repeated generation unusable.

### Bounded multi-file apply

“Apply atomically” means a bounded transaction, not a false claim of
filesystem-wide atomicity. Before the first rename, stage every new byte in a
same-filesystem temporary directory, capture every old regular-file preimage
and mode, and write a deterministic journal in that temporary directory.
Apply paths in canonical order, rechecking identity/bytes immediately before
each replacement/deletion. Write the advisory sidecar last. On any injected or
real write/rename/fsync failure, stop, restore already-applied paths from the
journal (remove newly created paths), and remove the sidecar projection. A
rollback identity mismatch is the terminal
`partial_apply_recovery_required` result; never report success or silently
continue. The temporary journal is not a repository trust record.

The implementation acceptance fixture injects failure at each ordered file and
at the sidecar write, then asserts complete rollback, unchanged foreign files,
and no partial sidecar. `--check` never enters this transaction.

## Acceptance fixtures before implementation review

Fixtures must use a supported declaration (`rust-crate-pipeline`, not the
obsolete `rust-crate` fixture). Each fixture runs through the integrated
`scan_target`/render/write-plan path, not only a helper unit.

### Positive

- **SI-P1 — current exact**: current renderer output is unchanged and exact;
  the later generated pass may exclude it, `--check` is clean, and a repeated
  generation is byte-identical.
- **SI-P2 — config drift with unchanged bytes**: change one generation policy
  input while output bytes remain unchanged. `--check` fails specifically with
  config-input drift before unrelated renderer validation; no file changes.
  With the full SHA of the unchanged `HEAD` supplied as
  `--baseline-revision`, a supported regeneration refreshes the advisory
  projection and the next check is clean.
- **SI-P3 — intentional current regeneration**: baseline B1 has exact old
  output bytes; B2's trusted config/source changes current desired bytes at
  the same typed path. With `--baseline-revision B1`, normal generation may
  replace the old bytes; the plan prints `GeneratedCurrentPriorExact` and
  proves the old B1 preimage, never the sidecar digest. Editing the old bytes
  makes the plan refuse and preserve the edit, including under `--force`.
- **SI-P4 — legitimate stale removal**: B1 contains `old.yml`; B2's trusted
  current renderer omits it. Exact B1 bytes classify `GeneratedStaleExact`;
  `--baseline-revision B1` plus `--check` reports the stale plan, and the same
  baseline plus `--force` deletes it with an exact preimage. An edited
  `old.yml` makes both normal generation and `--force` refuse; the file
  remains.

### Negative

- **SI-N1 — forged current digest**: edit a current-renderer workflow and an
  untracked composite action, then forge the sidecar digest to each edited
  body. The typed workflow is `ModifiedGenerated`; the action is
  `ForeignOrUnknown` unless the trusted/current path contract contains it.
  Both remain in `raw_inventory` and `detector_inputs`; check and ordinary
  generation fail without writes. A force run cannot use the sidecar as proof
  or silently hide/overwrite either body.
- **SI-N2 — forged path/header**: sidecar names an arbitrary workflow/action,
  uses `./`, repeated separators, traversal, absolute, or backslash spelling,
  or the file carries the exact generated header but is absent from the
  trusted baseline. It remains visible and is never stale-deleted.
- **SI-N2b — protected current-only path**: alter the candidate
  renderer/config so it emits a new workflow or composite action with bytes
  exactly matching that candidate render, but omit the path from the
  authenticated protected `CurrentRenderContract`. It classifies
  `CurrentOnlyUnbound`, remains in both inventory views, conflicts, and is
  preserved under `--force`. The same path is only creatable in an explicit
  local operator mode whose authority is reported.
- **SI-N3 — no baseline**: remove/withhold the independent baseline while a
  sidecar claims old output. With explicit `--local-no-baseline`, `.github`
  inputs stay visible; stale deletion, prior replacement, and ownership
  adoption are refused; no sidecar refresh claims success. In protected mode,
  the same missing/failing baseline is terminal before scanning and performs no
  output or sidecar write.
- **SI-N4 — filesystem race**: replace bytes or file identity after plan
  capture. Preimage revalidation fails and preserves the replacement; no
  output or sidecar is partially updated.
- **SI-N5 — path/symlink safety**: exercise symlinked output roots/ancestors,
  source/output symlinks, traversal, absolute, and repeated-separator forms.
  Reject before scan exclusion or mutation.

### Baseline and CLI protocol

- **SI-B1 — protected API proof**: fixture the trusted policy caller with the
  exact protected caller contract: repository/base identity, validator
  workflow blob, validator revision/closure, caller App/installation IDs,
  required `(context, app_id)` checks, active protection/ruleset enforcement,
  and exact bypass actor set. Assert API `/app`/installation identity and all
  raw response IDs/viewer/page records. Include a wrong-App response with the
  right check title and reject it. Acquire target and external generator
  repository commit/tree/blob objects, verify source closure and byte identity,
  then post-acquisition base-ref equality. Exercise each
  `baseline_unavailable` state: ref movement, missing/incomplete page,
  404/401/403, transport failure, missing required App/ruleset, blob mismatch,
  external generator mismatch/missing object, missing generator pin, and
  render mismatch. Every protected failure is terminal before rendering and
  produces no output or sidecar mutation; none may select current `HEAD`, PR
  config, sidecar, or a fallback baseline.
- **SI-B2 — operator lifecycle**: explicit `--local-no-baseline` first run
  creates missing current outputs and permits repeated current-exact runs;
  existing foreign/differing bytes conflict. Pass the full SHA from `git
  rev-parse HEAD` as `--baseline-revision` for config drift, current-prior
  replacement, and stale removal. Remove the object or omit both explicit
  modes and assert the bounded baseline-unavailable result with no
  output/sidecar mutation. Exercise replace refs, grafts, and alternates;
  reject all. Report source, full revision/tree, and closure on every result.
- **SI-B3 — raw versus detector views and bounded convergence**: plant exact
  prior/stale workflow and action bytes that would otherwise affect capability
  detection. Assert every file remains in `raw_inventory`, exact baseline
  outputs are absent from `detector_inputs`, modified/unknown files remain in
  both, and the mandatory three-pass protocol reaches a stable fingerprint.
  Give the scanner a deliberately changing/cyclic render and assert exactly
  `detector_fixed_point_unreachable`, no write, no sidecar refresh, and no
  fourth pass. Assert only the two code-owned fixed-control paths are excluded.
- **SI-B4 — transaction failure**: inject write/rename/fsync failure at each
  ordered generated output and at the sidecar. Assert journal rollback restores
  every prior byte/mode, removes new outputs, preserves foreign files, leaves
  no partial sidecar, and reports `partial_apply_recovery_required` if rollback
  itself detects a changed preimage.

### Existing regression gates

- The supported config-drift fixture must retain the parent behavior and not
  regress to “does not render”.
- A missing `[generator].revision` must produce
  `baseline_generator_unproven`; the protected entrypoint/environment pin is
  not an ownership fallback. A configured external generator repository must
  match the trusted repository identity and verified pinned closure.
- The checked-in workflow byte snapshot must pass on the final immutable
  candidate. The candidate review's `release.yml` drift is an implementation
  blocker, not a baseline to regenerate or waive in this design.
- `cargo fmt --all -- --check`, `git diff --check`, and clippy with
  `-D warnings` must pass; specifically remove the candidate's
  `needless_continue` without changing path semantics.
- Run the full library suite and the hostile matrix on one frozen candidate
  SHA. Do not claim G1 approval from this design or from helper-only tests.

## Non-goals and handoff

This artifact does not edit `crates/velnor-workflow`, generated workflows,
config, sidecars, or fixtures. It does not regenerate `release.yml`, invent a
signature service, or decide release acceptance. It defines the smallest
trust-boundary change for an implementation owner.

Implementation may begin only after `g0_runtime` independently reviews this
document, then must produce a fresh immutable source SHA and exact fixture
results. Preserve the rejected reports and this design artifact; do not
overwrite prior review evidence.
