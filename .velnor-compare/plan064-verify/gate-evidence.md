# Plan 064 verification — 2026-08-24

Range: `139fcdb..5ab7479`, branch `velnor-estate-standard`.

| Check | Command | Result |
|---|---|---|
| Focused suite | `rtk mise run test-focused -- -p velnorctl -p velnor-client -p velnor-model -p velnor-control -p velnor-render` | exit 0, 23/23 passed |
| Full gate | `rtk mise run check` | exit 0, 877/877 tests + deny ok |
| Whitespace | `git diff --check 139fcdb..5ab7479` | exit 0, clean |
| Scope audit | `git diff --name-only 139fcdb..5ab7479` | 17 paths, all in allowed set; no `fleet/release-refs.toml` in range |
| Secret scan | diff grep token/secret/password/ghp_/github_pat | exit 1 (no matches) |

Fixture: tailrocks/velnor-actions-fixture run `32714994603` (workflow_dispatch,
control-plane): status=completed, conclusion=success, 10:05:35Z→10:06:03Z
(28s ≤ 60s). Matches executor claim.

Criteria:
1. New crates exist with acyclic dependencies — SATISFIED (`dependency_boundaries`:
   seven packages, direction matches plan, acyclic — all pass in focused suite).
2. Parser/dispatch seams without leaf command — SATISFIED (`no_unimplemented_command_exits_success`,
   `every_legacy_runner_command_is_rejected`, legacy names never register).
3. No spawn/parse of old binary — SATISFIED (only `CARGO_BIN_EXE_velnorctl`
   spawned in cli_smoke.rs:68-86; no velnor_runner dep outside doc/rejection strings).
4. `rtk mise run check` passes — SATISFIED (exit 0).
5. Fresh fixture success run — SATISFIED (run 32714994603).

Caveat: HEAD advanced to f562d5b during verification (parallel 039 actor,
fleet/release-refs.toml only). 5ab7479 confirmed ancestor; gates ran on tree =
range + that single fleet commit.

VERIFY: PASS
