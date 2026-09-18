# CONSUMER-DRIFT watcher snapshot (2026-09-17, read-only)

Method: `git ls-remote HEAD` + `gh api .../git/ref/heads/main` (agree on all 3) + `gh api contents/...?ref=<SHA>` live reads. Zero clones, zero writes.
Baselines: ledger "Live SHAs (A0 re-resolved)" + /tmp/a0-consumers.md (C-apt-1, C-jack-1..5, C-ca-1..5); prep inputs /tmp/f1-plan.md, /tmp/g1-plan.md, /tmp/b2-prep.md.

## Drift table (old → new → consequence)

| # | Item | Old (ledger) | New (live) | Drift? | Consequence |
|---|------|--------------|------------|--------|-------------|
| J0 | jackin main SHA | `0be3fcf9` | `0be3fcf95cd33c14fd3fe47af08026f17135a157` (ls-remote + ref agree) | NONE | F1 Step 0 precondition SHA still valid; no revalidation needed |
| J1 | jackin workflow set | 9 files | 9 files, identical names (ci-main, ci-policy, ci-pr, ci-unit-{bun,docker,rust,swift}, maintenance, nightly) | NONE | C-jack-1 holds; F1 regen starts from stable 9-file surface |
| J2 | jackin unit count | 40 (36 rust / 1 bun / 1 docker / 2 swift), runners=github | 40 `[[unit]]`, same kinds (36/1/1/2), `runners = "github"` | NONE | 114+2 ledger (spec §9.1) holds; F1 Step 5 unchanged |
| J3 | jackin release.yml | 404 both SHAs, `[release] enabled=false` | 404 at live main | NONE | C-jack-3 holds; F1 Step 6 reconciliation (source-test rewrite, no restore/enable) still the plan |
| J4 | jackin docker-e2e invocation | defined (`[profile.docker-e2e]`, serial) but 0/9 workflows invoke | 0/9 workflows mention docker-e2e/capsule_bin/nextest --profile/JACKIN_CAPSULE_BIN; `[profile.docker-e2e]` still defined | NONE | C-jack-4/5 hold; §9.1 explicit-E2E obligation fully outstanding for F1 Step 8 |
| J5 | jackin .github/actions | 404 (remote pin used) | 404 | NONE | Healthy remote-pin shape unchanged |
| G0 | chainargos main SHA | `38a8fb57` | `38a8fb5777fd02fec8b3904d86387aba05940e9a` (ls-remote + ref agree) | NONE | G1 Phase 0 SHA still valid |
| G1 | chainargos workflow set | 11 files | 11 files, identical names | NONE | C-ca-1 holds |
| G2 | chainargos unit count | 71 (37 gradle / 17 rust / 11 docker / 4 bun / 1 node / 1 docs), runners=velnor | 71 `[[unit]]`, same kinds (37/17/11/4/1/1), `runners = "velnor"` | NONE | 213-execution baseline (spec §9.2) holds; G1 Phase 1.5 ledger check unchanged |
| G3 | chainargos .github/actions | 404 + all 6 unit workflows carry broken local `uses:` | 404; 6/6 unit workflows each carry exactly 1 `uses: ./.github/actions/report-velnor-ci-outcomes` | NONE | C-ca-4 holds UNMITIGATED; G1 Phase 1 "repair FIRST" obligation stands, load-bearing for all 71 units |
| G4 | ansible install-base.yml | `6c1e2ecf` | `6c1e2ecf` | SAME | C1 §6.1 source unchanged |
| G5 | ansible install-docker.yml | `85b9a2c1` | `85b9a2c1` | SAME | — |
| G6 | ansible install-docker-selene.yml | `b30a027e` | `b30a027e` | SAME | — |
| G7 | ansible hosts.ini | `cc0eda31` | `cc0eda31` | SAME | — |
| G8 | ansible requirements.yaml | `e8053df3` | `e8053df3` | SAME | — |
| G9 | ansible README.md | `9de32f6b` | `9de32f6b` | SAME | — |
| G10 | ansible docs/upgrade-debian.md | `7f887ad9` | `7f887ad9` | SAME | — |
| G11 | ansible update-packages.yml | `f3dd8199` | `f3dd8199` | SAME | — |
| G12 | ansible upgrade-debian.yml | `6680c698` | `6680c698` | SAME | C1 pins live content = audited content; C-ca-5 holds |
| A0 | velnor-apt main SHA | `d62820d4` | `d62820d47a3814e98c4e25512151b4af14e3c3e1` (ls-remote + ref agree) | NONE | B2 §0 baseline SHA still valid |
| A1 | velnor-apt tree | 33 entries, truncated=false | 33 entries, truncated=false | NONE | Promotion diff stays reviewable |
| A2 | velnor-apt omission blob | `3172bb88…` (809 bytes) | `3172bb883ec343a676d82c2594cb1a399191bf07` (809 bytes) | NONE | C-apt-1 holds; B1 genuinely unstarted upstream; no premature notice removal |
| A3 | velnor-apt workflow count | exactly `ci-unit-docs.yml` | exactly `ci-unit-docs.yml` | NONE | Clean negative baseline for B1 renamed-fixture gate; no hand-YAML APT workflow appeared |

## Prep-findings verdict

- **F1 (jackin): HOLDS fully.** Stale-gen config drift (§2.3, external checks → `[DCO]` without regen) is still the live-main state since SHA unchanged → live Policy `generated-tree` still expected red; F1 regenerates from F-gate pin. Release-reconciliation, docker-e2e, 5th-binary disposition, tool re-resolution all still outstanding as planned.
- **G1 (chainargos): HOLDS fully.** Broken local action refs still 404 + 6/6 broken `uses:` → Phase 1 repair-first stands. 71/213 ledger, docs-only gap, missing `revision` key (C-ca-3, unchanged file), ansible 9/9 SAME → C1 source pin still valid.
- **B2 (velnor-apt): HOLDS fully.** Omission blob byte-identical, single docs workflow, 33-entry tree → B2 atomic-promotion procedure inputs unchanged.

**Net: ZERO consumer drift. No ledger update required beyond this snapshot.**
