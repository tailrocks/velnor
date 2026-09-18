# Security audit — bastion privileged surfaces (docs/bastion-final-plan @ 137ca1f9)

Read-only adversarial review. Scope: NEW privileged surfaces merged on `docs/bastion-final-plan`
vs `main` (108 files, +26k/−3.8k). Focus: (1) credential flows, (2) trust enforcement,
(3) fail-closed paths, (4) secret references in APT config.

Verdict: **8 findings, 0 criticals** — 1 High, 4 Medium, 2 Low, 1 Info.

## Findings

### F1 [MEDIUM] Diagnostics persist container env (spent JIT) + unredacted job logs, no cleanup/perms
- **File:** `crates/velnor-runner/src/scaleset/worker/supervise.rs:223-229,275-302`
- **Exploit path:** `export_diagnostics` writes full `docker inspect` of the runner container to
  `<state-dir>/diagnostics/runner.inspect.json` (default perms) plus full `docker logs` to
  `runner.log`. The inspect JSON contains `Config.Env`, including
  `ACTIONS_RUNNER_INPUT_JITCONFIG=<blob>`. Export runs post-stop so the persisted JIT is
  normally spent — but `runner.log` is the RAW log: any secret value a job prints that was
  never registered as a mask (or pre-mask output) rests on shared host disk indefinitely:
  `owned_cleanup` (`supervise.rs:330-345`) removes containers/network/volumes but never the
  state dir. Any host-local reader (or diagnostics collector) harvests it.
  Directly contradicts the stated contract at `runner.rs:440-442` ("never written to disk")
  and `worker/mod.rs:292` ("never logged").
- **Fix direction:** redact before writing — capture only labels/State/NetworkSettings (drop
  `Config.Env`, or at minimum the `JITCONFIG` entry); `chmod 0600` diagnostics; delete (or
  age-out) the state dir once the permit releases.

### F2 [HIGH] APT feed verify never verifies Sigstore attestations on fetched source assets
- **File:** `crates/velnor-workflow/src/apt.rs` (zero `attest` hits); generated feed template
  `crates/velnor-workflow/src/primitives/release.rs:2388` (verify job has no
  `gh attestation verify` step)
- **Exploit path:** the feed's trust root is internal coherence (sidecars, record↔tag/commit
  binding with independently resolved commit, manifest↔deb digests, live OCI check) — all
  computable by anyone who can overwrite release assets. A `contents:write`-only compromise
  of the source repo (stolen release-job `GITHUB_TOKEN`, malicious release run) forges a
  self-consistent record+manifest+debs naming the tag's REAL commit; every `apt-verify`
  check passes, and the feed then signs attacker bytes with the publisher key → RCE on all
  APT consumers. Note the source release workflow DOES verify provenance before publishing
  (`release.rs:1429` template: tarball + deb attestation checks), and the doc comment at
  `release.rs:1152-1154` claims "the consumer lane verifies these attestations" — but the
  APT feed consumer (the lane that wields the signing key) does not. The runtime-products
  consumer flow (attestation + digest + self-report) is the in-repo model to copy.
- **Fix direction:** in the feed verify job, `gh attestation verify` every fetched deb (and
  record/manifest where attested) against the source repo + pinned signer workflow +
  `--source-ref`/`--source-digest` binding BEFORE `apt-verify`; fail closed on any miss.
  (Defense-in-depth follow-up: deb extraction at `apt.rs:1043,1165` runs `tar -x` on
  record-bound bytes — digest-before-extract already holds at `apt.rs:1677-1689`, keep it
  that way and consider `--no-overwrite-dir`/symlink hardening.)

### F3 [MEDIUM] Secret-bearing structs derive plaintext `Debug` (latent leak, inconsistent)
- **Files:** `crates/velnor-runner/src/scaleset/credentials.rs:16-24` (`GitHubAppAuth.private_key_pem`),
  `:69-73` (`PemJwtProvider.private_key_pem`), `:192-200` (`InstallationAccessToken.token`);
  `crates/velnor-runner/src/scaleset/client.rs:59-64` (`AdminToken.authorization_header`);
  `crates/velnor-runner/src/scaleset/worker/runner.rs:430-438` (`RunnerSpec.jit_config`);
  `crates/velnor-runner/src/scaleset/worker/mod.rs:284-296` (`ProvisionPlan.jit_config`, `pub`)
