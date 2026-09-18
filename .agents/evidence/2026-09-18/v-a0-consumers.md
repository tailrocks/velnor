# V-A0: independent verification of /tmp/a0-consumers.md correction ledger

Date: 2026-09-17. Method: read-only `gh api` only (ref, trees, contents,
blobs, compare, commits). No clones, no writes to repos. Every claim below
was re-fetched in this session; A0's /tmp/jackin-*.toml artifacts were NOT
reused (fresh fetches to /tmp/v-*).

Live mains re-checked via `git/ref/heads/main`:
- tailrocks/velnor-apt: `d62820d47a3814e98c4e25512151b4af14e3c3e1` — MATCH
- jackin-project/jackin: `0be3fcf95cd33c14fd3fe47af08026f17135a157` — MATCH
- ChainArgos/java-monorepo: `38a8fb5777fd02fec8b3904d86387aba05940e9a` — MATCH
Commit anchors re-checked: jackin audited `92f347ac` (#993 sync,
2026-09-16T15:22:10Z), live `0be3fcf9` (#995 cache-mount heal,
2026-09-16T23:27:33Z); CA audited `235e479b` (nominis docs,
2026-09-16T12:16:31Z), live `38a8fb57` (atlas fixtures,
2026-09-16T20:31:28Z); apt live `d62820d4` (#238 sync,
2026-09-16T15:30:26Z). All MATCH.

## Per-correction verdicts

| # | Re-check performed | Verdict |
| --- | --- | --- |
| C-apt-1 | Tree at `d62820d4`: truncated=false, 33 entries, workflows=[`ci-unit-docs.yml`] only. Omission blob `3172bb88…` size 809 via tree entry AND contents read; blob content spot-matches (descriptive label, `ci-unit-docs.yml` only, omitted set). project.toml: 1 unit (docs), `files=["ci-unit-docs.yml"]`, release.enabled=false. Gen revision `b9c3156c…`. | MATCH |
| C-jack-1 | 9-file workflow set byte-identical file lists at audited AND live. project.toml: 40 `[[unit]]` = 36 rust / 1 bun / 1 docker / 2 swift at BOTH SHAs, `diff` exit 0 (cksum 3868922607). `runners="github"`, release.enabled=false. Docker unit root `docker/construct`. Gen config holds 3 `[[units]]` overrides. | MATCH |
| C-jack-2 | Compare audited→live: status=ahead, ahead_by=1, 43 files, 0 under `.github/workflows/`, project.toml untouched. Gen-config diff is exactly `ruleset_external_status_checks` 5→1 (`["DCO"]`); revision unchanged. | MATCH |
| C-jack-3 | `release.yml` 404 at BOTH SHAs. `[release] enabled=false` at BOTH SHAs. | MATCH |
| C-jack-4 | `.config/nextest.toml`: `[profile.docker-e2e]` = 4-binary filter, serial `docker-e2e` group max-threads=1; default excludes exactly those 4. All 9 live workflows: ZERO matches for `docker-e2e\|e2e\|--profile\|capsule_bin` (case-insensitive). All 35 `mbx nextest run --locked --all-features … --no-tests pass` manifest commands lack `--profile` (the 140 `--profile` hits are clippy's `--profile test` flag; `docker-e2e` count in manifest = 0). `common.rs` panics unless `JACKIN_CAPSULE_BIN` = executable Linux `jackin-capsule` ELF. Tools: channel 1.97.1, bun 1.3.14, node 24.18.0, `github_runner=ubuntu-26.04` + `macos_runner=macos-26` (gen config). E2E sources `dind_e2e.rs`, `load_options_e2e.rs`, `session_send_e2e.rs`, `usage_broker_e2e.rs`, `dind_e2e/{common,fixtures}.rs` all exist at live. `fixtures.rs`: 0 `capsule` mentions. | MATCH |
| C-jack-5 | `per_mount_isolation_e2e.rs` exists at audited SHA (`904f2ebc`, 7535 B) and matches NEITHER nextest filter (both list only the 4 binaries) → default-profile resident. | MATCH |
| C-ca-1 | 11-file workflow set identical at BOTH SHAs. project.toml: 71 `[[unit]]` = 37 gradle / 17 rust / 11 docker / 4 bun / 1 node / 1 docs at BOTH SHAs, `diff` exit 0 (cksum 1978985974). `runners="velnor"` in manifest (see note N1 for `automatic`). Gen config 15 lines, 0 unit lines. | MATCH |
| C-ca-2 | Compare audited→live: status=ahead, ahead_by=1, 22 files — all under `docs/product/research/atlas-redesign/**` + `scripts/check-{atlas-preparation.py,docs-ignore.txt}`; 0 CI files; manifests byte-identical. | MATCH |
| C-ca-3 | Live `velnor-workflow.toml`: `revision` grep count = 0 (15-line `[generator]+[workflow]` file). No pin comment in headers surveyed. | MATCH |
| C-ca-4 | `.github/actions/` 404 at BOTH SHAs. Each of the 6 live unit workflows contains exactly 1× `uses: ./.github/actions/report-velnor-ci-outcomes`; 0 remote `report-velnor` refs. (Contrast jackin: remote pin `@b9c3156c…` confirmed; jackin `.github/actions` 404 at audited confirmed.) | MATCH |
| C-ca-5 | All 9 paths exist at live; blob SHAs match A0's table exactly (`6c1e2ecf 85b9a2c1 b30a027e cc0eda31 e8053df3 9de32f6b 7f887ad9 f3dd8199 6680c698`); independent per-file re-read at audited SHA → all 9 SAME; 0/22 gap files under `ansible-configs/`. (Stronger than A0's table: all 9 directly per-file matched, not just 3.) | MATCH |

## Notes (not disproofs — no ledger row asserts otherwise)

- N1: `automatic = "velnor"` lives in ChainArgos `.github-gen/velnor-workflow.toml`
  (`[workflow]`), not in `.github/ci/project.toml` (which carries only
  `runners = "velnor"`). A0 §3.2's sentence context suggests the manifest;
  the value itself is confirmed. Location precision only.
- N2: generated jackin `ci-unit-swift.yml` GitHub-lane `runs-on` is
  `ubuntu-26.04` although gen config sets `macos_runner = "macos-26"`.
  No ledger row asserts the generated runs-on value; flagged as an F1 input,
  not a ledger error.

## Verdict

**CERTIFIED** — 11/11 corrections MATCH; 0 disproven rows. Attempted
disproofs (manifest `--profile` hits, missing `macos` in workflows/manifest,
`automatic` location) all resolved in A0's favor on closer inspection.
