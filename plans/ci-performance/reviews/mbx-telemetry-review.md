# MBX telemetry review

Scope: source candidate `.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml`, both generator test modules, both cache-input renderers, and the checked-in generated action/workflow. Read-only telemetry review; no generated output or source action was edited here.

## Verdict

**The archive/compiler separation is directionally correct. Integration is HOLD.** The candidate stops deriving an archive result from compiler log prose, carries Velnor host warmth as a declaration, and reports an MBX false exact-hit as `unknown` when no matched archive key exists. Three correctness boundaries remain: incomplete GitHub key pairs, contradictory MBX signals, and the unversioned `host_warm` shape change. The generated action is still the old implementation, so current CI has none of the candidate behavior.

## Confirmed source behavior

At `.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml:192-247`:

- GitHub cache layers use `primary` and `matched` keys. MBX uses its exact-hit boolean plus optional explicit keys.
- MBX no longer searches the unit/compiler log for `exact hit`, `warm start`, or `miss`.
- `compiler.mbx_outcomes` still archives matching compiler log lines; it is separate from `cache_outcomes.mbx`, which is the archive transport result.
- `cache_declarations.host_warm_layers` carries the Velnor persistent-path declaration. Host warmth is no longer inserted into `cache_outcomes`.

This boundary matches the pinned [Mr. Boxington action metadata](https://raw.githubusercontent.com/jdx/mr-boxington-action/867fc530102eec5b756075d70d850dc8330d2272/action.yml) and [implementation](https://raw.githubusercontent.com/jdx/mr-boxington-action/867fc530102eec5b756075d70d850dc8330d2272/src/index.ts): the action exposes `cache-hit` and `cache-primary-key`, does not expose a matched key, and logs the restored key. Its `cache-hit=false` means either a miss or a prefix restore. The log is evidence of the action's human summary, not a stable reporter input.

The existing Rust fixtures exercise exact, explicit prefix, matched-without-primary, false-without-matched, compiler-log words, and Velnor declarations. The direct source-script probe `/tmp/mbx-telemetry-probe.py` reproduced the candidate behavior with Bash and jq:

| input | candidate output |
| --- | --- |
| GHA matched key only | `prefix` |
| MBX `hit=true`, primary `wanted`, matched `restored-prefix` | `exact` |
| MBX `hit=false`, primary == matched | `exact` |
| MBX `hit=true`, no keys | `exact` |
| MBX `hit=TRUE`, primary == matched | `exact` |
| Velnor `host_warm_layers=rustup,mbx,cargo` | declaration array; MBX outcome `unknown` |

The last three rows expose the remaining issues below. The misleading compiler-log case correctly produced MBX `unknown`.

## Blocking findings

### 1. Incomplete GitHub key pairs are misclassified

`classify_gha_cache` emits `prefix` whenever `matched` is nonempty and differs from `primary`; it does not require a nonempty primary. The executable probe with only `VELNOR_CACHE_CARGO_MATCHED=prefix-only` emitted `cache_outcomes.cargo=prefix`. That is not proof of a prefix restore: the producer contract is incomplete or the caller wiring is wrong.

Make the GitHub classifier fail closed: matched-only must be `unknown`; primary-only remains `cold` only if the producer contract explicitly says an absent matched output means no restore. Add the matched-only regression to both legacy and S2 action tests. Keep this as a separate known telemetry defect from the MBX repair; it is not evidence that the MBX archive was a prefix hit.

### 2. Contradictory MBX boolean/key signals are accepted

The current order lets `hit=true` win over any key mismatch, so `true + wanted + restored-prefix` reports `exact`. It also reports `exact` for `false + primary==matched`, and for an unrecognized `hit=TRUE` with equal keys. These combinations contradict the pinned producer's exact-hit boolean contract or contain malformed input. They must not become positive evidence.

Define and test one fail-closed truth table in both generator modules. At minimum:

- `true` with absent keys may be `exact` only if the boolean alone is an accepted producer proof;
- `true` plus unequal nonempty keys is `unknown`;
- `false` plus equal keys is `unknown`;
- unrecognized nonempty booleans are `unknown`;
- a prefix result requires both nonempty keys with `primary != matched` and no contradictory boolean.

Do not resolve this by trusting compiler text. The pinned action does not provide the missing matched key, so real MBX runs remain `unknown` for non-exact restores until a producer explicitly supplies that evidence.

### 3. `schema_version: 1` hides a breaking field/enum migration

The candidate still emits `schema_version: 1` at action line 326, but changes the report contract from `cache_outcomes.<layer> = "host_warm"` to a null keyed outcome plus `cache_declarations.host_warm_layers: [...]`. Adding a field is compatible for tolerant readers; moving a value out of an existing enum is not automatically compatible for readers that aggregate `cache_outcomes`.

Repository inspection found no typed consumer of `VELNOR_CI_REPORT`: `velnor-workflow` only renders/tests it, `tools/unit-collector` preserves raw observations, and the model/control crates parse unrelated telemetry schemas. `content/docs/guides/execution.mdx` describes the candidate shape, while older plan ledger rows still claim `host_warm` in `cache_outcomes`. Absence of an in-repo consumer does not prove external archives are compatible.

Before rollout, choose one explicit contract: bump this report schema and update readers/docs, or document and test that schema v1 permits this breaking enum move. Include an old-record/new-record compatibility fixture for the actual archive reader. Do not call the shape migration complete merely because the Rust generator tests pass.

## Generated/live boundary

`cmp -s .github-gen/sources/actions/report-velnor-ci-outcomes/action.yml .github/actions/report-velnor-ci-outcomes/action.yml` currently fails. The generated action still contains the old compiler-log scraping, maps MBX false to `prefix`, and emits Velnor `host_warm` outcomes. Current `.github/workflows/ci-unit-rust.yml` passes MBX hit and primary only; the pinned action has no matched-key output, so the candidate's explicit-prefix fixture is synthetic until another trusted producer supplies that input.

Regenerate the action and all workflow consumers from one final generator/runtime revision. Re-run the generated-parity tests and inspect the generated diff before interpreting any new CI report. Do not use source-only test output as live telemetry evidence.

## Deferred, separate defect

At source lines 181-185, if `VELNOR_JOB_QUEUED_AT` is absent the reporter computes `queue_seconds` from `github.run_started_at` and labels it `workflow_run_started_at`. This measures delay since workflow run creation, including planning and pre-job work; it is not the job's queue time. Keep this timestamp fallback fix separate from MBX archive classification and do not use its value in performance conclusions until the field is renamed or the source semantics are corrected.

## Acceptance evidence

- Source-script probe: all cases above exited 0; outputs recorded in this review.
- `rtk cargo test --locked -p velnor-workflow report_action_classifies_cache_outcomes_for_github_and_velnor_lanes -- --nocapture`: 2 passed (legacy and S2).
- The generated-parity test currently fails in both modules (2 failures), matching the `cmp` result and blocking live rollout until regeneration.
- Pinned upstream action: exact-hit boolean and primary key only; no matched key.
- Generated parity: currently fails, as expected while the source candidate is unregenerated.
- No wall-time, cache-hit-rate, compiler-reuse, or speedup claim follows from this review.

## Source implementation follow-up

The source action now applies one shared cache truth table. A matched-only key
pair is `unknown`; equal complete keys are `exact`; differing complete keys
are `prefix`. MBX `true` plus a nonempty primary key is exact even when the
pinned producer omits a matched key; missing primary, equal keys with
`false`, differing keys with `true`, false without matched evidence, and any
unrecognized boolean are `unknown`. Compiler log lines remain compiler
evidence only.

The source action now emits `schema_version: 2` for the relocated host-warm
declaration. Legacy and S2 focused tests pass locally. Generated parity and
live CI evidence remain pending regeneration by the parent; no performance or
cache-hit claim follows from this source-only result.

## Parent executable recheck

The isolated candidate at base `2447cc8d` passes 24 directly executed Bash/jq
input combinations for empty, true, false and invalid booleans against absent,
equal and differing keys. The source hash was stable throughout the probe.
The pinned producer's `true` plus primary key correctly remains `exact`
without a matched-key output; a missing primary is treated as incomplete
producer evidence. Both schema implementations' Rust classification tests
pass (2/2) in isolation from pending task-tool and policy changes.

Raw results: `../observations/mbx-telemetry-parent-truth-table-20260920.json`.
This resolves the source truth-table and schema findings. Regenerated parity,
exact committed-source checks and live CI remain required before rollout can
be called validated. No performance conclusion follows from these fixtures.
