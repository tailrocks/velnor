# rec-W verification: tailrocks/velnor#936 (`recovery/pr-W`)

Method: fresh detached worktree `/tmp/recw-wt` @ `67fc180e`; read-only except `/tmp`.
Refs: main `58a94b12`, source `8b7b4ac1`, base `dead5ecb`. Checklist = forensic S1+S2 LOST rows in scope
(18 rows: `crates/velnor-workflow/**` + 2 `setup-velnor-workflow/action.yml` + `velnor_first_ci` line).

## 1. COMPLETENESS — FAIL (1 MISSING)

Re-ported (spot-diff vs `8b7b4ac1` blobs / hunk-check), all verified:
- byte-IDENTICAL to source: `promote.rs`, `promote_atomic.rs`, `closure.rs`, `policy.rs`,
  `policy/tests.rs`, `README.md`, `build.rs`, both `action.yml` (SIGNER-C `--source-ref` + SRCID
  revision clause present), `render_tree` fn, 4 provisioner tests, `MANIFEST_ACCEPT_FILTER` (+doc).
- identical modulo verified mechanical adaptation: `apt.rs` (+1 `jobs` field — main's
  `ReleaseSpec.jobs`), `runtime.rs` (+1 `jobs`), `consumer_negatives.rs` (5-line
  `pin_candidate`→`head_candidate` — main renamed it: `head's candidate` present on main's s1),
  `ir.rs` (main's `token_env`/trap lines + same pin→head doc word), `config/mod.rs` (pure
  addition; B1 apt fields present, `apt` count 15=15), `release.rs` (B1/no-latest/PINFETCH
  present; main's tasks-publisher + `jobs` adaptations), `runtime_products.rs` (markers
  HEAD==branch exactly: stale 11, source-ref 9, revision-clause 2, single const def; sole
  branch-only line is the recomputed PINNED), `velnor_first_ci.rs` (1-line `--source-ref`
  assertion = branch's bytes on top of main's R2m transplant).
- correctly NOT re-ported (present-via-other-path; re-adding = duplication): PERF-GHA s1
  (`docker_hosted_full_command` absent; 3 `cache-from` hits pre-exist on main), SIGNER-P
  producer hunks (via #916), SRCID producer half (via #916). Item-set diff branch-vs-HEAD
  for lib.rs leaves only: 2 PERF-s1 items, 1 SEC-B1 test, `rendered_repository_files`
  (main replaced with fixture helpers — R2m), `checked_in_workflows_...` (base content,
  removed on main by `24d1df91`), `policy_candidate...to_pin...` (renamed `to_head` on main).
- dropped WITH stated reason (accepted, thin): SEC-B1 workflow bits — provisioner `check_slot`
  (branch 5 hits → HEAD 0; HEAD megaline byte-EXACT vs `40ca2764^`, proven), feed attestation
  (HEAD megaline byte-EXACT vs pre-SEC-B1, 12903 chars; branch delta = pure insertions),
  rollback-charset + rehash tests. Author: "pre-SEC-B1 tree (byte-exact)", "attestation-identity
  bits stay excluded". Module separation is principled (SEC-B1 is 90% runner-side) but no
  follow-up tracking is named — see HOLD-3.
- s2 sync (`ee245bf9`): FAITHFUL text sync, not redesign. `s2/mod.rs` delta = one 45-char
  insertion `(.revision | test(...)) and `; `s2/.../runtime_products.rs` = same filter const +
  doc + recomputed PINNED. No R2/d2b logic duplicated. Coherent: s2 producer already emits
  `--revision` (6 hits on main), so the stricter filter still matches.

MISSING (no re-port, no stated reason):
- **GUEST-PAYLOAD `release.rs` hunk (`6be652b5`)**: renderer `path: dist/microvm/*` wildcard →
  explicit 5-file list (1 line) + 17-line test. HEAD line 1327 still renders the wildcard,
  identical to main. Falls between PRs: `recovery/runner-side` recovers the `guest-image.rs`
  builder half but touches ZERO workflow files, so the renderer half (the actual EACCES fix)
  is recovered nowhere.

Scope discipline: PASS. 24 changed files = 18/18 in-scope LOST rows + 2 s2 text-sync +
4 generated/state (3 workflows = 4 filter-line pairs only; action copy in sync with source;
state = hashes). Nothing else touched.

## 2. FAITHFULNESS + HUNK SURGERY — PASS

All 6 shared-evolved files preserve main's evolution; every main→branch minus-line verified as
the branch's own replacement of base content (checked against `dead5ecb` blobs), never a drop
of main-unique content: `run()` scan block → `render_tree` (main block == base verbatim;
HEAD fn byte-identical to branch), 2-arg→1-arg provisioner (HEAD == pre-SEC-B1 branch form),
`Platform.runner: String`+`platforms(config)` → fixed `&'static str`+`platforms()` (base form,
branch's A2-403 refactor), tasks-publisher/`jobs`/token-env/transplant = main's kept.
No s2 duplication of R2/d2b logic (above). No PERF-GHA re-added to s1 (above).

## 3. PIN+BASE+DCO — PASS

- D19 pin `revision = "ec3995277f82473777f18969e58ea76f63e54cfd"` identical main vs HEAD;
  `GENERATOR_REVISION = "50"` on main/HEAD/source (skip claim valid).
- `merge-base == origin/main == 58a94b12`, re-verified at end (fresh, no HOLD-stale).
- DCO `Signed-off-by` on all 8 commits (1 each); all 8 subjects Conventional (`fix/feat/chore/test`).

## 4. REGEN — PASS

- `cargo build --locked -p velnor-workflow`: ok.
- `velnor-workflow --plain --force` → exit 0, "Generated 21 files", `git status` clean (0 changes).
- `velnor-workflow --plain --dry-run` → "0 files would change".
- Committed churn classified, all from recovered sources (4 filter-line pairs + action copy + state).
- CI's `--plain --check` step also clean (no `generated files differ` in unit log).

## 5. GATES (rerun locally) — PASS

- `cargo test -p velnor-workflow`: **1812 passed, 0 failed** (lib 1689; `promote_atomic` 4;
  `consumer_negatives` 19/19; `velnor_first_ci` 33).
- `cargo test -p velnor-runner`: all green (2166 + suites, 0 failed).
- contract (`crates/velnor-workflow-contract`, standalone workspace): 6 passed, 0 failed.
- `clippy --all-targets -p velnor-workflow -- -D warnings`: exit 0.
- `cargo fmt --check`: exit 0. Bare `actionlint` (CI form): exit 0.

## 6. CI — FAIL (1 root cause + downstream; run on HEAD `67fc180e`)

Fail set: `Policy` FAILURE; `rust-velnor-workflow / GitHub·hosted` FAILURE; 4× `/velnor` +
`prepare-cargo` FAILURE; 2 rollups. Baseline `main@58a94b12` fails IDENTICALLY on the
velnor-lane + prepare-cargo set ("Velnor rejected this job before workflow execution") —
those are environmental, not new. DCO SUCCESS. All other `/GitHub` SUCCESS.
- Root cause (NEW vs baseline): recovered B1 test
  `apt::tests::deb_control_fields_read_through_both_backends` fails on ubuntu:
  `package: tar failed with status exit status: 2`. **Reproduced** in `ubuntu:24.04` container:
  GNU tar says `Archive is compressed. Use -z option`. `run_tar_stdin` pipes `.tar.gz`
  payloads to `tar -x -C dir -f -` with no `-z` — works on bsdtar (macOS), fatal on GNU tar.
  Bug is original to `ed442855` (identical args), faithfully recovered — but red on Linux CI.
  Blast radius: shared helper feeds `deb_control_field` + `deb_extract_data` → assemble/verify
  flows; any GNU host without `dpkg-deb` hits it. Nextest fail-fast stopped at test 13/1812,
  so further latent ubuntu failures may hide behind this one — needs a FULL ubuntu run.
- Downstream: unit failure → `Prepare/Publish candidate generator product` steps skipped →
  Policy candidate path fails (`no same-repository PR run published candidate ...`).
  Policy's `generated files differ: ci-runtime-products.yml, ci-unit-rust.yml, release.yml,
  state` line is EXPECTED (pinned pre-PR renderer vs PR tree = exactly the `ee245bf9` set);
  the candidate path is the designed route and locally the PR generator reproduces the tree.
  No separate Policy defect.

## VERDICT: HOLD

1. **GUEST hunk**: re-port `6be652b5`'s `release.rs` renderer line + test (or state a drop reason).
   Currently recovered nowhere (absent from both `recovery/pr-W` and `recovery/runner-side`).
2. **GNU-tar portability**: fix `run_tar_stdin` compression handling (shared helper, not the test),
   then show a full green ubuntu `rust-velnor-workflow` run (fail-fast hid the rest) and Policy
   SUCCESS via published candidate.
3. **Track SEC-B1 follow-up**: the workflow-side `check_slot`/feed-attestation/rollback-charset
   exclusion is explicit and byte-verified but names no follow-up; record where it lands.

Not merged. Worktree left at `/tmp/recw-wt` (detached `67fc180e`); logs in `/tmp/wf-gh.log`,
`/tmp/policy.log`, `/tmp/prep.log`, `/tmp/vel.log`; repro `/tmp/repro.sh`.
