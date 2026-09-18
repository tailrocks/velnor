# v-d2a: independent verification of feat/d2-provider-schema

Verdict: **CERTIFIED** (content) — landing requires the restructure in §5. No edits made.

Checked: `origin/feat/d2-provider-schema` = `d9b9ce45` (D2 core) + `8db9e5ac`
(pin bump), fetched this session. Base `bde4d4ec`; campaign tip `0477e14f`
(9 commits ahead of branch). Scratch worktrees `/tmp/d2a-verify` (tip),
`/tmp/d2a-oldparse` (base). Both commits carry `Signed-off-by` (DCO ok; `%G?`
= N, i.e. no GPG — same as siblings, not a gate).

## 1. Diff inspection (d9b9ce45, 57 files)

- **ProviderId + 13 surfaces**: all present. `provider.rs` (`GithubHosted <
  GithubSelfHosted < Velnor`, strict parse, dup/empty rejection) + config
  (`providers`/`automatic_providers`/`default_dispatch_providers` +
  `[workflow.selectors.<id>]`) + scan (`providers: &ProviderSet`,
  `RepositoryShape.providers`) + IR (`provider: ProviderId` fields) + plans
  (`units` + `excluded: Vec<PlannedExclusion>` + `plan_digest` frozen in plan
  outputs, runtime.rs:858-1004) + validation (unknown/dup/subset/disjoint
  hard errors) + runs-on (`runs_on_for`, provider.rs:203) + bootstrap
  (provider-keyed `workflow_runtime_setup`) + cache key grammar
  (repo/trust/provider/platform/…, provider.rs:748) + artifacts
  (provider+plan-digest segments) + policy + dispatch (`providers`
  multi-select; `inputs.runner`/`inputs.lanes` gone everywhere) + UI names
  (`{label} · {provider} — {unit}`, provider verbatim) + aggregation (strict
  shell verdict, see §3).
- **RunnerMode removal**: `RunnerMode`/`RunnerLane`/`DispatchChoice` types
  gone; `lanes.rs`, `runners.rs` deleted; `LaneJob` gone;
  `LANE_ADMITTED_*` → `PROVIDER_ADMITTED_*`; `tests/lane_pairing.rs` →
  `tests/provider_pairing.rs`; `--runners` → `--providers`. `tests/` legacy
  grep: zero hits. `src/` naive design-§3 grep hits **only** lib.rs:9332-9333
  (see F2).
- **Typed platform/trust/capabilities + negatives**: `Platform`/`TrustReq`
  (`untrusted-ok`/`trusted-only`)/`Capabilities` + matrix; 6 provider tests
  (exact ids, dup/empty/unbounded sets, strict platform/trust, missing
  selector, unsupported capability naming unit+provider+cap, platform/trust
  exclusions) + disjointness negatives (lib.rs:18487, config:2038). Merged
  (declared-over-default) selectors validated at generation (lib.rs:1653-1657)
  and policy — the declared-only early check cannot strand a collision.
- **3-provider fanout**: `kind_reusable_jobs_are_linear_in_units_not_a_matrix_product`
  pins **25 callers** (8×3+control), 3 `verify-*` jobs, 3 `Run unit checks`,
  no `strategy:`/`matrix.unit` in callers; `all_providers_emit_one_caller_per_unit_per_provider`
  pins identical unit sets per provider.
- **Strict expected-result set**: enforced in the **rendered shell** (verified
  in committed ci-pr.yml:1663-1705): admitted+expected → only `success`
  passes (`skipped`/`cancelled`/`*` fail, incl. missing→empty); selected but
  unadmitted → must be exactly `skipped` (wrong-provider arm); undeclared →
  `skipped` only, `success`/any run fails as unexpected; missing plan digest
  fails. `success|skipped` asserted absent (lib.rs:13194). `ResultIdentity`
  tuple matches spec §2 field-for-field — but see F1.
- **Fail-fast disabled**: nextest `fail-fast = false` retained; caller jobs
  have no strategy; remaining `fail-fast: false` matrices are pre-existing
  release/preview surfaces.
- **No-legacy mechanical test**: `generator_source_and_renders_carry_no_legacy_provider_vocabulary`
  walks all of `src/` + dogfood + fixture renders against 21 patterns; passes
  (589 lib tests green).

## 2. Gates rerun in scratch worktree (tip 8db9e5ac)

