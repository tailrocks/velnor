# CI/CD cache-architecture evidence map

Repo: `tailrocks/velnor`. Captured 2026-09-13 after WP5 merge `#711`
(`b542a95`), WP6 merge `#716` (`aea5c7b`), evidence `#729` (`b91f837`),
Velnor-default `#732` (`c383c69`), guest-seed harden `#723` (`9657f21`),
Maintenance GitHub-hosted `#737` (`845d76d`).

## §8.2 work packages

| WP | Package | Status | Evidence |
| --- | --- | --- | --- |
| 0 | Phase timings / negative-boundary | MERGED | PR #664. `scripts/test-check-release-feature-boundary.sh`. |
| 1 | Cargo-source cache + frozen prep | MERGED | PR #668. |
| 1.5 | Policy transport + pin | MERGED | PRs #676 / #677. |
| P0 job-log | Velnor-lane job-log | MERGED | PR #685. Live rejection still on fleet v0.1.274 until apt upgrade. |
| 2 | Toolchain + min tools | MERGED | PR #686. |
| 3 | Preview/Release persistence + Docker seed | MERGED | PR #689. Docker cold save run `34745390724` job `103692529918`; warm hit `34745867993` job `103693815212`. Release cargo cold `34748391592` job `103700698681`; warm `34750575182` job `103706298786`. P0 fetch-online #692+#693. |
| 4 | Bounded snapshot + maintenance | MERGED; §8.2 (c) Apply attempt | PR #694. (a)(b) Preview `34753493092` job `103715332015` saved `…-594bb9c9…` then `34754981233` job `103718486096` saved `…-9e65eceb…` same compat `58835fa41d40`. (c) Maintenance GitHub-hosted shipped in #737 (`845d76d`). Dispatch `34768131336` is the post-merge Apply attempt. Prior blocked run still `34765436872` job `103745452027` (`unexpected argument '--mode'`). |
| 5 | Canonical `/out`, guest seeds, runtime verify | MERGED; guest-seed exact-hit BLOCKED | PR #711 squash `b542a95` + harden #723 (`9657f21`, restore-keys removed). #737 merged. SOURCE+POLICY `8c84b39d1f39e3fd7b3b450edc6e5dafa50383f8` (reachable; ghost `9ffbcd0` was not a git object). Same-repo PR Planning `34767851982` job `103751935130` published `velnor-workflow-runtime-8c84b39d1f39e3fd7b3b450edc6e5dafa50383f8-Linux-X64`. Default-branch producer is CI/main `34768089712` on `845d76d`. Guest-seed exact-only: Preview `34766333207` job `103748007415` — no restore-keys; cold save `velnor-guest-seed-x86_64-9ace6bd0…`. Exact-hit still blocked on unchanged hashFiles. |
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
| Evicted seed → one recovery | BLOCKED | Dispatch `34768131336` is the post-merge Apply attempt after #737. Prior blocked run `34765436872` job `103745452027` (`--mode`). |
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
- WP4 §8.2 (c): Maintenance GitHub-hosted shipped in #737 (`845d76d`).
  Dispatch `34768131336` is the post-merge Apply attempt. Prior blocked run
  still `34765436872` job `103745452027` (`unexpected argument '--mode'`).
- WP5 pins: #737 merged. SOURCE+POLICY
  `8c84b39d1f39e3fd7b3b450edc6e5dafa50383f8` (reachable; ghost `9ffbcd0` was
  not a git object). Same-repo PR Planning `34767851982` job `103751935130`
  published `velnor-workflow-runtime-8c84b39d1f39e3fd7b3b450edc6e5dafa50383f8-Linux-X64`.
  Default-branch producer is CI/main `34768089712` on `845d76d`. Guest-seed
  exact-only Preview `34766333207` job `103748007415` (no restore-keys; cold
  save `velnor-guest-seed-x86_64-9ace6bd0…`). Exact-hit still blocked on
  unchanged hashFiles.
- Sentry deploy stays apt-only. Fleet upgrade remains apt-only.