- **Exploit path:** latent — no production `{:?}` emission of these types exists today
  (verified: no tracing in `scaleset/`; `executor.rs:678-679` deliberately avoids argv logging).
  But `ActionsAuth` already carries a redacted `Debug` while its six siblings don't, and the
  scale-set client is not yet wired to a daemon — the first `debug!(?client)` or
  error-context `{:?}` in the wiring PR prints App keys / admin tokens / PAT / JIT blobs
  into trace.jsonl, stderr, and OTLP.
- **Fix direction:** redacted `Debug` impls (presence-only, mirroring `ActionsAuth`) for all
  six now, before the wiring lands; consider `secrecy`/`zeroize` for PEM/token storage.

### F4 [MEDIUM] Per-job CPU/RAM/PID ceilings removed host-wide — any admitted job can DoS the host
- **Files:** `crates/velnor-runner/src/container.rs:63-97` (`QUOTA_FLAGS` emission backstop),
  `crates/velnor-runner/src/github_adapter.rs:847-894` (admission strip),
  `crates/velnor-runner/debian/postinst` (deletes quota drop-in, FAILS install if any ceiling
  survives), `preflight.rs` (asserts infinity), `velnor-jobs.slice` identity-only
- **Exploit path:** deliberate architecture (count-capped via the permit ledger, resource-
  unbounded). Consequence: a fork-bomb, malloc-loop, or OOM-heavy build in ANY admitted job
  starves co-tenants and can OOM-kill the daemon/dockerd. The velnor lane admits same-repo
  PRs (`pull_request_on_velnor = true`, fork-gated but contributor-trusting), so the
  trigger is one malicious or buggy PR job — no host access needed.
- **Fix direction:** acknowledge as accepted risk, or add throughput-neutral backstops:
  generous `--pids-limit` (catches fork bombs only), `systemd-oomd` with daemon protection /
  `oom_score_adj`, `MemoryHigh` (throttle, not kill) — currently the installer forbids even
  operator ceilings; document that same-repo PRs are untrusted for capacity.

### F5 [MEDIUM] Live JIT blob travels via `docker create` argv and container env
- **File:** `crates/velnor-runner/src/scaleset/worker/runner.rs:479-510`, esp. `:486-487`
  (`--env ACTIONS_RUNNER_INPUT_JITCONFIG=<blob>`)
- **Exploit path:** host-local, short-lived. The single-use JIT credential is visible in the
  host process list during `docker create`, persists in the container config (readable by
  any docker-socket holder), and is then snapshotted to disk by F1. A compromised co-tenant
  with socket access (trusted lane) or a host-local observer can steal a live JIT and
  impersonate the runner to receive the job's credentials. (Log exfiltration ruled out —
  see F3 verification.) Partly inherent to the image's env contract (ARC does the same).
- **Fix direction:** pass via `--env-file` from a `0600` temp file deleted right after
  create (still lands in container config — unavoidable); fix F1 to stop the disk copy;
  remove the runner container/config promptly after the connected marker flips.

### F6 [LOW] Preview rollback version from live Packages file used unvalidated in curl path/URL
- **File:** generated feed template, `crates/velnor-workflow/src/primitives/release.rs:2388`
  (preview `prior` step: `rollback=$(awk …)` → `-o "prev/…_${rollback}_….deb"` + URL)
- **Exploit path:** the stable path shape-validates its live-feed input (`''|*[!0-9.]*`
  reject on `last-publish`); the preview path has no equivalent before embedding
  `$rollback` in a filesystem path and URL. Trust root is our own live feed (if that's
  compromised, consumers are already lost), and the `prev/{pkg}_` prefix blocks `..`
  escape while `/` fails closed (curl without `--create-dirs`). Defense-in-depth gap only.
- **Fix direction:** validate `$rollback` against the `valid_pool_version` charset and reject
  `/` before use, mirroring the stable gate.

### F7 [LOW] Provisioner slot install→exec TOCTOU on the shared host slot
- **File:** `crates/velnor-workflow/src/lib.rs:4630` (`workflow_pinned_policy_runtime_velnor`)
- **Exploit path:** digest is verified on the `$temporary` copy, then `install`ed to the
  shared `$CARGO_HOME/bin/velnor-workflow-policy` slot and executed from there. A
  concurrent malicious job on the same host (shared `CARGO_HOME`) that swaps the slot
  between install and exec defeats the digest check — the subsequent `--closure` /
  `--revision` self-checks trust a potentially lying binary, and the env pointer then
  exports the planted binary as the policy validator. Narrow race, needs a concurrent
  same-host attacker (velnor lane excludes forks but admits same-repo PRs).
