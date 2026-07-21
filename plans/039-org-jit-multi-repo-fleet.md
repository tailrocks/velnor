# Plan 039: Org-level JIT + multi-repo fleet operations

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/protocol.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/src/cli.rs docs/runner-usage.md`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P0 (V0.1 — 13 repos × 3 orgs cannot ship on per-repo daemons)
- **Effort**: M (smaller than assumed — see Current state)
- **Risk**: MED (fleet identity changes; job routing)
- **Depends on**: none (verify + ops work dominates)
- **Category**: direction / stability
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

The estate standardization registers 13 repos across `jackin-project`,
`ChainArgos`, and `tailrocks`. Per-repo JIT daemons at that scale are
ops-heavy and waste warm caches. Org-level JIT + runner groups gives one
fleet per org with shared host stores. KEY FINDING from the code audit: the
**protocol layer already supports org scope** — the remaining work is
verification, daemon ergonomics, docs, and fleet migration, not a protocol
build-out.

## Current state (verified against `48b04ad`)

- Revalidated 2026-07-21: `tailrocks` has only runner group id 1 (`Default`,
  `visibility=all`) and zero organization runners; five healthy slots remain
  registered to `tailrocks/velnor`. The authenticated token has `admin:org`,
  `repo`, and `workflow`, but creating/configuring the proposed
  `velnor-trusted` group remains an explicit operator-authority boundary.
- Sentry resolves to `5.9.55.237`; the presented ED25519 fingerprint is
  `SHA256:n42CpA98ASrRoKhbt5xhFTKnqIV/AbRNHHlMktbFtok`. It is not trusted yet;
  the operator must verify it out of band before accepting it.

- `crates/velnor-runner/src/protocol.rs:126-134` (verified excerpt):
  ```rust
  fn token_scope_path(segments: &[&str]) -> Result<String> {
      match segments {
          [org] => Ok(format!("orgs/{org}")),
          [first, second] if first.eq_ignore_ascii_case("enterprises") => ...,
          [owner, repo] => Ok(format!("repos/{owner}/{repo}")),
  ```
  `GitHubScope::parse` (`protocol.rs:68`) builds `jit_config_url` as
  `{token_scope}/actions/runners/generate-jitconfig` (`:83`) — so
  `--url https://github.com/<org>` already routes org-scoped JIT.
- JIT request: `runner.rs:281-286` `GitHubJitConfigRequest { name,
  runner_group_id, labels, work_folder: None }`; `runner_group_id =
  args.pool_id.unwrap_or(1)` (`:280`); labels normalized `runner.rs:5946`.
- Registration call: `RegistrationClient::generate_jit_config`
  (`protocol.rs:546`, curl subprocess).
- Daemon config: one `--url`/`--name`/`--labels`/`--pool-id` per daemon
  (`cli.rs:249-283`); slots under `<config>/slots/slot-N`
  (`runner.rs:1440`); re-registration per recycle
  (`recycle_daemon_slot`, `runner.rs:1050` → `configure()` `:1073`).
- Doctor: `runner.rs:5762` `doctor()` — REST `list_runners` against the
  configured scope.
- Trust: org pool shares stores across repos — the trust model already
  scopes writable stores by `<trust>/<repo>` (see
  `container.rs:773-815`, `executor.rs:4040`), so an org fleet is safe for
  same-trust repos; fork isolation stays a separate non-trusted pool
  (`VELNOR_TRUST_SCOPE`, enforced `runner.rs:2528`).
- Ops docs: `docs/runner-usage.md` production section (apt/systemd).
- Fleet plan (`VELNOR_PROJECTS_SETUP.md` §6): ChainArgos 10+4 slots,
  jackin-project 6–10, tailrocks 8–12 shared; label
  `self-hosted` + `velnor-target-mvp`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Org registration smoke (operator) | `velnor-runner configure --url https://github.com/tailrocks ...` | JIT config saved |
| Doctor (operator) | `velnor-runner doctor --url https://github.com/tailrocks ...` | fleet listed |

## Scope

**In scope**: org-scope verification tests (wiremock harness), CLI/docs
ergonomics for org fleets (runner-group flag docs, doctor output labeling),
`docs/runner-usage.md` org-fleet section, a fleet-migration runbook
(new `docs/org-fleet-migration.md`).
**Out of scope**: dynamic slot autoscaling (V1.4 — roadmap), multi-host
stores (V2.1), GitHub-side org/group creation (operator, via UI/API — the
runbook documents the steps, code does not call admin APIs).

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):`/`docs:` commits, `git commit -s`;
no push without operator instruction.

## Steps

### Step 1: Protocol-level org tests

In `crates/velnor-runner/tests/broker_protocol.rs` (wiremock harness — model
`broker_run_service_happy_path_acquires_and_completes_job`, `:24`): add
`org_scope_jit_config_targets_orgs_path` — a `GitHubScope::parse` of an org
URL produces `orgs/<org>/actions/runners/generate-jitconfig`, and (if the
harness reaches `generate_jit_config` — it uses curl; if not mockable, test
`token_scope_path`/URL construction as unit tests in `protocol.rs` instead).
Also `enterprise_scope_parses`, `repo_scope_unchanged`.

**Verify**: `cargo nextest run -p velnor-runner org_scope` → pass.

### Step 2: Runner-group ergonomics

`--pool-id` defaults to 1 (the org Default group). Add `--pool-name`
resolution: list runner groups via the REST API
(`GET orgs/<org>/actions/runner-groups`, same auth as `list_runners`) and
resolve name → id, replacing today's "pool-name requires numeric pool-id"
limitation (`runner.rs:276-278`). Keep repo-scope behavior untouched
(groups don't apply).

**Verify**: unit test on the resolver with wiremock; CLI help text updated.

### Step 3: Doctor labels the scope

`doctor()` output prefixes the scope kind (org/repo/enterprise) and, for
orgs, the runner-group of each runner if the REST payload provides it.

**Verify**: doctor formatter unit test.

### Step 4: Migration runbook

`docs/org-fleet-migration.md`: (1) create org runner groups per
`VELNOR_PROJECTS_SETUP.md` §6 capacities, granting repo access lists;
(2) per host: stop per-repo daemons cleanly (drain), reconfigure with org
URL + group, start; (3) label continuity — keep `velnor-target-mvp` so
estate `runs-on` never changes (atomic label migration rule); (4) trust
lanes — trusted org pool vs non-trusted fork pool with distinct labels;
(5) rollback path. Update `docs/runner-usage.md` to point at it.

**Verify**: doc exists; `VELNOR_PROJECTS_SETUP.md` §6 numbers mirrored.

## Test plan

3+ new tests (step 1–3). Operator acceptance: one org (tailrocks) migrated
first — doctor green, a velnor-repo CI run (plan 050) picked up by the org
fleet, warm-store hit confirmed on second run. Then the other two orgs.

## Done criteria

- [ ] Gates exit 0; new tests pass
- [ ] `--pool-name` resolves against org runner groups
- [ ] Runbook complete; runner-usage updated
- [ ] Operator: tailrocks org fleet live (or explicitly scheduled)
- [ ] No out-of-scope changes

## STOP conditions

- Org JIT registration returns 403/404 with a fine-grained PAT → the PAT
  scope/permission set is wrong (needs org self-hosted-runner admin);
  STOP and report the exact API error for the operator (credential change,
  not code).
- Runner-group REST shapes differ from expectation → align to
  actions/runner's client behavior (AGENTS.md source-of-truth rule); if
  ambiguous, STOP.

## Maintenance notes

- Org fleets make host stores multi-repo by default — the delivered storage,
  capacity, and GC contracts formerly tracked by plans 035–037 become
  load-bearing; do not migrate ChainArgos unless those live gates remain green.
- Slot counts in §6 are starting points; doctor + queue-wait SLOs (plan 045)
  drive resizing.
