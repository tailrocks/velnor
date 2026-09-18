# Scale-set re-pin analysis — campaign gate D1 (READ-ONLY)

Date (UTC): 2026-09-18. Method: repo reads + `gh api` GET only. No writes outside `/tmp`, no mutations, no PRs.

## Verdict

- **Re-pin `fb563005` → `e6daac70`: NO-OP, already complete in-tree.** The upstream
  delta is one CI-workflow commit with zero Go changes, and the tree already pins
  `e6daac70` everywhere. **Zero code changes required.**
- **Live canary: still NO-GO — external operator blocker unchanged** (no installed
  App with Administration:write; see §4).

## 1. Upstream repo: `actions/scaleset`, not `actions/runner`

- Pin source: `crates/velnor-runner/src/scaleset/upstream_pin.rs:18` —
  `UPSTREAM_REPO = "https://github.com/actions/scaleset"`, and
  `UPSTREAM_COMMIT` re-exports `velnor_model::SCALESET_UPSTREAM_COMMIT`
  (`crates/velnor-model/src/scheduler.rs:10` =
  `e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5`).
- AGENTS.md's "`actions/runner` is the protocol source of truth" covers the
  classic runner protocol (job messages, broker, expressions, credentials,
  run-service, timeline). The scale-set adapter (`crates/velnor-runner/src/scaleset/`,
  per `mod.rs:4-5`) explicitly ports the `actions/scaleset` wire protocol instead.
  Both rules coexist: per-slot JIT V2 follows `actions/runner`; the ScaleSetV2
  adapter follows `actions/scaleset`. The scout doc (`/tmp/upstream-scout.md`)
  reflects the same split (§§1–6 runner, §7 scaleset).

## 2. Diff `fb563005...e6daac70`: CI-only, consumed API surface identical

