# Feed artifact-layout defect — read-only analysis (gate B4)

VELNOR3 HEAD: `3e9e67fc`. velnor-apt: `origin/main @ 35f33ed` (#240 merge).
Line citations are HEAD-relative for velnor3, @35f33ed for velnor-apt, unless noted.

## 1. Verdict

**Purely generator-side. Nothing is wrong feed-side, and no source change remains to be written:
the fix is already committed in velnor3 (`420c2766`) and already shipped in four published
runtime products. B4 is a pin-sync + config + regen + dispatch task in velnor-apt, not a code task.**

## 2. The exact defect (FEED_COVERAGE §3 / fix-commit D3)

Mechanism: `actions/upload-artifact` v4 stores a directory's *contents* without the top-level
directory name, so a download only restores the `incoming/` / `public/` prefixes if it downloads
*into* those directories. The generated `release.yml` uploaded directories but downloaded with
`path: .`, scattering payloads at the workspace root while every downstream step addresses the
prefixed paths. Fail-closed, pre-mutation, in both handoffs.

Generated-workflow evidence (velnor-apt @35f33ed, rendered by pin `048a7bda`,
product `velnor-workflow-runtime-v1-32565cc6272a84d4`):

- `.github/workflows/release.yml:142-145` — upload `apt-incoming`, `path: incoming`
- `.github/workflows/release.yml:175-178` — **download `apt-incoming`, `path: .`** ← broken
- `.github/workflows/release.yml:200` — reads `incoming/release-record.json.sha256` (the live failure line)
- `.github/workflows/release.yml:231,240,243` — `--incoming incoming`, `incoming/manifest.json`
- `.github/workflows/release.yml:247-250` — upload `apt-staging`, `path: public`
- `.github/workflows/release.yml:278-281` — **download `apt-staging`, `path: .`** ← broken
- `.github/workflows/release.yml:294` (`--staged public`), `:298` (Pages `path: public`) — prefixed consumers
- Pin: `.github-gen/velnor-workflow.toml` → `revision = "048a7bdaed8240cf652127c94434e60528633dec"`
- Record: `.github-gen/FEED_COVERAGE.md` §3; PR `#240` ("docs: record feed artifact-layout defect + CI dispatch evidence", merged 2026-09-17, merge commit `35f33ed5`)
- Live proof: dispatch `35280645007` (main, stable, v0.1.274, explicit commit) → Admit success, **Verify success**, **Publish failure** (`awk: cannot open incoming/release-record.json.sha256`), Deploy skipped, Feed result fail-closed (re-verified via `gh api .../actions/runs/35280645007/jobs`).

Not an action-version issue: upload `v7.0.1` / download `v8.0.1` pins are correct; any v4+ pair behaves the same.

## 3. Generator source lines responsible

s1 template `render_apt_release` — the single `format!` string is the whole workflow:

- Pre-fix: `crates/velnor-workflow/src/primitives/release.rs:4282` @ `420c2766^` (fn at `:4238`) —
  contains both `name: apt-incoming\n … path: .` and `name: apt-staging\n … path: .` fragments.
- Post-fix (HEAD): `crates/velnor-workflow/src/primitives/release.rs:4287` (fn at `:4242`) —
  fragments are now `path: incoming` / `path: public`.
- Regression test (HEAD): `:8280-8322` `apt_artifact_handoffs_agree_on_directories` — asserts each
  download restores the directory the downstream steps address, and `!release.contains("path: .")`.

Out of scope, verified:

- s2 is NOT involved: `crates/velnor-workflow/src/s2/primitives/release.rs:4129-4136`
  `render_apt_release` delegates to the generic `verify-feed`/`update-feed` stub; the full
  verify/publish/deploy feed workflow is s1-only (and velnor-apt's `release.yml` is the s1 shape:
  `runner:` choice input, `admit-runner` job).
- `crates/velnor-workflow/src/apt.rs` runtime needed no layout change (`--incoming`/`--staging`
  already take directories); the defect was YAML-only.

## 4. Fix status: committed, tested, published — just not consumed

- `420c2766` "fix(workflow): repair apt feed signing, discovery, and artifact handoffs" **is an
  ancestor of HEAD** (`git merge-base --is-ancestor` confirmed); `048a7bda` (velnor-apt's pin) is an
  ancestor of the fix. Working-tree dirt (`promote.rs`, generator state) is unrelated.
- The same commit fixed the other two B4 blockers, so B4 has no remaining generator work:
  - D1 signing: `apt.rs:532,600-624` (`signing_key_secret` contract), `:2102,2162-2181,2527-2564`
    (isolated-keyring import + fingerprint agreement), template `--key-env` wiring, test
    `apt_publish_wires_the_signing_key_secret` (`primitives/release.rs:8325`).
  - D2 discovery: `run_resolve_commit` passes the full argv — `apt.rs:923-936`
    (`run_fixed("git", &argv, …)`), builder `:909-918`, live-`ls-remote` test `:4415-4441`.
- Post-fix products already published (release notes name the build commit; all four contain
  `420c2766` by ancestry): `…-83e34588fffe93e9` (Latest, from `9f40f929`), `…-6f74f2b31301972a`
  (from `3e276ad8`), `…-997f662c6ee999c4` (from `ac8a74c1`), `…-a55ade440d4fd658` (from `15c6bc2e`).

## 5. Fix prescription for the B4 author (no velnor3 edits; no hand-edits to `release.yml`)

In velnor-apt, as a normal sync PR:

1. Add the now-required key contract to `[release]` in `.github-gen/velnor-workflow.toml`:
   `signing_key_secret = "APT_GPG_PRIVATE_KEY"`. Required because `AptContract::resolve`
   rejects an empty value (`apt.rs:600-604`) and `render_apt_release` renders a
   "Release omitted" stub when resolution fails (`primitives/release.rs:4246-4250`).
   The secret already exists as a repository secret (per FEED_COVERAGE §1); velnor-apt's
   current `[release]` section has `passphrase_secret` but no `signing_key_secret`.
2. Bump `revision` to a post-fix product build commit (suggest Latest: `9f40f929…` /
   product `velnor-workflow-runtime-v1-83e34588fffe93e9`; policy requires product
   `--revision` == pin).
3. Regen and assert: `release.yml` contains `path: incoming` / `path: public`, no `path: .`,
   and the publish step wires both secrets plus `--key-env`.
4. Merge, then dispatch `Package feed` on main (explicit version+commit for a published tag
   on the first run) and expect Verify → Publish → Deploy → feed-result all success.

Sequencing note: the live feed stays at 0.1.274 until the source repo ships a new signed
release (v0.1.275/v0.1.276 tags currently have no Release objects, so no new debs exist).
The workflow fix unblocks publication; it does not itself advance the feed version.
