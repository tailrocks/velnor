# A1 timing baseline — bastion campaign (tailrocks/velnor, read-only)

Source: `gh api` on live runs. All durations derived from `started_at`/`completed_at` per job/step.

## Runs

| Run | Event / branch | Conclusion | Wall (run_started→updated) |
|---|---|---|---|
| 35159519365 (sha 33688938) | push / main | success | 22:49:36→23:04:00 = **14m24s** (864s; matches `run_duration_ms: 864000`) |
| 35162700625 | pull_request / docs/bastion-final-plan | failure (3 Velnor-lane units) | 23:31:56→23:43:44 ≈ **11m48s** |

## Main green run 35159519365 — job table (non-skipped, longest first)

| Job | Start→End (UTC) | Duration |
|---|---|---|
| Docker · Docker / GitHub | 22:50:13→23:03:50 | **13m37s (817s)** |
| Rust · velnor-runner / GitHub | 22:50:14→22:54:12 | 3m58s (238s) |
| Rust · velnor-workflow / GitHub | 22:50:14→22:53:00 | 2m46s (166s) |
| Rust · velnorctl / GitHub | 22:50:14→22:52:16 | 2m02s (122s) |
| Rust · velnor-bench / GitHub | 22:50:13→22:51:28 | 1m15s (75s) |
| Rust · Rust production topology / GitHub | 22:50:13→22:51:14 | 1m01s |
| Rust · velnor-control / GitHub | 22:50:14→22:50:58 | 44s |
| Docker · Docker / Velnor | 22:50:13→22:50:57 | 44s |
| Rust · velnor-client / GitHub | 22:50:14→22:50:53 | 39s |
| Rust · velnor-model / GitHub | 22:50:14→22:50:45 | 31s |
| Policy | 22:49:40→22:50:11 | 31s |
| Rust · Rust dependency policy / GitHub | 22:50:14→22:50:44 | 30s |
| Rust · velnor-render / GitHub | 22:50:14→22:50:43 | 29s |
| Bun · Bun package (velnor) / GitHub | 22:50:13→22:50:41 | 28s |
| Rust · unit-collector / GitHub | 22:50:13→22:50:39 | 26s |
| Rust · velnor-tools / GitHub | 22:50:14→22:51:04 | 50s |
| Rust · velnor-workflow-contract / GitHub | 22:50:14→22:50:37 | 23s |
| Control / Planning | 22:49:40→22:50:00 | 20s |
| Bun · Bun package (velnor) / Velnor | 22:51:04→22:51:23 | 19s |
| Documentation · Documentation / GitHub+Velnor, OpenTofu / GitHub+Velnor | — | 10–14s each |
| Control / Prepare Cargo / prepare-cargo | 22:50:33→22:50:42 | 9s |
| ci-required → Control / Required | 23:03:53→23:04:00 | 2s + 3s |

## Step detail — critical path + measured phases

**Docker / GitHub (13m37s) — the critical path:**
| Step | Duration |
|---|---|
| Set up Docker Buildx | 12s (22:50:21→22:50:33) |
| **Run unit checks** | **12m56s (776s, 22:50:34→23:03:30)** |
| Save Docker build seed | 8s (22:50:30→23:03:38) |
| Post Buildx / checkout / complete | ~10s |

**Planning (20s):** Checkout 5s, Set up Velnor workflow runtime 10s, Publish runtime 2s.
**Policy (31s):** Checkout history 5s, base-setup checkout 2s, Set up runtime 12s, Enforce policy 1s, actionlint setup 2s + lint 4s.
**unit-bootstrap per Rust job** (toolchain restore 6–9s + Mise 2–4s + **Mr. Boxington 3s on hit / 50–67s on miss** + mold ~2s + unit-cache restore 3–5s):
- velnor-runner/GitHub (main): Boxington 60s, unit checks 2m34s (22:51:35→22:54:09)
- velnor-workflow/GitHub (main): Boxington 50s, unit checks 42s, Post-Boxington cache save 38s
- velnorctl/GitHub (main): Boxington 65s, unit checks 36s
- unit-collector/GitHub (main): Boxington 3s (hit), unit checks 3s
- Prepare Cargo job: 9s total (Prepare Cargo sources 2s)

## Latest PR run 35162700625 — job table (non-skipped, by completion)

| Job | Duration | Notes |
|---|---|---|
| Rust · velnor-runner / GitHub | **4m18s (258s)** — Boxington 67s, unit checks 2m33s | PR critical path (ci-required starts 23:43:34, right after) |
| Rust · velnorctl / GitHub | 2m22s | |
| Rust · velnor-workflow / GitHub | 1m56s | |
| Rust · velnor-bench / GitHub | 1m26s | |
| Docker · Docker / GitHub | **50s** (vs 13m37s on main) | warm: PR commands use GHA cache |
| Control / Planning | 13s | |
| Everything else GitHub-lane | ≤1m | |

## Critical path

- **Main (green):** Planning/Policy (~30s, parallel) → fan-out → **Docker/GitHub `Run unit checks` 12m56s** → ci-required/Required (~7s). Docker unit checks = 776/864 = **~90% of the 14m24s wall**.
- **PR:** Planning (~13s) → **velnor-runner/GitHub 4m18s** (Boxington 67s + checks 2m33s) → Required. Docker is off-path here (50s).
- Root asymmetry (from `.github/ci/project.toml` unit `docker`): PR builds via `buildx ... --cache-from/to type=gha,scope=docker,mode=max` (warm ≈ 50s incl. overhead), while main's `github_full_commands` run **plain `docker build` with no GHA cache flags** followed by the cache-export build — a cold full rebuild every main run (~13min).

## Proposal (ONE, measured, NO cargo/source-build fallback)

**Run main's first Docker full-build through buildx with the same GHA cache flags the PR command already uses** (`--cache-from type=gha,scope=docker,mode=max --cache-to type=gha,scope=docker,mode=max`), keeping the existing `velnor-cache-export` second command unchanged.
- Measured basis: same image/target builds in ~50s warm (PR, GHA cache hit) vs 12m56s cold (main, no cache flags).
- Expected effect: Docker/GitHub on main drops from ~13m37s to ~1–2m; run wall 14m24s → ~3–4m, where velnor-runner (~4m) becomes the next critical path.
- Adds no cargo or source-build fallback: pure BuildKit layer-cache reuse; main also refreshes the GHA cache generation PRs read. No workflow-interface or cache-key changes required.
