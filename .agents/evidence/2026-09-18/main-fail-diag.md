# Main-branch CI failure diagnosis (READ-ONLY)

Worktree: main@c04aec98 (merge PR #924). No pushes/merges/dispatches made.

---

## FAILURE 1 — CI/main `Docker · Docker / GitHub` @ step "Run unit checks"

- Run: https://github.com/tailrocks/velnor/actions/runs/35204207659 (push, main@c04aec98, job 105146007057, run_attempt 1, step 18 "Run unit checks" = failure, all other steps success/skipped)
- Baselines:
  - PR #924 run https://github.com/tailrocks/velnor/actions/runs/35203060271 — same job 105142246406 = **success**, and its head SHA is `73356c2c` (the R2 tip commit itself), so R2 code was present and green.
  - Pre-R2 main run https://github.com/tailrocks/velnor/actions/runs/35199235193 (sha `a2840748`) — same job 105129851389 = **success**.

### Cause

Transient unauthenticated GitHub API rate-limit inside `docker build` (Dockerfile:45 `mise install --locked --yes rust mr-boxington`). No test failure, no compile error — the build never got past tool provisioning.

### Evidence (log excerpts, `gh run view 35204207659 --log-failed`)

```text
Docker · Docker / GitHub  2026-09-17T09:18:14.3401902Z #14 21.75 mise WARN  GitHub API returned a 403 Forbidden error. This is most commonly caused by exceeding the rate limit, though other causes (e.g. insufficient token permissions) are possible.
Docker · Docker / GitHub  2026-09-17T09:18:14.3402566Z #14 21.75 mise WARN  No GitHub token was found, so mise is making unauthenticated requests to GitHub which have a much lower rate limit.
Docker · Docker / GitHub  2026-09-17T09:18:14.3404317Z #14 21.75 mise ERROR Failed to install packslip:github.com/jdx/mr-boxington@1.11.1: fetching the release list of packslip:github.com/jdx/mr-boxington: HTTP status client error (403 Forbidden) for url (https://api.github.com/repos/jdx/mr-boxington/contents/.well-known/packslip.json?ref=HEAD)
Docker · Docker / GitHub  2026-09-17T09:18:14.3405213Z #14 21.75 github rate limit: 0/60 (core), resets at 1789637642
Docker · Docker / GitHub  2026-09-17T09:18:14.3405850Z #14 21.75 github response: {"message":"API rate limit exceeded for 20.169.69.134. ...","documentation_url":"https://docs.github.com/rest/overview/resources-in-the-rest-api#rate-limiting"}
Docker · Docker / GitHub  2026-09-17T09:18:20.7738700Z ERROR: failed to build: failed to solve: process "/bin/sh -c mkdir -p /opt/mise/bin ... && mise install --locked --yes rust mr-boxington ..." did not complete successfully: exit code: 1
Docker · Docker / GitHub  2026-09-17T09:18:20.7774517Z error: CI command failed for unit docker with exit status: 1
Docker · Docker / GitHub  2026-09-17T09:18:20.7827055Z ##[error]Process completed with exit code 1.
```

### R2-attribution: NO (flake)

Proof:

1. `git diff --name-only a2840748..c04aec98` (56 files) = only `crates/velnor-workflow/src/lib.rs` (+1 `mod s2` + dispatch shim), `crates/velnor-workflow/src/s2/**`, `tests/fixtures-s2/**`, `tests/generic_surface_literals.rs`, and 1-line scan-digest bump in `.github/ci/.github-actions-generator-state`. Zero changes to `Dockerfile`, `docker/build-mise.*`, `rust-toolchain.toml`, mise config, or any rendered `.github/workflows/*.yml` (73356c2c message: "All 21 rendered files are byte-identical").
2. The failing command (`Dockerfile:45` mise/mbx provisioning inside `docker build`) consumes none of the R2-touched inputs.
3. Same job passed WITH the exact R2 tip (`73356c2c`) as PR head on run 35203060271 — deterministic R2 breakage is excluded.
4. Log signature is purely environmental: unauthenticated 0/60 core quota exhausted on shared runner egress IP 20.169.69.134, `github auth: no`. No rerun attempted (run_attempt 1); classification rests on log text + the two green baselines, per instructions. Not a main-only condition: PR and main run the same unauthenticated provisioning; the PR lane just didn't hit the exhausted quota window.

### Fix shape (file-level, no patch; robustness, not R2 revert)

- `Dockerfile` (+ `docker/build-mise.*` as applicable): pass an authenticated GitHub token into the mise/mbx provisioning layer (e.g. build secret → `GITHUB_TOKEN`/`MISE_GITHUB_TOKEN`), so the shared-runner unauthenticated 60/hr quota can't fail the build; optionally add retry around the packslip fetch. No R2 code change needed.

---

## FAILURE 2 — Preview `Guest payload x86_64/aarch64` (pre-existing)

- Post-R2: https://github.com/tailrocks/velnor/actions/runs/35204207477 (push, main@c04aec98) — jobs 105145968569 (x86_64) + 105145970327 (aarch64) = failure.
- Pre-R2: https://github.com/tailrocks/velnor/actions/runs/35199235164 (push, main@a2840748) — jobs 105129803870 (x86_64) + 105129803628 (aarch64) = failure. Identical signature ⇒ pre-existing.

### Cause

`velnor-guest-image build --out dist/microvm` leaves its scratch tree at `dist/microvm/work/` (`rootfs-tree/` mmdebstrap tree + kernel sources; `crates/velnor-runner/src/bin/velnor-guest-image.rs:137`: `let work = out.join("work")`, never cleaned). The following "Upload guest payload" step uploads `path: dist/microvm/*`, which includes `work/`, and `actions/upload-artifact` scandir fails with EACCES on the mode-restricted `rootfs-tree/lib/ssl/private` dir from the noble tree. The build itself succeeds; only the artifact upload fails.

### Evidence (log excerpts)

Post-R2 (`gh run view 35204207477 --log-failed`), both arches:

```text
Guest payload x86_64   Upload guest payload  2026-09-17T09:30:03.1538267Z ##[error]EACCES: permission denied, scandir '/home/runner/work/velnor/velnor/dist/microvm/work/rootfs-tree/lib/ssl/private'
Guest payload aarch64  Upload guest payload  2026-09-17T09:29:07.0145233Z ##[error]EACCES: permission denied, scandir '/home/runner/work/velnor/velnor/dist/microvm/work/rootfs-tree/lib/ssl/private'
```

Pre-R2 (`gh run view 35199235164 --log-failed`) — byte-identical error lines:

```text
Guest payload aarch64  Upload guest payload  2026-09-17T08:36:56.4354422Z ##[error]EACCES: permission denied, scandir '/home/runner/work/velnor/velnor/dist/microvm/work/rootfs-tree/lib/ssl/private'
Guest payload x86_64   UNKNOWN STEP           2026-09-17T08:34:21.7275167Z ##[error]EACCES: permission denied, scandir '/home/runner/work/velnor/velnor/dist/microvm/work/rootfs-tree/lib/ssl/private'
```

Upload step source (generated `.github/workflows/preview.yml:463-469`, rendered by `render_guest_payload_job`):

```yaml
- name: Upload guest payload
  uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
  with:
    name: guest-payload-${{ matrix.arch }}
    path: dist/microvm/*
    if-no-files-found: error
```

### R2-attribution: NO (pre-existing)

Proof: identical EACCES failure on pre-R2 main (`a2840748`, run 35199235164, both arches) before either R2 commit existed; R2 touches no guest-image, release-primitive, or workflow files (see F1 file list).

### Fix shape (file-level, no patch)

Pick one (or combine); generator-owned files must be regenerated, never hand-edited:

1. `crates/velnor-workflow/src/primitives/release.rs` (`render_guest_payload_job`, ~L1264; same renderer string mirrored in `crates/velnor-workflow/src/s2/primitives/release.rs:1254`) — narrow the upload `path:` from `dist/microvm/*` to the explicit payload files (`vmlinux`, `rootfs.ext4`, `*.sha256`, agent bin), or emit a cleanup step removing `dist/microvm/work` before upload; then regenerate `.github/workflows/preview.yml` (and `release.yml`, which shares the renderer).
2. `crates/velnor-runner/src/bin/velnor-guest-image.rs` (~L137) — build `work/` outside `--out` (tempdir) or delete it after a successful build so scratch never lands in the artifact path.
3. `crates/velnor-runner/src/execution/guest_image.rs` (`build_rootfs`, ~L438+) — alternative/complement: permission fixup on the tree after the sudo mmdebstrap+chown (does not remove the scratch-upload waste; options 1–2 are structural).

### A3-scope verdict: OUT

STEP A3 (`origin/docs/bastion-final-plan:plans/bastion-three-provider-ci/work-plan.md:140-153`) gates strictly on the CI-main workflow's unit×provider expected-result set:

> "Obtain three consecutive FULL green `main` runs ... `gh run list --repo tailrocks/velnor --branch main --workflow <ci-main> --limit 5`"
> "complete expected-result set per spec §2 identity ... Every expected result is present and green"

Spec §2 (`spec.md:45+`) defines that set as the planner's per-unit three-provider fanout (`unit_id + provider + platform + ...` result identity) — CI verification units, not release workflows. `Preview` / `guest-payload` appears nowhere in the A3 step text, the A3 checklist acceptance line (URL/SHA/plan-digest/expected-results/timings + DCO), the spec's §2 result-identity section (the only "preview" hits in spec.md are APT "stable/preview suites" and a Jackin "no preview-release substitute" line), or `evidence.md` (zero preview/guest hits). The Preview release-publisher workflow (guest payload → deb → rolling release) is outside the declared A3 bootstrap scope; its failure does not fail the A3 gate as written (it belongs to later release-qualification steps, cf. checklist B3).

---

## DIAG verdict lines

```text
F1=flake F2=EACCES-upload-scans-rootfs-work-tree-left-in-dist/microvm A3-scope=out
```