- **Fix direction:** `flock` the slot across install+verify+export, or re-hash the slot
  path immediately before each exec, or use per-job slot paths.

### F8 [INFO] Ownership slug sanitization can collide; adoption gate fails closed
- **File:** `crates/velnor-runner/src/scaleset/worker/ownership.rs:81-94`
- **Exploit path:** distinct runner names sanitize to one slug (`a/b` vs `a b` → `a-b`) and
  would share Docker object names. Impact capped: the adoption gate compares the canonical
  (unsanitized) id (`dind.rs:305-313`, `runner.rs:579`) and refuses with "foreign
  ownership" — DoS, not takeover. Unreachable today: names are counter-generated
  (`<set>-<seq>`), never from GitHub traffic.
- **Fix direction:** mix a short hash of the canonical id into the slug, or reject names
  that need sanitization.

## Verified positives (attack surface checked, no finding)

- **Credential flows:** App keys/PAT/admin tokens stay in-process; in-process refresh with
  60s skew (`client.rs:224-270`); queue traffic uses the session token (`session.rs`);
  `AcquireJobs` swaps in the queue token, never admin (`client.rs:409-425`); only the
  single-job JIT blob enters runner containers — no App keys in argv (test-pinned,
  `runner.rs:833-843`). PAT/App are XOR-validated (`credentials.rs:168-183`).
- **APT secrets:** `passphrase_secret` is name-only, shape-validated to an uppercase
  identifier ≤64 chars (`apt.rs:150-155,543-547`), rendered strictly as
  `${{ secrets.NAME }}` into the `package-feed` environment (`release.rs:2388`); passphrase
  reaches gpg via `--passphrase-fd 0` stdin, never argv (`apt.rs:2261-2299,2355-2379`);
  values never appear in errors (`apt.rs:2006-2011`); publish requires the verify sentinel
  (`apt.rs:2034-2038`) and kills the agent after signing (`apt.rs:2079-2084`).
- **No secret values in repo:** `passphrase_secret`/secret grep over `.github-gen/`,
  `.github/ci/` is clean; signing-key fingerprint is a pinned public identity
  (`is_full_fingerprint` + `fingerprints_match`, `apt.rs:90-109`).
- **Trust enforcement:** no NEW `pull_request_target` (sole hit `ci-policy.yml` is
  pre-existing, untouched on this branch); all 17 velnor-lane jobs in `ci-pr.yml` AND
  `ci-main.yml` plus `prepare-cargo` carry the `head.repo == repository` same-repo gate,
  and reusable callees re-gate (defense in depth); producer workflow is push-main +
  dispatch only (absence of PR/schedule/tags triggers is test-pinned,
  `runtime_products.rs:923`); feed publish gated on default-branch ref + schedule/dispatch
  + `runner != velnor` + `package-feed` environment, with a secretless verify job and a
  rollback-deploy guard.
- **Fail-closed spot checks:** `PinnedImage` rejects tags/short digests by construction
  (`runner.rs:94-147`); digest+provenance hook blocks provisioning on mismatch
  (`runner.rs:313-401`); allocator refuses unconfigured ledgers and advertises capacity
  only post-reconcile (`allocator.rs:107,117-120`); `ensure_pin_present`'s fetch arg is
  40-hex-validated upstream (`policy.rs:628,696,551`) — no git flag injection;
  `promote` binds render≡stamp before writing (`promote.rs:244-268`), stages only owned
  paths via pathspec-scoped `add` (`promote.rs:449-462`), restores preimages on failure;
  stable-deb digest is verified before extraction (`apt.rs:1677-1693`).
- **Out-of-scope confirmations:** no new SSH credential code (docs-only mentions); no
  telemetry keys exist (OTLP is endpoint-only, `telemetry.rs:135-153`); `new_with_app` /
  `new_with_pat` have no production callers yet — the scale-set client is protocol-only,
  so the future wiring PR must load key material from `0600` files/env without logging
  (see F3).

## Method note

Read-only: `git diff main...docs/bastion-final-plan` (stat + targeted hunks), full reads of
`scaleset/{client,session,credentials,errors,config,backoff,upstream_pin,allocator}.rs`,
`scaleset/worker/{mod,runner,dind,ownership,supervise}.rs`, `apt.rs` (targeted),
`promote.rs`, `policy.rs` (targeted), `runtime_products.rs`/`release.rs` (generated
templates), all changed workflows, plus scripted verification of fork gates (34/34 velnor
jobs gated) and secret scans. No edits made.