| gate | result |
|---|---|
| `cargo build -p velnor-workflow` | ok (13 dead-code warnings, see F1/F3) |
| `generate --plain --dry-run` | `0 files would change`, exit 0 |
| `generate --plain --check` | `Generated files are current`, exit 0 |
| `cargo test -p velnor-workflow` | **648/0** (lib 589, first_ci 33, pairing 9, synthetic 6, handoff 5, promote 4, generic 2) |
| contract crate | 6/0 |
| `cargo clippy -p velnor-workflow --all-targets` | exit 0, 0 errors |
| `cargo fmt -p velnor-workflow -- --check` | exit 0 |
| `cargo check --workspace --all-targets` | exit 0 |
| `policy --workflow-root . --base-revision bde4d4ec…` | **11 rules, 0 failed**, exit 0 |

All match `/tmp/d2a-schema.md` figures exactly.

## 3. Flag-day / landing analysis

**(a) Old-parse capability: REMOVED.** New binary on base tree (schema 1):
exit 1, `unknown field requires_trusted…` (deny-unknown-fields; schema gate
additionally rejects schema≠2, test `schema_must_be_exactly_two`). No dual
parser, no compat path — per spec §3.1 staged-release model.

**(b) Committed tree: NEW schema** (`schema = 2`, provider keys,
3-provider callers; 12 workflow files, +4718/−1519 vs base). New generator
renders it byte-identically (check exit 0, every file `= unchanged`).
Cross-renderer identity is impossible by design: old binary's schema gate
(`config.schema_error`, "the only accepted schema value") rejects schema 2
just as hard. Render-identity holds only as new-generator determinism. ✓.

**(c) Landing verdict post-rendezvous: GREEN iff restructured.** Blockers now:
1. `8db9e5ac` self-bump pins `d9b9ce45` — unmerged, unpublished; violates
   spec §3.2 (pin = already-published trusted runtime). Verified pin-only
   diff (7341ef4b…→d9b9ce45… strings + digests). **Must go.**
2. Branch is 9 commits behind campaign tip (c2, d1-worker, d1-processor,
   opstore, security-audit). **Must rebase.**
3. Rendezvous fix `e6839cfa` is **NOT yet merged** into
   `docs/bastion-final-plan`. **Must merge first**: with pin kept at
   `7341ef4b` (campaign tip value), the base (old-gen) validator's `--check`
   on the schema-2 tree exits 1 → `if … --check; then` falls through to the
   candidate path keyed on **head** closure → head-built candidate validates
   the head tree byte-identically → green. Without it, no render-changing
   generator PR can land green (rendezvous message, verified in hunk).

**Prescription**: merge rendezvous → rebase d2a content (= d9b9ce45) onto new
tip → drop 8db9e5ac, keep `revision = 7341ef4b…` → regen (expect tree =
content commit's tree modulo rebase drift) → land. The schema-2 tree +
pin-7341ef4b combination is exactly the rendezvous-covered shape. At a later
gate, a published post-D2 product + promotion commit moves the pin.

## 4. Findings (non-blocking; follow-up, not landing gates)

- **F1** Dead scaffolding: `ResultIdentity`/`ObservedResult`/`ObservedOutcome`/
  `VerdictFailure`/`evaluate_verdict`/`RunIdentity` have **zero callers,
  zero tests** (6 of the 13 build warnings). Enforcement lives in the
  rendered shell (wired + tested), so the d2a-schema.md wording overclaims
  the Rust vehicle. Recommend delete-in-follow-up (no-legacy spirit).
- **F2** No-legacy test's own `LEGACY` list leaves 4 literals unsplit
  (lib.rs:9330-9333: `pull_request_on_velnor`, `velnor_runner_group`,
  `default_dispatch_runner`, `automatic_lanes`), so the literal design-§3
  `rg` proof hits lib.rs. Runtime self-check still passes (suffix trick).
  Recommend `concat!`-splitting.
- **F3** New dead-code warnings: `scan_repository*` (now `pub(crate)` +
  test-only callers), `control_plane_trusted_gate` (caller deleted).
  Stale names: `lane_admission_expression` doc ref (lib.rs:3003, dangling),
  2 `*lane_admission*` test names, `has_trusted_runner_gate` +
  `trusted-runners` rule name (semantics now event-expression-based, labels
  gone — verified), `default_selectors()` vs "never a generator default"
  comment (lib.rs:2247; defaults are per-provider + disjoint + merged before
  validation, disclosed in d2a-schema.md — letter-vs-spirit note only).

## 5. Return

- Branch `feat/d2-provider-schema` content: **CERTIFIED** — all d2a-schema.md
  claims reproduced independently in a scratch worktree.
- Landing: **requires restructure** — rendezvous-first, rebase, drop
  self-bump, pin stays `7341ef4b…` at gate time. Post-restructure shape is
  the rendezvous-covered case and expected green.
- No merges, no pushes performed. Worktrees removed.