`gh api repos/actions/scaleset/compare/fb56300…55c6...e6daac7…48e5` (GET):
status `ahead`, 1 commit, 2 files. The single commit is `e6daac70` "Bump the
actions group across 1 directory with 2 updates (#127)". Patch touches only:

- `.github/workflows/e2e.yaml`: `setup-go@v6`→`@v7`,
  `workflow-application-token-action@cb731e…`→`@dad2b8…`
- `.github/workflows/go.yaml`: `setup-go@v6`→`@v7` (4 jobs)

Zero `.go` changes by construction. Separately confirmed `e6daac70` is still
HEAD of upstream `main` (live ref is current; nothing newer to chase).

Consumed-surface assessment (baseline: `/tmp/upstream-scout.md` §7 at `fb563005`):

| Surface the adapter consumes | `fb563005` → `e6daac70` | Basis |
|---|---|---|
| Message types (`JobAvailable/Assigned/Started/Completed`, `JobMessageBase`, envelope `RunnerScaleSetJobMessages`) | NO CHANGE | `types.go` untouched |
| `AcquireJobs` (POST `.../acquirejobs`, queue-token auth, `{count, value[]}` subset) | NO CHANGE | `session_client.go` untouched |
| `TotalAssignedJobs` statistics authority | NO CHANGE | `types.go` untouched |
| ACK (`DELETE {queue}/{id}`, 204; redelivery on skip) | NO CHANGE | `session_client.go` untouched |
| Session create/PATCH-refresh/close, 401→refresh→retry-once | NO CHANGE | `session_client.go` untouched |
| Admin token chain (`getRunnerRegistrationToken`→`getActionsServiceAdminConnection`→`fetchAccessToken`), CRUD, JIT config | NO CHANGE | `client.go` untouched |
| Listener contract (`Scale(ctx,msg)`, `InitialMessageID=-1`, ACK-after-success, no concurrency) | NO CHANGE | `listener/listener.go` (#113 shape) untouched |
| Error taxonomy, JWT provider, config URL parsing, retry/timeout defaults | NO CHANGE | `errors.go`, `jwt_provider.go`, `config.go`, `common_client.go` untouched |

This confirms the in-tree audit comments (`upstream_pin.rs:11-14`,
`scheduler.rs:7-9`) rather than just trusting them.

## 3. In-tree impact: nothing to change (re-pin already landed)

The re-pin is already in this tree (landed via `486de4e0`/`356e3067` re-ports):
pin constant, fail-closed tests, both fixture manifests, and the protocol-test
header all read `e6daac70`. `require_pin` rejects `fb563005…` by design
(negative drift test). Remaining `fb563005` refs in `crates/` are intentional
(doc history notes + the negative test) — not drift.

Pin surface — files that WOULD need edits in a real re-pin (all already correct;
listed with reason, no edits made):

| File:line | Reason it belongs to the pin surface |
|---|---|
| `crates/velnor-model/src/scheduler.rs:5-10` | `SCALESET_UPSTREAM_COMMIT` const + audit doc |
| `crates/velnor-model/src/scheduler.rs:461-464` | pin-identity test asserting full SHA |
| `crates/velnor-runner/src/scaleset/upstream_pin.rs:9-18` | `UPSTREAM_COMMIT`/`UPSTREAM_REPO` + audit doc |
| `crates/velnor-runner/src/scaleset/upstream_pin.rs:46-55` | fail-closed drift tests |
| `crates/velnor-runner/src/scaleset/fixtures.rs:70,272,304` | `require_pin` enforcement + manifest writers |
| `crates/velnor-runner/tests/fixtures/scaleset/manifest.json:2` | recorded `upstream_commit` |
| `crates/velnor-runner/tests/fixtures/scaleset-worker/manifest.json:2` | recorded `upstream_commit` |
| `crates/velnor-runner/tests/scaleset_protocol.rs:4-5` | header pinning upstream behaviors |
| `crates/velnor-runner/src/node/scheduler.rs:63,99` | pin re-export + length assertion |
| `scaleset/{session,client,config,listener,errors}.rs:1`, `mod.rs:4-5` | doc headers floating on `UPSTREAM_COMMIT` (auto-correct, no edit even in a real re-pin) |

Stale non-code refs (campaign docs still record the 2026-09-15 ref; refresh
opportunistically, not gate-blocking for code): `plans/bastion-three-provider-ci/evidence.md:129,134`
("Scale Set main still `fb563005…`"), `goal.md:17`, `work-plan.md:71`;
`/tmp/d1-canary-prep.md` §2.2 step 4 (run-log header should record `e6daac70`, not `fb563005`).

## 4. D1 live-canary external blocker: GitHub App permission

Per `/tmp/canary-perms.md` (2026-09-17 read-only probe) + `/tmp/d1-canary-prep.md` §2:

- **Exact need:** org-owned GitHub App **`velnor-d1-canary`**, permissions
  **Administration: R/W + Actions: R**, **installed on org `tailrocks`**.
  (Administration:write is required for the scale-set/runner-group admin plane:
  set CRUD, sessions, `acquirejobs`, `GenerateJitRunnerConfig`.)
- **Current state:** 5 installed apps, none qualifies (`renovate` has only
  Administration:read; `chatgpt-codex-connector` has Actions:write but no
  Administration; rest have neither). Current PAT (org admin + `repo`,
  `admin:org`, `workflow`) CAN create the repo/group/workflows but CANNOT
  substitute — prep §2.1 mandates App auth and the harness FAILS CLOSED on PAT.
- **Minimal operator action (manual, UI/manifest flow — not doable via PAT REST):**
  1. Create App `velnor-d1-canary` (org-owned, Administration R/W + Actions R), install on `tailrocks`.
  2. Provision its private key + App ID + installation ID into the bastion host
     credential provider (`/etc/velnor`, mode 0600, root/velnor-mgmt only).
  3. Verify provider reports "App auth, key fingerprint …" with zero secret bytes logged.
  4. Record App/set/group/repo IDs + upstream pin `e6daac70` in the canary run-log header.
- Not blockers: scale-set REST 404s (expected — managed via runtime API/UI, not
  classic REST); canary repo/group/set names all free.

## Evidence commands (all read-only)

- `gh api repos/actions/scaleset/compare/fb563005…...e6daac70…` (+ `--jq` file/patch projection)
- `gh api repos/actions/scaleset/commits?per_page=5` (HEAD check)
- `git log -S 'fb563005'`, `git grep -n 'fb563005|e6daac70'`, fixture manifest reads
