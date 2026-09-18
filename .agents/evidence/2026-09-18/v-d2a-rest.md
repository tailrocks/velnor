# v-d2a-rest: independent verification of feat/d2-provider-schema-rest

Verdict: **MISMATCH (wrong pin)** — tip is `375c0787` with pin `08ea1b07`;
contract requires pin `7341ef4b` at tip. The pin-revert follow-up never
landed within the 30-min bound (27 polls, tip unchanged). Content itself
verifies clean (see §1–§4); no edits made, no merges, no pushes.

Checked: `origin/feat/d2-provider-schema-rest` = `375c0787` (single content
commit on `8b7b4ac1`), fetched this session. Scratch worktree
`/tmp/v-d2a-rest-wt` (detached at tip; removed after). Commit carries
`Signed-off-by` (DCO ok; `%G?` = N, same as siblings, not a gate).
Branch was observed to appear during polling (absent → one commit on
`8b7b4ac1`): fresh push, no amend/force-push history. Parent is exactly
`8b7b4ac1` = current `origin/docs/bastion-final-plan` tip.

## 1. Content fidelity (CERTIFIED)

- **Rebase shape**: `merge-base` = `8b7b4ac1`, `rev-list --count` = 1.
  `8db9e5ac` and `d9b9ce45` are NOT ancestors (true rebase, not merge).
  Zero `d9b9ce45` / `8db9e5ac` strings anywhere in the tip tree.
- **Self-bump absent**: `8db9e5ac`'s 13 pin-embedding files carry the base
  tip's values, not the self-bump. `provider.rs` is byte-identical
  d9→rest (zero diff).
- **File coverage**: rest touches all 57 d9 files + 19 more (report action
  fix, `platform.rs`, `check_profiles`/`docs_site`/`prepared_tools` +
  tests/fixtures, `closure_reuse`/`docker_multiarch`/`migration_contract`/
  `platform_prerequisites` tests). Every extra file is a tip-new (#917-era)
  surface converted to provider vocabulary — sampled, all consistent.
- **d9 key content carried**: 3-provider fanout tests
  (`kind_reusable_jobs_are_linear…`, `all_providers_emit_one_caller…`;
  3 `verify-*` jobs, 3 `Run unit checks`), no-legacy mechanical test,
  `success|skipped` absence assertion (lib.rs:13891), strict rendered
  shell in ci-pr.yml (plan-digest binding + unexpected-job arms per
  provider), 6 provider tests, `UNIT_PROVIDER` (zero `UNIT_LANE` in tree).
- **Adaptations justified** (each checked against tip origin):
  - Apple split: base lane-partition (`verify-github-apple`,
    `lane_input::APPLE_EXECUTOR`) → provider partition
    (`verify-github-hosted-apple`, `provider_input::APPLE_EXECUTOR`);
    same mechanism, provider vocabulary. Rendered in all 5 unit
    workflows + `platform_prerequisites` tests.
  - Swift: SwiftPM portable default, Xcode Apple-bound; new pin test
    `plain_swiftpm_packages_stay_portable` (d9 mapped all Swift to macOS).
  - `ProviderAdmission::info_id` + dependency-info step (ir.rs:2648).
  - `[[check_profile]]` on `runs_on_for` selectors incl. velnor runner.
  - Stress padding 25→20 units (60 callers) under the 8 MiB ceiling
    (d9: 75 callers; renders grew with tip content).
  - **Real defect fixed** (verified present in d9): d9's
    report-velnor-ci-outcomes action declares `ci_lane` while all 15
    call sites pass `ci_provider` (v-d2a ran no actionlint, so it
    slipped through). Rest renames the input; `VELNOR_CI_LANE` env +
    report `lane` field kept stable. Zero `ci_lane` left in `.github/`.
- **Schema**: generation config `schema = 2`, runtime `project.toml`
  `schema = 3`. Estate `inputs.lanes` hits are untouched tip docs.

## 2. Gates rerun in scratch worktree (tip 375c0787)

| gate | result |
|---|---|
| `cargo build -p velnor-workflow` | ok |
| `generate --plain --dry-run .` | `0 files would change`, exit 0 |
| `cargo test -p velnor-workflow` | **936/0** (lib 826, integration 110) — matches author figures exactly |
| contract crate (manifest path) | 6/0 (2+4) |
| `cargo clippy -p velnor-workflow --all-targets` | exit 0, 0 errors (warnings only) |
| `cargo fmt -p velnor-workflow -- --check` | exit 0 |
| `actionlint -shellcheck=` | exit 0, clean |
| full `actionlint` (shellcheck) | HUNG in this environment (>30 min, terminated with session); same as author observed. Changed `run:` bodies are tip-verbatim shell |

## 3. Pin verdict: MISMATCH

- Tip pin is `08ea1b07c19f525effcb762cd4614724afcd33ee` (= base tip's
  value). Required: `7341ef4b…`. Author's rationale (tip moved since the
  brief; 7341ef4b is an ancestor — verified) does not satisfy the
  contract update: 08ea1b07 is UNAPPROVED, revert required.
