# CI/CD cache-architecture evidence map

Repo: `tailrocks/velnor`. Captured 2026-09-13 after WP5 merge `#711`
(`b542a95`), WP6 merge `#716` (`aea5c7b`), evidence `#729` (`b91f837`),
Velnor-default `#732` (`c383c69`), guest-seed harden `#723` (`9657f21`).

## §8.2 work packages

| WP | Package | Status | Evidence |
| --- | --- | --- | --- |
| 0 | Phase timings / negative-boundary | MERGED | PR #664. `scripts/test-check-release-feature-boundary.sh`. |
| 1 | Cargo-source cache + frozen prep | MERGED | PR #668. |
| 1.5 | Policy transport + pin | MERGED | PRs #676 / #677. |
| P0 job-log | Velnor-lane job-log | MERGED | PR #685. Live rejection still on fleet v0.1.274 until apt upgrade. |
| 2 | Toolchain + min tools | MERGED | PR #686. |
| 3 | Preview/Release persistence + Docker seed | MERGED | PR #689. Docker cold save run `34745390724` job `103692529918`; warm hit `34745867993` job `103693815212`. Release cargo cold `34748391592` job `103700698681`; warm `34750575182` job `103706298786`. P0 fetch-online #692+#693. |
| 4 | Bounded snapshot + maintenance | MERGED; §8.2 (c) BLOCKED | PR #694. (a)(b) Preview `34753493092` job `103715332015` saved `…-594bb9c9…` then `34754981233` job `103718486096` saved `…-9e65eceb…` same compat `58835fa41d40`. (c) After #732 pin `9ffbcd0`, Maintenance `34765436872` job `103745452027` on `velnor-dogfood-slot-2-next-1640932-4` (Velnor Runner/0.1.274, image velnor/job-ubuntu:26.04). Collect failed: `error: unexpected argument '--mode' found` / `Usage: velnor-workflow <TARGET> [TARGET]`. No `evicted id=` lines. Cause: #732 put Maintenance on `[self-hosted, velnor-target-mvp]` and skipped setup-velnor-workflow; image/apt CLI has no `cache-plan`. Comment posted on #694. |
| 5 | Canonical `/out`, guest seeds, runtime verify | MERGED; runtime producer BLOCKED | PR #711 squash `b542a95` + harden #723 (`9657f21`, restore-keys removed). Pins SOURCE+POLICY `9ffbcd0` via #732. Zero artifacts named `velnor-workflow-runtime-9ffbcd0*`. `b542a95` artifacts exist only on branch `chore/pin-runtime-wp5` (runs `34761858221` / `34763245086` / `34763475737`), not main. Planning `34766123405` job `103747410096` (workflow_dispatch, Select succeeded `scope=full`) Prepare failed `install: No such file or directory` (`$HOME/.cargo/bin/velnor-workflow` missing). Publish skipped. Planning `34766230211` job `103747659923` (push) Select failed `error: trusted events require full CI scope` with empty `CI_SCOPE_OVERRIDE`. Publish skipped. Guest-seed cold save: Preview `34762272049` job `103738096096`: `Cache not found` then `Cache saved with key: velnor-guest-seed-x86_64-f57b588a260da900d4291a8f61a0e81270b6cd445c82c80a7fef5cf5c7f651ce`. Prefix-stale then rebuild: same run job `103738096068` aarch64 hit restore-key `2b72d682` then saved `f57b588a`. Preview `34765097380` job `103745220741` x86: restore-key `f57b588a`, agent bytes differ (`cmp` byte 25), saved `32e8532b`. Exact-hit BLOCKED: runner-src PRs keep changing hashFiles; #723 removed prefix restore-keys so next unchanged-recipe Preview is the hit. |
| 6 | Velnor capacity / persistent-builder | MERGED | PR #716 (`aea5c7b`), supersedes #709. Nightly `34747340103` (17 jobs, queue median 43s / exec median 30s). Host cache job `103697787049`. Docker `103684693084` → `103697887744`. Protocol blocked `103697787039` / `103689451226` on v0.1.274. Stall `34748391614`. No capability-gate bypass. |

## §9.2 scenarios

| Scenario | Status | Evidence |
| --- | --- | --- |
| Two hosted runners, same commit | PARTIAL | Preview metadata jobs `103715332015` / `103718486096` restore rustup+cargo+mbx. |
| Source-only change | PARTIAL | WP4 (b): same compat, new freshness save. |
| New PR on warmed Main | PARTIAL | PR #694 run `34751298318` restore-only mbx-v3 / docker-seed-v3 keys. |
| Merge SHA ≠ PR caches | BLOCKED | CI/main cancelled/failed Planning. |
| Lockfile change → one prep path | GENERATOR | WP1 fetch-then-offline. |
| Different target/profile/features | GENERATOR | `compatibility_digest_changes_when_a_fact_changes`. |
| Fresh BuildKit, source-only Docker | DONE | WP3 Docker warm `34745867993` job `103693815212`. |
| Docker `/out` products | MERGED + tests | Dockerfile `/out` in #711; tests `canonical_docker_products_*`. |
| Preview and tagged release provenance | OPEN | Preview ran; tagged release not recut. |
| Evicted seed → one recovery | BLOCKED | Maintenance never Applied. |
| Fork/untrusted PR cannot write trusted caches | GENERATOR | Trusted-gate on seed save; setup-action forks cannot bootstrap. |
| Two persistent Velnor jobs | MEASURED / BLOCKED protocol | WP6: host cache yes; runner identity no; job-log protocol fail on 0.1.274. |

## Sentry deploy (apt only)

Velnor on sentry (`sentry.tailrocks.internal`) is installed and upgraded
**only** via the official Debian apt repository
<https://velnor-apt.tailrocks.com/> (source
<https://github.com/tailrocks/velnor-apt>). `sudo apt-get install velnor-runner`.
Do not sideload a `.deb`, `cargo install`, or replace `/usr/bin/velnor-runner`.

## Known follow-ups (not absorbed)

- Fleet still v0.1.274. Clears only after a coherent tagged `velnor-runner` is
  published to velnor-apt and installed on sentry with apt.
- protect-tags ruleset 19573007 blocks preview publish rolling-tag replace.
- velnor-control `five_concurrent_daemons_migrate_read_write_without_corruption` flake.
- Static release/preview templates: no verification `CARGO_NET_OFFLINE`.
- docker-image-pipeline publish job builds Dockerfile without seed context.
- generator-rev policy bootstrap reds on open PRs when main pins move (rebase).
- Audit §6.3: preview still `cancel-in-progress: false` for the whole workflow.
- WP4 §8.2 (c) eviction log lines still blocked: Maintenance Collect failed
  after #732 (`--mode`; image/apt CLI has no `cache-plan`).
- Pins are `9ffbcd0`. Default-branch runtime product still missing. Planning
  and Maintenance need GitHub-hosted or `SOURCE_REV` setup until the apt CLI
  matches the pin. Fleet upgrade remains apt-only.
