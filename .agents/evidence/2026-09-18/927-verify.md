# PR #927 verification — fix(ci): authenticate Dockerfile mise provisioning

Target: branch `fix/docker-mise-gh-token` @ `f66356eb418f7643c4526f18a7675c427f846eb9`, from main@c04aec98.
Method: read-only on repo (all content reads pinned to explicit SHA `f66356eb`);
local gates run in `/tmp/pr927` (`git archive f66356eb` + snapshot commit).
Note: the shared worktree HEAD moved mid-session (f66356eb → 52c424b1); pinned reads unaffected.

## VERDICT: HOLD

Do not merge. Reasons (§4): (1) PR-caused contract-test failure — stale
`seed_compat` fixture; (2) Policy FAIL — no candidate published, candidate
packaging hit a pre-existing trap bug triggered by main advancing to 52c424b1;
(3) one runner unit-test flake needs a clean rerun to confirm.

---

## 1. WIRING: PASS

`Dockerfile` is hand-owned, not generated: it is absent from the rendered-files
manifest `.github/ci/.github-actions-generator-state`, and generator sources
only *reference* it (command strings, watch lists) — so direct edit is valid.

Secret chain end-to-end (all line numbers at `f66356eb`):
- `Dockerfile:51` — `RUN --mount=type=secret,id=github_token` (optional mount,
  no `required=true`; local/Velnor builds without the secret keep old behavior)
- `Dockerfile:58` — `if [ -f /run/secrets/github_token ]; then export
  MISE_GITHUB_TOKEN="$(cat /run/secrets/github_token)"; fi`
- `Dockerfile:59` — `mise install --locked --yes rust mr-boxington`
- `.github/ci/project.toml:54-55` — GitHub-lane docker commands carry
  `--secret id=github_token,env=GITHUB_TOKEN` (PR, full, cache-export)
- `.github/workflows/ci-unit-docker.yml:310` — checks step env
  `GITHUB_TOKEN: ${{ github.token }}`
- Generator: `crates/velnor-workflow/src/lib.rs:2632` (const),
  `:2668-2670` (PR command), `:2756-2759` (full commands);
  `crates/velnor-workflow/src/primitives/ir.rs:1682` (env helper),
  `:4932`, `:5299-5301`, `:5906` (all three checks-step render sites,
  GitHub-lane + Docker-kind gated)
- `crates/velnor-workflow/src/runtime.rs:2616-2622` — unit commands spawn via
  `bash -euo pipefail -c` with no env manipulation; zero `env_clear`/`env_remove`
  in runtime.rs → step env (incl. `GITHUB_TOKEN`) flows into buildx `--secret`.
- Precedent confirmed: release lane passes `github_token=${{ github.token }}`
  (`.github/workflows/release.yml:3142`, renderer `primitives/release.rs:1784`).

Diff files (8) reviewed in full: secret *names* only
(`github_token`, `GITHUB_TOKEN`, `${{ github.token }}`, `MISE_GITHUB_TOKEN`);
no secret values.

## 2. COMPLETENESS: PASS with noted residuals

All `mise install` sites at `f66356eb` (Dockerfiles, yml/yaml, sh, Makefile,
actions, scripts, plus `*.rs` emitters):

| Site | Status |
|---|---|
| `Dockerfile:59` (`rust mr-boxington`) | COVERED by this PR (mount + `MISE_GITHUB_TOKEN`) |
| `docker/job-ubuntu.Dockerfile:121,154` | COVERED pre-existing (`required=true` mounts; release lane passes secret) |
| `.github/ci/project.toml:56-57` Velnor-lane docker cmds | UNCOVERED, intentional per author; optional mount ⇒ keeps unauthenticated behavior (residual flake risk on Velnor lane, whose jobs already fail environmentally) |
| `velnor-runner/src/executor.rs:6895` job-time `locked_mise_install` | UNCOVERED, out of scope: Velnor-lane runtime path, installs against baked image toolchain as integrity re-check; no token env |
| `s2/` parallel docker builders | UNCOVERED, dormant: repo declares `schema = 1` (`.github-gen/velnor-workflow.toml:1`); s2 dispatch (`s2/dispatch.rs`) requires `schema = 2` marker or `--providers`, neither present. Author notes follow-up; agreed |

No other `mise install` / `mbx setup` provisioning sites exist in CI-consumed
shell/workflow/Makefile/actions/scripts paths. `docker/build-mise.*` is consumed
only by the now-covered root-Dockerfile layer.

## 3. NO-REGRESSION: PASS

- actionlint: clean (exit 0, run bare from tree root exactly as CI does;
  direct-file invocation needs the repo-local `.github/actionlint.yaml`).
- D19 pin `a6fa8d4a…`: 0 occurrences in `c04aec98..f66356eb` diff;
  `.github-gen/velnor-workflow.toml` `revision` line untouched.