- `7341ef4b` IS an ancestor of the base, so the revert keeps
  pin-reachable green. A re-verify of the new tip is required after the
  revert lands (pin-embedding surfaces change; rerun §2 there).
- Note: with EITHER pin (both pre-break), `generate --plain --check`
  exits 1 — the pin renderer cannot parse the schema-2 tree (`unknown
  field 'trust'`). The brief's "check exit 0 (+candidate notice
  expected)" is unsatisfiable by design for any §3(c) restructure: the
  pin render ERRORS (not differs), so `render_with_candidate` and the
  policy.rs:580 notice are unreachable, and v-d2a §3(c) itself predicts
  exactly this exit-1 shape ("base validator's --check exits 1").
  Observed: exit 1 with the pin-parse error — the §3(c)-predicted shape,
  not a branch defect. The "check exit 0" clause conflates v-d2a's
  self-bumped gate table with the restructured shape.

## 4. Landing: BLOCKED (two layers)

1. **Rendezvous absent** (prescription blocker 3 open): `e6839cfa`/`033ab546`
   (#918) is in `origin/main` but NOT in the campaign line; base and rest
   tree both carry the old-shape acquire (`equal the pin's candidate`
   ×2 in lib.rs; rendezvous shell only in main's ci-policy.yml).
2. **NEW finding — rendezvous may be insufficient for a schema break.**
   Empirical test: the pin binary (08ea1b07, the validator CI's Enforce
   step runs — rendered `rev:` confirms) fails parsing the schema-2
   generation config BEFORE any policy rule runs, with or without a
   bound head candidate manifest (closure+digest verified matching).
   All candidate logic lives inside policy evaluation, which never
   starts; and `regenerate_and_compare` propagates a pin-render ERROR
   before trying the candidate. So the §3(c) "candidate validates
   byte-identically → green" path cannot engage on a schema-break tree
   even post-rendezvous (rendezvous covers render-DIFFERS, not
   render-ERRORS). The flag-day landing mechanism (break-glass merge vs
   a parse-tolerant policy bootstrap) is unresolved — parent decision
   needed. Local `policy` run: 9/11 pass; `generated-tree` fails (above)
   and `pin-monotonic` fails only as a local-emulation artifact (head
   binary cannot parse the base config for the merge-base exception).

## 5. Return

- Content: **CERTIFIED** — faithful d9b9ce45 rebase with justified
  adaptations; DCO signed; fresh single-commit history; schema 2;
  dry-run 0; 936/0 + clippy + fmt + actionlint green.
- Contract: **MISMATCH** — tip pin `08ea1b07`, required `7341ef4b`;
  revert follow-up did not land in 30 min. Re-verify tip after revert.
- Landing: **BLOCKED** — rendezvous not in campaign (§4.1), plus the
  schema-break green-path gap (§4.2) needs a parent decision.
- No merges, no pushes performed. Scratch worktree removed.
