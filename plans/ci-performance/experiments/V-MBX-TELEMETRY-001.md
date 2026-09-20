# V-MBX-TELEMETRY-001: preserve cache evidence boundaries

Status: source implementation complete locally; generated action regeneration,
independent review, and live CI validation remain required before integration.

Run 35492230871, job 106029138322, contains `No mbx cache found` at
`2026-09-20T05:40:46.5166507Z`, while its report says
`cache_outcomes.mbx = prefix`. The reporter maps `cache-hit=false` to
`prefix`, then searches the combined compiler log for `exact hit`, `warm
start`, or `miss`. This confuses the MBX archive lookup with compiler-cache
statistics. The retained excerpt is
`plans/ci-performance/observations/velnor-35492230871-collector-order-cache.txt`.

The pinned `jdx/mr-boxington-action` v1.4.0 contract is the primary source:
its `action.yml` exposes `cache-hit` (exact-key boolean), `cache-primary-key`,
and `mbx-version`; it does not expose the restored/matched key. Its source
sets `cache-hit` from `restoredKey === primaryKey`, sets the primary output,
and logs the restored key. A false boolean therefore cannot distinguish a
miss from a restore-prefix hit. The pinned sources are [action metadata](https://raw.githubusercontent.com/jdx/mr-boxington-action/867fc530102eec5b756075d70d850dc8330d2272/action.yml),
[action implementation](https://raw.githubusercontent.com/jdx/mr-boxington-action/867fc530102eec5b756075d70d850dc8330d2272/src/index.ts),
and [GitHub cache semantics](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

## Remedies considered

1. Keep mapping false to `prefix`. Rejected: it manufactures a hit class from
   an output explicitly defined only as exact-hit status.
2. Parse compiler or MBX summary lines in the combined unit log. Rejected for
   archive classification: compiler summaries describe object lookups and
   bypasses, while the action output describes the GitHub archive. Human log
   wording is not the action output contract.
3. Add an explicit matched-key input to the reporter and classify
   `exact`/`prefix` only when a producer supplies the primary and matched keys.
   The source action now applies one complete-key truth table: a matched-only
   pair is `unknown`; a complete equal pair is `exact`; a complete differing
   pair is `prefix`. MBX `true` with a nonempty primary key is exact even when
   the pinned producer omits a matched key; a missing primary, contradictory
   key pair, false without matched evidence, or unrecognized boolean is
   `unknown`. This is the selected bounded design.
4. Replace the pinned action or add a wrapper that emits the restored key.
   This would provide stronger evidence, but it changes an independently
   pinned external product and its admission/provenance contract. Queue it as
   a separate transport change rather than guessing from logs.

The reporter will carry host-persistent declarations in a separate
`cache_declarations` field. A declared Velnor host-warm layer is useful
context, but it is not presented as a keyed archive outcome or compiler-cache
proof. Compiler lines remain in `compiler.mbx_outcomes` for independent
analysis. The executable fixture matrix covers exact, explicit prefix,
incomplete primary/matched evidence, false without matched-key evidence, and
misleading compiler-log text. An incomplete key pair is `unknown`.
Recorded outputs and source/script hashes are in
`plans/ci-performance/observations/mbx-telemetry-fixtures-20260920.json`.

The source-owned action emits report `schema_version: 2` because
`cache_declarations.host_warm_layers` is a relocated field, with no v1
compatibility alias. Legacy and S2 generator tests cover the truth table and
the compiler-log separation. The checked-in generated action remains stale
until the parent regenerates it from this source revision.
