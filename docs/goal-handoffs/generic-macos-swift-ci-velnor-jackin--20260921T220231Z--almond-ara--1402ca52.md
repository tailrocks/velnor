# GOAL: Generic, correct, fast macOS and Swift CI in Velnor; migrate Jackin

> Resume command: `/goal Read and resume docs/goal-handoffs/generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52.md`
> Pause applies until the user requests resumption. That resume command authorizes
> continuing the ORIGINAL goal; it does NOT authorize pausing again or regenerating this handoff.

## A. Identity and pause status

- Handoff ID: `generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52`
- Created (UTC): 2026-09-21T22:02:31Z. Last update (UTC): 2026-09-21T22:37:34Z (audit repairs; prior `23:05` was a hand-written clock error, corrected).
- Original goal status: `PAUSED_BY_USER` (requested disposition; not proof of runtime stop).
- Handoff status: `READY` (preservation + publication + independent review verified; audit record in §K).
- Worker stop status (verified via terminal subagent results, not mere Markdown):
  - `main/repin3-merge-1044/59` — TERMINAL before pause with STOP report (re-pin-3 done,
    #1044 merge blocked by mold defect). No further action from it.
  - `main/m4-pin-bump/60` — TERMINAL after pause: acknowledged stop message, returned stop
    report (M4 PR #1062 already merged; nothing in flight). Verified stopped.
  - Auditors 61–64 — all TERMINAL, read-only, no mutations.
  - No other goal-owned workers, watchers, retry loops, or background tasks exist in this
    session (roster checked; prior workers all terminal).
- Runtime goal-pause control: NOT AVAILABLE. Observed goal controls are only
  complete/blocked/progress-report; no pause API exists. The goal record was deliberately left
  active (marking complete/blocked would be false). Freeze is enforced by coordinator stopping
  all goal work. Recorded as limitation.
- Source agent: Muse Code CLI, session `almond-ara` (`01a0c0c6-f3c6-7640-a1c1-11f14fd24d07`),
  goal `goal-56104e90-5936-4fdf-a6c0-d390649489ab`.
- Repositories: primary `tailrocks/velnor` (this HANDOFF); subordinate `jackin-project/jackin`
  (preservation branches only, no file changes, no Jackin PR).
- Handoff path (velnor-relative): `docs/goal-handoffs/generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52.md`
- Source branch/HEAD at pause: velnor checkout on `m4-pin-bump` @
  `72d0d92bcceb93460cb932c5a194e03ab3ca76af` (clean); jackin checkout on
  `migrate/apple-ci-generic` @ `6ff54ce541b2a5bf0a2a128b0fb814503a7d54c9` (clean).
- Observed remote heads at audit: velnor `origin/main` = `45ef1ebe` (M4 #1062);
  jackin `origin/main` = `df4671e4`.
- Preservation branch: `goal-handoff/generic-macos-swift-ci--1402ca52` (velnor, from
  `origin/main` @ `45ef1ebe`). Handoff PR: https://github.com/tailrocks/velnor/pull/1066
  (DRAFT, auto-merge off; final head SHA in PR body).
- Recovery: fully remote-portable for all goal-owned Git work (preservation branches pushed,
  §E). Session logs are local-only conveniences, not required for resume (§K).
- Resume authorization: explicit later user request only.

## B. Original goal and success contract

The COMPLETE original goal-defining user prompt (102,446 chars, 688 lines) is embedded
verbatim in Appendix L of this document — no session access needed. Key passages are quoted
below for orientation; Appendix L is authoritative. Appendix M holds the atomic
source-to-handoff requirements matrix (G-### / H-###).

Source register (session log = `/Users/donbeave/.local/share/muse/sessions/2026/09/21/01a0c0c6-f3c6-7640-a1c1-11f14fd24d07/session.jsonl`):

| ID | Type | Location | Access |
|----|------|----------|--------|
| SRC-GOAL | original /goal text (102,446 chars) | session log line 19 (`model_user_messages[0]`); EMBEDDED in §L | FULL |
| SRC-DCO | DCO-signoff steers (5x) | session log lines 13812, 50960, 51656, 51785, 51966 | FULL |
| SRC-DELEG | delegate-first steer (6x identical) | session log lines 103501, 103984, 103987, 111968, 116793, 117358 | FULL |
| SRC-AUTON | autonomy steer | session log line 126488 | FULL |
| SRC-COMMIT | commit-often steer | session log line 126933 | FULL |
| SRC-PAUSE | pause/handoff instruction | session log line 126980 | FULL |
| SRC-PERF | slow-computer steers (off-goal) | session log lines 99261, 99738, 100150 | FULL (excluded, unrelated) |
| SRC-AUDIT | this audit instruction | parent delegation (post-pause; handoff-contract only, not original goal) | SECONDARY_ONLY |
| SRC-MERGE-STEER | separate merge-not-rebase steer | NONE EXISTS — goal-internal (SRC-GOAL §13 Execution rules) | UNAVAILABLE |
| SRC-SMALLPR-STEER | separate small-PRs steer | NONE EXISTS — goal-internal (SRC-GOAL §13 Execution rules) | UNAVAILABLE |

Verbatim (user-provided, all verified against SRC-GOAL):

- QUOTE (title): `# Generic macOS and Swift CI for Velnor: Jackin audit, implementation report, and /goal`
- QUOTE: `Research snapshot: **20 September 2026 UTC**.`
- QUOTE (recommendation): `extend Velnor's existing typed scanner, product graph, planner, and runtime so it can discover SwiftPM packages, Xcode/XcodeGen applications, and their native-library producers. Give each product explicit inputs, toolchain requirements, outputs, and validation rules. Generate GitHub Actions and supported Velnor execution from that model. Remove Jackin's opaque CI task workaround after this model preserves its complete application contract.`
- QUOTE (`/goal` opener): `/goal Generic, correct, fast macOS and Swift CI in Velnor; migrate Jackin` …
  `Implement a reusable macOS/Swift build model in tailrocks/velnor's velnor-workflow, and use it to remove Jackin's custom native CI orchestration while preserving and completing the required verification of Jackin's real macOS application.`
- QUOTE (executor contract, complete): `Make GitHub-hosted runners the default. The same typed build contract must support eligible Velnor executors through their actual capabilities. A standard Debian host can perform scanning, orchestration and supported Linux work; it cannot validate a native Xcode/SwiftUI/AppKit application. An ordinary Linux container running on a Mac also cannot supply that Apple build capability.`
- QUOTE (framing): `Use the complete block below as the implementation goal. It is intentionally an execution instruction rather than a request to write another plan. The preceding report supplies the detailed evidence and design constraints.` … `Start by delegating the baseline, current-architecture inspection, PR-review audit and independent design challenge in parallel; then execute the staged implementation through reviewable increments.`

Execution rules (verbatim excerpts from SRC-GOAL §13): `Delegate first.` … `A separate verifier must review
material conclusions and completed implementations without relying on an implementer's claim
that they work.` / `Do not ask clarification questions to resolve task ambiguity.` /
`Diagnose the structural cause before fixing a symptom.` / `Commit each coherent, verified
increment promptly and push regularly. Prefer one active integration branch per repository.` …
`Fetch and merge current main into working branches; do not rebase published history.` …
`Open small, coherent PRs, address all review comments, merge once each exact candidate
satisfies its required gates` … `Keep a concise committed progress and handoff document with
decisions, outstanding work, tested SHAs and concrete continuation steps.`
Further verbatim goal-internal rules: `Before changing anything, read applicable AGENTS.md instructions, repository architecture, current generated-file ownership, open PR descriptions, reviews and discussions. Refresh the PR/run identities: the research snapshot below is evidence, not a claim that today's head is unchanged.`;
`Judge changes by correctness, consistency and the goal; do not leave known faults merely because they are difficult or label them low value.`;
`Do not accumulate the entire effort in another large, unverified branch. Do not merge an implementation that temporarily breaks released consumer configuration: coordinate compatible rollout or publish the complete contract before migrating consumers.`;
`If something is blocked, attempt the authorized alternatives, establish the actual limitation, continue independent work, and report the exact blocked acceptance criterion and evidence. Missing optional infrastructure must never become an excuse to stop all work, or a reason to claim an unexecuted capability passed.`

Normative prohibitions (verbatim, binding on resumed work):

- `Do not present proposed TOML fields or CLI flags as already supported.`
- `Do not invent flags or break a clean checkout by suppressing needed work.` / `Do not invent stable JSON timing flags or assume release and debug artifacts are interchangeable.`
- `Do not hand-edit generated workflow output as the permanent fix.`
- `Do not replace swift-package-native-ci with a differently named opaque shell task.`
- `Keep generic code free of Jackin names, paths and task aliases.` (proven by renamed fixtures)
- `Do not equate platforms: [.macOS(...)] with “cannot run on Linux,” or absence of that declaration with portability.` (curly quotes in source)
- `Do not promise a numerical main-branch green rate: eliminate preventable PR/main contract differences and quantify actual reliability from evidence.`
- `Collect a larger continuing series for meaningful tail metrics; do not label three runs a reliable p95 estimate.` / `Do not invent an absolute speed target or claim a speedup until measured.`

Evidence anchors (SRC-GOAL §1; full table in §L): Jackin PR #1013 (`integrate/pr1002-multi-account`,
head `997c18fe`, base `fce94cea`); run 35535696169, job 106148084283/attempt 2 (merge
`ad70b92f`); failed attempt job 106145813603; Velnor audited at `59396040`; BoltFFI 0.30.1
(`2e6320a6`); runner macos-26-arm64 image 20260907.0351.1; arm64-only, floor macOS 26.0;
unresolved P1 Landlock review thread (discussion_r4057673954).

Session amendments (user steers; provenance in source register):

- DCO (SRC-DCO): `Commits must ALWAYS with Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>. Never anything else than Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` — binding on every resumed commit.
- Delegation (SRC-DELEG): `Use subagents aggressively for all work.` … `Always delegate work to subagents whenever delegation is possible. Treat subagents as the default execution mechanism, not an optional optimization.` + 8-bullet strategy (decompose into workstreams; spawn per workstream; parallelize all safe-concurrent work; subagents for research/implementation/review/testing/verification/cross-checking; avoid serial parent work; keep spawning as tasks unlock; independent verification; coordinate + synthesize) + `Do not merely recommend parallelization—actually execute the goal through subagents.` + `The parent agent should primarily orchestrate, resolve dependencies/conflicts, integrate results, run final deterministic checks, and ensure the complete goal is finished.` + `Default rule: **delegate first, parallelize aggressively, verify independently, then integrate.**`
- Autonomy (SRC-AUTON): `Never ask the user questions or wait for clarification. Work fully autonomously.` + 7-bullet self-unblocking protocol (spawn investigation subagents; analyze context/repo/docs/code/history/refs/best-practices; research alternatives; compare tradeoffs; verify assumptions independently; re-verify critical decisions; decide + continue) + `Prefer making a well-researched, reversible decision over asking the user.` + `Continue working until the goal is fully completed, verified, and no meaningful actionable work remains.` NOTE: the last sentence was SUPERSEDED by SRC-PAUSE for this session — resume only on explicit user request (§A).
- Commit cadence (SRC-COMMIT, 823 chars, quoted in full): `Always commit changes frequently while working. Prefer small, incremental, logically scoped commits instead of keeping a large dirty working tree for a long time and committing everything at the end. As soon as a meaningful unit of work is complete and verified, commit it. Push progress to the remote repository regularly so work is continuously propagated, recoverable, reviewable, and easy to bisect or revert. At the same time, avoid unnecessary branches. Prefer doing as much work as possible on a single working branch and keep committing to that branch throughout the task. Create additional branches only when there is a clear technical or workflow reason that makes working safely on the existing branch impractical or impossible. In short: **commit often, push regularly, and minimize branch proliferation.**`
- Merge policy: `Fetch and merge current main into working branches; do not rebase published history` is GOAL-INTERNAL (SRC-GOAL §13), not a separate steer. Squash-merge is SESSION PRACTICE (all 13 goal PRs), never a user steer — do not cite it as user authorization; follow each repo's merge policy on resume.
- Excluded: SRC-PERF (slow-computer complaints) — UNRELATED to this goal, correctly out of contract.

SUMMARY of scope/non-goals/acceptance (reconstructed from full text; not verbatim):

- Scope: typed schema-2 scanner discovery (SwiftPM/Xcode/XcodeGen/BoltFFI), product graph +
  identity, planner/placement, runtime execution/liveness, caching/artifact policy,
  GitHub-hosted macOS integration, Jackin migration off `swift-package-native-ci`,
  executor-portability proof, benchmarks, cleanup.
- Non-goals: unrelated subsystem rewrites; third Apple planner; unimplemented adapter claims
  (UniFFI/cbindgen/Tuist); Intel-slice removal (job already arm64); numeric green-rate promises.
- Acceptance contract (VERBATIM from SRC-GOAL; atomic decomposition in §M):
  1. `Static scanning executes no project code. Portable Swift, Apple-framework Swift, conditional manifests, local packages, remote XCFrameworks and tool artifact bundles are classified honestly. Ambiguities, include cycles/escapes and duplicate schemes/outputs are handled deterministically.`
  2. `A clean checkout discovers Jackin's actual app, Swift packages and Rust/BoltFFI producer, builds a missing XCFramework in correct order, and requires no Jackin task names in Velnor. A fixture with different names, paths and crate/package structure works through the same adapter. Build-only surfaces and no-test packages receive accurate commands/results.`
  3. `Source, transitive Rust dependencies, build.rs inputs, features, profile, lockfiles, flags, toolchain/SDK/deployment target, bindings config, headers, module maps and recipe changes invalidate every affected product. A Swift/resource-only edit reuses unchanged valid FFI while selecting the needed Swift/app checks. If identity is uncertain, reuse is conservative.`
  4. `Cold, exact-hit, compatible-prefix, absent, corrupt, truncated, wrong-architecture, wrong-profile, stale-source and untrusted-producer cases behave correctly. Exact product consumption verifies all required files and digests. Cache writes/reads preserve trust boundaries, and tests still execute on cache hits.`
  5. `Quiet successful children, chatty hangs, no-newline output, early-closed pipes, descendants retaining pipes, cancellation, cleanup and nonzero exit propagation have bounded regression tests. No test needs to wait a literal ten minutes.`
  6. `SwiftPM tests, actual app build, required harnesses, generated-binding drift, bundle/resources/linkage/architectures/deployment floor, dSYM identity where applicable, optimized verification and selected UI/runtime checks retain their intended assertions. Report build-only evidence separately from execution evidence.`
  7. `GitHub-hosted macOS succeeds; native Velnor macOS is proved by real execution before claimed supported; standard Debian cannot be selected as an Apple executor. A missing required platform fails the aggregate rather than becoming a green skip. Local and CI plans share semantic operations.`
  8. `Concurrent execution does not corrupt shared directories or products. Same-input serial and parallel builds produce equivalent verified manifests. Job artifacts have real producer dependencies; optional caches are never required for correctness.`
  9. `Generated files are deterministic and the repository's generation-check command rejects drift. Action/runtime pins and privileges remain correct. PR/merge-group/main required-result aggregation rejects missing, cancelled and skipped obligations and validates the correct source scope.`
  10. `PR #1013 has no unresolved blocking review concern, including the current disposition of the Landlock thread. The exact latest candidate/merge tree passes all required checks and independent review immediately before merge. Do not count earlier green heads, a successful job inside a cancelled run, or unrelated passing workflows as this gate.`
- Benchmark minimum (verbatim scenario list): `clean cold; dependencies warm; complete unchanged warm; Swift-only change; Rust FFI source change; transitive Rust change; binding-config/header change; dependency-lock change; profile change; Xcode/Rust/SDK change; and deleted/corrupt cached output.` Controls: `Keep source, runner class and toolchain controlled within each comparison. Repeat representative cold/warm cases and report sample count, individual runs, median/range, queue and critical-path time, per-phase work, transfer overhead, compiler reuse and coverage. Compare the same validation contract; report newly added app coverage separately from the historical Swift-only job.`
- Deliverables (verbatim): `merged PR/commit/release links and exact verified SHAs; current supported runner matrix; the real documented minimal repository configuration; scanner and adapter contracts; before/after command/product graph; retained/removed Jackin configuration; benchmark evidence; cache invalidation and liveness test results; review disposition; and any demonstrated remaining blocker with the precise unfinished criterion.`
- Post-resumption obligation (from pause instruction): integrate all required related goal
  work, resolve its PRs, and clean up verified-obsolete goal-owned local worktrees/branches —
  no blind merging of every experiment, no deletion of shared/unrelated resources.
- Merge permissions: `Open small, coherent PRs, address all review comments, merge once each exact candidate satisfies its required gates` (goal-internal; note the `exact candidate` qualifier); DCO signoff exactly `Alexey Zhokhov <alexey@zhokhov.com>` (SRC-DCO); protected distribution stays in protected jobs. Session practice used squash-merge for all 13 goal PRs — practice, not user authorization.

## C. State at the exact interruption point

Last completed actions (all 2026-09-21 UTC):

1. 21:03Z — Velnor PR #1061 (migfix3, defect-C sccache/mise edge) squash-merged as `155c6088`;
   runtime products published (run 35654843370).
2. ~21:15Z — Jackin #1044 re-pin-3 pushed: merge `8c46b0c7` (pin → velnor `eed474c4a1d9`)
   + `c02a37d9` (1-line generator-state fix) + forward merge `6ff54ce5` (= PR head, = local
   checkout). Policy failure at `8c46b0c7` fixed by `c02a37d9`.
3. 21:31Z — Velnor PR #1062 (M4 self-pin-bump to `eed474c4`) squash-merged as `45ef1ebe`
   after one `rust-velnor-workflow` flake retry; main CI green on merge.
4. Repin3 worker verified defect C fixed (mise installed 5/5 tools incl. boltffi 0.30.1) and
   returned STOP: new Velnor generator defect blocks #1044 merge (mold on macOS, §G).
5. Pause received; M4 worker acknowledged stop and returned its report; 4 read-only audits ran.

In progress at pause: NOTHING — both implementation workers were already terminal; no partial
edits, conflicts, or detached work in the two primary checkouts (both clean). The only
untracked item is this handoff skeleton (now this document).

Interruption point in one line: goal work is frozen between "M4 merged, main green" and
"fix mold defect → re-pin-4 → merge #1044".

FIRST resumption task (explicit; = T-001): reproduce the mold failure from
https://github.com/jackin-project/jackin/actions/runs/35657319778/job/106524303464
(`Set up mold 2.42.0` → `unsupported mold architecture: arm64`), then open a Velnor generator
fix PR that skips mold cache/setup on macOS runners (both s1 and s2 emitters + both unit-test
modules), merge it, confirm the runtime-products release for the new SHA, and only then do
Jackin re-pin-4 on #1044. Do NOT hand-edit Jackin's generated `ci-unit-rust.yml`.

Partially edited / inconsistent state: none in primary checkouts. Dirty volatile worktrees
(B2/B3/A4/80bc) were committed verbatim to `preserve/handoff-1402ca52/*` remote branches
(§E.2) — their local worktrees now sit on those branches, clean.

Open hypotheses: (a) whether the mold fix needs `ToolRequirement::Mold` insertion changes,
  `linker:` neutralization, or both (§G mold detail); (b) whether velnor #1054 (Mise-closure, green) is
  required by the Apple leg or lands independently; (c) #1044 vs jackin #1065 merge order.

## D. Requirement-by-requirement progress ledger

`ID | Requirement | Status | Evidence/files/commits | Remaining work | Dependencies`

| ID | Requirement | Status | Evidence | Remaining | Deps |
|----|-------------|--------|----------|-----------|------|
| R-EV | Evidence/liveness: bounded execution, no false silence-timeout | VERIFIED_DONE | velnor #985 `c832191f` (wall deadline), checks 23 pass | none | — |
| R-SCAN | Generic platform split + typed discovery groundwork | VERIFIED_DONE | #1025 `d771961b` (23 pass), #1030 `b68a6ea5`, #1031 `f3b94af3`, green mains | none | R-EV |
| R-PROD | Product contract: prereq/artifact/closure semantics | VERIFIED_DONE | #1036 `b634efd2`, #1043 `690b3935` (16/16), #1061 `155c6088` | mold fix is separate defect, see R-MOLD | R-SCAN |
| R-PACK | Preview packaging + per-arch artifacts | VERIFIED_DONE | #1035 `6737cdb3`, #1045 `a850b255` (22–23 pass) | observe main Preview @ `45ef1ebe` (peer tree green) | R-PROD |
| R-PIN | Self-pin rollouts (M2/M3/M4) | VERIFIED_DONE | #1026 `28f16b7a`, #1038 `e430c6a8`, #1062 `45ef1ebe` (23 pass, main green) | none | R-PACK |
| R-MOLD | Velnor generator: no mold on macOS | BLOCKED (fix NOT_STARTED) | failing job 106524303464; generated `ci-unit-rust.yml:367-370,796`; sources `s2/mod.rs:6183`, `src/lib.rs:5931` | fix PR both emitters + tests, merge, products release | R-PROD |
| R-MIG | Jackin migration #1044 merged | IN_PROGRESS (head green except mold leg) | branch `migrate/apple-ci-generic` @ `6ff54ce5`; 42 pass / 3 fail (2 consequential) | re-pin-4 after R-MOLD → green → merge | R-MOLD |
| R-5C | 5c hosted evidence (main Preview + hosted runs) | IMPLEMENTED_UNVERIFIED | main CI green @ `45ef1ebe`; Preview @ merge SHA unobserved | collect Preview run evidence | R-PIN |
| R-BENCH | Controlled benchmark matrix (§12.2) | NOT_STARTED | — | ≥3-rep cold/warm/Swift-only/Rust-change/… runs after R-MIG | R-MIG |
| R-PROV | Native macOS provider proof or demonstrated blocker | NOT_STARTED | — | implement + real-Mac e2e, or record external blocker | R-MIG |
| R-1013 | PR #1013 review disposition (incl. Landlock P1) | IMPLEMENTED_UNVERIFIED (was wrongly VERIFIED_DONE; corrected by audit) | jackin #1013 MERGED 07:58Z as `9ee50f6a` — merge proven, Landlock P1 thread disposition NOT verified; merge ≠ review disposition | T-011: re-verify thread discussion_r4057673954 state or record descoping authority | — |
| R-DOC | Committed progress/handoff doc | VERIFIED_DONE | `plans/apple-ci-progress.md` (D19) + this HANDOFF | keep current on resume | — |
| R-CLEAN | Remove superseded paths, final cleanup | NOT_STARTED | inventory §E (no cleanup performed during pause) | execute §E.6 after integration | R-MIG |

Stale-claim notes: (i) earlier "22/22 PR CI" counts refer to older PRs, not current heads;
(ii) #1044 title still says "pin b634efd2" — actual pin is `eed474c4`; (iii) velnor main had
carried failures at `eed474c4`/`c674f5bb` — historical, HEAD `45ef1ebe` is green.

## E. Change and preservation inventory

Primary-checkout changes: none pending — both primary checkouts are clean; all merged work is
on `origin/main` of each repo; #1044's branch is pushed (= remote head). The checkpoint code
is therefore the set of merged commits + open PR heads + preservation branches, all remotely
durable. No pre-existing/unrelated changes were touched. Recovery of scratch/rehearsal lines
is via `preserve/handoff-1402ca52/*` branches (§E.2), not via /tmp paths.

### E.1. Discovery scope and ownership

Inspected (read-only `git worktree list --porcelain`, `status`, `stash list`, `log`,
`for-each-ref`, `branch -vv`, `ls-remote --heads`, `gh pr view/checks`, `gh api` runs;
timestamps ~22:05–22:20Z; no pagination truncation encountered except noted `gh search`
repo-query rejection worked around by full list+title scan):

- R1 `/Users/donbeave/Projects/github/velnor` (origin `tailrocks/velnor`) — primary.
- R2 `/Users/donbeave/Projects/github/jackin` (origin `jackin-project/jackin`) — primary.
- R3 `/Users/donbeave/Projects/github/all-repo/tailrocks_velnor` — second velnor clone.
- R4 `/Users/donbeave/Projects/github/all-repo/jackin-project_jackin` — second jackin clone.
- R5 `/Users/donbeave/Projects/github/velnor-bastion` — checked, no goal work (UNRELATED).
- R6/R7 `agy-repos/*`, other `all-repo/*` — spot-checked, no goal work (UNRELATED).
- Linked worktrees: R1 5, R2 2, R3 22 (incl. W6) — 29 total, all enumerated (§E.2);
  R4 none. Initial audit undercounted R3; corrected per independent review (§K).
- Session logs under `/Users/donbeave/.local/share/muse/sessions/2026/09/21/01a0c0c6-.../`
  (local-only; retrieval instructions §K, not required for resume).

Coverage uncertainty: LOW. All registered worktrees enumerated; all local branches
enumerated; remote heads observed fresh via `ls-remote` (not just tracking refs);
PR discovery covered open+merged+closed via full-list scan (velnor #964–1063, jackin goal
range). Remaining risk: PRs outside scanned ranges with non-obvious titles (none indicated).

Ownership rule used: branch/worktree/PR is GOAL_EXCLUSIVE if created for this goal's
increments; GOAL_SHARED if it carries goal work plus other work or is a shared gate line;
UNRELATED if evidence shows other purpose; UNKNOWN if ambiguous (retained + preserved).

### E.2. Local worktree and clone ledger

Stable IDs W1–W12. Host: `mac-studio` (local operator Mac) for all.

| ID | Repo | Path | Type | Branch / HEAD | Upstream | State @observe | Ownership | Disposition |
|----|------|------|------|---------------|----------|----------------|-----------|-------------|
| W1 | R1 | `/Users/donbeave/Projects/github/velnor` | main checkout | NOW `goal-handoff/generic-macos-swift-ci--1402ca52` @ `8c904fbf` (clean; was `m4-pin-bump` @ `72d0d92b` at pause, moved by publication) | `origin/goal-handoff/generic-macos-swift-ci--1402ca52` | clean, no stash/lock | GOAL_EXCLUSIVE | KEEP (primary checkout; re-branch off fresh `origin/main` on resume; see VB7) |
| W2 | R2 | `/Users/donbeave/Projects/github/jackin` | main checkout | `migrate/apple-ci-generic` / `6ff54ce5` (clean, = origin) | `origin/migrate/apple-ci-generic` | clean | GOAL_EXCLUSIVE | KEEP (primary checkout; resume re-pin-4 here) |
| W3 | R2 | `/private/tmp/jackin-m1rehearsal` | linked | was detached `8fb49688` +13 dirty → NOW `preserve/handoff-1402ca52/jackin-m1rehearsal` @ `2c57f74b`, clean, pushed | origin preserve branch | preserved remotely | GOAL_EXCLUSIVE | INTEGRATE_THEN_REMOVE (adjudicate vs #1044, then remove) |
| W4 | R2 | `/private/tmp/jackin-scratch` | linked | was detached `c2306de9` +dirty+untracked → NOW `preserve/handoff-1402ca52/jackin-scratch` @ `0b02a313`, clean, pushed | origin preserve branch | preserved remotely | GOAL_EXCLUSIVE | INTEGRATE_THEN_REMOVE |
| W5 | R1 | `/private/tmp/velnor-gen-c832191f` | linked | was detached `c832191f` +rust.rs edit → NOW `preserve/handoff-1402ca52/velnor-rust-scan` @ `01e3ce81`, clean, pushed | origin preserve branch | preserved remotely | GOAL_EXCLUSIVE | INTEGRATE_THEN_REMOVE (triage 4-line edit) |
| W6 | R3* | `/private/tmp/velnor-main-80bc` | linked (of R3) | was detached `80bc420d` +1-line pin → NOW `preserve/handoff-1402ca52/velnor-pin-80bc` @ `473eb7b6`, clean, pushed | origin preserve branch | preserved remotely | GOAL_SHARED (gate line) | INTEGRATE_THEN_REMOVE |
| W7 | R1 | `/private/tmp/velnor-1057-generated-repair` | linked | `codex/1057-generated-repair` @ `73c0071a`, clean | none observed | clean | UNRELATED (#1057 off-goal) | NOT_APPLICABLE (leave for owner) |
| W8 | R1 | `/private/tmp/velnor-1057-rebase-current` | linked | `codex/1057-rebase-current` @ `70268cd5` = #1057 head, clean | none observed | clean | UNRELATED | NOT_APPLICABLE |
| W9 | R1 | `/private/tmp/velnor-review-1050-current` | linked | detached `96b835d9` = velnor #1050 head, clean | n/a | clean | GOAL_SHARED (review aid) | REVIEW_SHARED |
| W10 | R1 | `/private/tmp/velnor-wavepin2` | linked | detached `4dec6b9e` = #1041 merge, clean | n/a | clean | GOAL_SHARED (pin archaeology) | REVIEW_SHARED |
| W11 | R3 | `/Users/donbeave/Projects/github/all-repo/tailrocks_velnor` | independent clone main checkout | `red-main/velnor-pin-bump` @ `93ad5c4d`, clean; local `main` = `4dec6b9e` | n/a | clean (corrected per review; earlier `a77a2c10` claim was wrong — that SHA is only an old controller commit in R3) | GOAL_SHARED | REVIEW_SHARED |
| W12 | R4 | `/Users/donbeave/Projects/github/all-repo/jackin-project_jackin` | independent clone main checkout | `rollout/velnor-wave` @ `986f94bc`; local `main` = `d0d4ee09` | n/a | clean (corrected per review; `0ec60b9d` does not exist in R4) | GOAL_SHARED (mirror) | REVIEW_SHARED |
| W13 | R3 | 21 linked `/private/tmp/*` worktrees (group; W6 listed separately) | linked ×21 | see enumeration below | n/a | all clean except `velnor-pin-be61acbb` (3 untracked cargo-install artifacts — disposable, see enumeration) | GOAL_SHARED | REVIEW_SHARED (retain; re-observe before any action) |

W13 enumeration (all `/private/tmp/…`, `git worktree list` + `status --porcelain`;
enumerated at pause, all 21 re-verified at recorded SHAs by audit ~22:35Z; all clean
unless noted): `g11-gate` b4fbe636 detached;
`g11-main` c832191f detached (= goal PR #985 merge — goal-adjacent);
`velnor-4fa7a3a8` 4fa7a3a8 detached; `velnor-b9c3156` b9c3156c detached;
`velnor-fresh` f3b94af3 detached (= goal PR #1031 merge — goal-adjacent);
`velnor-g1` 113a6cda [rollout/velnor-g1]; `velnor-g10` 93675b2a [rollout/velnor-g10];
`velnor-g11` 9c29152d [rollout/velnor-g11]; `velnor-g12` 533aafa6 [rollout/velnor-g12];
`velnor-g4` f6ca3469 [rollout/velnor-g4]; `velnor-g5` 3fe0b19f [rollout/velnor-g5];
`velnor-g7` 4b2e2f3d [rollout/velnor-g7]; `velnor-g8` cb8417bb [rollout/velnor-g8];
`velnor-g9` 78ad2dd2 [rollout/velnor-g9]; `velnor-gen-6737` 6737cdb3 detached
(= goal PR #1035 merge — goal-adjacent); `velnor-gen-80bc` 80bc420d detached;
`velnor-gen-eed4` eed474c4 detached (= #1044's pin — goal-adjacent);
`velnor-pin-be61acbb` be61acbb detached +3 untracked cargo-install artifacts
(`.crates.toml`, `.crates2.json`, `bin/`; born 18:12Z pre-pause) — disposable class,
no remote preservation needed;
`velnor-verify` f3b94af3 detached; `velnor-verify2` be61acbb detached;
`velnor-wavepin` be61acbb detached.
Note: host is shared — `cq-*`, `chainargos-*` /tmp names and other goals' worktrees exist;
only the above were inspected. R4 has no linked worktrees (single checkout).

\* W6's common dir is R3's `.git`. No locks, no in-progress merge/rebase/cherry-pick, no
stashes, no submodules, no detached tips remaining anywhere after preservation commits.
No goal-owned running processes (all subagents terminal; no watchers/daemons started).

Inaccessible worktrees: NONE — every registered worktree path existed and was readable.

### E.3. Local and remote branch ledger

Velnor R1 branches (observed post-preservation; remote state via `ls-remote --heads`):

| ID | Ref | Tip | Upstream / remote head | PR | Ownership | Disposition |
|----|-----|-----|------------------------|----|-----------|-------------|
| VB1 | `m4-pin-bump` | `72d0d92b` (W1) | STALE tracking; remote DELETED | #1062 MERGED | GOAL_EXCLUSIVE | delete after resume re-branch (fully merged) |
| VB2 | `main` (local) | `59396040` (98 behind `origin/main`) | `origin/main` = `45ef1ebe` | n/a | GOAL_SHARED | fast-forward on resume; never delete |
| VB3 | `fix/e0277-include-str` | `90e71de5` = #1017 head | remote deleted | #1017 MERGED | GOAL_EXCLUSIVE | delete (fully merged) |
| VB4 | `fix/unit-platform-split` | `d4b0570f` (+1 scheduler commit past #1025 head `6603e98c`) | remote deleted | #1025 MERGED | GOAL_SHARED | salvage +1 commit first, then delete |
| VB5 | `integrate/apple-ci-s2` (+ stale `origin/` tracking) | `326fd414` (diverged past #985 with scaleset commits) | remote STILL ON ORIGIN (stale) | #985 MERGED | GOAL_SHARED | salvage/confirm scaleset commits, then delete local + remote |
| VB6 | `preserve/stray-runner-header-0abc3675` | `0abc3675` | PUSHED this handoff (was local-only) | none | UNKNOWN | REVIEW_SHARED (now remotely durable) |
| VB7 | `goal-handoff/generic-macos-swift-ci--1402ca52` | branch head (= PR head; see §A) | pushed during publication | handoff PR #1066 (draft) | GOAL_EXCLUSIVE | KEEP until goal completes |

Velnor remote-only relevant: `fix/apple-mise-tool-closure` (#1054 OPEN, §E.4);
`preserve/handoff-1402ca52/{velnor-rust-scan@01e3ce81,velnor-pin-80bc@473eb7b6}` (this
handoff). Deleted-from-origin (verified): all merged goal branches except VB5's remote.

Jackin R2 branches:

| ID | Ref | Tip | Upstream / remote head | PR | Ownership | Disposition |
|----|-----|-----|------------------------|----|-----------|-------------|
| JB1 | `migrate/apple-ci-generic` | `6ff54ce5` (W2, = origin) | in sync | #1044 OPEN BLOCKED | GOAL_EXCLUSIVE | KEEP — resume re-pin-4 here |
| JB2 | `main` (local) | `fce94cea` (pre-#1013 era) | `origin/main` = `df4671e4` | n/a | GOAL_SHARED | fast-forward on resume |
| JB3 | `integrate/velnor-apple-ci` | `8fb49688` (M1 pre-commit; 693-line project.toml diff vs #1044) | PUSHED as `preserve/handoff-1402ca52/integrate-velnor-apple-ci` | none (competing formulation) | GOAL_EXCLUSIVE | adjudicate vs #1044 (keep one), then delete |
| JB4 | `pr-1013` | `997c18fe` | local-only | none | UNRELATED (leftover) | retain (not goal-owned) |

Jackin remote-only relevant: `preserve/handoff-1402ca52/{jackin-m1rehearsal@2c57f74b,
jackin-scratch@0b02a313,integrate-velnor-apple-ci@8fb49688}` (this handoff);
`cicd/repromote-major1-9660c9ff` (closed PR #1062's branch, still on origin — remote cleanup
candidate after resume); #1065's branch (OPEN, head `909a9f54`, not fetched locally).

R3/R4 long tail (all-repo clones): ~25 `rollout/*` + `codex/*` branches, mostly `[gone]`
upstreams mirroring merged gate work (e.g. `rollout/wave-b-*`, `p962-*`), plus a few live
ones. Each recorded with tip SHA in the worktree auditor's report (`/tmp/audit-wt.txt`,
local-only convenience copy; canonical record: the branches themselves in the persistent
R3/R4 clones). Ownership GOAL_SHARED; disposition REVIEW_SHARED; remote preservation NOT
performed (mirrors of merged work; local clones retained; re-verify before any deletion).

Stashes: none in any repo. Reflog-only tips: none goal-relevant identified.

### E.4. Related PR ledger

Velnor `tailrocks/velnor` (all states scanned #964–1063):

| ID | PR | Title | State | Head | Merge/base | Checks @head | Reviews | Future action |
|----|----|-------|-------|------|------------|--------------|---------|-------------|
| VP985 | #985 | bound CI cmd exec by wall deadline | MERGED 11:42Z | `2f684383` | `c832191f` | 23 pass | COMMENTED, non-blocking | done |
| VP1017 | #1017 | destructure include_str_paths tuple (e0277) | MERGED 12:10Z | `90e71de5` | `ef42b0f9` | pass+12 skip | none | done |
| VP1025 | #1025 | split collapsed hosted jobs by platform | MERGED 12:52Z | `6603e98c` | `d771961b` | 23 pass | none | done |
| VP1026 | #1026 | bump D19 pin to 9b0d2a8f | MERGED 13:34Z | `9b20ab46` | `28f16b7a` | 23 pass | none | done |
| VP1030 | #1030 | BoltFFI Cargo profile via [native.apple] | MERGED 13:15Z | `76f90a08` | `b68a6ea5` | pass+skips | none | done |
| VP1031 | #1031 | Apple deploy floor as MACOSX_DEPLOYMENT_TARGET | MERGED 14:05Z | `dd398209` | `f3b94af3` | pass+skips | none | done |
| VP1036 | #1036 | Apple CI generator fixes (migfix1) | MERGED 15:30Z | `9c2ddd84` | `b634efd2` | pass+skips | none | done |
| VP1035 | #1035 | Preview packaging: aarch64 linker + deb twin | MERGED 16:03Z | `351b9aad` | `6737cdb3` | 23 pass | none | done |
| VP1038 | #1038 | bump D19 pin to 6737cdb3 (M3) | MERGED 16:29Z | `5b3d0e51` | `e430c6a8` | 23 pass | none | done |
| VP1043 | #1043 | install-subset closure + XcodeGen join (migfix2) | MERGED 18:35Z | `48d186c1` | `690b3935` | pass+skips | none | done |
| VP1045 | #1045 | twin debian legs via per-arch artifacts | MERGED 20:12Z | `5080aa52` | `a850b255` | 23 pass 1 skip | none | done |
| VP1061 | #1061 | install subsets over mise backends (migfix3) | MERGED 21:03Z | `84877bf5` | `155c6088` | 12 pass 12 skip | none | done |
| VP1062 | #1062 | bump D19 pin to eed474c4 (M4) | MERGED 21:31Z | `72d0d92b` | `45ef1ebe` = origin/main | 23 pass 1 skip | none | done |
| VP1054 | velnor #1054 | share derived Mise provider facts | OPEN, MERGEABLE/CLEAN | `256c24bb`, base `45ef1ebe` | — | 8 pass 14 skip, run 35658288961 | none | land on resume if Apple leg needs it |
| VP1050 | #1050 | evidence-lossless | OPEN green CLEAN | `96b835d9` | — | green | none | triage on resume (adjacent) |
| VP1052 | #1052 | rust-bootstrap-skip | OPEN DIRTY/CONFLICTING | `b084c416` | — | 22 pass (stale basis) | none | triage (adjacent; rebase state on resume) |
| VP1055 | #1055 | product-receipts | OPEN green CLEAN | `2a270947` | — | green | none | triage (adjacent) |
| VP1056 | #1056 | native-product-closure | OPEN green CLEAN | `9f795b2e` | — | green | none | triage (adjacent) |
| VP1058 | #1058 | composable-regen | OPEN FAILING | `93cd45e9` | — | ❌ rust-velnor-workflow, run 35659333172 | none | BLOCKED triage (likely out of scope) |
| VP1063 | #1063 | p962-port | OPEN FAILING BLOCKED | `5349ec32` | — | ❌ Policy | none | BLOCKED triage (likely out of scope) |
| VP-HO | handoff PR #1066 | `GOAL: Generic macOS and Swift CI in Velnor; migrate Jackin — paused handoff [1402ca52]` | DRAFT, head `8c904fbf`, base `45ef1ebe` | `goal-handoff/...` | `45ef1ebe` | ❌ 4 fail (`Control / Required`, docs, Policy, `ci-required`) — EXPECTED red, never-merge draft; observe only, do not repair-loop. READY ≠ green. | none | KEEP until resume; close after goal done |

Adjacent merged (context, not goal-owned): #1039 D3 (7576f40f), #1041 G10 wavepin (4dec6b9e),
#1046 Major-1 (9660c9ff), #1048 cache guards (80bc420d), #1053 cargo-bin flag (eed474c4 —
#1044's pin), #1059 s2 Bun watch.

Possibly goal-adjacent OPEN (reviewer-flagged; triage on resume, not goal-owned until
confirmed): velnor #962 `fix: route arm64 release producers to native hosted runners`;
velnor #978 `fix(workflow): resolve tool prerequisites and complete package inputs`;
jackin #1007 `ci: pin the attested workflow runtime and regenerate`. Newer velnor
#1064–1065 / jackin #1066–1070 are other goals'/features' PRs — out of scope.

Additional OPEN PRs inventoried by audit (triage-or-out-of-scope; treat as OUT OF SCOPE
until a goal requirement links to them): velnor #1044 `feat(workflow): enforce active
renderer lifecycle` (OPEN BLOCKED `60bb9326`) — triage (renderer lifecycle adjacent);
velnor #963 `feat: integrate approved workflow scanner sources` (OPEN DIRTY `056362aa`) —
triage (scanner adjacent); velnor #980 `fix(ci): reconcile validation architecture and
performance campaign` (OPEN DIRTY `ab2f12fa`) — triage (validation/benchmark adjacent);
velnor #979 `fix(ci): make required validation fail closed` (OPEN DIRTY `9bbf4a4e`) —
triage (gate adjacent); velnor #973 `fix(package-release): preserve pre-existing rolling
tag` (OPEN DIRTY `04da35e4`) — out of scope (release mechanics); jackin #1064 `feat(ci):
collect durable first-attempt evidence` (OPEN DIRTY `f1402583`) — triage (CI evidence
adjacent); jackin #1004 `feat(release): verify preview package handoff` (OPEN DIRTY
`073ebbc5`) — triage (Preview adjacent); jackin #1063 `fix: usage-broker fallback, capsule
channel discipline, attach diagnosis` (OPEN DIRTY `ec3dbcf6`) — out of scope (product
feature). Sibling consolidation handoffs (velnor #1064/#1065, jackin #1068–#1070, created
22:08–22:24Z) overlap §E.6's branch-consolidation scope — coordinate before any deletion.

Jackin `jackin-project/jackin`:

| ID | PR | Title | State | Head | Checks @head | Reviews | Future action |
|----|----|-------|-------|------|--------------|---------|---------------|
| JP1013 | #1013 | multi-account feature | MERGED 07:58Z as `9ee50f6a` | `5fb4cb5b` | all pass | Landlock P1 thread disposition UNVERIFIED | T-011: verify thread or record descoping (R-1013) |
| JP1044 | #1044 | migrate Apple CI to generic recipe (title stale: "pin b634efd2"; actual `eed474c4`) | OPEN, MERGEABLE/BLOCKED | `6ff54ce5`, base `df4671e4` (0 behind) | ❌ 3 fail / 42 pass / 38 skip, run 35657319778 | none blocking | re-pin-4 after mold fix → merge |
| JP1065 | #1065 | s2-repromote to 80bc420d | OPEN BLOCKED | `909a9f54` (not fetched locally) | ❌ same 3-fail shape (FFI Apple 40s + gates), run 35658518788 completed failure | none | resolve order vs #1044 (fold or sequence) |
| JP1062 | #1062 | Major-1 repromote | CLOSED unmerged | branch `cicd/repromote-major1-9660c9ff` still on origin | n/a | n/a | superseded by #1065; delete remote branch after resume |
| JP-P | #1030/#1045/#1058/#1060 | proof PRs (do-not-merge) | various | — | — | — | retire per policy after #1044 |

Adjacent merged jackin: #1017 s1-mise-desktop, #1018 phases pin, #1019 change-aware pin,
#1024 swift-packing-liveness, #1041 pin f406baff, #1042 D1 repromote 01bc16b2, #1052 schema-2
wave @4dec6b9e (moved pin BACKWARD off D3), #1054 D3 repromote 7576f40f.

Refresh note: this ledger was refreshed after pushing the `preserve/handoff-1402ca52/*`
branches; no new PRs were created by preservation (branches only, no PRs).

### E.5. Integration map and ordered landing plan — FUTURE EXECUTION ONLY

Map (`worktree → local branch → remote ref → PR → target`):

```
W2 → JB1 migrate/apple-ci-generic → origin (same) → JP1044 → jackin main (after R-MOLD + re-pin-4)
W3 → preserve/handoff-1402ca52/jackin-m1rehearsal@2c57f74b → origin (same) → none → ADJUDICATE vs JP1044
W4 → preserve/handoff-1402ca52/jackin-scratch@0b02a313 → origin (same) → none → ADJUDICATE vs JP1044
W5 → preserve/handoff-1402ca52/velnor-rust-scan@01e3ce81 → origin (same) → none → triage 4-line rust.rs edit
W6 → preserve/handoff-1402ca52/velnor-pin-80bc@473eb7b6 → origin (same) → none → record only (re-derivable)
VB6 preserve/stray-runner-header-0abc3675 → origin (same) → none → REVIEW_SHARED
JB3 integrate/velnor-apple-ci@8fb49688 → preserve/…/integrate-velnor-apple-ci → none → ADJUDICATE vs JP1044
(no wt) fix/apple-mise-tool-closure@256c24bb → origin → VP1054 → velnor main (if needed)
(no wt) #1065 branch@909a9f54 → origin → JP1065 → jackin main (order vs JP1044 TBD)
W1 → VB7 goal-handoff/… → origin → VP-HO (draft) → CLOSE after goal done (never merge as feature)
```

Dependency-ordered landing plan (execute only after explicit resumption):

1. Velnor mold-fix PR (new): source = fresh branch from velnor main; target = velnor main;
   prereq = repro evidence; validation = `hosted_mold_setup_tests` (both emitters) + full
   PR matrix + products release for merge SHA; merge method = squash-merge.
2. Optionally VP1054 if the Apple leg needs Mise-closure (green, CLEAN, base `45ef1ebe`);
   re-validate base hasn't moved; squash-merge. Parallelizable with step 1 (different files
   expected — verify no overlap before concurrent landing).
3. Adjudicate JP1044 vs JP1065 vs JB3/W3/W4 (three formulations + main-line repromote):
   keep ONE migration line; document supersession rationale for the others. Expected overlap:
   same generated tree (`.github/`, `.github-gen/`) — conflicts likely; resolve by regen, not
   hand-merge.
4. Re-pin-4 on JB1 → JP1044 green → merge per Jackin policy. Validation: full 83-check
   matrix incl. `rust-jackin-usage-ffi / Apple`, `swift-package-native`, xcodegen legs,
   `ci-required`, `Control / Required`.
5. Triage W5 rust.rs edit (01e3ce81) and VB6 (0abc3675): integrate, supersede with rationale,
   or reject — record decision.
6. Retire proof PRs JP-P per repo policy; close VP-HO handoff PR after goal completion.

The resuming agent MUST reread all PR review comments, compare live heads with recorded SHAs,
and reconcile intervening changes before landing anything. This checkpoint is NOT landed work.

### E.6. Post-integration local cleanup runbook — FUTURE EXECUTION ONLY

Documented now; execute only after explicit resumption + verified integration. Re-observe each
candidate immediately before any destructive action; if head/contents/owner/processes changed,
stop that deletion and reconcile.

| Resource ID | Exact host/path/ref | Expected HEAD/tip | Final target | Integration proof | Recovery reference | Gates | Proposed action | Status |
|-------------|---------------------|-------------------|--------------|-------------------|--------------------|-------|-----------------|--------|
| W3 | mac-studio `/private/tmp/jackin-m1rehearsal` | `2c57f74b` | JP1044 or recorded supersession | adjudication record | origin `preserve/handoff-1402ca52/jackin-m1rehearsal` | §E.6 gates 1–5 | `git worktree remove` (R2) | PENDING |
| W4 | mac-studio `/private/tmp/jackin-scratch` | `0b02a313` | JP1044 or recorded supersession | adjudication record | origin preserve branch | gates 1–5 | `git worktree remove` (R2) | PENDING |
| W5 | mac-studio `/private/tmp/velnor-gen-c832191f` | `01e3ce81` | velnor main or rejection record | triage record | origin preserve branch | gates 1–5 | `git worktree remove` (R1) | PENDING |
| W6 | mac-studio `/private/tmp/velnor-main-80bc` | `473eb7b6` | record only | n/a (re-derivable) | origin preserve branch | gates 1–5 | `git worktree remove` (R3) | PENDING |
| VB1 | R1 `m4-pin-bump` | `72d0d92b` | velnor main `45ef1ebe` | PR #1062 MERGED | merge commit | gates | `git branch -d` after re-branch | PENDING |
| VB3 | R1 `fix/e0277-include-str` | `90e71de5` | main `ef42b0f9` | PR #1017 MERGED | merge commit | gates | `git branch -d` | PENDING |
| VB4 | R1 `fix/unit-platform-split` | `d4b0570f` | main + salvaged commit | PR #1025 + salvage record | merge commit + salvage ref | salvage first | `git branch -d` | PENDING |
| VB5 | R1 `integrate/apple-ci-s2` + remote | `326fd414` | main + scaleset salvage | PR #985 + salvage record | merge commit + salvage ref | salvage first | delete local + remote | PENDING |
| JB3 | R2 `integrate/velnor-apple-ci` | `8fb49688` | JP1044 or supersession | adjudication record | origin preserve branch | gates | `git branch -D` only w/ coverage proof | PENDING |
| W9/W10 | review/archaeology wts | `96b835d9`/`4dec6b9e` | n/a | n/a | PR #1050 / merge #1041 | no-use check | `git worktree remove` | PENDING |
| R3/R4 tail | rollout/* [gone] etc. | various (recorded) | merged mains | per-branch verification | merge commits | per-branch | guarded `-d` only | PENDING |
| REM-VB5 | remote `tailrocks/velnor:integrate/apple-ci-s2` | expected `326fd414` (re-observe) | velnor main | PR #985 MERGED + scaleset salvage record | merge commit + salvage ref | gates 1–5 + no other-goal use | `git push origin --delete integrate/apple-ci-s2` | PENDING |
| REM-JC | remote `jackin-project/jackin:cicd/repromote-major1-9660c9ff` | expected `0d369a85` (re-observe) | superseded by JP1065 | JP1062 CLOSED-unmerged + JP1065 disposition | JP1065 ref | gates 1–5 + no other-goal use | `git push origin --delete cicd/repromote-major1-9660c9ff` | PENDING |

Required gates (ALL must hold): (1) every required change verified in target or explicitly
superseded/rejected with rationale + recovery retained; (2) all commits/edits/stashes/nested
repos accounted for, no unpreserved work; (3) no active agent/session/process/other-goal/
stacked-PR dependency; (4) post-integration validation passed at relevant revision; handoff +
recovery refs durable outside candidate; (5) exact repo/host/common-dir/path/ref/expected-tip
verified. Never bulk/force/wildcard delete. Never `git worktree remove --force` on dirty
work. Prefer guarded `git branch -d`; narrowly-scoped exception after squash merge only with
independent coverage proof. Local cleanup never deletes remote branches except the two
explicitly authorized stale ones (VB5's remote, `cicd/repromote-major1-9660c9ff`) after
dependency + recovery checks. After cleanup: re-enumerate worktrees/branches, reconcile with
ledger, write durable receipt.

KEEP exclusions (never cleanup candidates): W1, W2 (primary checkouts), VB2/JB2 (local
mains), VB7 (until goal done), JB1 (until #1044 merged), JB4 (unrelated), W7/W8 (unrelated),
VB6 (until REVIEW_SHARED resolved), W11/W12 clones (shared mirrors).

## F. Decisions, findings, assumptions, and rejected approaches

Decisions (with rationale):

- D1 Fix generators upstream, never Jackin workarounds (goal-amended constraint). Applied to
  defect C (migfix3) and now mold (R-MOLD). Rationale: generated files carry "do not
  hand-edit"; Jackin has zero `mold|linker` config surface.
- D2 Merge main, never rebase; squash-merge small DCO PRs. Applied to all 13 merged Velnor
  PRs + M4.
- D3 Re-pin-3 target = velnor main HEAD `eed474c4` (had published products) rather than
  migfix3 `155c6088` directly. Rationale: pin to newest shippable main.
- D4 Correct mold fix = skip mold on macOS, NOT mapping `arm64` into the case arms.
  Rationale: mold is Linux/ELF-only (auditor finding; re-verify from mold docs on resume).
- D5 Preserve-first pause: all dirty/local-only goal work pushed to `preserve/*` remote refs
  before writing this doc. Rationale: /tmp volatility + local-only risk.

Findings (with sources):

- F1 Defect C fixed: `install_args: … cargo-binstall cargo:sccache`, mise 5/5 incl. boltffi
  0.30.1 in 227s (job 106524303464 log).
- F2 Mold block pre-existing + newly unmasked: byte-identical at old pin `6f025e5a`; was
  masked by defect C (job died at mise install in ~46s).
- F3 #1065 reproduces the same Apple FFI failure at a different pin (80bc420d) → generator-side
  defect, not branch-specific.
- F4 Velnor main green at `45ef1ebe` (products + CI/Main success); carried failures at
  `eed474c4`/`c674f5bb` historical.
- F5 No approving reviews on any goal PR (`reviewDecision` empty everywhere) — process gap.
- F6 Local velnor `main` (59396040, 98 behind) and jackin `main` (fce94cea, pre-#1013 era)
  are severely stale; always use `origin/main`.

Rejected approaches:

- R1 Mapping `arm64` in the mold case statement — rejected (mold can't link Mach-O).
- R2 Hand-editing Jackin's generated `ci-unit-rust.yml` — rejected (violates D1 + file header).
- R3 Merging #1044 with red Apple leg — rejected (BLOCKED state enforced).
- R4 Re-pin-4 before mold fix — rejected (would reproduce the same failure).

Assumptions needing validation: A1 mold Linux-only (recheck docs); A2 VP1054 independence
from Apple leg; A3 JP1044-vs-JP1065 fold direction; A4 Preview @ `45ef1ebe` green (peer tree
evidence only).

## G. Verification evidence and known failures

Conventions: PASS / FAIL / INTERRUPTED / NOT RUN / STALE. Commands run by auditors/workers
(read-only unless noted); working dirs R1/R2 unless noted.

| Check | Command / source | Rev tested | Result | Evidence |
|-------|------------------|------------|--------|----------|
| #1061 merge + checks | `gh pr view 1061`, `gh pr checks 1061` | `84877bf5` | PASS | MERGED 21:03Z as `155c6088`; 12 pass 12 skip 0 fail |
| migfix3 products | velnor run 35654843370 | `155c6088` | PASS | all jobs success incl. Publish; 3 runtime artifacts |
| #1062 merge + checks | `gh pr view/checks 1062` | `72d0d92b` | PASS | MERGED 21:31Z as `45ef1ebe`; 23 pass 1 skip |
| main CI @ HEAD | velnor CI/Main + products | `45ef1ebe` | PASS | products + CI/Main success (one flake retry) |
| Preview @ `45ef1ebe` | — | `45ef1ebe` | NOT RUN | peer Preview on identical tree green (unverified claim) |
| re-pin-3 push state | `git log`, `gh pr view 1044` | `6ff54ce5` | PASS | head == local == remote; tree clean |
| pin products @ `eed474c4` | velnor run 35654862115 + release `velnor-workflow-runtime-v1-ec8123d55e2f34e2` | `eed474c4` | PASS | success; manifest + 3 binaries live; macOS digest matches |
| defect C fix | job 106524303464 log | `6ff54ce5` | PASS | mise 5/5 tools, boltffi 0.30.1, 227s |
| #1044 full matrix | jackin run 35657319778 (83 checks) | `6ff54ce5` | FAIL (3) | `rust-jackin-usage-ffi / Apple` 4m40s (root cause) + `ci-required` + `Control / Required` (consequential); 42 pass, 38 skipping |
| Policy @ #1044 | run 35657315637 | `6ff54ce5` | PASS | success |
| Prior Policy fail | run 35656343929 | `8c46b0c7` | STALE (fixed) | fixed by `c02a37d9`, superseded by 35656783056 success |
| #1065 Apple leg | run 35658518788 (completed `failure`; was in-progress at handoff) | `909a9f54` | FAIL | same 3-fail shape (FFI Apple 40s + 2 aggregate gates) — independent cross-pin repro, strengthens F3 |
| Local gates (mold) | NOT RUN | n/a | NOT RUN | no local fix attempted (pause before fix) |
| Benchmarks | NOT RUN | n/a | NOT RUN | blocked on R-MIG |

Known failure detail (mold): `Set up mold 2.42.0` prints `unsupported mold architecture:
arm64`, exit 1 (~21:42:16Z, job 106524303464). Generated `ci-unit-rust.yml` emits mold setup
unconditionally for github-hosted Rust jobs; `case "$(uname -m)"` matches only
`x86_64|aarch64`; Darwin reports `arm64`. Block appears twice in the file (lines ~367 and
~796 — both jobs affected). Generator sources: `hosted_mold_setup()` in
`crates/velnor-workflow/src/s2/mod.rs:6183` and `src/lib.rs:5931`; emitted at
`s2/primitives/ir.rs:9758` / `primitives/ir.rs:7239`, gated only on hosted-lane +
`ToolRequirement::Mold` (inserted s2:6983,9341 / s1:4165,6957; `linker:` uses s2:3987,4203,
4256 / s1:1991,2191,2244). Tests: `hosted_mold_setup_tests` in both files. Line numbers are
auditor-observed at `eed474c4`-era tree — re-verify on resume (nit: worker cited `:355`,
actual block at `:367-370`).

No full validation campaign was run during handoff (per instruction); only bounded read-only
checks to verify preservation/document accuracy.

## H. Ordered remaining-work plan

DO NOT execute during pause. Stable task IDs `T-001…T-011` (= legacy H1…H10 + T-011);
every task maps to §M requirement IDs. Each item: outcome, starting state, repo/branch/files,
next action, deps/parallelism, pitfalls, validation/completion, blockers.

1. **T-001 (H1) — Velnor mold-fix PR (FIRST).** Reqs: G-020 (hosted macOS green), G-030 (no
   invented behavior), R-MOLD. Start: mold failure repro'd at jackin job 106524303464; no fix
   branch exists (verified). Repo/branch: velnor, FRESH branch from `origin/main` (re-branch
   W1; do not reuse VB1/VB7). Files: `crates/velnor-workflow/src/{s2/mod.rs,
   s2/primitives/ir.rs,lib.rs,primitives/ir.rs}` + both `hosted_mold_setup_tests` (re-verify
   line numbers: recorded s2/mod.rs:6183, src/lib.rs:5931 at `eed474c4`-era tree). Action:
   skip mold cache/setup on macOS runners (D4 — NOT arm64 case-mapping); decide
   `ToolRequirement::Mold` insertion vs `linker:` neutralization vs both from code evidence
   (open hypothesis §C-a). Deps: none. Pitfalls: R1 (arm64 mapping), R2 (Jackin hand-edit);
   cover BOTH s1+s2 emitters + both test modules. Validation: `cargo test -p velnor-workflow
   hosted_mold_setup` green (verify package/test path in repo first); full PR matrix green;
   products run success + `velnor-workflow-runtime-v1-*` release live for merge SHA; prefer a
   merge SHA with green CI/Main. Complete when: merged + release live. Blocker: none known.
2. **T-002 (H2) — VP1054 disposition.** Reqs: G-012 (Mise closure), R-PROD. Start: velnor
   #1054 OPEN MERGEABLE/CLEAN @`256c24bb`, base `45ef1ebe`, 8 pass/14 skip. Action: land
   (squash per session practice, or repo policy) or defer with written rationale in
   `plans/apple-ci-progress.md`. Deps: none; parallel with T-001 after verifying no file
   overlap (`git diff --stat` both branches). Validation: PR checks green on current base.
3. **T-003 (H3) — #1044/#1065/JB3 adjudication.** Reqs: G-040 (one migration line), R-MIG.
   Start: JP1044 @`6ff54ce5` (pin `eed474c4`), JP1065 @`909a9f54` (pin 80bc420d), JB3/W3/W4
   pre-M1 formulations (693-line project.toml diff). Action: choose ONE line; write rationale
   + supersession record in `plans/apple-ci-progress.md` (velnor) AND reference from JP1044
   or JP1065 (whichever survives). Deps: after T-001 (need green-capable pin). Pitfall:
   same generated tree conflicts — resolve by regen, never hand-merge. Validation: rationale
   committed + losing lines' PRs/branches dispositioned per §E.5 step 3.
4. **T-004 (H4) — Re-pin-4 + merge #1044.** Reqs: G-040…G-044 (migration + contract preserved),
   R-MIG. Start: JB1 `migrate/apple-ci-generic` @`6ff54ce5` (W2, clean, = origin). Action:
   forward-merge `origin/main` only if past `df4671e4`; bump pin to T-001 merge SHA (must
   have products release); regen; byte-stable `--check`; push; watch matrix. Deps: T-001
   (+T-003). Validation: 83-check matrix green incl. `rust-jackin-usage-ffi / Apple`,
   `swift-package-native`, xcodegen legs, `ci-required`, `Control / Required`; regen
   `--check` clean; actionlint clean; merge per Jackin policy; delete remote branch after.
5. **T-005 (H5) — 5c hosted evidence.** Reqs: G-050 (hosted proof), R-5C. Start: main CI green
   @`45ef1ebe`; Preview @ merge SHA unobserved (A4). Action: observe the velnor `Preview`
   workflow run for the current `origin/main` HEAD (and T-001's merge HEAD); record run URL
   + per-arch artifact listing + identity-check outcome in `plans/apple-ci-progress.md`.
   Deps: T-001/T-004. Validation: recorded run URLs show success, or failures filed as new
   defects (not silently dropped).
6. **T-006 (H6) — Benchmark matrix (§L §12.2).** Reqs: G-060…G-062 (11 scenarios, controls,
   reporting), R-BENCH. Start: NOT_STARTED; contract = §B scenario list (11 scenarios) +
   reporting set. Action: run matrix on GitHub-hosted macOS runners from JB1-line post-T-004
   state (same candidate per comparison); record in a committed benchmark report
   (`plans/apple-ci-benchmarks.md` — create it; inherit D19 doc conventions). Deps: T-004.
   Pitfalls: no invented p95/speedups; same-contract comparison; new app coverage reported
   separately. Validation: ≥3 reps per cold/warm case, median/range, all §B reporting fields,
   run links.
7. **T-007 (H7) — Native macOS provider.** Reqs: G-070 (proved-by-execution or demonstrated
   blocker), R-PROV. Start: NOT_STARTED; design space in §L §8 + `crates/velnor-runner/src/
   execution` + provider lifecycle. Action (bounded investigation first): inventory actual
   runner/executor code paths (`execution/backend.rs`, provider lifecycle); decide route A
   (manage official self-hosted runner on macOS) vs B (Velnor-native macOS process backend)
   with written criteria; implement smallest verifiable increment on a velnor branch; run the
   §B acceptance-7 contract on a real Mac. Deps: T-004 (parallel with T-006). Pitfalls:
   labels/unit-tests ≠ proof; Debian-Apple placement must fail, not skip-green. Validation:
   executed runs green on real Mac, or external blocker demonstrated with capability recorded
   unsupported in code + docs.
8. **T-008 (H8) — W5/VB6 triage + proof-PR retirement.** Reqs: G-080 (every line
   dispositioned), R-CLEAN (part). Start: W5 `01e3ce81` (+4/−1 rust.rs), VB6 `0abc3675`
   (UNKNOWN), JP-P proofs. Action: integrate/supersede-with-rationale/reject each; update
   §E dispositions in THIS handoff file (it stays the canonical inventory until T-010) +
   `plans/apple-ci-progress.md`. Deps: T-004. Validation: every preserved line has a final
   disposition entry.
9. **T-009 (H9) — §E.5 integration + §E.6 cleanup.** Reqs: H-010…H-013 (integration +
   cleanup), R-CLEAN. Start: §E.5 map + §E.6 table (13 rows). Action: execute landing plan
   in dependency order; then eligible cleanup rows with LIVE re-observation per row (tips,
   owners, processes). Deps: T-001…T-008. Pitfalls: sibling consolidation handoffs overlap
   branch scope — coordinate; never bulk/force/wildcard; see gates. Validation:
   re-enumeration receipt committed (location: `plans/` + §E update).
10. **T-010 (H10) — Final docs + close VP-HO.** Reqs: G-090 (deliverables), R-DOC. Start:
    progress doc + handoff. Action: requirement-by-requirement completion audit against §B
    acceptance + §M; update `plans/apple-ci-progress.md`; close VP-HO #1066 (never merge).
    Deps: T-009. Validation: audit table complete; PR closed.
11. **T-011 — Landlock P1 disposition (R-1013).** Reqs: G-100 (acceptance-10 review gate).
    Start: jackin #1013 MERGED `9ee50f6a`; thread discussion_r4057673954 state UNKNOWN.
    Action: `gh pr view 1013 --repo jackin-project/jackin --comments` + review-threads API;
    if resolved pre-merge with evidence → record resolution (commit/review link) in progress
    doc; if NOT → either fix-forward (structural Landlock fix + verify) or record explicit
    descoping authority (user decision required at resume — this is the ONE item that may
    need user input; do not self-descope). Deps: none; parallel with T-001. Validation:
    resolution evidence linked or descoping recorded.

FIRST actionable resumption task: T-001 (mold repro + fix PR). No other task may precede it
except `git fetch` + head re-verification (§J step 3). T-002 and T-011 may run in parallel
with T-001 (different repos/branches; verify no overlap).

## I. Environment and operational recovery

- Platforms: Apple Silicon Mac (operator host `mac-studio`); GitHub-hosted runners
  (macos-26-arm64 etc.); standard Debian Velnor lanes. Tool versions: repo-pinned
  (`mise.toml`, `rust-toolchain.toml`, `Cargo.lock`); Velnor runtime via release
  `velnor-workflow-runtime-v1-*` per pin.
- Working dirs: R1 `/Users/donbeave/Projects/github/velnor`, R2 `.../jackin`.
  Authenticated `gh` CLI required (API + `gh run view --log`).
- Setup: `git fetch --prune` in R1/R2 (+R3/R4 before touching tail branches);
  fast-forward local mains; `gh pr checks` to re-verify. Do NOT trust recorded SHAs without
  re-observation — mains may have moved during pause.
- External operations performed: 13 velnor merges + 1 jackin merge (all durable remote
  state); preservation pushes (§E.2/§E.3); CI runs triggered by those pushes (observed, not
  repaired during pause). Repeating pushes is safe (new branches); re-running CI is safe.
  No deployments, migrations, releases (other than automatic products), or infra changes.
- Generated artifacts (reproducible): `.github/`, `.github-gen/` outputs, Preview artifacts,
  runtime releases. Irreplaceable: none — everything goal-owned is in git remotes after §E
  preservation. Session logs are convenience-only.
- Rollback: merged PRs revert via normal revert PRs if needed; open PRs simply unmerged.
  No rollback executed or needed at pause.

## J. Fresh-agent resume runbook

Retrieval: this file lives on velnor branch `goal-handoff/generic-macos-swift-ci--1402ca52`
(DRAFT PR VP-HO — URL in §A after publication), NOT necessarily on velnor `main`. Fetch it:

~~~sh
cd /Users/donbeave/Projects/github/velnor
git fetch origin goal-handoff/generic-macos-swift-ci--1402ca52
git show origin/goal-handoff/generic-macos-swift-ci--1402ca52:docs/goal-handoffs/generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52.md | head -5
~~~

Exact resume command:

```text
/goal Read and resume docs/goal-handoffs/generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52.md
```

The future agent MUST:

1. Read this document completely + repo `AGENTS.md` files + `plans/apple-ci-progress.md`.
2. Recover checkpoints into exclusive branches/worktrees without overwriting unrelated work
   (primary checkouts W1/W2 are the resume homes; re-branch W1 off fresh `origin/main`).
3. Compare live repo/PR/CI state with recorded SHAs (`git ls-remote`, `gh pr view/checks`);
   detect intervening changes; never trust stale IDs or blindly rerun side-effecting cmds.
4. Reconcile completed work without discarding progress, rewriting shared history, or
   restarting finished tasks.
5. Resume the ORIGINAL goal at T-001 (mold fix), not the handoff task. Use subagents with clear
   ownership; preserve constraints (DCO signoff, merge-not-rebase, small PRs, delegation +
   independent verification); verify real acceptance criteria.
6. Maintain the goal progress record; scoped checkpoint commits during resumed work.
7. After goal validation: execute §E.5 integration, verify combined target state, then ONLY
   eligible §E.6 cleanup with live re-observation before each action; final disposition +
   receipt for every inventoried resource.

Interpretation rule: the pause lasts until the user requests resumption. A later `/goal Read
and resume <this-file>` authorizes ORIGINAL-goal continuation — NOT another pause, handoff
regeneration, or recursive handoff PR.

## K. Blockers, omissions, and independent review

Blockers/omissions (honest inventory):

- K1 No runtime goal-pause control exists (tooling limitation) — goal record left active;
  freeze enforced by coordinator. Remedy: none available; resuming user steer overrides.
- K2 RESOLVED BY AUDIT: full 101,314-char objective is now embedded verbatim in §L
  (extracted from session log line 19, boundaries verified). No session access needed.
- K3 R3/R4 `rollout/*` long tail recorded per-branch in `/tmp/audit-wt.txt` (local-only
  convenience) + canonical branches retained in persistent clones — NOT individually
  enumerated in this doc (volume). Remedy: resuming agent re-enumerates via
  `git for-each-ref` in R3/R4 before touching them.
- K4 Jackin #1065 head `909a9f54` never fetched locally — needs `git fetch` on resume.
- K5 Mold-fix line numbers are auditor-observed, must be re-verified (tree moved since).
- K6 Preview @ `45ef1ebe` unobserved; `rust-velnor-workflow` flake (1 retry on #1062)
  unattributed — could recur.
- K7 Zero approving reviews across all goal PRs — process debt for resumed work.
- Otherwise NONE: no inaccessible worktrees, no unpreserved goal-owned Git work, no
  unstopped workers, no failed preservation/publication ops (all pushes succeeded).

Independent review: performed by `main/review-handoff/65` (read-only; report
`/tmp/review.txt`, local-only convenience). Initial verdict NOT-READY; all findings resolved:

- E1 (branch/PR unpublished at review time) — resolved by publication (branch pushed, PR #1066 created).
- E2/E3 (W11/W12 rows wrong) — ACCEPTED; rows rewritten from direct `git worktree list`
  evidence. Note: reviewer also claimed `velnor-pin-be61acbb` unregistered — refuted; it IS
  registered in R3 (detached `be61acbb` + 3 dirt lines).
- E4 (W6 "unregistered") — REFUTED with direct evidence: W6 IS in R3's `worktree list`
  (`473eb7b6 [preserve/handoff-1402ca52/velnor-pin-80bc]`); reviewer evidently checked R1's
  registry. `git worktree remove` (R3) stands.
- E5 (behind count) — ACCEPTED; corrected to 98 (verified `rev-list --count`).
- Reviewer-report §2 missing R3 tail — ACCEPTED; added W13 group enumeration (21 worktrees, all observed).
- Reviewer-report §2 adjacent PRs — ACCEPTED; added #962/#978/#1007 triage lines (all verified OPEN).
- Cleanup-gate structure — reviewer confirmed complete; added REM-VB5/REM-JC rows with
  expected tips for the two authorized remote deletions.
- Secrets — reviewer scan clean; coordinator grep clean.
- Fresh-agent recoverability — reviewer: main line yes (goal, H1 first action, resume homes,
  remote checkpoints all accurate); friction items (E1, R3/R4) fixed above.

Corrections made: W11, W12, W13 (new), VB2 count, F6, E.1 counts, E.4 triage lines, E.6
remote rows, VB7/§A/§J publication states. Unresolved: none — READY after publication verification.

## Appendix L. Original goal-defining user prompt (VERBATIM, 101,314 chars)

Source: session log line 19 `model_user_messages[0]`, `<objective>` envelope content only (control wrapper excluded). reproduced byte-for-byte; do not edit.

---BEGIN VERBATIM OBJECTIVE---
# Generic macOS and Swift CI for Velnor: Jackin audit, implementation report, and /goal

Research snapshot: **20 September 2026 UTC**.

**Recommendation:** extend Velnor's existing typed scanner, product graph, planner, and runtime so it can discover SwiftPM packages, Xcode/XcodeGen applications, and their native-library producers. Give each product explicit inputs, toolchain requirements, outputs, and validation rules. Generate GitHub Actions and supported Velnor execution from that model. Remove Jackin's opaque CI task workaround after this model preserves its complete application contract.

The cited job spent **14m13.696s of its 16m25s active duration preparing the native XCFramework dependency**. Most of that interval falls within the target-build and host metadata/binding stages, both of which invoke Rust compilation. The native job had no generated build-cache configuration. A previous attempt was killed by Velnor's 600-second silence watchdog. These are the first problems the implementation must resolve. [Successful job](https://github.com/jackin-project/jackin/actions/runs/35535696169/job/106148084283), [failed first attempt](https://github.com/jackin-project/jackin/actions/runs/35535696169/job/106145813603).

This document contains source-backed findings, a proposed architecture, concrete changes in both repositories, validation requirements, and a self-contained implementation **/goal** at the end. The investigation inspected repository source, complete job logs, API metadata, Apple/Swift/GitHub guidance, and ten representative public project CI surfaces. **No production code was changed, PR merged, macOS build executed, or optimization benchmark produced during this report.** Proposed types, configuration fields, and behavior are explicitly proposals.

## 1. Evidence identity and interpretation

| Item | Audited identity |
| --- | --- |
| Jackin PR | [#1013](https://github.com/jackin-project/jackin/pull/1013), branch integrate/pr1002-multi-account |
| PR head at inspection | 997c18fe595f044a46146bf3365dccdc0838dbab |
| PR base at inspection | fce94cea8a15de0c2db3bb4ff880d741baf5c00a |
| Requested run | 35535696169 |
| Requested job / attempt | 106148084283 / attempt 2 |
| Run head SHA | 6713d01445d8454e30419e2535dc92e4f82956ed |
| Actual merge SHA checked out by that job | ad70b92f8c5633326c41576b044618cddb80def7 |
| Workflow policy revision used by that run | 4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d |
| Velnor main audited for architecture | 5939604042ae5191b1b6d742e19e0db2c163ea1d |
| BoltFFI version used by Jackin | 0.30.1; upstream commit 2e6320a6d92cb591d22b908477f3a47da7ebc9bc |
| Hosted runner | macos-26; actual image macos-26-arm64, 20260907.0351.1 |
| Observed runtime | macOS 26.6.2; Actions runner 2.337.0; Rust 1.97.1, aarch64-apple-darwin |
| Existing native distribution contract | arm64 only, minimum macOS 26.0 |
| Declared shipping Xcode policy | Xcode 26.6 in native project/README; resolve and verify the actual installed Xcode separately |

The cited **job succeeded**, while its enclosing run was **cancelled** when inspected. The PR had advanced beyond the source tested in that job. These are different facts. The desktop orchestration source at the executed merge and inspected PR head was byte-identical, so it can explain the measured chain. Before implementing or merging, reacquire live refs and checks; historical success does not verify the current candidate. [Run metadata](https://api.github.com/repos/jackin-project/jackin/actions/runs/35535696169), [Jackin desktop source](https://github.com/jackin-project/jackin/blob/ad70b92f8c5633326c41576b044618cddb80def7/crates/jackin-xtask/src/desktop.rs), [native project](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/native/project.yml).

PR #1013 was already large: 157 commits and 433 changed files. One unresolved, non-outdated P1 review thread concerned Landlock permissions for workspace mounts and auxiliary worktree Git paths. Its existence is verified; the underlying security implementation was not independently validated by this CI research. Resolve and verify that finding before merging. The PR description also claimed generated GitHub files were unchanged, while the changed-file inventory showed otherwise. Do not attribute the whole pre-existing native CI architecture to this PR. [P1 review](https://github.com/jackin-project/jackin/pull/1013#discussion_r4057673954), [PR source and discussion](https://github.com/jackin-project/jackin/pull/1013).

## 2. What the slow job actually does

### 2.1 Measured timings

| Interval | Duration | Meaning |
| --- | ---: | --- |
| Job queue | 6s | Job created to started; excludes upstream planning and the earlier attempt |
| Active job | **985s / 16m25s** | GitHub job started to completed |
| Velnor timing-marker total | 976s | Excludes some Actions setup/teardown |
| Mise tool setup | 13s | Tool cache hit; setup still validates/installs Rust state |
| Unit checks | **948s / 15m48s** | The opaque native Swift CI command |
| cargo xtask start to native-pack start | 36.038s | Automation startup/build and preflight; not proven pure compilation time |
| BoltFFI Apple-target stage | **393.326s / 6m33s** | Native Rust build stage |
| BoltFFI binding-generation stage | **408.978s / 6m49s** | Includes another Cargo compilation for metadata |
| XCFramework assembly stage | 13.849s | Framework container creation |
| Complete outer XCFramework command | **853.696s / 14m13.696s** | 90.1% of checks; 86.7% of active job time |
| Swift main build | 53.16s | Build tool's reported duration |
| Swift test build | 21.62s | Build tool's reported duration; actual test execution is additional |

Nested rows overlap their parent interval and must not be added to it. Swift tool durations are not complete job-step durations. Log boundaries identify stage elapsed time; hidden subprocess output prevents a reliable split into network, per-crate compilation, optimization, and linking. [Complete job](https://github.com/jackin-project/jackin/actions/runs/35535696169/job/106148084283), [attempt-specific job metadata](https://api.github.com/repos/jackin-project/jackin/actions/runs/35535696169/attempts/2/jobs?per_page=100&amp;page=1).

Two nearby successful native jobs took **14m04s** and **17m28s**, with the same absent native-unit cache configuration. They used different PR revisions and are observational comparisons, not a controlled warm/cold benchmark. No defensible speedup multiplier or absolute sub-five-minute result follows from these observations. [14m04s job](https://github.com/jackin-project/jackin/actions/runs/35535429853/job/106143540413), [17m28s job](https://github.com/jackin-project/jackin/actions/runs/35531278756/job/106134506970).

### 2.2 Configuration and command chain

Jackin's source configuration is **.github-gen/velnor-workflow.toml**. It excludes native/Package.swift from scanning, then adds a synthetic Swift unit with a custom ci_tasks entry, four mise tools, an XCFramework capability, and a dependency on the Rust FFI unit. That declared unit has no cache policy. Generated project configuration and workflows consequently supply empty cache paths and key files. The PR caller waits on the plan job; the Rust dependency does not transport an Apple library into the Swift job. [Generator configuration](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/.github-gen/velnor-workflow.toml), [generated unit](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/.github/ci/project.toml), [PR caller](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/.github/workflows/ci-pr.yml).

The effective chain is:

~~~text
velnor-workflow run
  → mise run swift-package-native-ci
    → mise run desktop-xcframework
      → cargo xtask desktop xcframework
        → cargo run --quiet --package jackin-xtask
          → boltffi ... --profile desktop-release pack apple
            → explicit Apple-target Rust build
            → host Rust metadata build and Swift/header generation
            → XCFramework assembly and verification
    → swift build
    → swift test --parallel
~~~

The xtask deletes the previous framework and ZIP to avoid stale slices, regenerates Swift into the committed source tree, then checks the module map, plist, static archive count, and arm64 architecture. Those checks have a correctness purpose and must survive genericization. Existence-only caching around this command would be unsafe. [Mise tasks](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/mise.toml), [Cargo alias](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/.cargo/config.toml), [desktop implementation](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/crates/jackin-xtask/src/desktop.rs).

### 2.3 Why binding generation costs almost seven minutes

Pinned BoltFFI 0.30.1 performs two distinct compiler-backed operations. Target production passes aarch64-apple-darwin explicitly. Metadata extraction later invokes cargo rustc for the host, using metadata-specific cfg and environment values. Jackin passes the same desktop-release profile into both. That profile inherits release, enables thin LTO, and uses one codegen unit. The contexts differ in target selection, flags, environment, and output directories. [Apple pack implementation](https://github.com/boltffi/boltffi/blob/2e6320a6d92cb591d22b908477f3a47da7ebc9bc/boltffi_cli/src/pack/apple/mod.rs), [generation context](https://github.com/boltffi/boltffi/blob/2e6320a6d92cb591d22b908477f3a47da7ebc9bc/boltffi_cli/src/build/expansion.rs), [metadata build](https://github.com/boltffi/boltffi/blob/2e6320a6d92cb591d22b908477f3a47da7ebc9bc/boltffi_bindgen/src/metadata.rs), [Jackin profiles](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/Cargo.toml).

This establishes substantial repeated work over related Rust inputs. It does **not** establish that the two outputs can be combined into one compile, or that every second of the binding stage was LLVM optimization. Measure Cargo fingerprints and timings before changing profiles or sharing intermediate artifacts.

The current pack operation uses one Cargo argument set for both phases. No independent metadata-profile option was verified. A real adapter/upstream change or validated staged invocation is necessary to give metadata extraction a cheaper profile. The existing no-build option still allows metadata regeneration, while disabling regeneration requires previously generated state. Neither is a safe blanket substitution on a clean checkout.

A useful first implementation can cache a **complete, validated composite native product** while retaining the existing pack semantics. It should then expose and improve the target and metadata phases without inventing flags or discarding header/API checks.

### 2.4 The first attempt failed because silence was treated as failure

Attempt 1 ran for 11m44s, then Velnor killed the command after 600 seconds without stdout/stderr. The same merge revision succeeded on retry. The two native attempts consumed **28m09s of active runner time** in total.

BoltFFI suppresses target compiler output unless verbose output is enabled. Its metadata compilation uses buffered Command::output(), so verbose mode alone does not make the entire operation observable. A quiet process is not sufficient evidence of a deadlock. The logs do not prove which compiler task was running at the instant of termination. [Failed attempt](https://github.com/jackin-project/jackin/actions/runs/35535696169/job/106145813603), [BoltFFI stream handling](https://github.com/boltffi/boltffi/blob/2e6320a6d92cb591d22b908477f3a47da7ebc9bc/boltffi_cli/src/build.rs), [buffered metadata subprocess](https://github.com/boltffi/boltffi/blob/2e6320a6d92cb591d22b908477f3a47da7ebc9bc/boltffi_bindgen/src/metadata.rs).

Velnor's current runtime also permits the converse failure: output bytes keep resetting the timer, so a chatty hang has no effective deadline from this guard. It kills the immediate shell, not a verified complete process tree. Its EOF branch can enter blocking wait while a live process has closed both output streams. These source-level failure paths warrant focused behavioral tests. [Velnor runtime](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/runtime.rs).

**Required fix:** typed phase deadlines, live diagnostic streaming where supported, silence as a diagnostic condition rather than universal proof of failure, bounded process-tree cancellation, and reliable exit-status/result capture. A longer idle timeout or fake heartbeat alone leaves the underlying defect.

### 2.5 Cache and telemetry evidence

| Layer | Observed state | Consequence |
| --- | --- | --- |
| Velnor runtime binary cache | Miss; setup around 10s | Real improvement opportunity, but a small share of this job |
| Mise tool cache | Hit, approximately 47 MB | Cached tool installation does not cache project compilation |
| Native unit build cache | Empty inputs; restore and save skipped | Neither native Rust nor Swift output reuse was configured here |
| sccache | Installed; no observed wrapper/backend/statistics | Installation alone does not establish active compiler caching |
| MBX | Explicitly disabled | Cannot explain this run as an MBX compile-cache miss |
| XCFramework | Recreated by current command | Needs product identity before safe exact reuse |

The historical report nevertheless classified MBX as cold. Current Velnor source has moved toward unknown for absent evidence. Implement explicit disabled, not-run, miss, compatible seed, exact hit, invalid, and saved states per phase/cache layer; do not apply a historical string patch blindly to current main. Buffered Cargo output also makes reported zero-download counters inconclusive. [Executed workflow](https://github.com/jackin-project/jackin/blob/ad70b92f8c5633326c41576b044618cddb80def7/.github/workflows/ci-unit-swift.yml), [current rendering/reporting](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/primitives/ir.rs).

## 3. The missing correctness contract: package versus application

The native Swift package exposes libraries and five harness executables. **It does not contain the JackinDesktop application product.** The real application, app resources, Info.plist, unit tests, UI tests, and schemes are declared by **native/project.yml**, an XcodeGen source specification. A successful Swift package check therefore cannot establish that the application bundles or launches correctly. [Package.swift](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/native/Package.swift), [XcodeGen app specification](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/native/project.yml).

Jackin's fuller desktop-ci contract includes binding drift checks, project generation, formatting, linting, Rust tests, five Swift behavioral harnesses, the app build, counted release Swift tests, and bundle verification. Desktop-merge adds UI tests; desktop-scheduled adds Periphery. The inspected PR's native package job executes a narrower contract. The native README's required-PR description and generated checks must be reconciled. Required release/app failures should be exposed on the merge candidate before main. [Mise contract](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/mise.toml), [native documentation](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/native/README.md), [desktop main workflow](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/.github/workflows/desktop-merge.yml).

Preserve these obligations while removing duplicate construction:

- Generate bindings in staging and compare them with committed files before consumption. Ordinary CI must not silently rewrite stale committed bindings and then test only the rewritten tree.
- Build/test the intended Swift products and execute Jackin's behavioral harnesses.
- Generate and validate the actual application project; test bundle resources, plist values, linkage, minimum OS, and supported architecture.
- Preserve appropriate app launch/UI verification and observed test counts.
- Preserve required optimized-build and release-layout checks before merge; signing credentials remain in protected distribution jobs.
- Share the native product across these consumers when identity matches. Do not run the whole XCFramework pack again inside every top-level desktop task.

Jackin's UI script invokes xcodebuild test for individual test methods with a shared DerivedData directory and deliberate UI serialization. A separate improvement is build-for-testing once followed by compatible test-without-building invocations, retaining isolation and per-test evidence. Do not parallelize UI tests into the same desktop session without validating state independence. This script was not the measured 16m25s job. [UI test script](https://github.com/jackin-project/jackin/blob/997c18fe595f044a46146bf3365dccdc0838dbab/native/Scripts/run-ui-tests.sh).

Test reporting must understand both XCTest and Swift Testing. The cited native log lists 78 XCTest executions plus two Swift Testing tests. The separate prototype log lists eight XCTest executions and a final Swift Testing count of zero. That final line does not mean the prototype ran no tests. Derive expected frameworks and test inventory, rather than requiring every repository to report nonzero tests in both frameworks.

## 4. Primary guidance and public-project comparison

### 4.1 Apple and Swift principles to encode

Apple connects incremental performance and correctness to accurate dependencies and declared script inputs/outputs, and recommends collecting build timings. Represent native generation as a producer whose outputs and inputs are known; otherwise every build repeats it or risks consuming stale output. Optimize the dependency critical path, with concurrency bounded by actual resources. [Incremental builds](https://developer.apple.com/documentation/xcode/improving-the-speed-of-incremental-builds), [Xcode parallelization](https://developer.apple.com/videos/play/wwdc2022/110364/).

Swift test builds and runs tests. Remove a preceding identical swift build only after confirming package, configuration, flags, target, and required product coverage. On a cold run, the compilation still has to happen inside the test command; removing a duplicate invocation does not automatically save the whole measured 53 seconds. Build application products outside that closure explicitly. [Swift library tutorial](https://www.swift.org/getting-started/library-swiftpm/), [SwiftPM test implementation](https://github.com/swiftlang/swift-package-manager/blob/main/Sources/Commands/SwiftTestCommand.swift).

Xcode supports compilation caching through **COMPILATION_CACHE_ENABLE_CACHING** and diagnostic remarks through **COMPILATION_CACHE_ENABLE_DIAGNOSTIC_REMARKS**. These are Xcode build settings, not generic swift build switches. Gate them on the selected toolchain and measure real reuse. Xcode 26-era compiler guidance described cross-machine path and module-cache limitations; that dated evidence requires retesting on newer selected versions. [Build settings](https://developer.apple.com/documentation/xcode/build-settings-reference), [Xcode 26 release notes](https://developer.apple.com/documentation/xcode-release-notes/xcode-26-release-notes), [Swift compiler discussion](https://forums.swift.org/t/about-swift-shared-cache-across-machines/81850).

Keep XCFramework as the initial integration format. Apple supports assembling static archives with the library/headers form of create-xcframework. Jackin already needs only one arm64 slice; the assembly stage was about 14 seconds. Replacing the format would not remove the hundreds of seconds spent building its contents. A direct static archive plus module map/system-library integration is possible, but it requires coordinated SwiftPM and Xcode linkage changes and is not the first required correction. [Apple XCFramework guidance](https://developer.apple.com/documentation/xcode/creating-a-multi-platform-binary-framework-bundle), [SwiftPM target definitions](https://docs.swift.org/package-manager/PackageDescription/PackageDescription.html).

Use stable Cargo timings and locked dependency resolution, including nested adapter-driven Cargo invocations. For application dependencies with committed Package.resolved files, use the pinned Swift/Xcode toolchain's supported locked-resolution behavior and fail unintended lockfile drift. Preserve a library's deliberate dependency-compatibility testing policy instead of forcing every library to use an application lock policy. Target and host build directories have different semantics; profile changes can alter assertions, overflow behavior, optimization, and linkage. Do not invent stable JSON timing flags or assume release and debug artifacts are interchangeable. Do not enable Swift library evolution on internal modules that always build and ship together solely because a Rust library is wrapped in an XCFramework. [Cargo build](https://doc.rust-lang.org/cargo/commands/cargo-build.html), [Cargo build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html), [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html), [Swift library evolution](https://www.swift.org/blog/library-evolution/).

### 4.2 Ten representative CI surfaces

These are source inspections at immutable commits, not executed performance benchmarks. An observed project runner label is not proof that GitHub offers that label generally.

| Project and inspected source | Useful observed practice | What Velnor should avoid copying blindly |
| --- | --- | --- |
| [SwiftNIO](https://github.com/apple/swift-nio/blob/9cb66c6d7a636fef586a93a15827eb247f1c12e9/.github/workflows/macos_tests.yml) | Reusable platform/toolchain policy; separate SwiftPM and Xcode checks; metadata/matrix work on Linux | Apple's private self-hosted pool, broad project-specific matrix, mutable shared refs |
| [Alamofire](https://github.com/Alamofire/Alamofire/blob/bda9ed57d72988a3a2ada33d824583541f86eac6/.github/workflows/ci.yml) | Explicit Xcode/destinations/test plans; preserved results; several platform checks | Unconditional clean test; treating compile-only rows as executed tests |
| [IINA](https://github.com/iina/iina/blob/5cfdfb75277e1a9eb9c8960208de687dad687bf2/.github/workflows/ci.yml) | Prebuilt native dependencies precede Xcode consumption; explicit distribution architecture intent | Mutable binary download indexes/URLs; automatic packaging on every validation path |
| [Rectangle](https://github.com/rxhanson/Rectangle/blob/12a9bc79f99abeb86297da3d7436b4489f920fa2/.github/workflows/build.yml) | App archive/export/bundle pipeline is explicit | Every push/PR packages a DMG; no explicit cache evidence; ad-hoc signing is not notarization |
| [Ghostty](https://github.com/ghostty-org/ghostty/blob/27e8b3fa85d9cf8c7cd5ae2ced348bcb0a4fba9c/.github/workflows/test.yml) | Native Zig-core framework feeds Swift/Xcode; [release graph](https://github.com/ghostty-org/ghostty/blob/27e8b3fa85d9cf8c7cd5ae2ced348bcb0a4fba9c/.github/workflows/release-tag.yml) separates optimization and distribution | Zig/Nix-specific commands, Namespace storage, or observed CAS flags as universal SwiftPM defaults |
| [Zed](https://github.com/zed-industries/zed/blob/395def7a067b32ed598fcb3f048969ada9b54942/.github/workflows/run_tests.yml) | Rust generates workflows; target/dependency selection; compiler cache statistics; separate bundling | Provider-specific runner/cache assumptions; skipping macOS checks in merge-group execution |
| [Nuke](https://github.com/kean/Nuke/blob/c96b1fb5fd1e3563bef2a4fb64a0459a653bd4a8/.github/workflows/ci.yml) | Shared local/CI matrix, platform job grouping, Linux lint, exact Xcode, preserved logs/results | Literal job count, retries, simulator diagnostics or parallelism exceptions without equivalent evidence |
| [Composable Architecture](https://github.com/pointfreeco/swift-composable-architecture/blob/377da4061db10d26337a71bb279c506bb951f50f/.github/workflows/ci.yml) | Scoped DerivedData restoration; app/examples coverage | Timestamp/inode workarounds as a supported universal cache guarantee; incomplete compiler identity |
| [Penny / Vapor CI](https://github.com/vapor/penny-bot/blob/ebe9963e31156739b167a322657b9ddeadf00137/.github/workflows/test.yml) | Thin consumer delegates common Swift policy to a [shared workflow](https://github.com/vapor/ci/blob/bb142946c09c8545b4cca68adadbfa577a95e074/.github/workflows/run-unit-tests.yml) | Mutable main references; literal latest-stable in cache identities; missing flag/config dimensions |
| [Ice](https://github.com/jordanbaird/Ice/blob/11edd39115f3f43a83ae114b5348df6a0e1741cf/.github/workflows/lint.yml) | Linux SwiftLint placement | Inspected workflow directory establishes lint only; it supplies no app-build benchmark |

Zed is particularly relevant to Velnor's implementation language: its [workflow source is typed Rust](https://github.com/zed-industries/zed/blob/395def7a067b32ed598fcb3f048969ada9b54942/tooling/xtask/src/tasks/workflows/run_tests.rs), with separate [platform/runner types](https://github.com/zed-industries/zed/blob/395def7a067b32ed598fcb3f048969ada9b54942/tooling/xtask/src/tasks/workflows/runners.rs). Nuke's [shared script](https://github.com/kean/Nuke/blob/c96b1fb5fd1e3563bef2a4fb64a0459a653bd4a8/.scripts/ci.sh) demonstrates local/CI parity and phase/result diagnostics.

Ghostty and Zed rely on Namespace cache volumes. The inspected [cache action](https://github.com/namespacelabs/nscloud-cache-action/blob/1124a6f3ce44e5cf84cc22111530961f4d2a15f9/action.yml) offers semantic cache kinds and detection, but its [documented prerequisite](https://github.com/namespacelabs/nscloud-cache-action/blob/1124a6f3ce44e5cf84cc22111530961f4d2a15f9/README.md) is an attached provider cache volume. The transferable design is **semantic cache policy with a supported backend**, not copying that action onto an unrelated runner.

## 5. What Velnor already has

| Existing surface | Verified behavior | Structural change |
| --- | --- | --- |
| Schema-2 Swift scanner | Package.swift becomes swift build plus swift test; generic home SwiftPM cache | Discover products, tests, dependency closure, native constraints and project generation |
| Apple classification | String checks for binaryTarget plus xcframework; limited Xcode SDK substring tests | Structured evidence, explicit uncertainty, runtime metadata validation |
| Synthetic unit insertion | Cache only exists if declared | Preserve detected semantics; missing native producer must not require replacing the unit with a command string |
| NamedProduct / Prerequisite | Product name, task, env; adds selection dependency and consumer preparation task | Typed recipe, declared outputs, identity, validation, reuse and transport |
| Prepared-tool handoff | Producer/outcome/ABI/digest validation and staging | Extend shared machinery for source-built products with complete local input identity |
| Provider capabilities | Native macOS only on GitHub hosted; local providers advertise Linux | Capability per executor; real native macOS backend and lifecycle before eligibility |
| Collapsed language jobs | Any Apple member can put the whole Swift group on macOS | Partition by execution environment and trust, not language alone |
| Runtime supervision | 600-second output-silence timer | Deadline/cancellation/process-tree contract independent of pretty output |
| Reporting | Some phase/cache states conflated | Explicit per-phase execution and cache states with applicability |

Sources: [Swift scanner](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/scan/swift.rs), [unit creation](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/mod.rs), [product contract](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/platform.rs), [prepared tools](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/primitives/prepared_tools.rs), [providers](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/provider.rs), [renderer](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/primitives/ir.rs).

Schema 1 and schema 2 remain in active bridge dispatch. Extend the canonical schema-2 direction and reconcile affected duplicate discovery paths; do not add a third independent Apple planner. Follow Velnor's current migration rules for the code actually migrated, without expanding this task into unrelated subsystem rewrites. Repository instructions require generic names, scan-first work, configuration outside generated GitHub files, and deterministic regeneration. [Dispatch](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/dispatch.rs), [crate rules](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/AGENTS.md), [root rules](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/AGENTS.md).

## 6. Proposed generic scanner and build-product model

### 6.1 Two stages of discovery

**Static discovery must remain non-executing.** Read tracked manifests, project/workspace files, schemes, test plans, XcodeGen specs/includes, Cargo manifests, lockfiles, binding configuration and references. Do not execute Package.swift, arbitrary tasks, build scripts, project generators, or plugins during the filesystem scan. Velnor explicitly promises this property. [Scan contract](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/scan/mod.rs).

**Runtime metadata evaluation belongs in a bounded job on a compatible executor.** A command returning JSON may still execute a Swift manifest. Evaluate as ordinary untrusted project code, without signing credentials or trusted cache-write authority. Bind the resulting metadata to source and toolchain identity. Resolve missing binary products before operations that require those artifacts to load the package. Runtime refinement must not silently shrink the required coverage set or accept an unsafe initial executor; unknown static requirements need conservative placement and coverage.

The scanner should recognize these cases:

| Repository shape | Automatic behavior | Boundary requiring explicit facts or diagnostics |
| --- | --- | --- |
| Conventional SwiftPM library with tests | Discover package graph, tests, tool requirements and supported environments | Conditional manifests, native SDK dependencies, ambiguous support |
| SwiftPM executable without tests | Build declared product; do not issue a guaranteed failing test command | Extra behavior checks remain explicit product policy |
| SwiftUI/AppKit package | Require Apple SDK/runtime capability from dependency evidence | Platform clauses alone do not establish exclusivity |
| Committed Xcode project/workspace | Discover shared schemes, actions, test plans, settings and resources | Ambiguous intended schemes/destinations |
| XcodeGen source only | Recognize schema, follow includes, generate once, then inspect/build | Arbitrary project.yml is not automatically XcodeGen |
| Generated project plus source spec | Deduplicate into one project surface | Inconsistent source/output evidence must be reported |
| Local XCFramework plus known BoltFFI producer | Join normalized output/module references; construct product dependency | Multiple matching producers require a small typed mapping |
| Remote binary library or tool artifact bundle | Classify artifact kind, supported targets and integrity requirements | A ZIP URL or binaryTarget token alone is insufficient |
| Unknown generator/native build tool | Preserve unresolved surface and give exact missing facts | Never claim support by guessing a shell recipe |

Package.platforms supplies minimum deployment information, not a complete exclusivity declaration. Swift tools version is not a precise Xcode pin. A missing Apple substring does not establish Linux support. Conditional imports, transitive frameworks, native artifacts, target triples and actual product graph all matter. [SwiftPM manifest semantics](https://docs.swift.org/package-manager/PackageDescription/PackageDescription.html).

Normalize paths, reject repository escapes and include cycles, preserve stable IDs including container/root to avoid duplicate scheme-name collisions, and handle renames/deletions. If uncertainty affects required coverage, choose conservative validation or a precise plan failure. Do not silently skip the surface.

### 6.2 Build products and dependencies

Extend existing NamedProduct and Prerequisite rather than introducing a detached scheduler. Each supported product must record:

- Typed recipe/adapter and version; argument arrays and structured environment.
- Producer package/root and transitive semantic input closure.
- Execution host OS/architecture separately from output target triple, SDK, architecture, deployment floor and build profile.
- Features, cfg values, relevant compiler/link flags and declared environment inputs.
- Toolchain and project-generator identity.
- Expected files, module/API names, content digests, output layout and consumer references.
- Verification requirements and provenance.
- Same-job placement or verified cross-job artifact transport.
- Timing, bounded execution and cancellation policy.

A suitable conceptual graph is:

~~~mermaid
flowchart TD
  R["Rust source and configuration"] --&gt; L["Target static library"]
  R --&gt; M["Host metadata and bindings"]
  M --&gt; H["Headers and Swift sources"]
  L --&gt; X["Validated XCFramework"]
  H --&gt; X
  X --&gt; P["Swift package tests"]
  H --&gt; P
  X --&gt; A["Xcode app and bundle checks"]
  H --&gt; A
  G["XcodeGen specification"] --&gt; A
  A --&gt; U["App runtime and UI checks"]
~~~

The graph expresses dependencies; it does not prescribe one GitHub job per box. Colocate a producer and its consumer when that avoids unnecessary queue/transfer time. Share a verified product across jobs when it has several independent consumers. Parallelize target and metadata builds only after proving their adapter inputs/outputs and scratch/target directories are independent. Cargo locks and limited runner memory can erase naive parallel gains.

A Swift-only source change must leave the native-product input digest unchanged when its native dependency closure is truly unchanged. A transitive Rust dependency, build script, feature, generator config or native toolchain change must invalidate that product. If the complete closure cannot be determined, conservatively include broader workspace inputs. Arbitrary undeclared build-script inputs cannot be safely inferred by a filename scanner; require declarations or rebuild conservatively.

### 6.3 Minimal declarative configuration

The standard manifests should remain authoritative: Cargo.toml, Cargo.lock, boltffi.toml, Package.swift, Package.resolved and native/project.yml. Velnor should infer unambiguous relationships from them. Keep only policy and unresolved facts in .github-gen/velnor-workflow.toml.

The following is **proposed schema**, not currently accepted Velnor syntax. It illustrates an explicit override for an ambiguous project, not boilerplate every repository should copy:

~~~toml
# PROPOSAL: final names must be implemented and validated in schema 2.
[apple.toolchain]
xcode = "26.6"

[apple.validation]
architectures = ["arm64"]
cargo_profile = "ci-native"

[apple.distribution]
architectures = ["arm64"]
cargo_profile = "desktop-release"

[[apple.binary_dependencies]]
consumer_manifest = "native/Package.swift"
consumer_target = "JackinUsageFFI"
producer_manifest = "crates/jackin-usage-ffi/Cargo.toml"
adapter = "boltffi"
adapter_manifest = "crates/jackin-usage-ffi/boltffi.toml"
~~~

For Jackin, the producer mapping should disappear when normalized BoltFFI output paths and Swift/Xcode consumer references identify one producer. The deployment floor comes from the Swift/Xcode project contract and current xtask environment, **not from a nonexistent deployment field in its BoltFFI file**. Conflicting declarations should fail validation rather than silently selecting one.

No ci_tasks string, raw YAML template, repository name switch, or hidden desktop-xcframework alias is part of this model. Mise remains useful for pinned tools and environment setup. Optional local aliases may invoke the same Velnor plan; they must not contain a second build recipe.

Implement actual supported adapters first: SwiftPM, Xcode/XcodeGen, Cargo static-library production and Jackin's verified BoltFFI contract. UniFFI, cbindgen, Tuist and other tools are future adapters only when implemented and tested; listing names in an enum is not support.

### 6.4 Profiles and generated bindings

Introduce a measured native validation profile where appropriate; retain the existing shipping profile unchanged. Compare metadata/binding output and runtime/link behavior across profiles, including cfg, features, assertions and panic/overflow behavior. Any required optimized configuration that could otherwise first fail on main must remain a premerge check for affected changes.

Start with the current one-slice XCFramework format. A composite cached product must include the static archive, headers/module map, generated Swift, and manifest. Validate all parts together. Generate or restore into staging and compare committed bindings on every run, including an exact product-cache hit, before installing any output. A change only to committed generated Swift must fail the drift check rather than be overwritten by cached files. Atomically install verified untracked products; avoid mixing cached headers with new libraries or leaving obsolete slices.

Provide an upstream/adaptor solution for metadata streaming and separate metadata profile if supported. Do not rely on unverified command flags. Do not drop a build phase merely because its name sounds like code generation.

## 7. Cache, precompiled tools, and artifact policy

### 7.1 Separate the layers

| Layer | Suggested contents | Identity and reuse rule |
| --- | --- | --- |
| Tool installation | Velnor runtime, BoltFFI CLI, XcodeGen, Rust components | Exact version, source/release digest, host ABI; validate restored installation |
| Dependency downloads | Cargo registry/git sources; SwiftPM downloads/checkouts | Resolution configuration and lock/source identity; build tool validates content |
| Compiler cache | Supported Rust sccache; supported Xcode compilation cache | Compiler, SDK, target, flags and content; report actual hits and uncacheable work |
| Build intermediates | Scoped Cargo host/target state, SwiftPM .build, Xcode DerivedData | Compatible toolchain, target/profile/features, dependencies, paths and cache schema; always execute build freshness checks |
| Exact native product | Library, headers/module map, bindings, XCFramework, manifest | Complete producer-input digest plus output/provenance validation; may skip only that unchanged producer |
| Test/distribution outputs | xcresult, logs, dSYM, app bundle/package | Current candidate identity, test execution results and distribution policy; never a substitute for required tests |

GitHub cache storage is an acceleration mechanism, not a reliable job-to-job data bus. A real producer dependency uses shared workspace or a declared artifact with a required dependency edge. A cache miss must have a correct build path. Artifact digest mismatch must fail verification even if an underlying download action only warns. [GitHub dependency cache behavior](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching), [sharing job artifacts](https://docs.github.com/en/actions/tutorials/store-and-share-data).

### 7.2 Keys and validation

Use a versioned compatibility fingerprint for compiled state:

~~~text
cache schema + unit/recipe identity
+ host OS and architecture + output target/SDK
+ actual rustc / Swift / Xcode build / SDK identity
+ profile, configuration, features, cfg, compiler/link flags
+ deployment target + dependency locks/resolution policy
+ binding/project-generator identity + native dependency digest
+ relevant path/environment compatibility
~~~

An exact producer key additionally includes its transitive local source/build-script/configuration closure. Do not use only Cargo.lock for a local FFI artifact. Do not hash every unrelated repository file into every source-download cache. Do not drop compiler/SDK/architecture/profile boundaries from restore prefixes.

A product's content key may remain reusable across different commits when all semantic producer inputs match; its provenance separately records the producing commit and trusted execution. Requiring equality with the whole consumer commit would destroy valid Swift-only reuse. Accept another producing revision only under verified trust and exact producer-input equivalence. If a build script embeds the Git SHA or dirty state, those values become genuine producer inputs and must invalidate the key.

A whole-workspace hash is only conservative for local file inputs. It cannot establish a complete identity for arbitrary build scripts or generators that read undeclared environment, external paths, Git state or network responses. Exact producer skipping requires a complete declared or verified input contract, or an execution environment that controls those inputs. If that contract remains unknown, restore compatible intermediate state and run the producer; do not claim exact artifact reuse is safe merely because tracked files match.

Missing optional caches rebuild normally. Corrupt, tampered, wrong-architecture or incomplete products are rejected, with a clean isolated rebuild where permitted; they never silently become a hit. Tests still execute. Persistent Velnor stores need explicit per-job namespaces, locking and trusted promotion so concurrent builds cannot corrupt shared mutable directories.

### 7.3 Precompiled things worth using

Use verified, versioned prebuilt Velnor and binding-generator binaries where available. Build changed local tools once per exact source/toolchain closure using the existing prepared-tool mechanism. Publish the native runtime for every supported execution ABI before bumping consumer pins.

Mise already hit in the cited job, so downloading its tools was not the dominant delay. Validate requested rustup targets/components after tool-cache restoration; a restored tool-manager symlink is not proof that the corresponding toolchain state exists. [Mise action definition](https://github.com/jdx/mise-action/blob/main/action.yml), [documented Rust-cache interaction](https://github.com/jdx/mise-action/issues/215).

The default Rust-cache action primarily preserves dependencies rather than workspace outputs and disables incremental compilation. Confirm that both BoltFFI's host metadata context and Apple target context are covered. sccache needs actual Cargo wrapper/backend configuration and has uncacheable Rust/linking cases; it does not cache Swift just because it is installed. Choose and benchmark coherent Cargo-state/compiler-cache policy rather than stacking contradictory settings. [Rust cache](https://github.com/Swatinem/rust-cache), [sccache](https://github.com/mozilla/sccache), [Rust limitations](https://github.com/mozilla/sccache/blob/main/docs/Rust.md).

Swift package prebuilts or copied Xcode compiler caches require explicit support in the pinned toolchain and compatible artifact provenance. Do not turn experimental flags into default requirements, disable required validation, or download an old application as a substitute for building changed source.

### 7.4 Hosted versus persistent cache implementation

GitHub hosted uses verified action/cache/artifact capabilities. A real native macOS Velnor executor may use compatible persistent stores or supported remote services. The semantic key and verification contract remain identical; transport and storage differ.

Pin action revisions and verify runner compatibility. The audited current cache action documentation requires newer runner support for Node 24 actions. Do not assume a Velnor-native executor implements JavaScript actions, Node versions, GitHub cache protocol and artifacts because it can launch commands. [Cache action](https://github.com/actions/cache), [secure action use](https://docs.github.com/en/actions/reference/security/secure-use).

A version-specific detail matters: cache-mode is a workflow/job keyword, not an actions/cache input. Client-side ACTIONS_CACHE_MODE handling was added in toolkit 6.2.0. Audited released actions/cache v6.1.0, commit 55cc8345863c7cc4c66a329aec7e433d2d1c52a9, bundles toolkit 6.1.0; current main had newer handling. Use explicit restore-only and trusted conditional save behavior with verified release pins unless the complete newer capability is proven. Do not imply unreleased behavior is present in that release. [Released dependency metadata](https://github.com/actions/cache/blob/55cc8345863c7cc4c66a329aec7e433d2d1c52a9/package-lock.json), [toolkit release history](https://github.com/actions/toolkit/blob/main/packages/cache/RELEASES.md), [workflow cache policy](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

Keep secrets, signing state and credentials out of caches. Untrusted PRs must not publish trusted native products or modify shared trusted executable state.

## 8. Runner portability and platform constraints

| Work | GitHub hosted | Native macOS executor managed by Velnor | Standard Debian Velnor executor |
| --- | --- | --- | --- |
| Static scan, plan, code generation for workflows | Linux normally | Possible | Appropriate |
| Portable Rust tests | Matching OS/target | Possible | Appropriate |
| Portable SwiftPM tests | Compatible Linux or macOS toolchain | Possible | Appropriate after Linux support is established |
| SwiftUI/AppKit/Xcode app compile/link | Compatible native macOS | Requires real native host backend and Xcode | Cannot provide equivalent Apple app build |
| App launch/UI checks | Suitable macOS session | Requires suitable session/lifecycle | Cannot validate macOS runtime |
| Signed/notarized release validation | Protected native macOS | Protected native macOS | Route Apple-dependent work to macOS |
| Linux Docker jobs | Linux hosted | Docker on a Mac still runs Linux guest workloads | Supported |

Current Velnor local provider tables do not expose native macOS. The audited runner backend is container-oriented; a Mac host supplying Linux containers is not an Apple SDK executor. Enabling a macOS label without implementing native execution would create a false capability. [Provider table](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-workflow/src/s2/provider.rs), [runner backend](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-runner/src/execution/backend.rs), [runner platform handling](https://github.com/tailrocks/velnor/blob/5939604042ae5191b1b6d742e19e0db2c163ea1d/crates/velnor-runner/src/platform.rs).

Two implementation routes are valid: manage a current official GitHub self-hosted runner natively on macOS, or implement and verify a Velnor-native macOS process backend with the necessary lifecycle and protocol capabilities. Keep that work separate from Linux container execution. Do not reintroduce virtual-machine infrastructure to disguise the missing native backend.

GitHub hosted macOS is the immediate default. The current standard runner table lists arm64 macOS options separately from Intel-labeled variants. Resolve label/image/Xcode compatibility centrally and record the actual image; mutable latest labels are not toolchain identities. The cited job already ran on arm64. [Hosted runner inventory](https://docs.github.com/en/actions/reference/runners/github-hosted-runners), [Apple Xcode compatibility](https://developer.apple.com/xcode/system-requirements).

Docker job/service containers require Linux in GitHub Actions. Rust Darwin cross-compilation has documented possibilities, so the correct claim is not that every cross-compile is impossible: a standard Debian lane is not equivalent to building, linking and executing the actual Apple application with Xcode and its SDK. [GitHub container requirements](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idcontainer), [Rust Darwin support](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html).

Advertise native macOS eligibility only after end-to-end evidence. A Debian-only pool with no allowed macOS route must fail the complete application's plan or report an unsatisfied required capability; it must not label an omitted Apple job successful. Provider fallback must be configured and visible. Use isolated hosted capacity for untrusted fork code unless native host isolation has been explicitly implemented and verified; ephemeral registration alone does not clean reused hardware. [Self-hosted security guidance](https://docs.github.com/en/actions/reference/security/secure-use).

## 9. Generated GitHub Actions behavior

1. **Plan from the correct event revision.** Record source, base/merge tree, selected units, product identities, required results, toolchain and placement. Handle pull_request, configured merge_group, and main push consistently.
2. **Partition by execution requirements.** Separate portable Linux Swift from Apple Swift; select the runtime binary by execution ABI rather than language.
3. **Prepare exact tools once.** Use the installed compatible Xcode through a verified DEVELOPER_DIR; do not accidentally shadow it with an unrelated Swift toolchain. Validate tool availability early.
4. **Execute dependency nodes once per identity.** Materialize and verify the native binary dependency before SwiftPM/Xcode consumption. Preserve same-job reuse or use required artifact dependencies.
5. **Run the actual checks.** Skip duplicate build invocations only after proving coverage. Keep app, bundle, behavioral and optimized obligations.
6. **Preserve evidence.** Stream logs, record phase outcomes, build timings, cache metrics, test reports and failure diagnostics with bounded retention. Preserve nonzero status through output formatting.
7. **Use a stable aggregate gate.** It always runs and checks the planned result set. A failed planner, missing product, cancelled job or unexpected skip fails. An intentional no-op is an explicit plan result.
8. **Cancel stale PR work by PR identity.** Do not share a cancellation group with unrelated PRs, main, release or merge-queue candidates.
9. **Pin and regenerate.** Central action/tool policy resolves to immutable revisions; generated YAML is validated and checked for drift.

Workflow-level path filters can leave required checks pending; skipped jobs can appear successful. Merge queues need merge_group coverage. An aggregate using always() must inspect actual planned outcomes rather than treating every skipped dependency as acceptable. [Required-check guidance](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

Changes in Rust dependencies, manifests, locks, build scripts, bindings, resources, plist/entitlements, project specs/includes, schemes/test plans, toolchains or generator policy must propagate to affected consumers. Incomplete diff discovery, failed merge-base lookup or unknown dynamic inputs require conservative coverage. Prove PR/main semantic parity for the same candidate and required target set. Infrastructure failures remain possible; do not promise a literal success percentage without supporting measurements.

Reusable workflows can be an output mechanism, but cannot replace scanning and dependency modeling. Their permissions, environment propagation and runner availability have explicit rules. Keep nesting shallow and pass needed data explicitly. [Reusable workflow reference](https://docs.github.com/en/actions/reference/workflows-and-actions/reusing-workflow-configurations).

### Protected distribution verification

Keep Developer ID signing in a protected macOS release job. For the notarized distribution path, preserve hardened runtime, secure timestamp and valid signatures on embedded executables; remove the production debugging entitlement get-task-allow. Use a temporary job-scoped keychain, retain only the necessary credentials for the job, and clean up afterward. Submit with notarytool, inspect the result, staple where appropriate, and verify the final signed/stapled archive or app before publication. Carry forward exact source, native-product and toolchain provenance; a signed artifact is a separate verified output from its unsigned input. Preserve matching app/dSYM identity where applicable. [Apple notarization guidance](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## 10. Concrete changes in each repository

### 10.1 Velnor implementation map

Paths below are relative to the Velnor repository. Existing symbols and paths were inspected; proposed new responsibilities still require implementation.

| File/module | Required work |
| --- | --- |
| crates/velnor-workflow/src/s2/scan/swift.rs | Structured SwiftPM/Xcode/XcodeGen discovery; target/test facts; precise unresolved diagnostics |
| crates/velnor-workflow/src/s2/scan/rust.rs | Expose FFI library, path dependencies, features and build-input facts for product matching |
| crates/velnor-workflow/src/s2/scan/mod.rs | Join detector facts after discovery; deduplicate surfaces; preserve non-executing scan |
| crates/velnor-workflow/src/s2/config/mod.rs and canonical.rs | Validate minimal typed Apple/product/toolchain/destination policy; mark proposal fields explicitly until implemented |
| crates/velnor-workflow/src/s2/platform.rs | Evolve NamedProduct/Prerequisite from task invocation to typed recipe, identity and consumable output |
| crates/velnor-workflow/src/s2/mod.rs | Preserve discovered semantics, revise serialization/runtime contract, avoid synthetic cache loss |
| crates/velnor-workflow/src/s2/provider.rs, routing.rs, planner.rs | Resolve actual executor capabilities; reject unsupported Apple placement; validate active call paths |
| crates/velnor-workflow/src/s2/primitives/ir.rs and providers.rs | Partition by execution tuple; generate typed phases, correct runtime ABI and real artifact needs |
| crates/velnor-workflow/src/s2/primitives/cache.rs and snapshot.rs | Per-layer compatibility, trust, source identity and correct cache-state reporting |
| crates/velnor-workflow/src/s2/primitives/prepared_tools.rs and runtime_products.rs | Reuse verification and distribution machinery; extend identity to complete local source/build inputs |
| crates/velnor-workflow/src/s2/runtime.rs | Typed execution, bounded metadata evaluation, streaming, deadlines and process-tree cleanup |
| crates/velnor-workflow/src/s2/results.rs and primitives/aggregate.rs | Validate required product/platform/test outcomes and intentional exclusions |
| .github-gen/sources/actions/report-velnor-ci-outcomes/action.yml | Update reporting source; regenerate its owned output |
| crates/velnor-runner/src/execution and provider lifecycle | Implement native macOS support before advertising it; preserve Linux container behavior |

Reuse existing tests such as platform_prerequisites, provider_pairing, prepared_tool_handoff, selection_artifact_handoff, synthetic_surface and generic_surface_literals. Some current fixtures exercise schema 1; add actual schema-2 integration coverage rather than assuming test names prove coverage. No repository-specific package names, paths, commands, pins or provider labels belong in generic scanner logic.

### 10.2 Jackin migration map

| Current item | Action after generic replacement is verified |
| --- | --- |
| .github-gen/velnor-workflow.toml native scan exclusions | Remove the workaround; allow discovery of native packages, XcodeGen app and known native producer |
| Synthetic swift-package-native with ci_tasks | Remove; retain only genuinely ambiguous facts and explicit project policy |
| mise swift-package-native-ci task | Delete |
| CI dependence on desktop-xcframework alias | Replace with typed native product execution |
| Generic build/pack logic in jackin-xtask desktop.rs | Move to reusable adapters; preserve product validations and Jackin-specific behavioral assertions |
| Optional developer aliases | Thin calls into the same generic plan; no duplicated orchestration |
| boltffi.toml | Retain standard producer facts; update obsolete wrapper-only instructions |
| native/Package.swift | Preserve the binary-target/module contract during initial migration |
| native/project.yml | Preserve as app source of truth; discover and generate through XcodeGen adapter |
| Cargo profiles | Keep desktop-release semantics; add validated native verification policy as appropriate |
| Rust/tool pins | Retain deterministic versions and required Apple target/component validation |
| Binding drift checks | Generate in staging and compare; fail stale committed sources |
| Existing harnesses, app checks, UI tests | Preserve behavior; connect reusable products and remove repeated construction |
| .github/ci/project.toml and workflows | Regenerate from source config and published/pinned Velnor |
| Native README and PR description | Update to match checks actually executed, including premerge app coverage |

Do not hand-edit generated workflow arrays. Do not preserve an old and new build recipe indefinitely. Remove superseded logic as each supported migration completes. App identity, resources, signing policy and behavioral requirements are legitimate repository facts; generic orchestration belongs in Velnor.

## 11. Execution sequence: small verified increments

The sequence is driven by dependencies and correctness. Research, implementation, test fixtures and reviews can run concurrently where they do not mutate the same files or contracts.

| Increment | Deliverable | Gate before moving consumers |
| --- | --- | --- |
| 1. Evidence and liveness | Reproducible baseline, structured phase/result reporting, correct timeout/cancellation behavior | Quiet success, chatty timeout, closed-pipe and descendant cleanup tests |
| 2. Product contract | Existing schema-2 graph extended with typed products and strict validation | Clean fixtures, ambiguity/cycle/mismatch tests, deterministic serialization |
| 3. Swift/XcodeGen discovery | Conventional package/app/native relationships discovered | No execution during scan; actual app identified; renamed fixture works |
| 4. Cache and artifact execution | Both host/target Rust state and exact composite products; correct Swift/Xcode layers | Cold fallback, source invalidation, provenance, missing/corrupt product tests |
| 5. Hosted macOS integration | Published runtime, compatible toolchain, generated native graph | Real GitHub-hosted clean/warm runs, actual app coverage, gate correctness |
| 6. Jackin migration | Remove synthetic task workaround, regenerate, preserve full desktop contract | Prospective merge-tree checks, binding/app/runtime evidence, no lost checks |
| 7. Native macOS provider | Implement actual native executor/official-runner management and lifecycle | Run same contract on real macOS host; reject Debian Apple placement |
| 8. Cross-repository proof and cleanup | Portable Swift, Xcode app and unrelated mixed-language examples | Equivalent outcomes across supported providers and final documentation |

Keep one active working branch per repository when safe, using separate directories/worktrees only where necessary for parallel isolation. Make small coherent commits after relevant checks; push regularly. Merge current main into the work branch and resolve conflicts, following the user's merge preference. Publish Velnor runtime products before a Jackin pin depends on them. Merge each complete reviewed green increment promptly; do not accumulate another enormous PR.

PR #1013's existing functionality and review findings remain part of its merge gate. Read current review submissions and unresolved threads before deciding it is ready. Do not mark comments resolved without a verified fix or evidence-backed resolution. Reacquire refs/checks immediately before merging; main then receives a smoke/required-contract verification. A long-running main issue should produce a fix or evidence-backed rollback, not abandonment.

The immediate hosted migration can land before native macOS runner support, with that capability explicitly absent. Completion of the overall portability goal requires the native macOS end-to-end proof, or a demonstrated external blocker with the unsupported capability clearly recorded. Labels and unit tests alone do not establish that proof.

## 12. Acceptance tests and measurement contract

### 12.1 Required behavioral and negative tests

| Area | Cases that must pass |
| --- | --- |
| Discovery | Portable Swift library; executable without tests; Apple-framework package without binary target; remote XCFramework; cross-platform tool bundle; conditional manifests; nested package graph |
| Project generation | XcodeGen spec only; source plus generated project; recursive includes; include cycle/escape rejection; build-only scheme; ambiguous/shared scheme names |
| Native producers | Different crate/framework names and paths; clean missing binary target; supported BoltFFI mapping; ambiguous producer; stale/extra slice; mismatched header/API/module |
| Invalidation | Direct and transitive Rust source; build.rs inputs; Cargo lock/features/profile/cfg; binding tool/config; Swift/SDK/Xcode; deployment target; Xcode spec/resources/plist; same-size changed content |
| Reuse | Swift-only edits preserve unchanged native product; required Swift/app checks still execute; source changes rebuild affected producer; metadata and target contexts remain distinct |
| Cache integrity | Absent, exact, compatible seed, corrupt, missing files, wrong ABI/arch/profile, untrusted producer; clean fallback equals uncached result |
| Execution | Quiet valid command; chatty hang; no-newline output; closed streams while live; descendants holding pipes; full cancellation; nonzero status; bounded output drain |
| Platform | Mixed Linux/Apple Swift placement; actual native macOS; Mac running Linux Docker stays Linux; Debian-only pool cannot satisfy app; unavailable Xcode is explicit |
| Tests/results | XCTest-only, Swift Testing-only, both, intentionally no tests, zero discovered when tests expected, corrupt/missing reports, build-only product |
| Gates/events | Failed planner, missing/cancelled/unexpectedly skipped job; docs-only planned no-op; merge-group candidate; renamed/deleted input; incomplete diff fallback |
| Regeneration | Same inputs produce identical plan/YAML; generated drift fails; supported runtime published; no project-name special cases |
| Distribution | Optimized configuration and intended slice coverage before merge; bundle/resources/plist/linkage; dSYM UUID match; signed final artifact checked when released |

These are implementation acceptance tests, not tests executed during this report. Favor behavioral assertions over tests that merely repeat serialized implementation strings.

### 12.2 Controlled benchmark matrix

Run baseline and revised plans on the same candidate or a controlled equivalent change, runner class and resolved toolchain. Distinguish tool cache, dependency state, intermediate state and exact native product state explicitly. The historical job had narrower coverage than the required full application contract: compare equivalent checks, and report newly added app/UI coverage separately rather than attributing its time to an optimization regression or an invented speed gain.

| Scenario | Expected correctness/performance observation |
| --- | --- |
| Everything cold | Complete build succeeds without prior generated project/framework or cached product |
| Tool cache only | Shows actual compiler cost separately from bootstrap |
| Warm dependency/intermediate cache | Build tools validate freshness; reports prove actual reuse |
| Exact native-product hit | Native producer is verified and reused; consumer checks execute |
| Swift-only edit | No unnecessary target/metadata rebuild for unchanged native closure |
| Direct/transitive Rust edit | Required native products and consumers invalidate |
| Lock/features/profile/bindgen change | Compatible layers may seed; incompatible products are rejected |
| Xcode/SDK/Rust toolchain change | No false hit across incompatible compiler/SDK identity |
| Missing/corrupt product | Safe rebuild or explicit failure; no false green |
| Optimized release-like build | Correct shipping semantics remain validated |
| Native Velnor macOS | Equivalent outputs/checks to hosted within declared environment contract |
| Debian-only placement | Explicit unsatisfied Apple capability, while portable work is valid |

Record queue, setup, restore, compilation, linking, metadata extraction, binding emission, packaging, Swift build/test, Xcode app/UI phases, uploads and gate time. Save source/runner/toolchain identities, cache hit/rejection reasons, bytes transferred, compile requests/hits/uncacheable counts, test inventory, artifacts and raw logs.

Use at least three comparable repetitions per baseline/candidate scenario for initial median/range reporting. Collect a larger continuing series for meaningful tail metrics; do not label three runs a reliable p95 estimate. Disclose sample count and uncertainty. Compare end-to-end critical path and wasted retry time, not only a single compiler phase.

Success requires **eliminating unnecessary repeated producer work, preserving required coverage, recovering correctly from cache absence, and avoiding the demonstrated false timeout**. Set numeric latency goals after the first controlled baseline; any claimed improvement must link to before/after runs and explain changed work. A measured speed gain that skips the application or optimized validation fails acceptance.

### 12.3 Reacquiring the original evidence

These commands are read-only and intended for an authenticated GitHub CLI environment:

~~~sh
mkdir -p ci-evidence

gh api repos/jackin-project/jackin/actions/runs/35535696169 \
  &gt; ci-evidence/run.json

gh api --paginate --slurp \
  'repos/jackin-project/jackin/actions/runs/35535696169/attempts/2/jobs?per_page=100' \
  &gt; ci-evidence/attempt-2-jobs.json

gh api --paginate --slurp \
  'repos/jackin-project/jackin/actions/runs/35535696169/attempts/1/jobs?per_page=100' \
  &gt; ci-evidence/attempt-1-jobs.json

gh run view 35535696169 --repo jackin-project/jackin \
  --attempt 2 --job 106148084283 --log \
  &gt; ci-evidence/native-attempt-2.log

gh run view 35535696169 --repo jackin-project/jackin \
  --attempt 1 --job 106145813603 --log \
  &gt; ci-evidence/native-attempt-1.log
~~~

Collect all job pages and attempt identities; a normalized first-page wrapper may omit timestamps or reuse prior-attempt jobs. Keep raw evidence separate from derived tables.

## 13. Copy-ready implementation /goal

Use the complete block below as the implementation goal. It is intentionally an execution instruction rather than a request to write another plan. The preceding report supplies the detailed evidence and design constraints.

~~~text
/goal Generic, correct, fast macOS and Swift CI in Velnor; migrate Jackin

Implement a reusable macOS/Swift build model in `tailrocks/velnor`'s `velnor-workflow`, and use it to remove Jackin's custom native CI orchestration while preserving and completing the required verification of Jackin's real macOS application. Work against:

- Velnor: https://github.com/tailrocks/velnor
- Jackin PR #1013: https://github.com/jackin-project/jackin/pull/1013
- Investigated native job: https://github.com/jackin-project/jackin/actions/runs/35535696169/job/106148084283
- Previous failed native attempt: https://github.com/jackin-project/jackin/actions/runs/35535696169/job/106145813603

The outcome is implemented, tested, reviewed, incrementally merged generator/runtime changes, a migrated Jackin consumer, reproducible evidence, and an accurate account of supported executor capabilities. Make GitHub-hosted runners the default. The same typed build contract must support eligible Velnor executors through their actual capabilities. A standard Debian host can perform scanning, orchestration and supported Linux work; it cannot validate a native Xcode/SwiftUI/AppKit application. An ordinary Linux container running on a Mac also cannot supply that Apple build capability.

## Execution rules

Delegate first. Create independent workstreams for repository/PR inspection, native-build forensics, Apple/Swift best-practice research, public-project comparisons, scanner/product design, executor/cache design, implementation, verification, and review. Use all useful available concurrency. Give agents focused ownership and explicit dependencies; parallelize non-conflicting changes, and reuse agents as new work becomes available. The coordinating agent integrates decisions and changes and runs the final deterministic gates. A separate verifier must review material conclusions and completed implementations without relying on an implementer's claim that they work.

Work autonomously. Resolve ordinary ambiguity through repository evidence, primary documentation, small experiments and independent reviewers. Do not ask clarification questions to resolve task ambiguity. Preserve the user's work and honor concrete access or execution restrictions. If something is blocked, attempt the authorized alternatives, establish the actual limitation, continue independent work, and report the exact blocked acceptance criterion and evidence. Missing optional infrastructure must never become an excuse to stop all work, or a reason to claim an unexecuted capability passed.

Diagnose the structural cause before fixing a symptom. Remove the condition that permits recurring failures whenever feasible. Judge changes by correctness, consistency and the goal; do not leave known faults merely because they are difficult or label them low value. Keep the scope connected to this build architecture and its actual dependencies.

Commit each coherent, verified increment promptly and push regularly. Prefer one active integration branch per repository. Use extra worktrees or branches only for concrete concurrent-edit or review requirements. Fetch and merge current `main` into working branches; do not rebase published history. Open small, coherent PRs, address all review comments, merge once each exact candidate satisfies its required gates, and continue the next increment from updated main. Do not accumulate the entire effort in another large, unverified branch. Do not merge an implementation that temporarily breaks released consumer configuration: coordinate compatible rollout or publish the complete contract before migrating consumers.

Before changing anything, read applicable `AGENTS.md` instructions, repository architecture, current generated-file ownership, open PR descriptions, reviews and discussions. Refresh the PR/run identities: the research snapshot below is evidence, not a claim that today's head is unchanged. Keep a concise committed progress and handoff document with decisions, outstanding work, tested SHAs and concrete continuation steps.

## Evidence to reproduce, not assumptions to repeat

At the 20 September 2026 research snapshot:

- Jackin PR #1013 was open on `integrate/pr1002-multi-account`, head `997c18fe595f044a46146bf3365dccdc0838dbab`, base `fce94cea8a15de0c2db3bb4ff880d741baf5c00a`. It contained 157 commits and 433 changed files. Its description's assertion that `.github/*` matched main was stale.
- The cited job was a successful attempt 2 of run `35535696169`; the run-level conclusion was `cancelled`. Its run head was `6713d01445d8454e30419e2535dc92e4f82956ed`, and the actual checkout was merge SHA `ad70b92f8c5633326c41576b044618cddb80def7`. Do not confuse these revisions or the job/run conclusions.
- The native job ran for 985 seconds, or 16m25s. The nested `cargo xtask desktop xcframework` command consumed **853.696 seconds**, or 14m13.696s: approximately 86.7% of active job time. Its BoltFFI target-build boundary was 393.326s, metadata/bindings boundary 408.978s, and assembly boundary 13.849s. These intervals are nested and must not be added to the outer duration.
- The subsequent Swift main build reported 53.16s, and the test-build 21.62s. Those tool-reported durations are not a complete additive job waterfall. Removing a redundant `swift build` does not eliminate the compilation that `swift test` still needs on a cold checkout.
- The runner was already ARM64: `macos-26-arm64`, image `20260907.0351.1`, macOS `26.6.2`, Actions runner `2.337.0`, Rust `1.97.1`, target `aarch64-apple-darwin`, deployment floor `26.0`. The produced XCFramework already contained only one macOS arm64 slice. Do not propose eliminating a nonexistent Intel slice or switching this job to arm64 as its solution.
- Native unit cache paths/key inputs were empty, so native cache restore/save were skipped. The mise tools cache hit. Installing `sccache` did not activate it: no configured wrapper/backend/statistics were observed for the nested Rust build. A tool-install cache hit is not native compiler reuse.
- The first native attempt was killed at Velnor's 600-second output-inactivity threshold. It did not report a Swift compiler failure. The successful rerun does not prove the precise child activity at the first kill, but establishes why treating buffered compiler output as a reliable liveness signal is unsound.
- Jackin's `desktop-release` Cargo profile uses thin LTO, one codegen unit and preserved symbols. Pinned BoltFFI `0.30.1`, upstream commit `2e6320a6d92cb591d22b908477f3a47da7ebc9bc`, compiles the target library and later invokes host `cargo rustc` to collect binding metadata using the same profile arguments, with distinct target/cfg/artifact contexts. The 408.978s phase is not mere Swift text generation. Full per-crate/compile/link attribution still requires profiling.
- BoltFFI target compiler output is hidden unless verbose is enabled; its metadata path uses buffered `Command::output()`. Verbose alone therefore does not fix every silent phase. There is no verified independent metadata-profile switch in this pinned command. `pack apple --no-build` does not alone bypass metadata generation, and `--regenerate false` assumes existing generation/header state. Do not invent flags or break a clean checkout by suppressing needed work.
- Jackin's `native/Package.swift` describes libraries and harness executables. `native/project.yml` is the XcodeGen source for the actual `JackinDesktop` app, its bundle, resources and UI tests. Passing SwiftPM tests is not proof that the app builds or runs.
- An unresolved, non-outdated P1 review thread concerned Landlock workspace mounts and auxiliary Git/worktree paths: https://github.com/jackin-project/jackin/pull/1013#discussion_r4057673954 . Reinspect its current state, reproduce or disprove the concern, fix the structural issue when present, and address the review before merging #1013. CI-speed changes do not resolve it automatically.

Velnor was audited at `5939604042ae5191b1b6d742e19e0db2c163ea1d`. Its active schema-2 pipeline already has typed units, `NamedProduct`/`Prerequisite`, prepared-tool identities, artifact handoff, routing and result aggregation. However, the product prerequisite currently prepends a mise preparation task and rebuilds it in the consumer; a `depends_on` relationship is not a produced-artifact transfer contract. Its local-provider capability tables advertise Linux, and its inspected execution backend does not provide a verified native macOS process executor. These are implementation gaps to close, not capabilities to imply by changing `runs-on` labels.

## Required architecture

### 1. Extend the existing schema-2 pipeline

Evolve scan → typed facts and products → dependency selection → placement → runtime execution → results. Extend existing product, prepared-tool, artifact and aggregation primitives rather than adding another scheduler, a parallel configuration language or a third Swift detector. Keep generic code free of Jackin names, paths and task aliases. Do not replace `swift-package-native-ci` with a differently named opaque shell task.

The repository declares product facts that cannot be inferred safely: supported architectures, deployment floors, chosen schemes/test plans, shipping profile, signing policy and ambiguous producer relationships. Velnor owns generic build orchestration, provisioning, caches, actions and command rendering. Preserve mise as a tool-version/environment manager. Ordinary project adoption must not require copying shell build recipes.

Design and document actual validated schema types before using them. Do not present proposed TOML fields or CLI flags as already supported. Prefer inference when manifest facts identify exactly one producer; use small typed overrides with precise diagnostics for genuine ambiguity.

### 2. Discover surfaces without executing repository code

Statically inspect tracked SwiftPM manifests, governing resolved files, local package dependencies, Xcode projects/workspaces/shared schemes/test plans, xcconfigs, recognized XcodeGen specifications and includes, Cargo workspace/package/library facts, and supported binding-generator configuration. Exclude fetched dependencies and build trees. Preserve provenance, known facts and unresolved requirements. Detect cycles, duplicate/ambiguous producers, conflicting output paths, include traversal and generator/project duplication.

Do not run `swift package dump-package`, project generation, plugins, build scripts or mise tasks during static scanning: Swift manifests are executable code. When authoritative metadata requires evaluation, run a bounded refinement phase on a compatible executor under the ordinary untrusted-build policy, with no signing secrets or trusted cache-write authority. Bind metadata to source and toolchain identities before consuming it. Runtime refinement must not silently reduce required coverage or conceal unsafe initial placement; preserve conservative validation for unresolved facts.

Do not equate `platforms: [.macOS(...)]` with “cannot run on Linux,” or absence of that declaration with portability. Classify imports/frameworks, transitive targets, binary artifact formats and target triples together. Distinguish remote XCFrameworks from portable tool artifact bundles. Unknown requirements must trigger a supported conservative verification route or a precise configuration diagnostic, never silently omitted checks.

Recognize XcodeGen through its document structure and references rather than every filename `project.yml`. Generate its project as an execution node before querying/building the chosen scheme. Deduplicate committed generated projects. Discover the real app separately from SwiftPM. Do not call `test` on a build-only scheme or invent a universal test destination.

### 3. Model native production and Swift consumers explicitly

Represent the following operations and their real edges: host binding-metadata collection, Rust target staticlib compilation, binding/header/module-map generation, XCFramework assembly/validation, SwiftPM test/product compilation, Xcode project generation, app build/test, bundle verification, and trusted distribution. A supported BoltFFI adapter is the first native integration; do not advertise unimplemented UniFFI/cbindgen/Tuist support merely to make the model look generic.

Each product needs a typed recipe/adapter, source/package identity and transitive input closure, host execution ABI, target platform/triple/architecture, SDK/deployment floor, toolchain, profile/features/flags, generation settings, declared output set, consumer location, provenance and content digests. Model Rust host metadata separately from target compilation. Derive their actual dependency and shared-state relationship before parallelizing them.

Join Jackin's local Swift `.binaryTarget` path to `boltffi.toml`'s XCFramework output and Cargo crate when unambiguous. A clean checkout must materialize the missing binary before SwiftPM needs it; a late package plugin is not a substitute for this ordering. Keep the valid one-slice XCFramework contract initially. A change to direct `.a`/module-map linking requires verified consumer and local-development changes and does not remove Rust compilation by itself.

Build one exact product once per compatible plan. Reuse it within the same job or transfer it through a verified artifact edge to dependent jobs with real `needs`. Do not use an optional cache as the sole data bus. Do not force every conceptual node into its own job: measure placement, queue, transfer and shared-workspace effects. Coordinate writers to Cargo/build/output directories and verify that parallel and serial execution produce matching manifests.

Generate bindings into a staging tree and compare with committed bindings. Perform the drift comparison on exact product-cache hits too, before restoring anything over tracked files. Never quietly mutate tracked generated sources and then test only the altered tree. Preserve module/import names, headers, API signatures, platform slices, deployment floor, plist correctness and linkage assertions. Publish only complete validated outputs; reject partial, stale or mismatched products.

### 4. Apply distinct cache and prebuilt policies

Implement separate classes for verified tool binaries, dependency downloads, compiler caches, incremental build state, exact native products, and distribution artifacts. Reuse existing strict prepared-tool/product validation instead of duplicating weak integrity checks. Extend identity for locally compiled tools/products to their source/build-script/configuration closure; `Cargo.lock` alone does not identify local source.

Partition compiled-state compatibility by cache schema, resolved compiler/Xcode/SDK build, host and target ABI, deployment version, profile/features/flags, dependency graph, binding inputs, recipe version and relevant path assumptions. Exact produced artifacts also need the complete semantic input digest and output manifest. A larger workspace closure is conservative only for local files; it does not cover undeclared environment, external paths, Git state or network inputs. If a complete input contract cannot be established or controlled, disable exact producer skipping with a reason, restore compatible intermediate state and run the producer. Never guess a narrow key that can yield a false green.

Intermediate restores are acceleration seeds: run the build/test tool afterward. Exact product reuse may skip that producer only after complete identity and manifest validation. Producing commit is provenance; exact native source/recipe/toolchain identity can permit reuse across a Swift-only consumer commit. If the producer reads Git SHA or dirty state, include those values as inputs. It never skips required consumer tests. Missing/corrupt caches must rebuild correctly; required same-run artifacts must have hard-failing integrity/provenance checks. Preserve permissions/symlinks/file layout when packaging native outputs, and verify them after transfer.

For Rust, inspect compatible dependency/target caching and configure `sccache` explicitly if selected. Verify eligible invocations, required incremental settings, backend permissions and hit/miss statistics. Do not claim `sccache` caches Swift or removes unsupported final-link invocations. Include host metadata outputs as well as explicit-target outputs.

Enforce committed Cargo resolution in nested native build commands. For app dependencies with committed Package.resolved files, use supported pinned-toolchain locked resolution and fail unintended lock drift; retain library-specific dependency-testing policy. For SwiftPM and Xcode, distinguish dependency stores, `.build`, scoped DerivedData and supported compilation caching. Use stable per-unit paths, validated compatibility boundaries and cold-versus-warm experiments. Probe the selected Xcode before enabling supported compilation-cache settings; Xcode build settings are not automatically valid `swift build` flags. Do not promise distributed Xcode cache reuse from a local setting or adopt mtime/inode workarounds without correctness evidence.

Prefer verified pinned native Velnor/BoltFFI/helper binaries where available; publish required runtime binaries before pointing consumers at a revision that expects them. Provision only required tools/targets and validate restored rustup/mise state. Avoid redundant toolchain ownership or broad Homebrew upgrades unrelated to the plan.

Keep cache writers trust-scoped and credentials out of cached paths. Use verified pinned action releases and explicit restore/save policy. Do not confuse the workflow/job `cache-mode` keyword with an action input, or assume a published action supports behavior seen only on its main branch. Native Velnor stores must enforce their own equivalent trust and integrity rules.

### 5. Correct execution, observability and required coverage

Replace output-idleness-as-failure with a typed bounded execution policy. Stream nested diagnostics where supported, retain complete failure logs, and use hard phase/job deadlines. Silence should trigger diagnostics unless a verified tool protocol makes it meaningful evidence of failure. A chatty hung process must still time out. Keep deadlines active after stdout/stderr close. Cancel and clean up the full process group/tree with bounded graceful shutdown and escalation. Synthetic heartbeat output is not proof of forward progress.

Record resolved source/merge SHA, image, Xcode/Swift/Rust/SDK identities, host/target/profile, phase start/end/outcome, queue/setup time, cache bytes/transfer time, exact/prefix/miss/rejection, compiler statistics and test results. Distinguish disabled, skipped, unknown and cache miss. Do not infer zero downloads or compiler work from suppressed logs. Capture stable Cargo timing reports and Xcode timing summaries/xcresult evidence without inventing unsupported stable JSON flags.

Use `swift test` without an unnecessary identical preceding build only after proving its product/configuration coverage. Build additional products outside the test closure explicitly. For Xcode, use `test` or `build-for-testing` plus `test-without-building` according to real reuse needs. Jackin's per-method UI isolation can share one compatible test build, but do not parallelize UI sessions blindly. Preserve test enumeration, clean app state, result counts and failure status. Detect XCTest and Swift Testing independently; a zero-count summary from one is not the total from both. A package with no tests must produce an honest build-only result.

The required premerge contract must include the actual app and its relevant optimized/platform checks, not leave those first exercised after merge. Preserve signed release work on trusted refs. Use protected Developer ID signing with hardened runtime, secure timestamp, valid embedded signatures and no production get-task-allow; notarize with notarytool, staple where appropriate, and verify the final signed output before publishing. Use job-scoped keychain lifecycle and preserve source/native-product provenance. Fast validation and shipping profiles are distinct identities; assess a less expensive PR/metadata profile through output/API/runtime comparison and retain release-like checks before merging affected code. Do not weaken shipping flags or invent a BoltFFI metadata-profile option. A necessary adapter/upstream capability change is a separately reviewable dependency; a complete validated composite pack is an acceptable initial implementation while that change is delivered.

Generate an always-present aggregate gate using the planned required result set. It must reject failure, cancellation, missing results and unexpected skips, including failed prerequisites. Keep event-correct PR, configured `merge_group` and main validation semantics. Unknown diffs/manifests/dependencies must conservatively schedule relevant checks or fail planning. Resource, entitlements, project, lockfile, codegen, Rust transitive dependency and policy changes must select their affected consumers. Cancel superseded PR runs without cancelling unrelated PRs, main, release or valid merge-queue candidates.

## Ordered implementation and rollout

1. **Refresh and baseline.** Inventory current Velnor and Jackin branches, instructions, reviews, source ownership and exact CI contracts. Save complete run/job/attempt evidence. Reproduce the old graph on an eligible Mac and expose Rust target, host metadata, packaging, Swift, test and cache boundaries. Establish reproducible cold and warm baseline scenarios. Fix the watchdog/process lifecycle and per-phase reporting in a focused Velnor increment with deterministic regression tests. Publish/runtime-pin changes through the repository's normal path.

2. **Typed discovery and product graph.** Extend existing schema-2 scan/config/product/prerequisite/planner types and deterministic serialization. Implement static discovery plus bounded runtime refinement, BoltFFI producer matching, real app/XcodeGen detection, full affected-input selection and precise unresolved diagnostics. Add renamed fixture repositories that prove there are no Jackin literals in generic logic. Keep current consumers working until the replacement executes completely.

3. **Generic Apple execution and reuse.** Implement typed adapters, tool acquisition, product verification, phase execution, compatible cache classes and artifact transport. Add verification/shipping policy and actual SwiftPM/app/test coverage. Benchmark candidate job placement and safe parallelism. Integrate supported profile improvements with verified BoltFFI behavior. Release a complete generator/runtime contract and its required architecture binaries before consumer migration.

4. **Jackin migration and PR #1013 completion.** Merge current main into its existing branch and resolve conflicts without discarding functionality. Replace the manual `swift-package-native` unit and native scan exclusions with detected SwiftPM/XcodeGen/native products and only necessary typed facts. Remove `ci_tasks = ["swift-package-native-ci"]` and the corresponding mise CI task once replacement parity is demonstrated. Move reusable XCFramework mechanics into Velnor. Keep normal tool pins, manifests, `boltffi.toml`, shipping settings, signing policy and Jackin-specific behavioral tests. Thin local aliases may call the same generic plan; they must not duplicate its recipe. Regenerate owned configuration/YAML from source. Preserve required binding checks, harnesses, tests, app/bundle verification, dSYM checks and relevant UI coverage. Read and resolve every review, including the Landlock P1 concern, and correct stale documentation/PR claims. Merge #1013 only when its exact current candidate passes all repository requirements; do not wait for unrelated future optimizations after that gate is genuinely satisfied.

5. **Prove executor portability.** Partition plans by actual OS/architecture/SDK/execution capability, not language or provider name. Verify GitHub-hosted macOS first. Support an actual native official self-hosted macOS runner only with verified toolchain, trust and lifecycle requirements. Velnor-managed/native macOS eligibility requires the real provisioning/execution/cache/artifact/cancellation lifecycle; implement missing prerequisites in focused increments and test them on a real Mac before calling that route supported. A native backend is not automatically compatible with JavaScript actions or GitHub cache/artifact APIs: implement or explicitly validate the required contract. Keep Docker-on-macOS and Debian eligible for their Linux work, route Apple work to a compatible Mac, and fail a required Apple plan that has no eligible executor. Do not claim macOS support from renderer snapshots alone.

6. **Independent verification, benchmark report and cleanup.** Have independent agents inspect the generated configuration, product/cache identities, threat/trust boundary changes, actual app coverage, tests and CI results. Adopt the same generic configuration in unrelated SwiftPM and Xcode/XcodeGen/native fixtures. Remove superseded paths within the migration scope, update examples and schema documentation, verify byte-stable regeneration, and record final before/after evidence. Continue focused corrective increments until the acceptance contract below is complete or a remaining environment capability has been concretely demonstrated unavailable.

Use the existing Velnor edit surfaces as the starting map: `src/s2/scan/{swift,rust,mod}.rs`, `config`, `platform.rs`, `provider.rs`, `routing.rs`, `planner.rs`, `primitives/{ir,providers,cache,snapshot,prepared_tools,runtime_products}.rs`, `runtime.rs`, `results.rs`, aggregate primitives, and the source for generated reporting actions. Follow actual current code after refresh. Executor lifecycle belongs under the real runner/provider implementation, not a Swift-only renderer special case. Do not hand-edit generated workflow output as the permanent fix.

## Acceptance contract

Use meaningful deterministic tests for architectural behavior and real macOS execution for Apple build claims. Existing integration tests and fixtures are starting points; verify which schema/version and paths they exercise.

- Static scanning executes no project code. Portable Swift, Apple-framework Swift, conditional manifests, local packages, remote XCFrameworks and tool artifact bundles are classified honestly. Ambiguities, include cycles/escapes and duplicate schemes/outputs are handled deterministically.
- A clean checkout discovers Jackin's actual app, Swift packages and Rust/BoltFFI producer, builds a missing XCFramework in correct order, and requires no Jackin task names in Velnor. A fixture with different names, paths and crate/package structure works through the same adapter. Build-only surfaces and no-test packages receive accurate commands/results.
- Source, transitive Rust dependencies, `build.rs` inputs, features, profile, lockfiles, flags, toolchain/SDK/deployment target, bindings config, headers, module maps and recipe changes invalidate every affected product. A Swift/resource-only edit reuses unchanged valid FFI while selecting the needed Swift/app checks. If identity is uncertain, reuse is conservative.
- Cold, exact-hit, compatible-prefix, absent, corrupt, truncated, wrong-architecture, wrong-profile, stale-source and untrusted-producer cases behave correctly. Exact product consumption verifies all required files and digests. Cache writes/reads preserve trust boundaries, and tests still execute on cache hits.
- Quiet successful children, chatty hangs, no-newline output, early-closed pipes, descendants retaining pipes, cancellation, cleanup and nonzero exit propagation have bounded regression tests. No test needs to wait a literal ten minutes.
- SwiftPM tests, actual app build, required harnesses, generated-binding drift, bundle/resources/linkage/architectures/deployment floor, dSYM identity where applicable, optimized verification and selected UI/runtime checks retain their intended assertions. Report build-only evidence separately from execution evidence.
- GitHub-hosted macOS succeeds; native Velnor macOS is proved by real execution before claimed supported; standard Debian cannot be selected as an Apple executor. A missing required platform fails the aggregate rather than becoming a green skip. Local and CI plans share semantic operations.
- Concurrent execution does not corrupt shared directories or products. Same-input serial and parallel builds produce equivalent verified manifests. Job artifacts have real producer dependencies; optional caches are never required for correctness.
- Generated files are deterministic and the repository's generation-check command rejects drift. Action/runtime pins and privileges remain correct. PR/merge-group/main required-result aggregation rejects missing, cancelled and skipped obligations and validates the correct source scope.
- PR #1013 has no unresolved blocking review concern, including the current disposition of the Landlock thread. The exact latest candidate/merge tree passes all required checks and independent review immediately before merge. Do not count earlier green heads, a successful job inside a cancelled run, or unrelated passing workflows as this gate.

Benchmark at minimum: clean cold; dependencies warm; complete unchanged warm; Swift-only change; Rust FFI source change; transitive Rust change; binding-config/header change; dependency-lock change; profile change; Xcode/Rust/SDK change; and deleted/corrupt cached output. Keep source, runner class and toolchain controlled within each comparison. Repeat representative cold/warm cases and report sample count, individual runs, median/range, queue and critical-path time, per-phase work, transfer overhead, compiler reuse and coverage. Compare the same validation contract; report newly added app coverage separately from the historical Swift-only job. Do not invent an absolute speed target or claim a speedup until measured.

Deliver merged PR/commit/release links and exact verified SHAs; current supported runner matrix; the real documented minimal repository configuration; scanner and adapter contracts; before/after command/product graph; retained/removed Jackin configuration; benchmark evidence; cache invalidation and liveness test results; review disposition; and any demonstrated remaining blocker with the precise unfinished criterion. Do not promise a numerical main-branch green rate: eliminate preventable PR/main contract differences and quantify actual reliability from evidence.

## Primary references to recheck while implementing

- Apple build speed and scheduling: https://developer.apple.com/documentation/xcode/improving-the-speed-of-incremental-builds and https://developer.apple.com/videos/play/wwdc2022/110364/
- Apple build settings, selected-toolchain support and XCFramework creation: https://developer.apple.com/documentation/xcode/build-settings-reference ; https://developer.apple.com/xcode/system-requirements ; https://developer.apple.com/documentation/xcode/creating-a-multi-platform-binary-framework-bundle
- SwiftPM model and build/test semantics: https://docs.swift.org/package-manager/PackageDescription/PackageDescription.html ; https://www.swift.org/getting-started/library-swiftpm/
- Cargo cache, profiles and supported build flags: https://doc.rust-lang.org/cargo/reference/build-cache.html ; https://doc.rust-lang.org/cargo/reference/profiles.html ; https://doc.rust-lang.org/cargo/commands/cargo-build.html
- Pinned BoltFFI implementation: https://github.com/boltffi/boltffi/tree/2e6320a6d92cb591d22b908477f3a47da7ebc9bc
- GitHub runners, caches, artifacts, required checks and trust: https://docs.github.com/en/actions/reference/runners/github-hosted-runners ; https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching ; https://docs.github.com/en/actions/tutorials/store-and-share-data ; https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks ; https://docs.github.com/en/actions/reference/security/secure-use
- Public reference patterns: Ghostty native-core/Xcode separation, Zed typed workflow generation and Rust compiler caching, Nuke local/CI parity and test diagnostics, SwiftNIO/Alamofire platform contracts, Vapor reusable Swift workflows, and Point-Free's measured incremental-build workarounds. Inspect actual current/pinned source; do not copy private runner labels, unbounded matrices, mutable binary URLs, cache-key omissions or project-specific retries as generic best practice. These comparisons establish patterns, not Jackin benchmark results.

Start by delegating the baseline, current-architecture inspection, PR-review audit and independent design challenge in parallel; then execute the staged implementation through reviewable increments.
~~~
---END VERBATIM OBJECTIVE---

## Appendix M. Source-to-handoff requirements matrix

`Requirement ID | Authoritative source | Operative requirement | Exact HANDOFF section | Current state/evidence | Remaining task IDs and acceptance check | Coverage result`

G = original-goal contract; H = handoff contract. Documentation coverage (last column) is
separate from engineering progress (§D).

| ID | Source | Operative requirement | § | State/evidence | Remaining + acceptance | Cover |
|----|--------|----------------------|---|----------------|------------------------|-------|
| G-001 | §L acc-1 | No-execution static scan; honest classification of 6 surface kinds; deterministic ambiguity/cycle/escape/dupe handling | §B acc-1, §D R-SCAN | IMPLEMENTED_UNVERIFIED: #1025/#1030/#1031 merged; full §12.1 table unverified | T-004 (discovery exercised by migration) + T-010 (acceptance audit) | COVERED |
| G-002 | §L acc-2 | Clean-checkout discovery of app+packages+BoltFFI producer; correct XCFramework ordering; no Jackin literals; cross-fixture proof; build-only accuracy | §B acc-2, §D R-PROD | IMPLEMENTED_UNVERIFIED: #1036/#1043/#1061 merged; fixture proof pending | T-004 + T-010 | COVERED |
| G-003 | §L §6.2, acc-3 | Full invalidation input list; Swift-only reuse; conservative-on-uncertainty | §B acc-3, §D R-PROD | IMPLEMENTED_UNVERIFIED | T-004 + T-006 (invalidation runs) | COVERED |
| G-004 | §L acc-4 | Cold/hit/prefix/absent/corrupt/truncated/wrong-arch/wrong-profile/stale/untrusted behavior; digest verification; trust boundaries; tests execute on hits | §B acc-4, §D R-PROD | IMPLEMENTED_UNVERIFIED | T-006 + T-010 | COVERED |
| G-005 | §L acc-5 | Bounded liveness regression tests (quiet/chatty/no-newline/closed-pipes/descendants/cancel/cleanup/nonzero); no literal-10-minute waits | §B acc-5, §D R-EV | IMPLEMENTED_UNVERIFIED: wall deadline #985 merged; full case list unverified | T-010 (verify case coverage or file gap task) | COVERED |
| G-006 | §L acc-6 | SwiftPM tests, app build, harnesses, drift, bundle/resources/linkage/arch/floor, dSYM, optimized verification, UI checks; build-vs-execution evidence split | §B acc-6, §D R-MIG | IN_PROGRESS: #1044 @`6ff54ce5`, 42 pass, Apple FFI leg red | T-001 → T-004 (green matrix) | COVERED |
| G-007 | §L acc-7 | Hosted macOS green; native-macOS proved by real execution; Debian never Apple executor; missing platform fails aggregate; local/CI parity | §B acc-7, §D R-MIG/R-PROV | BLOCKED (mold) + NOT_STARTED (provider) | T-001, T-004, T-007 | COVERED |
| G-008 | §L acc-8 | Concurrency safety; serial/parallel manifest equivalence; real artifact `needs`; caches never required for correctness | §B acc-8 | NOT_STARTED (no evidence of verification) | T-006 + T-010 | COVERED |
| G-009 | §L acc-9 | Deterministic regen; drift gate; pins/privileges; PR/merge-group/main aggregation; source-scope validation | §B acc-9, §D R-PIN | VERIFIED_DONE (regen/drift/pins via merged PRs); aggregation IMPLEMENTED_UNVERIFIED | T-004 (matrix proof) + T-010 | COVERED |
| G-010 | §L acc-10 | #1013 review disposition incl. Landlock P1; exact-candidate + independent-review gate; anti-counting rule | §B acc-10, §D R-1013 | IMPLEMENTED_UNVERIFIED: merged `9ee50f6a`; thread state unknown | T-011 (verify or record descoping) | COVERED |
| G-012 | DERIVED (session) | Mise provider-facts closure (velnor #1054) | §E.4 VP1054, §D R-PROD | IN_PROGRESS: OPEN CLEAN @`256c24bb` | T-002 (land or defer w/ rationale) | COVERED (flagged derived, not user-explicit) |
| G-020 | §L §5–§9 | Hosted-macOS integration: published runtime, compatible toolchain, generated native graph | §D R-MIG/R-5C, §G | BLOCKED (mold defect, job 106524303464) | T-001 (fix+release) | COVERED |
| G-030 | §L prohibitions | No invented flags/fields/behavior; no hand-edited generated output; no opaque-task rename; no Jackin literals | §B prohibitions, §F D1/R1/R2 | Continuing constraint (0 violations recorded) | Governs T-001…T-011; T-010 re-checks | COVERED |
| G-040 | §L step 4 | ONE Jackin migration line; remove `swift-package-native-ci` + scan exclusions after parity | §D R-MIG, §E.5 | IN_PROGRESS: 3 formulations + #1065 repromote | T-003 (adjudicate) → T-004 | COVERED |
| G-050 | §L §5 | Hosted evidence: main Preview + hosted runs observed | §D R-5C | IMPLEMENTED_UNVERIFIED: CI green, Preview unobserved | T-005 (record runs) | COVERED |
| G-060 | §L §12.2 | 11-scenario benchmark matrix, controlled variables | §B scenarios, §D R-BENCH | NOT_STARTED | T-006 (matrix + report) | COVERED |
| G-061 | §L §12.2 | Reporting set (queue/critical-path/per-phase/transfer/compiler-reuse/coverage), same-contract comparison | §B controls | NOT_STARTED | T-006 | COVERED |
| G-062 | §L §12.2 | ≥3 reps, median/range, no invented p95/speedup | §B controls | NOT_STARTED | T-006 | COVERED |
| G-070 | §L §8 | Native macOS executor proved by real execution, or external blocker demonstrated + capability recorded unsupported | §D R-PROV | NOT_STARTED | T-007 (investigate → prove or record) | COVERED |
| G-080 | §L step 6 | Every preserved line dispositioned; superseded paths removed; docs updated | §D R-CLEAN, §E.5/§E.6 | NOT_STARTED | T-008 + T-009 | COVERED |
| G-090 | §L deliverables | 10 deliverables (links/SHAs, runner matrix, minimal config, contracts, graphs, evidence, disposition, blocker) | §B deliverables, §D R-DOC | PARTIAL: links/SHAs in §E/§G; rest pending | T-010 (completion audit) | COVERED |
| G-100 | §L step 4 | `Read and resolve every review, including the Landlock P1 concern` | §D R-1013 | IMPLEMENTED_UNVERIFIED | T-011 | COVERED |
| G-110 | SRC-DCO | Every commit carries ONLY `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` | §B, §J-5 | Continuing (all goal commits comply) | Governs all resumed commits | COVERED |
| G-111 | SRC-DELEG | Delegate-first orchestration + independent verification | §B, §J-5 | Continuing | Governs resumed execution | COVERED |
| G-112 | SRC-AUTON | Full autonomy, no user questions (except T-011 descoping edge) | §B, §J-5 | Continuing; `continue-until-complete` SUPERSEDED by SRC-PAUSE | Governs resumed execution | COVERED |
| G-113 | SRC-COMMIT | Small verified commits, push regularly, one branch unless technical need | §B, §J-6 | Continuing | Governs resumed execution | COVERED |
| G-114 | §L §13 | Merge-not-rebase; small PRs; exact-candidate gates; AGENTS.md-first; blocked-work protocol; progress doc | §B, §J | Continuing | Governs resumed execution | COVERED |
| H-001 | SRC-PAUSE §4 | Preserve ALL recoverable goal-owned work on durable remote refs | §E.2/§E.3, §A | DONE: 6 refs pushed, verified live | None (T-009 consumes) | COVERED |
| H-002 | SRC-PAUSE §5.E | Exhaustive inventory: repos/clones/worktrees/branches/stashes/PRs with IDs + ownership | §E.1–§E.4 | DONE: 7 repos, 33 checkouts, ledgers verified (2nd audit) | None | COVERED |
| H-003 | SRC-PAUSE §E.5 | Dependency-ordered integration map for every entry | §E.5 | DONE (10 links verified) | Execute at T-009 | COVERED |
| H-004 | SRC-PAUSE §E.6 | Cleanup runbook: per-resource rows, 5 gates, KEEP exclusions | §E.6 | DONE (13 rows + exclusions) | Execute at T-009 | COVERED |
| H-005 | SRC-PAUSE | No merge/cleanup during pause | §A, §E.5/§E.6 headers | DONE (verified: nothing merged/cleaned) | None | COVERED |
| H-006 | SRC-PAUSE §6 | Draft handoff PR, GOAL: title, no auto-merge | §A, §E.4 VP-HO | DONE: #1066 DRAFT, autoMerge null | Close at T-010 | COVERED |
| H-007 | SRC-PAUSE §5.J | Resume runbook + exact `/goal Read and resume` command | §J | DONE (fresh-reader verified executable) | None | COVERED |
| H-008 | SRC-PAUSE §5/§9 | Fresh-agent sufficiency without session access | §B+§L+§M, §N audit | DONE after audit (§L embedded, §M added) | None | COVERED |
| H-009 | SRC-AUDIT §10–11 | Audit record + evidence-based outcome | §N | DONE (this audit) | None | COVERED |
| H-010 | SRC-PAUSE §B/H | Post-resumption integration + cleanup obligation | §B, §H T-009 | Documented, NOT executed (correct) | T-009 | COVERED |

Superseded: `Continue working until the goal is fully completed` (SRC-AUTON) — superseded by
SRC-PAUSE for this session (resume only on explicit user request). Nothing else superseded;
no contradictory instructions remain simultaneously active.

## Appendix N. Audit-and-repair record (this audit)

- Audit time (UTC): 2026-09-21, began ~22:31Z (coordinator clock; exact commit times in git).
- Auditors (all read-only, terminal): af-intent/66, af-state/67, af-integ/68, af-fresh/69.
- Source coverage: SRC-GOAL FULL (embedded §L); SRC-DCO/SRC-DELEG/SRC-AUTON/SRC-COMMIT/
  SRC-PAUSE/SRC-PERF FULL (line refs in §B register); worker STOP reports SECONDARY
  (operative claims corroborated by live `gh` evidence); R3/R4 tail per-branch detail
  PARTIAL (branches retained locally, re-enumerable; K3).
- State re-verification: ZERO drift on mains/PRs/checks (both mains unmoved, no new goal
  PRs, #1044 still 3-fail mold, #1054 still CLEAN, preserve refs at recorded SHAs).
  Post-handoff evolutions recorded, not contradicting: #1065 run finished (same 3-fail),
  #1066 CI red-as-expected, W1 moved by publication.
- Material omissions found and repaired:
  1. R-1013 wrongly VERIFIED_DONE as "off-goal" → downgraded to IMPLEMENTED_UNVERIFIED +
     T-011 (Landlock P1 evidence or descoping authority). [MAJOR]
  2. Full goal text local-only → embedded verbatim §L (101,314 chars, boundaries verified).
  3. Squash-merge misattributed as user steer → reattributed to session practice (F25).
  4. Merge-not-rebase misfiled as separate steer → reattributed goal-internal (F26).
  5. Truncated executor-contract quote → restored 2 dropped sentences (F1/F2).
  6. 8 normative prohibitions + evidence anchors + full acceptance/benchmark/deliverables
     added to §B (F4–F21).
  7. Amendments expanded with provenance + supersession note (F22–F24, F27).
  8. W1 row, E.4 heads (1055/56/63), VP1052 DIRTY, 8 unlisted PRs, W13 dirt content,
     dangling §refs, clock-skew timestamp corrected.
  9. §H → T-001…T-011 with requirement links + concretion (rationale/report files, T-007
     bounded investigation, T-011 descoping rule).
  10. VP-HO red disclosed (READY ≠ green).
- Unresolved gaps: NONE blocking. Residual limitations (documented, not gaps): K3 (tail
  re-enumeration on resume), K4 (#1065 fetch), K5 (line re-verification), K6 (flake),
  K7 (review debt), A1–A4 (open hypotheses with decision paths).
- Fresh-reader re-verification: post-repair doc answers goal/amendments/state/recovery/
  order/first-action/gates from doc alone (only §L now in-doc; prior gaps closed).
- Final audit outcome: **VERIFIED**. Authoritative source coverage sufficient (full goal +
  all steers recovered verbatim with provenance); all operative requirements actionably
  documented in §M; preservation/resume/publication re-verified live; no unresolved
  handoff-quality gaps remain. (Engineering work itself remains paused, not complete.)