- `merge-base(origin/fix/docker-mise-gh-token, origin/main)` = `c04aec98` ✓;
  head still `f66356eb` (branch not updated); main has since advanced to
  `52c424b1` (#925).
- Final mergeable state: `MERGEABLE` / `BLOCKED` (blocked = failing checks).
- No local `docker build` run (per instructions); `buildx build --check` only (§5).

## 4. CI: FAIL

PR runs `35207507480` (CI/PR) + `35207507282` (Policy), testing merge
`b6f5d865` (head onto base `52c424b1`). Full non-skipped set concluded.

- `Docker · Docker / GitHub`: **SUCCESS** (3m0s) — the FAILURE-1 flaked job is green ✓
- `Policy`: **FAIL** (required SUCCESS) — pin render differs (expected), fell
  through to candidate path, then `no same-repository PR run published candidate
  velnor-workflow-candidate-f2c8d9668d593d19-Linux-X64` after ~5 min wait.
- `Rust · velnor-workflow / GitHub`: **FAIL** — unit tests 1624/1624 green,
  clippy green; then candidate-packaging step died with
  `fatal: '/home/runner/work/_temp/velnor-workflow-head' is not a working tree`,
  exit 128. Root cause: **pre-existing latent script bug** — `ci-unit-rust.yml`
  sets `trap 'git worktree remove …' EXIT` (:595) AND explicitly removes (:608-610)
  with no `trap - EXIT`; the second remove fails and the failing EXIT trap
  overrides the step status. Proven locally: failing EXIT trap overrides exit
  status; double `git worktree remove --force` reproduces the byte-identical
  fatal + exit 128. Script byte-identical at base `c04aec98` (PR untouched).
  Trigger: base advanced to `52c424b1` (#925 changed generator sources, which
  the candidate closure covers), so merge closure ≠ head closure → worktree
  path instead of the unit-binary shortcut #925 took (its run was green).
  No candidate artifact ⇒ Policy FAIL. Fix needs rebase (likely restores the
  shortcut) and/or a `trap - EXIT` root fix (author's call; script is generated).
- `Rust · velnor-workflow-contract / GitHub`: **FAIL — PR-caused.** Test
  `parameterized_callees_resolve_to_the_pre_parameterization_cache_keys` pins
  `velnor-docker-seed-v3-9c97194a807b-…` in fixture
  `crates/velnor-workflow-contract/tests/fixtures/pre_parameterization_cache_keys.rs:52,54,55`,
  but the PR rotated `seed_compat` to `8a4ee3c72d39`. Reproduced locally
  byte-identical. Author's gate list ran only `-p velnor-workflow`; this crate
  is not even a workspace member, so the miss is structural.
- `Rust · velnor-runner / GitHub`: **FAIL — flake, not PR-caused.**
  `protocol::tests::artifact_upload_sends_finalize_hash_and_rejects_unsuccessful_finalize`:
  `send artifact blob PUT: request body transfer failed` + `BrokenPipe` in a
  hand-rolled loopback-HTTP/sentinel test (timing race). Test code, job
  definition (`ci-unit-rust.yml` untouched by PR), and runner identical to the
  green main@52c424b1 run (35206688661: runner SUCCESS); PR shares no inputs
  with the `upload_artifact_blocking` path.
- Baseline-environmental fails (match main@c04aec98 run 35204207659 set):
  5 Velnor-lane jobs, `Control / Prepare Cargo / prepare-cargo`,
  `Control / Required`, `ci-required`.

Fail set is NOT within the affected-scope environmental set (contract +
packaging + runner are outside the baseline).

## 5. GATES (rerun locally in `/tmp/pr927` @ `f66356eb`): author's list confirmed, plus one miss

| Gate (author claim) | Rerun result |
|---|---|
| `cargo test -p velnor-workflow` green | CI nextest 1624/1624 green. Local `cargo test`: 1502/1504 paired-parallel; the 2 deltas are harness artifacts, not the patch: (a) my restricted PATH hid `/sbin/sha256sum`; (b) s1+s2 `closure_of_tree…` tests share pid-keyed fixture dir `velnor-closure-fixture-{pid}` (`closure.rs:…`) → deterministic collision under `cargo test`, green single-threaded; both files untouched by PR, identical failure on base `c04aec98` control |
| `cargo fmt --check` clean | PASS (exit 0) |
| `cargo clippy` clean | PASS (`--all-targets`, no errors/warnings) |
| regen `--plain --dry-run` 0 files | PASS (`0 files would change`) |
| regen `--plain --check` | PASS (`Generated files are current`; needed `--default-branch main` + fetching pin object into the snapshot — both snapshot artifacts, not content) |
| actionlint on 3 touched workflows | PASS (exit 0, bare run as CI does) |
| `docker buildx build --check` no warnings | PASS (`Check complete, no warnings found`, exit 0; lint-only, no creds, nothing built) |
| (author missed) `velnor-workflow-contract` tests | **FAIL reproduced locally, byte-identical to CI** |

## What the author must do (not done here — verify-only)

1. Update `pre_parameterization_cache_keys.rs:52,54,55`
   `9c97194a807b` → `8a4ee3c72d39` (rotation itself is legitimate: recipe changed).
2. Rebase onto current main (`52c424b1`) so merge closure == head closure
   (restores the candidate-packaging shortcut as in #925); consider also fixing
   the double-`worktree remove` trap bug at its generator source.
3. Rerun: needs `Docker / GitHub` SUCCESS + `Policy` SUCCESS + a clean
   `velnor-runner` run (flake confirmation) + `mergeable == MERGEABLE`.

```
WIRING=PASS COMPLETENESS=PASS(residuals-noted) NO-REGRESSION=PASS CI=FAIL GATES=author-list-PASS+contract-miss VERDICT=HOLD
```
