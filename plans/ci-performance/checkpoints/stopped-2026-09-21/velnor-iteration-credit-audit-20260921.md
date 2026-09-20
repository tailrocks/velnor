# Iteration-credit audit — 2026-09-21

Canonical README still reports zero substantive optimization iterations, zero plateau credit, and no accepted speedup. That remains correct. The audit finds five defensible **diagnostic/correctness** credits if section 13 allows an intervention that improves correctness or feedback ordering without improving elapsed time:

1. `V-MBX-001.transport`: directory transport passed a real manual-dispatch exercise, with an independent conditional transport PASS. Bootstrap, trust, compiler reuse, and performance stay unresolved.
2. `V-BUN-001`: the scanner regression proves the old watch set misses a required Bun/TypeScript input; the candidate preserves it and passed exact PR gates. This is coverage correctness, not a speedup.
3. `V-MISE-LOCK-001`: actual Mise 2026.9.11 probes and independent contract review prove missing lock membership fails and generation uses locked installation. Bootstrap and scheduled-child fixture changes are supporting work, counted once.
4. `V-RUST-ORDER-001.failfast`: deliberate failure-order samples reproduce earlier actionable feedback while preserving the command set. The independent review holds every Velnor timing claim because host load confounded the samples.
5. `V-ROLLBACK-SIGNAL-001`: Bash 3/5 and process-group fixtures plus exact CI validate fail-fast, lock retention, cleanup, and single rollback. Scheduler cancellation and performance remain unvalidated.

These are recommendations, not a ledger edit. They must not be presented as performance acceptance, target/10 progress, or a plateau. The five units are not split by fixture, provider, or phase.

The Parallax embed-ui guard is a separate provisional sixth candidate. Its final external review and fixture validation are strong, but it is not indexed in the canonical experiment set; add one canonical record before crediting it. Keep guard and watch-selection evidence together.

No credit is recommended for MBX telemetry yet: initial truth-table work lacked generated/live schema4 proof, and later writer phases exposed aggregate and chronology gaps. The unchanged measurement rerun, the superseded rolling lookup result, and the unsafe 0dc pin downgrade remain zero. Pending policy, identity, watch, gate, cancellation, metadata, consumer, and cache-reuse work remains zero until its stated independent and real-CI evidence exists.

Machine-readable detail: `/private/tmp/velnor-iteration-credit-audit-20260921.json`.
