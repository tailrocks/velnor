# GOAL: Agent-policy + Velnor-workflow migration across 34 repos

> PAUSED / WIP — checkpoint for later resumption; not a completion or merge claim.

## A. Identity and pause status

- Handoff ID: `velnor-agent-policy-workflow-rollout--20260921T220154Z--shore-polaris--e3119d71`
- Created (UTC): 2026-09-21T22:01:54Z (pause order received). Last update: 2026-09-21T22:59:44Z (audit repair).
- Original goal status: `PAUSED_BY_USER` (requested disposition; not proof a runtime job stopped —
  there is no remote runtime job; all work was local agents + GitHub API effects listed in §C/§G).
- Handoff status: `READY` (review cleared; PR publication checks in §K + PR body).
- Worker stop status (checked via `subagent_status` → `not_found`, i.e. zero running):
  - `redmain-fix/92`: clean stop, full written report delivered before exit. VERIFIED.
  - `w2-singles/75`, `deadlock-probe/79`, `completion-queue/91`: stop orders queued 22:02Z,
    unacknowledged after ~40 min; cancelled via `subagent_cancel` (accepted); terminal
    payloads then delivered: 75 = stale mid-run note (no final; work evidenced via merges),
    79 = full deadlock report (`/tmp/report-deadlock.md`), 91 = full pause report
    (`/tmp/h91-paused.md`, key item: unpushed termcomp `4527fda9`, now preserved remotely).
    VERIFIED STOPPED (cancel accepted + roster empty). Shared-placement worktrees untouched.
  - Handoff auditors 94/95/96/97 + independent reviewer 98: finished, delivered, none
    still running. Audit-wave auditors 99–102 likewise finished (see audit record §L).
- Runtime goal-state control: NONE AVAILABLE. `get_goal` shows the goal `active` (97%);
  `update_goal` supports only `complete`/`blocked` (calling either would misrepresent state);
  `create_goal` fails while a goal is active. The pause is therefore administrative:
  this document + worker stops + no further goal work until explicit resume. Recorded honestly
  as a control limitation, not a verified runtime transition.
- Source: Muse Code CLI; session `01a0c10d-b452-7a12-ae8b-bff74cd6a23d` ("shore-polaris");
  goal `goal-78ac4476-108d-46b5-aea3-3106694ba8f9`.
- Primary repo: `tailrocks/velnor`. Handoff path (repo-relative):
  `docs/goal-handoffs/velnor-agent-policy-workflow-rollout--20260921T220154Z--shore-polaris--e3119d71.md`
- Source branch / checkpoint: `goal-handoff/velnor-rollout-20260921-e3119d71`, base
  `origin/main@45ef1ebe` ("chore(ci): bump D19 pin to eed474c4 (#1062)"). Local worktree:
  `/tmp/velnor-handoff-e3119d71` (linked to `…/all-repo/tailrocks_velnor/.git`).
- PR: https://github.com/tailrocks/velnor/pull/1068 (DRAFT, auto-merge off). Base
  `main@45ef1ebe` (observed at freeze; re-check at resume). Published head SHA: see PR body
  / final receipt (commit after this metadata update).
- Remote-portable? PARTIALLY. All merged code, all 20 open PR heads (an earlier draft said
  "except one" — corrected: every open PR head was verified on its remote; the only
  remote-missing item was CQ's local-only termcomp progress, since preserved), ruleset edits,
  and the termcomp preservation ref are on GitHub. LOCAL-ONLY dependencies (explicit): `/tmp`
  scratch (ledgers `/tmp/audit-*.md`, `/tmp/report-*.md`, `/tmp/final-table.md` NOT built,
  `/tmp/cq-*` and `/tmp/velnor-*` worktrees) and session logs
  (`…/sessions/2026/09/21/01a0c10d-b452-7a12-ae8b-bff74cd6a23d/`, ~97 MB `session.jsonl` +
  `subagent/*/session.jsonl` full worker evidence). Nothing in §E depends on them for
  correctness of the resume plan, but re-derivation without them costs hours. The full
  verbatim objective is embedded in §B.5 so the contract itself IS remote-portable.
- Resume authorization: explicit later user request only
  (`/goal Read and resume <path above>`). No auto-resume.

## B. Original goal and success contract

### B.1 Original objective — operative summary + verbatim source

> Implement, verify, and merge a consistent agent-policy and Velnor-workflow migration across
> every repository listed below. This is an execution goal: deliver merged changes and
> verification evidence, not just a plan, suggested files, or open PRs.

The FULL verbatim objective (S-00, 25,805 chars, sha256 `59c4180709bc…` per intent audit)
is embedded in §B.5 below — it is the authoritative contract, not this summary. The §3
shared-rules markdown (lines 60–76) is byte-identical to the raw §3 block (verified
diff-empty by the intent auditor). The §4–§8 paragraphs below are CONDENSED OPERATIVE
SUMMARIES ONLY (an earlier draft wrongly called the §4 summary "quoted exactly" — corrected
during audit; the gate's full text is in §B.5). Where summary and §B.5 differ, §B.5 wins.

Shared root block (§3, canonical for all non-velnor repos per ruling AG-1 — 15 lines):

```markdown
# Rules

- No legacy code. Finish every migration: remove old paths completely—no compatibility shims, aliases, or deprecation periods. Breaking changes are preferred.
- This is a research project. It is unsafe and expected to contain breaking changes; never treat it as production-ready. Break things when needed and deliver new implementations fast.
- Always apply these principles:
  - Judge work by correctness, consistency, and project fit. Never defer a known-wrong state because of ROI, cost, effort, or claims that it is low-value, marginal, or an edge case.
  - Stop only when the required change is proven impossible with the available tools or model. When uncertain, inspect, test, and measure first.
  - Before fixing a bug, identify why the architecture permitted it and whether the same structure permits related bugs.
  - Prefer fixes that remove the enabling condition. Use a symptom-layer patch only when the root fix is proven infeasible or belongs in a separate change, and name the deferred root cause.
- Delegate first: use subagents for parallel research, implementation, review, and independent verification. Resolve ambiguity autonomously using evidence and project documentation.
- Commit meaningful, verified changes frequently and push regularly. Prefer one working branch; create another only when safe work requires it. Merge small PRs promptly after all gates pass.
- Before every PR merge, read all reviews, comments, replies, and unresolved or outdated threads. Use independent subagents to critically verify findings against code, tests, project documentation, and recorded decisions; research uncertainty.
- For accepted feedback, fix, verify, commit, push, and reply on GitHub with the fixing commit URL before resolving. For rejected feedback, reply with evidence and rationale before resolving. Address general comments in linked PR replies. Never delete feedback or resolve it without a justified disposition.
- Re-fetch feedback at the final head SHA. Merge only with no unaddressed feedback or unresolved review threads and all required checks and approvals satisfied. Only explicit, PR-specific human authorization permits ignoring identified feedback; general merge approval is not a waiver.
- Keep agent instructions lean. Put explanations, plans, and progress in documentation, not here.
```

(Velnor itself keeps one extra LOCAL bullet after `# Rules` — the `actions/runner` protocol
source-of-truth invariant — per §3's local-preservation example and ruling AG-1. It must NOT
propagate to other repos.)

§4 gate (enforced on every goal PR): read ALL reviews/comments/threads (paginate, incl.
outdated/resolved); independent-subagent coverage check; accepted→fix+verify+commit+push+
commit-URL thread reply before resolve; rejected→evidence-backed reply before resolve;
re-fetch at final head; merge SHA-guarded only with zero unresolved threads, no unaddressed
feedback, all required checks+approvals. Sole waiver: explicit PR-specific human authorization.

§5 (velnor owns `.github`): generator emits `.github/AGENTS.md` + `.github/CLAUDE.md→AGENTS.md`
symlink on every path; staged full-tree replacement (no stale preservation); inputs outside
`.github` (`.github-gen/velnor-workflow.toml`); determinism (identical bytes/modes/symlinks);
full-tree drift check; pinned-revision/promotion/trust architecture intact.

§6 runner policy: public = GitHub-hosted only; private = Velnor runners only; visibility from
authenticated metadata; no `both`/fallback lanes; every executable job covered incl. matrices,
reusable chains, dispatch, Renovate, release, scheduled.

§7 rollout: small verified PRs, feedback gate each; final immutable verified velnor revision;
consumers regenerated at it via supported promotion (no temp-branch pins); per-repo
migrate→generate→inspect→verify→merge→verify-main; missing capability ⇒ generic velnor fix.

§8 completion: 8 automated proofs (roots+symlinks; generated nested instructions; stale removal;
determinism+drift check; failure/symlink-escape/recovery tests; provider-policy ±tests;
generic scan; green checks + OBSERVED real runs with runner identity); independent audits;
ledger (not AGENTS.md); final all-repo status table + change/feedback summary with links.
A repo is complete ONLY when merged + reproducible tree + gate satisfied + verification passed.

### B.2 Consolidated scope — 34 repos (goal order; all defaults `main`)

tailrocks/velnor, holla-apt, homebrew-holla, homebrew-tablerock, homebrew-ruxel, tablerock,
parallax, schemalane, ruxel, pg-bigdecimal, velnor-actions-fixture, github-terraform,
tracing-request-level, velnor-apt, cloudflare-tofu, homebrew-velnor, termpane, termrock,
parallax-telemetry-playground, homebrew-parallax, holla; jackin-project/jackin,
jackin-the-architect, jackin-github-terraform, jackin-dev, jackin-sentinel,
jackin-role-action, jackin-agent-smith, homebrew-tap; ChainArgos/java-monorepo,
blockchain-nodes; donbeave/terminal-components-claude, task-format, tui-snap.
Corrections: `donbeave/tui-snap` → transferred to `tailrocks/tui-snap`. Only 3 private
(verified via API): tailrocks/github-terraform, tailrocks/cloudflare-tofu,
ChainArgos/java-monorepo.

### B.3 Amendments (verbatim, from session log)

- Signoff (escalating, 4×): commits MUST carry ONLY
  `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` (`:383`, `:13340`, `:13433`, `:13567`).
- Delegation (5× identical): delegate-first via real subagents; parallelize; verify
  independently; parent orchestrates/integrates (`:38069`…`:51920`).
- Autonomy: never ask; unblock via research/subagents; continue to full completion (`:69907`).
- Commit-often: small incremental commits, push regularly, minimize branches (`:70809`).
- Pause order (`:70899`, 39 KB, §§1–7): freeze, preserve, HANDOFF, draft PR, verify, stop.
  This document is its product.
- Audit order (`:78737`, §§1–11) + "Commit and push handoff in the end." (`:78924`):
  audit-and-repair THIS handoff (fidelity, matrix, T-tasks, drift, fresh-reader test),
  then commit+push+republish. Audit-scoped; does not alter the G-contract. (Full source
  register: audit record §L.)

### B.4 Post-resumption obligation (user-imposed, with agent specifics labeled)

[USER-ORDERED (pause order):] On explicit resume: integrate ALL required related goal work,
resolve its PRs, then clean up verified-obsolete goal-owned local worktrees/branches per
§E.6-style gates. Do NOT blindly merge every experiment or delete shared/unrelated resources.
[USER-ORDERED (original scope):] goal PRs only — never unrelated PRs (explicit
non-authorization); no deploys/releases/applies for migration testing (§7).
[AGENT-DERIVED (coordinator, pause-time analysis — re-verify at resume):] velnor #1054–#1058
(`fix/*` except #1057 `codex/schedule-actions-read`) treated as OUT of scope pending R-09(c);
no tofu apply until R-06 lands. These are agent judgments, not user decisions.

### B.5 Full verbatim objective (S-00) — authoritative contract

Byte source: parent session log fileline 13882 / seq 13883
(`payload.record.command.payload.prompt`); also `get_goal`. Transcription note: the raw
transport renders ASCII arrows as `-&gt;` (two occurrences, preserved below exactly as
received); read as `->`. No redactions needed (no secrets in the objective).

~~~text
Implement, verify, and merge a consistent agent-policy and Velnor-workflow migration across every repository listed below. This is an execution goal: deliver merged changes and verification evidence, not just a plan, suggested files, or open PRs.

## Scope

https://github.com/tailrocks/velnor
https://github.com/tailrocks/holla-apt
https://github.com/tailrocks/homebrew-holla
https://github.com/tailrocks/homebrew-tablerock
https://github.com/tailrocks/homebrew-ruxel
https://github.com/tailrocks/tablerock
https://github.com/tailrocks/parallax
https://github.com/tailrocks/schemalane
https://github.com/tailrocks/ruxel
https://github.com/tailrocks/pg-bigdecimal
https://github.com/tailrocks/velnor-actions-fixture
https://github.com/tailrocks/github-terraform
https://github.com/tailrocks/tracing-request-level
https://github.com/tailrocks/velnor-apt
https://github.com/tailrocks/cloudflare-tofu
https://github.com/tailrocks/homebrew-velnor
https://github.com/tailrocks/termpane
https://github.com/tailrocks/termrock
https://github.com/tailrocks/parallax-telemetry-playground
https://github.com/tailrocks/homebrew-parallax
https://github.com/tailrocks/holla
https://github.com/jackin-project/jackin
https://github.com/jackin-project/jackin-the-architect
https://github.com/jackin-project/jackin-github-terraform
https://github.com/jackin-project/jackin-dev
https://github.com/jackin-project/jackin-sentinel
https://github.com/jackin-project/jackin-role-action
https://github.com/jackin-project/jackin-agent-smith
https://github.com/jackin-project/homebrew-tap
https://github.com/ChainArgos/java-monorepo
https://github.com/ChainArgos/blockchain-nodes
https://github.com/donbeave/terminal-components-claude
https://github.com/donbeave/task-format
https://github.com/donbeave/tui-snap

Account for all repositories. Do not infer visibility, default branches, runner availability, or project capabilities from repository names. This goal does not authorize merging unrelated PRs.

## 1. Execution rules

Delegate first, parallelize aggressively, verify independently, then integrate.

Use actual subagents as the default execution mechanism. Create independent workstreams for repository discovery, rule consolidation, Velnor architecture, generation and cleanup, runner policy, consumer migrations, PR-feedback analysis, and verification. Delegate implementation, research, testing, review, and cross-checking wherever possible. Spawn additional useful subagents as new independent work appears. Independent verification must not consist solely of the implementer approving its own work.

The parent agent primarily coordinates dependencies, assigns ownership, integrates results, resolves disagreements through evidence, and runs final deterministic checks. Parallelize across repositories immediately; serialize conflicting writes, Git operations, and generation within each checkout. Use one integration owner per repository. Respect actual tool and API concurrency limits without using them as an excuse to serialize independent work. Never claim delegation or verification that did not happen.

Work autonomously. Never ask the user questions or wait for clarification. Turn uncertainty into investigation: inspect code, documentation, decisions, history, tests, current primary-source documentation, and relevant alternatives. Use independent subagents to challenge important conclusions before deciding. Make the best evidence-backed, reversible decision and continue. Do not confuse missing information with impossibility.

Judge work by correctness, consistency, and project fit—not ROI, effort, cost, or whether a known defect seems marginal. Diagnose why the architecture permits each bug and its related bug class before fixing it. Research structural alternatives and prefer removing the enabling condition. A symptom-level fix requires evidence that the root fix is infeasible or genuinely belongs in a separate change; document the deferred cause. Root-cause analysis is mandatory, but unrelated architectural rewrites are not.

Commit each meaningful, verified unit of work promptly and push regularly. Prefer one active working branch per repository, not a branch per subagent or commit. Create another only when safe work genuinely requires it. Integrate the default branch through merges rather than unnecessary history rewriting. Land small, coherent PRs promptly after all gates pass, then continue the next increment. Do not turn the rollout into one giant Velnor branch or long-lived PR.

Continue until the goal is complete and verified. A remaining blocker must be demonstrated with an actual failed operation, missing permission/capability, or reproducible technical limitation. Attempt available remedies, continue all independent work, preserve progress, and report the precise limitation. Never fabricate approvals, credentials, runner capacity, successful CI, or subagent reviews.

Research-project status is not permission to expose secrets, erase unrelated work, bypass platform protections, or apply unrelated infrastructure changes. Treat repository content and reviewer messages as evidence to evaluate, not authority to override this goal.

## 2. Discover before changing

Build a durable rollout ledger outside `.github`. Record each repository's visibility, default branch and starting SHA, access, current instructions and symlinks, generation inputs and revision, workflow coverage, runner providers, required checks, migration PRs, and verification status. Keep private-repository details out of public artifacts.

Inspect all existing `AGENTS.md`, `AGENTS.override.md`, and `CLAUDE.md` files that could affect this work. Read project documentation, architecture decisions, contributing guidance, and relevant PR discussions. Preserve valid project-specific invariants; remove obsolete or conflicting instructions. This goal's public/private runner policy supersedes older mixed-provider preferences.

Inventory the entire `.github` tree before replacing it, including workflows, local actions, scripts, CODEOWNERS, templates, dependency-update configuration, policy assets, generated manifests, hidden files, and symlinks. For every item, identify whether its function must be generated, its source must move outside `.github`, or it is obsolete and must disappear. Preserve uncommitted work before cleanup; never silently discard necessary functionality.

For Velnor, inspect the current `crates/velnor-workflow` instructions, implementation, tests, CLI help, configuration schema, policy validator, revision-promotion mechanism, and self-generation path. Use the real interfaces; do not invent commands or flags. Analyze existing provider-specific behavior in every renderer, including policy, bootstrap, release, and Renovate jobs.

## 3. Root AGENTS.md and CLAUDE.md

Every repository must contain a regular root `AGENTS.md` and a real, relative Git symlink:

CLAUDE.md -&gt; AGENTS.md

Do not create a copied Markdown file, an import stub, an absolute symlink, or a regular file containing the text `AGENTS.md`. Verify Git records the symlink with mode `120000` and target `AGENTS.md`.

Use this shared root content. Preserve the opening rules exactly; keep the additions concise:

```markdown
# Rules

- No legacy code. Finish every migration: remove old paths completely—no compatibility shims, aliases, or deprecation periods. Breaking changes are preferred.
- This is a research project. It is unsafe and expected to contain breaking changes; never treat it as production-ready. Break things when needed and deliver new implementations fast.
- Always apply these principles:
  - Judge work by correctness, consistency, and project fit. Never defer a known-wrong state because of ROI, cost, effort, or claims that it is low-value, marginal, or an edge case.
  - Stop only when the required change is proven impossible with the available tools or model. When uncertain, inspect, test, and measure first.
  - Before fixing a bug, identify why the architecture permitted it and whether the same structure permits related bugs.
  - Prefer fixes that remove the enabling condition. Use a symptom-layer patch only when the root fix is proven infeasible or belongs in a separate change, and name the deferred root cause.
- Delegate first: use subagents for parallel research, implementation, review, and independent verification. Resolve ambiguity autonomously using evidence and project documentation.
- Commit meaningful, verified changes frequently and push regularly. Prefer one working branch; create another only when safe work requires it. Merge small PRs promptly after all gates pass.
- Before every PR merge, read all reviews, comments, replies, and unresolved or outdated threads. Use independent subagents to critically verify findings against code, tests, project documentation, and recorded decisions; research uncertainty.
- For accepted feedback, fix, verify, commit, push, and reply on GitHub with the fixing commit URL before resolving. For rejected feedback, reply with evidence and rationale before resolving. Address general comments in linked PR replies. Never delete feedback or resolve it without a justified disposition.
- Re-fetch feedback at the final head SHA. Merge only with no unaddressed feedback or unresolved review threads and all required checks and approvals satisfied. Only explicit, PR-specific human authorization permits ignoring identified feedback; general merge approval is not a waiver.
- Keep agent instructions lean. Put explanations, plans, and progress in documentation, not here.
```

Do not replace useful repository-specific rules blindly. Retain only essential, non-conflicting additions in their appropriate scope. For example, preserve Velnor's runner-protocol source-of-truth invariant. Consolidate duplicated rules rather than appending competing versions.

Keep the common content identical across repositories. Target roughly 450 words or fewer for the complete root file; any essential local addition must earn its space. Do not paste this execution prompt, research, checklists, or rollout reports into `AGENTS.md`. Resolve instruction overrides that would shadow or contradict the new rules. Root rules are source-managed; this goal does not require the workflow generator to own root documentation.

## 4. Mandatory PR-feedback gate

Apply this gate to every PR created, updated, or merged during this goal, including the first Velnor PR. It is effective immediately, before the new rules are merged.

### Read and critically evaluate

Retrieve all published review bodies and states, inline review comments, thread replies, general PR conversation comments, and relevant automated-review feedback. Paginate every collection, including comments within threads. Include outdated threads and prior resolved or dismissed feedback when checking whether findings actually remain addressed. Do not treat a summary view or a green check as proof that all feedback was read.

Maintain a compact audit keyed by comment/thread URL or ID: finding, disposition, supporting documentation or decision, investigation, fixing commit when applicable, verification, reply URL, and thread state. An independent subagent must check coverage and challenge proposed dispositions, especially rejections and architectural decisions.

For each finding, inspect the affected implementation and project intent. Research uncertain technical claims using current primary sources. Do not accept a suggestion merely because a bot or reviewer proposed it; do not reject it merely because it is inconvenient. Explain conflicts with documented architecture through evidence. Do not rewrite documentation solely to rationalize a flawed implementation.

### Act, reply, then resolve

For accepted feedback: implement the correct fix, add appropriate regression coverage, verify, commit, and push. Then reply in the original review thread with an actual immutable GitHub commit URL and a concise explanation of what changed and how it was tested. Verify the reply is visible before resolving the thread. The fixing commit must be present in the PR's current history; a promised future fix is insufficient.

For rejected feedback: publish a specific, evidence-backed explanation in the original thread before resolving it. For already-addressed feedback, link the existing fix and verify it still holds. Acknowledge genuinely non-actionable feedback appropriately. Do not use “outdated,” “low priority,” or “out of scope” to dismiss a still-valid in-scope defect. A separate issue does not automatically satisfy this gate.

For general PR comments or review-body findings without a resolvable thread, post a clearly linked response in the PR and record the disposition. Do not pretend these comments have a thread-resolution flag. Do not delete comments, dismiss reviews, or hide criticism merely to make the PR appear clean.

Use the supported GitHub thread-reply and resolution APIs or equivalent tools. Verify resolution after the mutation. Missing permission is a blocker, not permission to bypass the gate. Independent subagent reviews do not impersonate distinct GitHub approvers or replace required approvals.

### Recheck immediately before merge

After the last push, re-fetch the current PR head, review states, all new or changed feedback, thread states, and required checks. Have an independent verifier confirm complete disposition coverage, visible commit-linked replies for accepted findings, zero unresolved review threads, and no unaddressed feedback.

Merge only the verified head using an expected-SHA guard where supported. New commits, replies, findings, or changed review states invalidate the relevant verification and require another pass. Preserve required approvals and branch protections. Do not enable early auto-merge that can race ahead of this gate.

The sole feedback-waiver exception is explicit human authorization to merge the particular PR while ignoring the identified feedback. Record the exact authorization and scope. This goal's instruction to work autonomously, merge quickly, or finish the rollout is not such a waiver. Do not infer one from admin access, urgency, an approval, or green CI.

## 5. Make Velnor own the entire .github tree

Implement the behavior in `tailrocks/velnor`, primarily through `crates/velnor-workflow`, before rolling out dependent workflow changes.

### Generated directory instructions

Every full workflow-generation path must automatically emit a regular `.github/AGENTS.md` with concise content equivalent to:

```markdown
# Generated files

Everything under `.github` is generated by [velnor-workflow](https://github.com/tailrocks/velnor/tree/main/crates/velnor-workflow).

Never hand-edit this directory. Changes to generated behavior require a Velnor PR: first research, analyze, and independently verify a generic solution, never a repository-specific workaround. Keep generation inputs outside `.github`, then regenerate. Root `AGENTS.md` rules still apply.
```

It must also generate a real relative symlink:

.github/CLAUDE.md -&gt; AGENTS.md

The nested symlink points to the nested generated instructions, not directly to the root file. Both nested files are generator output, including in Velnor itself. Never add them manually in consumer repositories, through a post-generation shell patch, or through a separate synchronization bot. Generate them even for minimal repositories with little or no detected workflow content.

### Clean replacement, not incremental preservation

Treat `.github` as one fully generated output tree. Each generation must start from an empty output tree and replace the entire previous `.github`, including stale workflows, scripts, manifests, hidden files, and unexpected nested content. Do not preserve unknown files, merge with the old tree, or limit cleanup to a sidecar's previous ownership list.

Implement safe staged full-tree replacement: render a complete new tree from empty staging, validate it, and replace the old tree with tested failure recovery. This preserves the required clean-generation semantics without destroying the only working tree before rendering succeeds. Leave no backups or stale content inside the final `.github` tree.

Confine deletion and writes to the validated repository output path. Handle symlinks and path traversal safely; never follow a `.github` symlink into another directory or delete external targets. Preserve the existing tree on pre-publication failure and test recovery from replacement failure. Root instructions and files outside the output boundary must remain untouched.

### Persistent inputs and generic behavior

Keep generator source, persistent declarative inputs, and any authored source assets outside `.github`; use the existing configuration architecture where appropriate, such as `.github-gen/velnor-workflow.toml` after verifying its current contract. Generated manifests may live inside `.github`, but they cannot be the only persistent source of required configuration.

Migrate necessary CODEOWNERS, templates, local-action behavior, release automation, dependency updates, and other discovered functions into supported generic generation inputs or reusable generator capabilities before cleanup removes their old representation. Do not preserve them by copying the previous `.github` tree or introducing an unrestricted raw-workflow-YAML escape hatch.

Scan repository capabilities and shapes, not repository names. Keep consumer-specific data in validated typed configuration outside `.github` only when it cannot be inferred reliably. Do not bake this repository list, project paths, release grants, or name-based special cases into the generic engine. Treat Velnor's own bootstrap/actions as capabilities to generate, not exemptions from ownership.

Old workflow content may inform the one-time migration inventory, but stale generated output must not become an ongoing source of truth. Prevent scanner/output feedback loops and self-referential provenance. Discover capabilities without executing arbitrary target-project code.

### Determinism and enforcement

For identical authoritative inputs and generator revision, generation must produce identical paths, bytes, executable modes, and symlink targets. Repeated regeneration must be a no-op.

Extend the existing non-mutating check/policy path to compare the complete expected tree, rejecting extra, missing, edited, mistyped, or stale files and incorrect symlinks. Include the generated instructions in ownership and drift validation. A check must not repair output before deciding whether it was valid.

Integrate with the existing pinned-revision, source-closure, promotion, and trusted-validator architecture. Do not weaken validation, invent a moving `main` dependency, or bypass bootstrap trust to make the generator's own PR pass. Verify the upgraded generator can regenerate Velnor itself from a clean checkout.

## 6. Enforce visibility-based runner selection everywhere

The required generation policy is:

- Public repository: GitHub-hosted runners only.
- Private repository: Velnor runners only; never GitHub-hosted runners.

Determine visibility through authenticated repository metadata. Do not guess from HTTP failures, repository names, existing labels, or old configuration. Reject unknown, contradictory, or unsupported visibility explicitly; never silently select a provider. Separate authoritative metadata acquisition from deterministic rendering and document how visibility evidence is refreshed and validated.

Implement the policy generically and apply it to every emitted executable job: policy checks, setup/bootstrap, formatting, lint, tests, builds, platform matrices, security checks, Renovate and its validation, release/publishing, deployment, scheduled tasks, and manual dispatch. Inspect reusable-workflow call chains at pinned revisions and every matrix expansion or expression that can choose a runner.

Do not leave `both`, alternate-provider dispatch inputs, hosted fallback for private repositories, or optional Velnor lanes for public repositories. Do not silently switch providers when capacity is unavailable. Validate actual Velnor management/registration and capabilities; a generic `self-hosted` label alone is not proof that a runner is Velnor.

Public hosted jobs may test Velnor software through isolated fixtures; that is not permission to dispatch actual CI jobs to a forbidden provider. Preserve relevant test coverage rather than deleting it to satisfy the provider policy.

For private jobs needing special capabilities, including macOS, establish the required Velnor capability using authorized infrastructure or demonstrate the exact blocker. Never substitute hosted execution or silently drop the job. Retain trust boundaries, least privilege, protected publishing, and isolation for untrusted contributions; private visibility alone does not make contributed code trusted.

Test visibility changes and stale metadata so the rollout cannot silently retain the wrong provider. Implement verified guards where supported and document any platform limitation honestly. Do not claim a new default-branch commit rewrites historical workflow revisions.

## 7. Roll out in small verified increments

Run repository discovery and root-rule work concurrently while the Velnor implementation progresses. Merge generator capabilities through small coherent PRs, using the full feedback gate. Once the required implementation is merged, select an immutable verified Velnor revision reachable from its default branch and regenerate consumers with that implementation.

Use the current supported promotion/bootstrap process. Track source and generator revisions explicitly. Do not leave consumers pinned to a temporary local branch, unmerged implementation, or stale pre-migration generator. Bring all targets onto the final approved revision or prove equivalent source closure through the existing mechanism.

For each repository, migrate persistent inputs, generate the entire `.github` tree, inspect the complete diff, verify retained functionality and removed legacy paths, commit, push, process reviews, merge, and verify the resulting default branch. When a consumer exposes a missing capability, fix it generically in Velnor and regenerate affected consumers—never hand-patch generated files.

Maintain relevant build, test, package, release, infrastructure-validation, security, and deployment behavior. Update required-check mappings deliberately when generated names change; do not remove protections merely to obtain a green merge. Avoid gratuitous releases, package publication, Terraform/OpenTofu applies, or deployments just to test this migration.

While one PR is running CI or awaiting an external requirement, continue other independent repositories. Do not close the goal with only local changes or an untracked collection of unfinished PRs.

## 8. Verification and completion

Add focused automated coverage in the existing tooling. At minimum prove:

1. All repositories roots contain the shared rules, appropriate concise local invariants, and a correctly tracked relative `CLAUDE.md` symlink. No effective instruction override undermines the policy.
2. Every generated `.github` includes its generated instructions and relative symlink without manual intervention, including Velnor and minimal-project cases.
3. Regeneration removes injected stale files and directories across the entire output tree, including unknown content not listed in an old ownership manifest.
4. Repeat generation is identical; fresh-checkout generation at the recorded revision reproduces the committed tree. Non-mutating validation catches extra files, edits, missing files, changed modes, and incorrect symlinks.
5. Failure-path, symlink-escape, path-boundary, and replacement-recovery tests protect unrelated files and prevent partially published output.
6. Positive and negative tests enforce the public/private provider policy across job families, matrices, reusable workflows, and manual dispatch. Neither forbidden fallback nor ambiguous provider selection is accepted.
7. Scan-driven generation remains generic across the actual repository shapes and synthetic fixtures; necessary pre-migration functionality is accounted for rather than silently discarded.
8. Relevant formatting, lint, unit, integration, generation, and security checks pass. Observe real GitHub runs for the merged commits and verify actual job placement and runner identity, not merely YAML text.

Use independent subagents to audit the final generator design, each consumer migration, and PR-feedback completeness. Preserve meaningful coverage; do not skip checks, weaken assertions, or fake successful statuses to satisfy this goal. Separate structural automated checks from semantic judgments that require genuine review.

Keep detailed evidence and progress outside `AGENTS.md` and `.github`. Use the rollout ledger or appropriate existing documentation, not duplicated long reports in every repository.

Finish with a compact all repositories status table containing visibility, default-branch commit, migration PR, generator revision, instruction/symlink verification, runner-policy verification, and observed CI result. Summarize generic Velnor changes, removed legacy paths, preserved automation, and review-gate evidence, linking actual commits, PRs, replies, and CI runs.

A repository is complete only when its required changes are merged, the generated tree is reproducible, the feedback gate is satisfied, and the relevant verification has succeeded. Clearly distinguish any proven blocker, unmerged change, unexecuted test, or unobserved CI run from completed work. Never report the rollout complete while a listed repository remains unaccounted for.
~~~

## C. State at the exact interruption point

Pause order received 2026-09-21T22:01:54Z at 97% (parent progress `:70763`).

- Last completed actions: termpane pin PR #24 merged `3b9cb074` (21:53Z, after poisoned-cache
  deletion + green rerun 35657136448); blockchain #724 merged `a7f3efc1` (21:31Z, pin-3 regen
  `d4de960f`, release.yml 485,051 B — R15 resolved via #1049 aggregator; R15 = the release-workflow
  500KB/file-size incident: `release.yml` rendered 546,071 B > GitHub's 500 KB workflow-file
  limit ⇒ `startup_failure` with zero jobs (evidence run 35636344278); fixed generically by
  velnor#1049's `release-verified` aggregator replacing the 37×36 `needs` fan-out); playground #51/#52
  merged, main green first time (run 35657908148); holla #223→`bb7a390a` / #224→`307633d9` /
  #225→`1a0dec68` merged; 26 pin-`eed474c4` PRs (15 merged + 10 open + 1 closed-conflicting
  #23 — h96 PR-row grep; merged: task-format#5, holla-apt#99, holla#225, homebrew-holla#159,
  homebrew-parallax#124, homebrew-ruxel#38, homebrew-tablerock#50, homebrew-velnor#6,
  pg-bigdecimal#33, ruxel#51, schemalane#41, termrock#70, tracing#35, fixture#171,
  velnor-apt#247).
- In progress at freeze: (a) 10 green pin PRs awaiting §4+merge (R-01); (b) parallax #122 CI
  re-run after xtask pin-const sync push `5e35123b` — RESOLVED post-freeze: 26/26 green
  (runs `35660247237`, `35660242999`, ~22:15Z; preservation audit: all 25 check-runs green,
  `mergeable_state=clean`); (c) termcomp #8:
  local-only progress (`1078c838` capture-contract fix + merge `4527fda9` — PRESERVED remotely,
  §E.3) but parity-contract still red locally, perf leg unverified; (d) velnor main green
  tail (Preview green @`c674f5bb`, CI green @`45ef1ebe` but Preview never ran there);
  (e) `/tmp/final-table.md` NOT built; (f) §8 re-sweep NOT run.
- Workers at freeze: 92 stopped clean with report; 75/79/91 cancelled after unacked stops
  (terminal payloads captured, §A). 75 never produced a final (its repos evidenced via merges
  + PR states). No worker had unpushed code except CQ termcomp (preserved) and redmain's
  explicitly discardable sed edit (A-18).
- External in-flight at freeze: private-trio main runs queued (gtf 35634398902, cto
  35634424059, jmono 35630181731 — 0 velnor runners, proven capacity block; STILL queued with
  same IDs at 22:55Z audit); gtf#31 checks queued (DCO success, Policy+Planning queued at
  22:55Z); blockchain Renovate schedule run finished `completed/cancelled` (`35661757848`,
  post-freeze); #122 CI green (post-freeze, see above).
- Post-freeze UNRELATED PRs in goal repos (observed, do-not-touch): velnor #1066/#1067
  (other handoffs, 22:21/22:24Z), termcomp #10 (other-goal handoff, base
  `refactor/holla-parity`, 22:39Z); also velnor#1064/#1065, jackin#1068–70, jmono#2064,
  velnor-bastion#1 (concurrent-goal checkpoints — resume must not touch them).
- Partially edited / inconsistent: termcomp #8 head `885d0695` predates main `f3313476`
  (base stale); playground #50 base predates s2; termpane #18 / tablerock #81 / parallax #120 /
  blockchain #722 agent-policy PRs open against moved mains (close-with-proof after verifying
  substance — R-02, except #722 TBD). java #2062 base `05a7320b` EQUALS main HEAD (not moved);
  handled with #2063 (R-04).

## D. Requirement-by-requirement progress ledger

Statuses: `VERIFIED_DONE` | `IMPLEMENTED_UNVERIFIED` | `IN_PROGRESS` | `NOT_STARTED` | `BLOCKED`.

| ID | Requirement | Status | Evidence / files / commits | Remaining | Deps |
|---|---|---|---|---|---|
| REQ-01 | §3 roots: shared block + `CLAUDE.md` 120000 symlink, all repos | IN_PROGRESS | 13 GEN-344 repos already §3-clean (AG-1); 17 need 6-bullet append (folded into pin PRs — merged ones verified: holla-apt 344w, velnor-apt 344w, fixture 344w — h97 §1); termpane fixed; termcomp#7 open; java#2062 open; blockchain#722 open | R-01 merges, R-02 closes, R-03/R-04 | R-01…R-04 |
| REQ-02 | §4 feedback gate on every goal PR | IN_PROGRESS | 0 unresolved threads on all 20 open PRs (h96 GraphQL first:100); merged PRs gated per worker reports | Re-gate each R-01 merge at final head (§4 recheck); retro-check #724 attribution (R-09a) | R-01, R-09 |
| REQ-03 | §5 velnor owns `.github` (inc1–3, G-groups, herd, G12, #1049 aggregator) | VERIFIED_DONE | velnor#990, #992, #1006, G1/G3/G4/G5/G7/G8/G9/G10/G11, #1042 (G12, `bdffa8f4`), #1049 all merged; self-regen proven; R15 release 485 KB < 500 KB live | None (behavioral; re-verify via §8 re-sweep proofs 2–5) | R-10 |
| REQ-04 | §6 visibility runner policy everywhere | IN_PROGRESS | Public violations fixed (termpane/telemetry/velnor-apt regen; blockchain #724 merged hosted-only); private trio fully velnor-only in YAML (audit full-file inventory) | Prove with OBSERVED runs: trio queued = BLOCKED on capacity; record as proven blocker per §6 | Capacity |
| REQ-05 | §7 consumer rollout @ final pin | IN_PROGRESS | Final consumer pin = `eed474c4` (contains G12; SOUND per h97 §3 — runtime products green, redness self-CI-only; "no pin-4" stands). "15 merged" is `pin-eed474c4`-branch-scoped; pin-current mains = **17** (+ termpane #24 `pin-final`, blockchain #724 `wave`). 10 open (R-01); 4 repos never got pin PRs (R-12: jackin@pin-2, tui-snap@pin-1, cto@pin-2, gtf@pin-2 — verified live) | R-01 + R-12 merges + post-merge green checks | R-01, R-12 |
| REQ-06 | §8 proofs 1–7 (structural, in velnor tooling + audit) | IMPLEMENTED_UNVERIFIED | Proofs landed with generator PRs (worker-reported); strict repo-level sweep 0/34 PASS pre-fix (`/tmp/audit-sec8.md`) | R-10 re-sweep at post-fix heads | R-01…R-05 |
| REQ-07 | §8 proof 8 (observed CI + runner identity) | IN_PROGRESS | Dozens of observed green runs linked in worker reports + h97 §1 (e.g. velnor CI 35657630434, playground 35657908148, termpane 35659754832) | Observe R-01 post-merge runs; trio blocked (capacity); blockchain red (R-05) | R-01, R-05 |
| REQ-08 | Red mains fix-forward | IN_PROGRESS | tablerock/agent-smith/task-format/tui-snap GREEN (reruns); velnor CI green @`45ef1ebe`, Preview green @`c674f5bb`; termcomp Perf green (#9) | Blockchain NEW red (R-05); termcomp gates red (R-03); confirm velnor Preview-on-HEAD | R-03, R-05 |
| REQ-09 | Ruleset/R8 flips + terraform alignment | IN_PROGRESS | 11 red mains fixed via API Policy-add; blockchain 15177496 + playground 19573032 + task-format 23789846 + tui-snap 23746094 + tablerock 19573034 carry Policy | R-06 verify-then-PR variables.tf (BLOCKS any apply); R-07 gtf#31 (R-08 folded into R-07 — was a duplicate: #31 IS the termpane R8 change and live already requires `[DCO, Policy, ci-required]` via ruleset `23746101`) | R-06, R-07 |
| REQ-10 | Final table + change/feedback summary (§8 ¶4) | NOT_STARTED | `/tmp/final-table.md` NOT built | R-10 after R-01…R-05 | R-10 |
| REQ-11 | Post-integration cleanup (§E.6) | NOT_STARTED | Ledgers ready (§E.1–E.4) | R-11 LAST | R-10 |

## E. Change and preservation inventory

### E.1 Discovery scope and ownership

- Host: `donbeave-mac` (`Alexeys-MacBook-Pro.local`, user `donbeave`). Single-machine goal.
- Inspected (read-only, 22:02–22:15Z): `/Users/donbeave/Projects/github/all-repo/*` (34 goal
  clones = Farms A+B), `/tmp/velnor-*` + `/tmp/cq-*` + `/tmp/*-checkout` (Farms G–K),
  `…/tailrocks/velnor-project/{velnor,velnor3}` (Farms C–D),
  `…/jackin-project/jackin` (E), `…/github/velnor` (F), `~/Projects/**` L1+L2 (N),
  session dir `…/sessions/2026/09/21/01a0c10d-b452-7a12-ae8b-bff74cd6a23d/` (O + `subagent/*/`),
  `all-repo/ledger/` (O, coordination docs, not git).
- Methods: `git worktree list --porcelain`, `rev-parse HEAD/symbolic-ref/@{u}`,
  `status --porcelain=v1`, `stash list`, `branch -a`, cached-ref ahead/behind (NO fetch by
  auditors; coordinator fetched only `tailrocks_velnor` origin for the handoff base).
  Full command list in `/tmp/h95.report.md` §6. PR discovery: `gh pr list --state all`
  per repo + `head=` probes + GraphQL threads (h96).
- Coverage limits: no-fetch ⇒ behind/ahead vs possibly-stale cached refs; ~300 clean detached
  review clones aggregated (repro commands in h95, not row-listed); 2 corrupt `.git` dirs;
  4 transient sweep candidates; live trees mutated during audit (1 observed transient);
  pre-09-12 goal PRs with deleted branches invisible to the PR method (velnor#988, jackin#1016
  recovered via probes; older merged generator PRs live in main history).
- Ownership classes: `GOAL_EXCLUSIVE` (this rollout only), `GOAL_SHARED` (mixed with other
  agents/goals — RETAIN), `UNRELATED`, `UNKNOWN` (retain until resolved).

### E.2 Local worktree and clone ledger

Farm table (IDs stable; details in `/tmp/h95.report.md`):

| Farm | Common dir (short) | Worktrees | Class | Notes |
|---|---|---|---|---|
| A | `all-repo/tailrocks_velnor/.git` | 23 at audit + A-HO = 24 | GOAL_EXCLUSIVE | Primary velnor clone; handoff worktree ADDED here during pause (row A-HO) |
| B×33 | `all-repo/<repo>/.git` | 1 each | GOAL_EXCLUSIVE | One main worktree per consumer clone |
| C | `velnor-project/velnor/.git` | 85, 11 stashes | GOAL_SHARED | RETAIN — other agents/goals active |
| D | `velnor-project/velnor3/.git` | 369 | GOAL_SHARED | RETAIN — review/fixture farm |
| E | `jackin-project/jackin/.git` | 110 | GOAL_SHARED | RETAIN (2 velnor-adjacent wts noted) |
| F | `github/velnor/.git` | 6 | GOAL_SHARED | RETAIN |
| G | `velnor-ci-quota-repair/.git` | 6 | UNKNOWN (h95: GOAL_EXCLUSIVE as "quota-repair goal") | RECLASSIFIED with rationale: quota-repair sits outside the rollout farm (`~/Projects/work/`, not `all-repo` or `/tmp` wave worktrees) and may belong to a separate goal; owner ruling required before any action. RETAIN either way; `work/*` dirty wts untouched |
| H/I | `dual-lane-apt`, `dual-lane-homebrew` | 7/6 | GOAL_SHARED | RETAIN |
| J | ~120 self-contained `/tmp/velnor-*` | 1 each | mixed | 25 dirty/ahead exceptions row-listed in h95 §4; rest clean detached |
| K | `/tmp/velnor`,`-main`,`-live`,… satellites | 2–8 each | GOAL_SHARED | Small farms; exceptions in h95 §4 |
| CQ | `/tmp/cq-{termpane,velnor,gtf,parallax,termcomp}` | 5 | GOAL_EXCLUSIVE | CQ-v2 worktrees; termcomp had UNPUSHED `4527fda9` — PRESERVED (§E.3) |
| L/M/N | actions-checkout, source audits, `~/Projects` misc | — | UNRELATED/UNKNOWN | Excluded; 3 mass-staged-deletion clones flagged gap-G-09 in Farm M (do not commit blindly) |
| O | `all-repo/ledger/` (not a repo) | n/a | GOAL_SHARED | Coordination docs (`ledger.md`, workorders); read-only reference for resume |

Farm A (complete, all clean except noted; all `rollout/*` UNPUBLISHED in cached refs):

| ID | Path | Branch @ HEAD | Status | Disposition |
|---|---|---|---|---|
| A-00 | `…/all-repo/tailrocks_velnor` | `red-main/velnor-pin-bump@93ad5c4d` | clean, PR #1060 merged (`c674f5bb`); remote ref AUTO-DELETED post-merge (`delete_branch_on_merge=true`, upstream `[gone]` — audit 22:51Z) | keep; local `branch -d` after R-11 coverage proof (substance on main) |
| A-01/02 | `/tmp/g11-gate`, `/tmp/g11-main` | detached `b4fbe636` / `c832191f` | clean | keep (gate evidence) |
| A-03/04/05 | `/tmp/velnor-4fa7a3a8`, `-b9c3156`, `-fresh` | detached pins | clean | keep (fleet refs) |
| A-06…A-14 | `/tmp/velnor-g{1,10,11,12,4,5,7,8,9}` | `rollout/velnor-g*@113a6cda/93675b2a/9c29152d/533aafa6/f6ca3469/3fe0b19f/4b2e2f3d/cb8417bb/78ad2dd2` | clean, +1…+8 vs main | POST-MERGE LEFTOVERS: substance on main (squash); verify tree-coverage at R-11, then remove worktrees + delete branches |
| A-15/16/17 | `/tmp/velnor-gen-{6737,80bc,eed4}` | detached pins | clean | keep until R-11 (pinned binaries), then remove |
| A-18 | `/tmp/velnor-main-80bc` | FOREIGN (was detached `80bc420d`+U1) — TAKEN OVER 22:10Z by concurrent goal `1402ca52`: branch `preserve/handoff-1402ca52/velnor-pin-80bc@473eb7b6`, committed+pushed (velnor#1066) | RETAIN — owned by another live handoff; gate (3) FAILS for this goal | DO NOT TOUCH (see §E.6) |
| A-19 | `/tmp/velnor-pin-be61acbb` | detached `be61acbb` | T3 cargo output | droppable at R-11 (verify not referenced) |
| A-20/21/22 | `/tmp/velnor-verify{,2}`, `-wavepin` | detached | clean | keep → R-11 |
| A-HO | `/tmp/velnor-handoff-e3119d71` | `goal-handoff/velnor-rollout-20260921-e3119d71@45ef1ebe+` | THIS HANDOFF | RETAIN (primary checkpoint; never a cleanup candidate) |

Worktree-less local-only branches in A: `rollout/velnor-g3@a10da8a1` (+1),
`rollout/velnor-ownership@815f9c40` (+9), `rollout/velnor-pin-inc3@aea5c53f` (+1),
`rollout/agent-policy@3555fd05` (+0). All pre-merge-era; substance on main via squash
merges — VERIFY tree coverage at R-11 before deleting (squash ⇒ no ancestry proof).

Farm B (all clean, stash 0; Publ=NO ⇒ local-only tip, see §E.3). Rows showing "wave
heads" without SHAs were not individually snapshotted — tip = live re-observe at R-11
(§E.6 gates require it anyway); all were verified clean with the stated Publ status:

| Repo checkout | Branch @ HEAD | Publ |
|---|---|---|
| ChainArgos_blockchain-nodes | wave@`b7236da6` | yes |
| ChainArgos_java-monorepo | agent-policy@`bb69f554` | yes |
| donbeave_task-format | wave@`cabd0732` | yes |
| donbeave_terminal-components-claude | red-main/perf-fix-fwd@`640c44d2` | yes |
| donbeave_tui-snap | wave@`99ce8038` | NO (+4) |
| jackin homebrew-tap/agent-smith/-dev/github-terraform/role-action/sentinel/the-architect | wave heads | NO (+1 each) |
| jackin-project_jackin | wave@`986f94bc` | NO (+4) |
| cloudflare-tofu/github-terraform/holla-apt | wave heads | yes |
| tailrocks_holla | wave-pin@`1aed386d` | yes |
| homebrew-holla/-parallax/-ruxel | wave heads | NO (+1 each) |
| homebrew-tablerock | wave@`6db6cfb8` | NO (+4) |
| homebrew-velnor | wave@`4c3cfd26` | yes |
| parallax-telemetry-pg | main@`28bb557a` | yes |
| parallax | wave@`f9009e83` | yes |
| pg-bigdecimal | wave@`3c4035fe` | NO (+2) |
| ruxel/schemalane | wave heads | yes |
| tablerock | wave@`731b4e97` | NO (+3) |
| termpane | wave-s1bump@`4463d6de` | yes |
| termrock/tracing-req-level | wave heads | yes |
| velnor-actions-fixture | wave@`487d7b4c` | NO (+2) |
| velnor-apt | wave@`6b4850c0` | yes |

CQ farm (corrected during audit — two rows were misrecorded at freeze, per reflog):
`/tmp/cq-termpane` (`rollout/pin-final@ffcc1711` since 21:25Z, NOT unit-cache-keys),
`/tmp/cq-velnor` (on `rollout/unit-cache-keys`, no commits, clean),
`/tmp/cq-gtf` (`rollout/termpane-r8-checks@4f63dd63` = gtf#31 head, PUSHED, since 19:39Z —
NOT `@ee961e29`), `/tmp/cq-parallax` (`rollout/pin-eed474c4@5e35123b`, PUSHED),
`/tmp/cq-termcomp` (`rollout/velnor-wave@4527fda9`, was UNPUSHED → preserved §E.3, clean).
Plus ~25 `/tmp/cq-*` pin-branch clones MISSED by h95 (found by preservation audit 22:51Z):
one per open pin-PR head (#83, #122, #53, #477, #47, #47, #153, #188, #212, #504) and per
merged pin head (15, §C) — every SHA == recorded PR head, all clean, no unpreserved work;
owner likely agent-75 (UNVERIFIED — re-derive live). Plus `cq-px` (parallax main `9d71bec8`)
and `cq-velnor-fresh` (detached `a850b255`, verified on-main, no unique content).
CQ extras (verified on disk during handoff): `/tmp/cq-bc` = blockchain-nodes checkout
@`d4de960` (clean; equals merged #724 head → remove at R-11); `/tmp/cq-final` = bare
`velnor-workflow-macOS-ARM64` product binary (reproducible download → droppable at R-11);
`/tmp/xtask.log` (505 KB failure-log extract → keep until R-10, then drop).

### E.3 Local and remote branch ledger (goal-relevant only; full map in h96)

- PRESERVED DURING HANDOFF: `donbeave/terminal-components-claude`
  `goal-handoff/termcomp-wave-4527fda9` = `4527fda964d8e6269df1128acf09ab856f0eb354`
  (contains `1078c838` capture-contract fix + merge of `origin/main@f3313476`). PR #8 head
  deliberately NOT moved during freeze. ANCESTRY PRE-PROVEN (audit 22:51Z):
  `885d0695` IS an ancestor of `4527fda9` (`1078c838` contained; compare = ahead 4 /
  behind 0) → fast-forward push. Resume: review → push → continue R-03 (keep the
  merge-base check as a 1-command confirmation).
- UNPUBLISHED Farm-B tips (16): post-merge PR-branch leftovers (remotes deleted the PR
  branches after squash merges; local checkouts still sit on them). Substance is on main;
  at R-11 verify `diff <tip> <merge-commit>` is empty-ish (squash trees) before deleting.
  Highest-attention: `jackin wave@986f94bc` (+4, #1052 merged `edef2c1e`), `tablerock
  wave@731b4e97` (+3, #82 merged), `tui-snap wave@99ce8038` (+4, #5 merged).
- UNPUBLISHED Farm-A tips (9 + 4 branch-only): same post-merge-leftover class (G-PRs merged).
- `red-main/velnor-pin-bump@93ad5c4d`: remote AUTO-DELETED post-#1060 (upstream `[gone]`);
  local delete at R-11 after coverage proof. `red-main/perf-fix-forward@640c44d2`
  (termcomp): remote STILL LIVE (asymmetric — repo has no auto-delete); local delete at
  R-11, remote RETAIN (no remote-delete auth).
- Stashes: Farm C 11 entries (GOAL_SHARED — other-goal decisions, RETAIN); Farm A/B/CQ: none.
- Live remote `rollout/*` branches all map to a PR (h96: zero true orphans).

### E.4 Related PR ledger

114 goal PRs: 77 merged / 20 open / 17 closed-unmerged (h96, IDs PR-001…; all authors and
mergers `donbeave`, all bases `main`). Per-repo tables: `/tmp/h96.report.md` (LOCAL-ONLY;
key rows inlined below so the HANDOFF is self-contained for resume).

OPEN (20) — the resume worklist:

| PR | Head → Base | Checks | Action |
|---|---|---|---|
| tablerock#81 (agent-policy) | `2994844f` → main@`94245a2d` | Policy FAIL | R-02 close-with-proof after #83 |
| tablerock#83 (pin-eed474c4+§8) | `f87c1fbb` → @`886e192e` | 16/16 green CLEAN | R-01 merge |
| parallax#120 (agent-policy) | `443528a2` → @`5af3c016` | Policy FAIL + 4 COMMENTED reviews (2 threads, both resolved) | R-02 close-with-proof after #122 |
| parallax#122 (pin-eed474c4+§8) | `5e35123b` → @`9d71bec8` | 26/26 GREEN (runs `35660247237`/`35660242999`), `mergeable_state=clean` | R-01 merge |
| gtf#31 (R8 termpane checks) | `4f63dd63` → @`ee961e29` | DCO ok; Policy+Planning QUEUED | R-07 (capacity) |
| termpane#18 (agent-policy) | `0030b694` → @`7602430f` | Policy+fuzz+ci-required FAIL | R-02 close-with-proof |
| playground#50 (agent-policy) | `f334191c` → @`54d09bf7` | 19 fails, base predates s2 | R-02 close-with-proof (supersede-comment posted) |
| playground#53 (pin-eed474c4+§8) | `18ce4617` → @`28bb557a` | 21/21 green CLEAN | R-01 merge |
| architect#477, jackin-gtf#47, dev#47, sentinel#153, role#188, smith#212, tap#504 (pin) | various → mains | 5–7/7 green CLEAN | R-01 merge (×7) |
| java#2062 (agent-policy) | `bb69f554` → @`05a7320b` | 1 canc | R-04 with #2063 |
| java#2063 (schema-2+provider) | `fbc3a925` → @`05a7320b` | Control/ci-required FAIL, 72 skipped | R-04 (capacity) |
| blockchain#722 (agent-policy) | `9e0e7e0d` → @`835a3e1e` | 1 ok, 1 resolved thread | R-02 disposition TBD (verify substance post-#724) |
| termcomp#7 (agent-policy) | `bc6a04aa` → @`7b27732a` | gates+perf FAIL | R-03 after #8 + R-09(b) ruling |
| termcomp#8 (wave@4dec6b9e) | `885d0695` → @`7b27732a` | 8/14 ok; FAIL: perf, rust-junie-tui, rust-gates, ci-required, Control/Required; base stale | R-03 (FF-push preserved `4527fda9` line — ancestry proven §E.3) |

Merged (notable, post-freeze verification in h97 §1): velnor #1042 (`bdffa8f4`), #1060
(`c674f5bb`), #1062 (`45ef1ebe`); #724 → `a7f3efc1`; termpane #21→`61926c4`, #22→`e77f5dc`,
#24→`3b9cb07`; playground #51→`0dd43c5`, #52→`28bb557`; holla #224→, #223→, #225; jackin
#1052→`edef2c1e` (+unrelated #1053→`df4671e4` — NOT goal work, do not touch); termcomp
#9→`f3313476`; parallax#121 = MERGED_SUBSTANCE `9d71bec8` (tree-identical, 502 on PR-state
update — CONFIRMED via tree hash `12057163638a…`, single parent `5af3c016`).
Closed-unmerged: agent-policy mass-close 21:27–21:35Z (12 repos, superseded by wave content —
SPOT-VERIFY substance at R-02, do not assume); termpane #19/#20/#23; parallax#121 (substance
merged, see above); holla#222.
OUT OF SCOPE: velnor#1054–#1058 (donbeave, opened 20:10–20:32Z; `fix/*` except #1057
`codex/schedule-actions-read`) — R-09(c).

### E.5 Integration map and ordered landing plan — FUTURE EXECUTION ONLY

Nothing below was executed during the pause.

Map (worktree → branch → remote ref → PR → target):

- `/tmp/cq-termcomp` → `rollout/velnor-wave@4527fda9` (local) → `goal-handoff/termcomp-wave-4527fda9`
  (remote, `4527fda9…`) → termcomp#8 → `main`. R-03.
- `/tmp/cq-parallax` → `rollout/pin-eed474c4@5e35123b` (pushed) → parallax#122 → `main`. R-01.
- Other 9 pin branches (pushed; local clones RESOLVED by audit: `/tmp/cq-*`, one per PR head,
  SHAs == PR heads, all clean — §E.2) → 9 pin PRs → mains. R-01.
- Agent-policy branches (pushed) → 5 docs PRs → CLOSE-WITH-PROOF, no merge (#120, #50, #18,
  #81, + #722 TBD). R-02. CARVE-OUTS (not closes): java#2062 → R-04 merge-track with #2063;
  termcomp#7 → R-03/R-09(b) merge-policy TBD.
- java branches (pushed) → #2062/#2063 → `main` when capacity. R-04.
- gtf `rollout/termpane-r8-checks` (pushed) → #31 → `main` when capacity. R-07.

Landing order: R-01 (10 merges, parallelizable across repos; §4 recheck each at head;
post-merge CI green each) → R-12 (open the 4 missing pin PRs; merge jackin + tui-snap;
cto/gtf per zero-capacity doctrine) → R-02 (closes cite main SHAs + file URLs) → R-03
(termcomp: restore preserved line → rebase → parity-replay decision → push → CI → merge) +
R-05 (blockchain red-main fix, independent, parallel OK) → R-06 (verify-only first, then PR;
NEVER apply) → R-08 (termpane R8 entry) → R-04/R-07 (capacity-gated) → R-09 rulings as
needed → R-10 (re-sweep + final table) → R-11 (cleanup). Merge methods: `--squash`
MANDATORY in all 14 landing repos (verified live: `allow_merge_commit=false`,
`allow_rebase_merge=false` everywhere) with `--match-head-commit` guard; remote-branch
AUTO-DELETION on merge is expected (`delete_branch_on_merge=true` — explains A-00).
Re-read ALL PR threads before each landing; check-runs WILL have completed/moved since the
snapshot — re-fetch every head, review, and check.

### E.6 Post-integration local cleanup runbook — FUTURE EXECUTION ONLY

Execute ONLY after R-10, re-observing each candidate live. The 5 gates (quoted so a fresh
agent need not fetch the pause order): (1) every required change from the candidate is
verified in the intended target (or explicitly superseded/rejected with rationale + recovery
retained) — compare actual saved AND current tips; (2) all commits/edits/stashes/artifacts
accounted for — no unpreserved work or unresolved ownership; (3) no live agent/process/goal/
PR/worktree/recovery-path depends on it — shared/unknown stays intact; (4) required
post-integration validation passed and this HANDOFF + recovery refs remain reachable after
removal; (5) exact repo/host/common-dir/path/ref/expected-tip re-verified — individually
named resources only. Squash-merges: verify tree coverage, not ancestry. NEVER
wildcard-delete, force-remove dirty worktrees, prune metadata affecting shared farms, or
delete remote branches/tags (remote cleanup NOT authorized).

| Resource | Owner | Expected tip | Integration proof | Recovery | No-use gate | Proposed action |
|---|---|---|---|---|---|---|
| A-06…A-14 worktrees + branches; 4 branch-only `rollout/*` | wave G-owners (gone; resume owner acts) | tips §E.2 | G-PR squash trees cover tips (verify live) | main history (immutable) | re-verify clean + no live process in path | `git worktree remove` (clean only) + `branch -d` after coverage proof |
| Farm-B 16 unpublished tips | wave owners (gone; resume owner acts) | tips §E.2 | PR squash trees cover tips; merge commits cited §E.3–E.4 | main history + cited merges | re-verify tip == recorded tip + clean | checkout main + `branch -d` |
| `red-main/velnor-pin-bump` (local) | redmain-fix/92 (stopped) | `93ad5c4d` | #1060 merged `c674f5bb` (tree-coverage proof) | main history | remote already auto-deleted; local clean | local `branch -d` |
| `red-main/perf-fix-forward` (local) | redmain-fix/92 (stopped) | `640c44d2` | #9 merged `f3313476` | main history | local clean; remote live → RETAIN (no remote-delete auth) | local `branch -d` |
| CQ core 5 (`cq-termpane/-velnor/-gtf/-parallax/-termcomp`) | CQ-v2/91 (stopped) | §E.2 (corrected tips) | R-03 pushed (termcomp); R-01 merged (parallax); #24 merged (termpane); #31 merged (gtf) | preservation ref `4527fda9…` (termcomp); main history (rest) | re-verify no new unpushed commits + no live process + tips unchanged | remove after proof |
| CQ ~25 pin clones | likely agent-75 (UNVERIFIED) | == recorded PR heads (§E.2) | merged ones: main history; open ones: post-R-01 merge | PR heads + main history | re-verify tip == PR head + clean + no live process | remove after proof |
| `cq-px`, `cq-velnor-fresh`, `cq-bc`, `cq-final`, `xtask.log` | CQ-v2/91 (stopped) | `9d71bec8`, `a850b255`, `d4de960`, binary, log | on-main/no-unique-content (verified §E.2); xtask.log kept till R-10 | reproducible/none | re-verify unchanged | remove (log after R-10) |
| A-15…A-22, J/K clean review clones | various (gone) | pins/detached | n/a (no unique content — VERIFY: `status` clean + tip reachable from a remote ref or recorded pin) | recorded pins | re-verify clean + unreferenced live (grep session/farm refs) | remove |
| h95 §4 J/K dirty+ahead exceptions (migfix2 U17, pr953 U17, g3-integration +15, pin +74, `pin3`, `goal-965`, ~15 more) | UNKNOWN (mixed farms) | h95 §4 rows | NOT established | local only | BLOCKED until owner ruling | RETAIN (re-verify live; owner ruling before any removal) |
| A-18 | FOREIGN goal `1402ca52` (velnor#1066) | `preserve/handoff-1402ca52/velnor-pin-80bc@473eb7b6` | n/a — not ours | their pushed branch | GATE (3) FAILS (live foreign owner) | RETAIN — DO NOT TOUCH |
| A-19 | redmain-fix/92 (declared) | `be61acbb` + T3 cargo output | owner declaration (redmain report) | n/a (declared droppable) | declaration on file + re-verify unreferenced | remove |
| Remote stale branches (13× agent-policy LIVE→closed, termpane×4 stale, task-format×3, architect/jackin/homebrew-velnor/tui-snap stales per h96) | — | h96 branch map | closes/merges recorded | main history | REMOTE DELETE NOT AUTHORIZED | explicit RETAIN |
| A-HO handoff worktree + branch | coordinator (this doc) | this doc | NEVER (retained record) | origin branch (pushed) | permanent | KEEP |
| Farms C–I, L/M/N, gap-G-09 clones (Farm M), Farm C stashes | shared/unknown | — | shared/unknown | local only | BLOCKED | KEEP (REVIEW_SHARED/BLOCKED) |
| `goal-handoff/termcomp-wave-4527fda9` (remote) | coordinator | `4527fda9…` | R-03 merged | itself (pushed ref) | permanent | RETAIN (remote delete not authorized) |

Post-cleanup: re-enumerate `git worktree list` + `branch -a` per farm, reconcile with §E.2,
write cleanup receipt. Every inventoried goal resource needs a final disposition before R-11
is complete.

## F. Decisions, findings, assumptions, rejected approaches

Settled (with rationale):
- Advisory merge ruling: merge iff platform allows AND only Policy failures are advisory
  pre-flip required-checks; record repo for W6.2 flip. (Else blocks.)
- Termpane ruling-(a): merge #21 despite trusted-runners FAIL = b9c3156-validator /
  4fa7a3a8-template skew on dual-lane `if:` gate; transient tree; PR2 deletes the jobs.
- AG-1: §3 canonical = velnor main AGENTS.md MINUS line 3 (velnor-local runner-protocol
  bullet). F1 VOID (13 repos already clean); F2/F3/F4 append the 6 trailing bullets only.
- Wave pins: be61acbb → 4dec6b9e → bdffa8f4 (G12); consumer-final pin `eed474c4` SOUND
  (runtime products green; self-CI redness root-caused); "no pin-4" stands — do NOT re-pin
  to `45ef1ebe` (contains #1059/#1061 behavior changes).
- Zero-capacity doctrine: private repos merge platform-allowed; unobservable execution =
  PROVEN BLOCKER with failed-operation evidence (0 runners in `velnor-trusted` group).
- Tablerock plutil-shim symptom-fix accepted with deferred root cause recorded.

Findings (sources cited):
- Cache-key structural flaw (CQ, termpane #24): `id_segment` per-kind + `restore-keys` prefix
  + PATHS not in key ⇒ poisoned exact-hit took CI offline. Playbook: delete poisoned caches.
  Follow-up NOT implemented (`/tmp/cq-velnor` on `rollout/unit-cache-keys`, no commits).
- Terraform drift RM-4 CONFIRMED live (audit 22:55Z): `variables.tf` has termpane and
  tui-snap at `required_checks = []` while live rulesets require checks (plus ~8 more repos
  lacking Policy in tf) ⇒ next apply re-reddens mains. R-06 is the fix vehicle (verify-first,
  then PR, NEVER apply).
- Merge-API 502-after-commit pattern (parallax#121, holla#223/#224 era): PR shows closed but
  commit exists ⇒ merged-in-substance close with tree-hash proof. Verified for #121.
- Debug-built binaries produce different scan digests than release products — ALWAYS render
  with release product binaries (playground proof: Policy 9/11 → 10/11).
- Mass agent-policy close 21:27–21:35Z needs substance spot-checks at R-02 (do not assume).

Open questions: termcomp parity-replay CI step (product-CI decision, R-03); termcomp#7
gates-red merge policy (R-09b); #724 §4 retro-attribution (R-09a — starting state known:
0 reviews + 1 bot comment, so the retro-check is trivial); velnor#1054–58 scope (R-09c).
(RM-4 resolved: drift CONFIRMED, see above.)
W6.2 flip record: NO consolidated list exists in the handoff sources (`/tmp/ruleset-survey.md`
has no flip/W6 section — grep-verified during audit). Re-derive at R-06/R-07 time from the
survey + the 5 Policy-carrying ruleset IDs in REQ-09 + the 11-repo remediation set in the
ruleset-remediation worker report (`subagent/01a0c539-07d9-…/session.jsonl`).
The AG-1 "6 trailing §3 bullets" (appended by F2/F3/F4 fixes) are, in order: (1) "Delegate
first…", (2) "Commit meaningful, verified changes…", (3) "Before every PR merge…",
(4) "For accepted feedback…", (5) "Re-fetch feedback at the final head SHA…", (6) "Keep
agent instructions lean…". (GEN-161 repos already carry the first 9 lines through "…name
the deferred root cause."; GEN-192 velnor-apt additionally drops its line 3.)

Glossary for fresh readers: D19 = velnor's self-hosting pin lane (the `.github-gen`
revision velnor uses to render its own `.github`; #1062 bumped it to `eed474c4`).
`herd` = a batch of small generator fixes landed together (see REQ-03). W6.2 = the
ruleset-flip workstream (add `Policy` to required checks post-green). W5-R7 = blockchain
advisory ruling R7 (dco-2 GitHub App install impossible via API — needs org-admin web flow;
statics kept as passthrough).

## G. Verification evidence and known failures

Live snapshot 22:22–22:35Z (h97; `gh api` read-only):

- PASS (observed green): velnor CI@`45ef1ebe` 35657630434; velnor Preview@`c674f5bb`
  35657208164; playground dispatch 35657908148 (Policy 11/11); termpane CI 35659754832;
  tablerock/agent-smith/tui-snap reruns; termcomp Perf 35652602140; 10 pin-PR rollups green.
- FAIL: blockchain main CI 35657653497 (`docker-celo-op-node` unit checks) + release dispatch
  35657671669 (Verify release); termcomp main CI 35652601666 (gates stable + 1.88.0);
  red open PRs per §E.4 table.
- QUEUED (capacity): gtf 35634398902, cto 35634424059, jmono 35630181731 (unchanged IDs,
  still queued at 22:55Z audit); gtf#31 Policy+Planning (still queued at 22:55Z); blockchain
  Renovate finished `completed/cancelled` (`35661757848`, post-freeze).
- Preview never ran on velnor@`45ef1ebe` (latest Preview = `c674f5bb` green) — confirm on resume.
- Parallax F12 persists: no CI on HEAD `9d71bec8` (latest = Maintenance@`5af3c016`); #122 merge
  produces first signal.

Handoff-operation checks (coordinator, bounded/nonmutating unless noted):
- `git fetch origin` (tailrocks_velnor) rc=0; base `45ef1ebe` recorded.
- `git worktree add` handoff worktree: OK, clean.
- `git push origin 4527fda9:goal-handoff/termcomp-wave-4527fda9`: OK, verified via ls-remote.
- `subagent_status(running)` → `not_found`: all workers stopped. PASS.
- No product validation campaign run during handoff (per spec).

## H. Ordered remaining-work plan

| ID | Outcome | Deps / parallel | Next action + validation |
|---|---|---|---|
| R-01 | Merge 10 green pin PRs (§E.4) | FIRST TASK; parallel across repos | §4 recheck at head → SHA-guarded merge → post-merge main CI green each. Order: #83 → #122 → #53 → jackin×7 |
| R-02 | Close-with-proof: #120, #50, #18, #81 (+#722 TBD) | After R-01 for #120/#81 | Verify substance on main (file URLs) → explanatory close comment |
| R-03 | termcomp#8: restore `4527fda9` → rebase → fix parity/capture/perf → merge; then #7 | Needs R-09(b) for #7 | CI green on #8 (or ruled waiver); gates decision for #7 |
| R-04 | java#2063 (+#2062) merge | BLOCKED: velnor capacity | Merge when queue drains; until then proven-blocker record |
| R-05 | Blockchain red main fix | Independent (parallel OK) | Fix celo-op-node unit checks + Verify release; main green |
| R-06 | Terraform alignment PR (no apply) | Verify-first | Read `variables.tf` vs live rulesets → PR Policy additions |
| R-07 | gtf#31 merge | BLOCKED: capacity | Merge when queued checks drain |
| R-08 | ~~Termpane R8 map entry~~ SUPERSEDED — folded into R-07 (audit: gtf#31 `variables.tf` +1/−1 IS the termpane R8 change; live already requires `[DCO, Policy, ci-required]` via ruleset `23746101`; a separate unblocked item risked double-application) | — | No action; R-07 covers it |
| R-09 | Rulings: (a) #724 retro-check (b) #7 gates policy (c) #1054–58 scope | Before affected merges | Parent decides with evidence; record |
| R-10 | §8 re-sweep + final table + feedback summary | After R-01…R-05, R-12 | Strict 7-check sweep; publish table + links |
| R-11 | Cleanup per §E.6 | LAST, after R-10 | Gates §E.6; cleanup receipt |
| R-12 | Open MISSING pin PRs: jackin (main@pin-2 `4dec6b9e`), tui-snap (main@pin-1 `be61acbb`) — public, CI observable; cto + gtf (mains@pin-2, private) — open; merge per zero-capacity doctrine at resume | After R-01 (recipe below); cto/gtf merge gated like R-04/R-07 | 4 new pin PRs §4-gated; blockchain already pin-current via #724 (verified `revision = "eed474c4…"`); java + termcomp EXCLUDED here — in-flight wave PRs #2063/#8 cover them (R-04/R-03) |

FIRST ACTIONABLE RESUMPTION TASK: R-01 — re-fetch tablerock#83 head/reviews/checks, §4-gate,
SHA-guarded merge, verify post-merge main CI green; then continue the R-01 order.
Copy-paste template (re-fetch FIRST; never reuse snapshot SHAs):

```
gh pr view 83 --repo tailrocks/tablerock --json headRefOid,mergeStateStatus,reviewDecision
gh pr checks 83 --repo tailrocks/tablerock
# …§4 gate: paginate + read ALL reviews/threads, disposition each, reply where required…
gh pr merge 83 --repo tailrocks/tablerock --squash --match-head-commit <re-fetched-head-SHA>
gh api repos/tailrocks/tablerock/actions/runs?branch=main --jq '.workflow_runs[0] | "\(.conclusion)/\(.status) \(.head_sha[0:8])"'
```

Pin-PR recipe (R-12; same recipe R-01's PRs were built with — validated pattern, not a
guaranteed flag set): (1) fresh worktree at target `main`; (2) set
`.github-gen/velnor-workflow.toml` `revision = "eed474c4a1d9b071fd1b5de00c769c8997398e5a"`;
(3) regenerate the FULL `.github` tree with the RELEASE product binary at that revision
(exact render invocation: recover from CQ-v2 session log
`subagent/01a0c55c-7ec7-…/session.jsonl` — grep for the render command; DO NOT invent
flags; debug-built binaries are FORBIDDEN — different scan digests, §F); (4) determinism
proof: second regen is a byte-identical no-op + drift check passes; (5) fold in the AG-1
6-bullet append where the repo is GEN-161/GEN-192 (§F); (6) `commit -s`, push
`rollout/pin-eed474c4`, open PR, full §4 gate, `--squash --match-head-commit` merge,
post-merge main CI green.

§8 re-sweep's 7 strict checks (R-10; from `/tmp/audit-sec8.md:6-15`, with the AG-1
canonical correction): overall PASS requires ALL of (1) root `AGENTS.md` head
byte-identical to canonical — USE THE 15-LINE AG-1 CANONICAL (§B.1), not the audit's
16-line velnor-main text; (2) root `CLAUDE.md` git mode `120000` → `AGENTS.md`;
(3) `.github/AGENTS.md` generated marker (`# Generated files` + `velnor-workflow`);
(4) `.github/CLAUDE.md` mode `120000` → `AGENTS.md`; (5) generator pin present
(`revision = "<40hex>"` in `.github-gen/velnor-workflow.toml`, else
`VELNOR_WORKFLOW_POLICY_REVISION` in `.github`); (6) runner policy clean for visibility
(public: zero `self-hosted`/`velnor` runs-on; private: zero github-hosted runs-on);
(7) latest `main` run conclusion == `success`.

## I. Environment and operational recovery

- Host: donbeave-mac, user `donbeave`, TZ UTC+7. Tools: `gh` (authed as donbeave; quota
  5000/5000 at 22:22Z), `git`, `cargo/rust 1.98.x`, `mise`. All 34 repos: GitHub, default
  `main`, squash-or-merge per repo policy (tui-snap mandates squash).
- Workdirs: `/Users/donbeave/Projects/github/all-repo/<repo>` (34 clones); `/tmp/cq-*`,
  `/tmp/velnor-*` (worktrees/scratch); session logs (evidence only).
- External effects performed: ~100 merges via `gh pr merge --squash --match-head-commit`;
  ruleset API edits (Policy adds: 11 remediation repos + tablerock/tui-snap/task-format/
  blockchain/playground); termpane Actions cache deletions (2 entries); termcomp
  preservation branch push (handoff op). Repeating merges is guarded by SHA checks; ruleset
  edits are idempotent; cache deletion is safe to repeat.
- Generated vs irreplaceable: pin-product binaries reproducible from velnor main;
  CI run logs live on GitHub (URLs in §G/worker reports); `/tmp` ledgers + session logs are
  irreplaceable — back up before any `/tmp` purge (or accept re-derivation cost).
- Permissions: org admin (tailrocks) held, yet some routes impossible via API (dco-2 app
  install needs web flow — W5 R7). PR token gets ruleset-API 403 (redmain) — entrypoint
  compares declared-vs-declared pre-merge.

## J. Fresh-agent resume runbook

1. Read this document completely + repo `AGENTS.md`/`.github/AGENTS.md` + linked essentials
   (`/tmp/h95.report.md`, `/tmp/h96.report.md`, `/tmp/h97.report.md` if on this host).
2. Recover the checkpoint: `git -C …/all-repo/tailrocks_velnor fetch origin` then
   `git worktree add /tmp/velnor-handoff-resume goal-handoff/velnor-rollout-20260921-e3119d71`
   (exclusive worktree; do not disturb shared checkouts).
3. Compare LIVE state vs this snapshot: re-fetch all §E.4 PR heads/threads/checks, main SHAs,
   queued runs, velnor main vs `45ef1ebe`. Detect intervening changes FIRST. Stale PIDs/SHAs
   in this doc are starting hypotheses, not facts.
4. Reconcile completed work (R-items already done stay done); do not rewrite shared history
   or restart green tasks.
5. Resume the ORIGINAL goal at R-01 (first task, §H), NOT the handoff task. Delegate with
   clear ownership (one integration owner per repo), keep DCO signoff + §4 gates + AG-1 +
   pin discipline (`eed474c4`, no pin-4).
6. Maintain progress in the goal record + scoped checkpoint commits on resume work.
7. After goal completion + R-10 validation, execute §E.5 integration (already mostly merged;
   verify), then ONLY eligible §E.6 cleanup with live re-verification. Account every
   worktree/branch/PR with a final disposition + cleanup receipt.

Resume command:

```
/goal Read and resume docs/goal-handoffs/velnor-agent-policy-workflow-rollout--20260921T220154Z--shore-polaris--e3119d71.md
```

INTERPRETATION RULE: the pause holds until the user requests resumption. A later `/goal Read
and resume <this-file>` authorizes continuation of the ORIGINAL goal. It does NOT instruct the
future agent to pause again, regenerate this handoff, or recursively create another handoff PR.

## K. Blockers, omissions, independent review

Blockers/omissions:
- B-1 (proven, external): velnor-target-mvp fleet = 0 runners ⇒ trio mains + gtf#31 + java
  PRs unverifiable. Remedy: infra owner provisions runners + applies group membership (#29);
  until then record per §6 (never substitute hosted).
- B-2: 75 never produced a final report (cancelled; stale mid-run note only). Mitigation:
  its repos evidenced via merges + h96/h97 live checks. Resume must still re-verify each.
- B-3: no runtime goal-pause control exists (§A) — pause is administrative + worker stops.
- B-4: `/tmp/final-table.md` never built; §8 re-sweep never run (R-10).
- B-5 (scope): velnor#1054–#1058 ownership unverified (R-09c); Farm G `work/*` dirty
  ownership UNKNOWN (retained); G-09 mass-staged clones intent UNKNOWN (untouched).
- Otherwise NONE unchecked: all goal workers stopped (roster verified empty); all essential
  code remotely preserved (merged or preservation ref).

Independent review: DONE (agent `handoff-review/98`, report `/tmp/h98.review.md`,
LOCAL-ONLY). Initial verdict BLOCKED with 12 must-fix + 7 suggestions; all 9 live
spot-checks CONFIRMED (preservation ref, #83, #477, blockchain red, velnor HEAD, #121 tree
equality, #24, #1052, #8 head untouched). All 12 must-fix applied (M-1 dangling §E.2a;
M-2 worktree count; M-3 Farm G rationale; M-4 J/K dirty RETAIN row; M-5 new R-12 for 4
missing pin PRs + live pin verification; M-6 jackin-gtf#47; M-7 java#2062 base==HEAD;
M-8 pin counts 15/10/1=26; M-9 unverified pin-branch locations; M-10 Owner/Recovery columns
+ strengthened gates; M-11 cq-bc/cq-final/xtask.log rows; M-12 R15 defined) and all 7
suggestions applied (S-1…S-7). Recovery test passed: reviewer derived the exact resume
command and R-01/tabby-#83 first task independently. 20/20 open PRs mapped; every §E.6 row
now carries owner + recovery + no-use gate. Reviewer assessment: honest, survivor-verified,
resumable without the parent conversation.

## L. Audit appendix 1 — source register (added by audit 2026-09-21T~23:0xZ)

All seq numbers byte-verified by the intent auditor against the raw 78,929-line session log.
No UNAVAILABLE sources.

| S-ID | Source | Location | Access |
|---|---|---|---|
| S-00 | Original `<objective>` (25,805 chars, sha256 `59c4180709bc…`) | session log fileline 13882 / seq 13883 | FULL — embedded verbatim in §B.5 |
| S-01…S-04 | DCO signoff amendments (4× escalating) | seqs 383, 13340, 13433, 13567 | FULL |
| S-05…S-09 | Delegation amendment (5× byte-identical, 1,167 chars) | seqs 38069, 39020, 39148, 46158, 51920 | FULL |
| S-10 | Autonomy amendment (1,276 chars) | seq 69907 | FULL |
| S-11 | Commit-often amendment (823 chars) | seq 70809 | FULL |
| S-12 | Pause order (39,385 chars, §§1–7 + §§A–K spec) | seq 70899 | FULL |
| S-13 | Audit order (19,455 chars, §§1–11) | seq 78737 | FULL |
| S-14 | "Commit and push handoff in the end." (35 chars) | seq 78924 | FULL |
| S-15 | `/tmp/h94.report.md` (prior goal extraction, verified correct) | local `/tmp` | SECONDARY (verified) |
| S-16 | This HANDOFF pre-audit (559 lines) | PR #1068 @`94eff19d` + local | FULL |
| S-17 | Handoff PR live state (#1068 draft, auto-merge off, head) | `gh` live | FULL |
| S-18…S-21 | Audit-wave reports: intent (`/tmp/a805d`), state (`/tmp/a8123`), preservation (`/tmp/a101`), fresh-reader (`/tmp/a102`) | local `/tmp` | FULL (local-only) |
| S-22 | Worker finals (G12/ruleset/W5/W2/W3/playground/redmain/CQ-pause/deadlock) | `subagent/*/session.jsonl` + `/tmp/h91-paused.md`, `/tmp/report-deadlock.md` | FULL (local-only) |

Supersession map: S-12 supersedes "continue autonomously to completion" (S-00 §1 + S-10)
with freeze+pause; S-13/S-14 scope only this audit (commit+push+republish) and do not alter
the G-contract. All other amendments REMAIN BINDING at resume (signoff, delegation,
autonomy, commit-often). No user message exists between seq 70899 and 78737.

## M. Audit appendix 2 — source-to-handoff requirements matrix

Coverage AFTER audit repairs (verbatim now in §B.5 converts prior VAGUE/MISSING meanings to
COVERED; state/evidence/tasks columns make each row actionable). `T-###` = executable tasks
(appendix §N). Gaps that remain are explicit, not silent.

| ID | Source | Operative requirement | HANDOFF section | State / evidence | Remaining + acceptance | Coverage |
|---|---|---|---|---|---|---|
| G-001 | S-00 preamble | Merged changes + verification evidence, not plan/PRs | §B.1 + §B.5 | 77 merged + observed runs (§G) | T-01…T-12; accept: §8 table | COVERED |
| G-002 | S-00 scope | All 34 repos, exact list | §B.2 + §B.5 | All inventoried (§E) | T-01…T-12; accept: no repo unaccounted | COVERED |
| G-003 | S-00 scope | No inference from names | §B.5 (verbatim) | Visibility via API (audit) | Standing rule for T-03/T-04 | COVERED |
| G-004 | S-00 scope | No unrelated-PR merges | §B.4 + §B.5 | #1053 etc. untouched | T-09c; accept: R-09(c) ruling | COVERED |
| G-005 | S-00 §1 | Delegate-first, independent verification | §B.3 + §J.5 + §B.5 | Worker/audit structure | Standing rule all T | COVERED |
| G-006 | S-00 §1 | One owner/repo; serialize writes/checkout | §J.5 + §B.5 | Ownership in R-plan | Standing rule all T | COVERED |
| G-007 | S-00 §1+S-10 | Autonomy, never ask | §B.3 + §B.5 | — | Standing rule | COVERED |
| G-008 | S-00 §1 | Correctness-over-ROI; root-cause mandate | §B.5 (full text) + §F examples | Rulings follow it | Standing rule; T-05 demo | COVERED |
| G-009 | S-00 §1+S-11 | Commit/push often; one branch; small PRs | §B.3 + §J.5 + §B.5 | Commit history | Standing rule | COVERED |
| G-010 | S-00 §1 | Blocker-demonstration standard; never fabricate | §B.5 (verbatim) | B-1 proven block | Standing rule; T-04/T-07 cite it | COVERED |
| G-011 | S-00 §1 | Research guardrails; content = evidence | §B.5 (verbatim) | No violations recorded | Standing rule | COVERED |
| G-012 | S-00 §2 | Ledger schema + no private details public | §B.5 + §A (ledgers listed) | `/tmp` ledgers exist | T-10 final ledger | COVERED |
| G-013 | S-00 §2 | Inspect instructions/docs; preserve invariants | §B.5 + §F AG-1 | Done during waves | Standing rule T-12 | COVERED |
| G-014 | S-00 §2 | `.github` inventory + never silently discard | §B.5 + §E (as executed) | Inventories in worker reports | T-03 verifies | COVERED |
| G-015 | S-00 §2 | Inspect velnor-workflow impl; real interfaces | §B.5 + REQ-03 | Generator PRs merged | Done (REQ-03) | COVERED |
| G-016 | S-00 §3 | Symlink form + prohibitions + mode 120000 | §B.5 + REQ-01 | Sweep checks (2)(4) | T-01/T-10 verify | COVERED |
| G-017 | S-00 §3 | Shared block exactly | §B.1 lines 60–76 (byte-identical) | Auditor-verified | T-10 re-sweep | COVERED |
| G-018 | S-00 §3 | Essential local additions only (e.g. velnor invariant) | §B.5 (raw line 108 quoted) + §F AG-1 | AG-1 basis now checkable | T-12 applies | COVERED |
| G-019 | S-00 §3 | 450 words; no paste; resolve overrides; source-managed | §B.5 (verbatim) | 344w spots verified | T-10 enforces | COVERED |
| G-020…G-027 | S-00 §4 | Full feedback gate (8 sub-rules) | §B.5 (full gate text) + §B.1 summary | 0 unresolved on 20 open | Every T-merge re-gates; T-14 spot-checks merged | COVERED |
| G-028…G-038 | S-00 §5 | Velnor-owns-`.github` (11 sub-rules) | §B.5 (full text) + REQ-03 | All merged, R15 live proof | Done; T-10 re-verifies proofs | COVERED |
| G-039…G-045 | S-00 §6 | Runner policy (7 sub-rules) | §B.5 + §F doctrine + REQ-04 | YAML clean; runs queued | T-04/T-07 or proven-blocker record | COVERED |
| G-046…G-050 | S-00 §7 | Rollout discipline (5 sub-rules) | §B.5 + §B.4 + §E.5 | Pin `eed474c4`, small PRs | T-01/T-12 complete it | COVERED |
| G-051…G-058 | S-00 §8 | Eight proofs | §B.5 + §D + §H 7-check sweep | Partially proven | T-10 proves all | COVERED |
| G-059 | S-00 §8 | Independent audits; no weaken/fake | §B.5 + §K + §L–N | 8 audit agents used | T-10/T-14 | COVERED |
| G-060 | S-00 §8 | Evidence in ledger, not AGENTS/`.github` | §B.5 (verbatim) | Ledgers in `/tmp` + this doc | T-10 | COVERED |
| G-061 | S-00 §8 | Final table 7 fields + summary links | §B.5 (field names verbatim) | NOT built | T-10 builds it | COVERED |
| G-062 | S-00 §8 | Repo-complete definition; never unaccounted | §B.1 line 105 + §B.5 | Applied per-repo | T-10 applies | COVERED |
| G-063…G-066 | S-01…S-11 | Signoff, delegation, autonomy, commit-often | §B.3 + §B.4 | Applied | Standing rules | COVERED |
| H-001…H-021 | S-12 | Freeze/inventory/plan/runbook/PR (21 items) | §§A,C–K (intent audit: 19 COVERED, H-005/H-007 now COVERED via §B.5 embed) | PR #1068 published | Audit republishes (this commit) | COVERED |

No CONTRADICTORY or SOURCE_UNAVAILABLE rows remain. Prior audit verdicts (VAGUE/MISSING on
~25 G-items) were caused by the missing verbatim embed — repaired by §B.5 + this matrix.

## N. Audit appendix 3 — executable task plan (T-###)

T-01…T-12 mirror R-01…R-12 (same order, same scope; R-IDs retained for traceability).
T-13/T-14 close gaps the audit found (unowned follow-up; merged-PR evidence gap).
Standing constraints on every T: S-01…S-11 (signoff `Alexey Zhokhov <alexey@zhokhov.com>`,
delegate-first, autonomy, commit-often), §4 re-gate at final head, `--squash
--match-head-commit` (mandatory all 14 repos), no unrelated PRs, no tofu apply.

- T-01 (R-01; G-001/2/16/17/20-27/46-50): merge 10 green pin PRs (#83, #122, #53, #477,
  jackin-gtf#47, #47, #153, #188, #212, #504). Start: all green CLEAN at snapshot heads
  (§E.4; #122 26/26). Per PR: re-fetch head/reviews/checks/threads → full §4 gate → merge →
  post-merge main CI green (record run URL). Parallel across repos, one owner each.
  Pitfalls: stale snapshot SHAs (re-fetch); #122's earlier xtask skew (resolved — still
  re-verify). Done when: 10/10 merged + 10 post-merge green runs linked.
- T-02 (R-02; G-020-27): close-with-proof #120, #50, #18, #81 (+#722 TBD). Start: open vs
  moved mains. Per PR: verify substance on main (quote file URLs + SHAs) → explanatory
  close comment → close (no merge). #722: decide merge-vs-close by substance check first.
  Done when: 4–5 closed with proof comments linked.
- T-03 (R-03; G-001/2/14/16/20-27): termcomp#8 → FF-push preserved `4527fda9` (ancestry
  proven §E.3; 1-command re-confirm) → fix parity-replay (product decision) + capture
  (done in `1078c838`) + perf leg → CI green → §4 → merge; then #7 per T-09b ruling.
  Repo: donbeave/terminal-components-claude; worktree: fresh (NOT W3's dirty checkout).
  Pitfall: base moved to `f3313476` (merge already in `4527fda9`). Done when: #8 merged +
  main green.
- T-04 (R-04; G-039-45): java#2063 (+#2062) merge. BLOCKED: 0 velnor runners (B-1). Start:
  #2063 red (Control/ci-required FAIL, 72 skipped), base == HEAD `05a7320b`. Merge when
  queue drains; until then extend the proven-blocker record (G-010: failed-operation
  evidence, no fabrication, never hosted-substitute). Done when: merged + green, or
  documented proven blocker at goal close.
- T-05 (R-05; G-008): blockchain red main fix. Start: CI `35657653497`
  (docker-celo-op-node unit checks) + release `35657671669` (Verify release) FAIL.
  Root-cause first (G-008), structural fix preferred, symptom-fix only with deferred-cause
  record. Independent + parallel-safe. Done when: main CI + release green (run URLs).
- T-06 (R-06; G-049): terraform alignment PR (NO APPLY). Start: RM-4 drift CONFIRMED
  (termpane + tui-snap `required_checks = []`, ~8 more missing Policy). Verify-first: diff
  live rulesets vs `variables.tf` → PR adding Policy/checks → merge PR only. Repo:
  tailrocks/github-terraform. Done when: PR merged; live==tf for Policy entries.
- T-07 (R-07 + folded R-08; G-049): merge gtf#31 (`termpane-r8-checks@4f63dd63`, IS the
  termpane R8 change). BLOCKED: Policy+Planning queued (capacity). Merge when drained;
  R-08's "live requires nothing" premise was CONTRADICTED (live requires
  `[DCO, Policy, ci-required]`) — do NOT double-apply. Done when: merged.
- T-08 (R-08): SUPERSEDED — no action (folded into T-07; record retained for traceability).
- T-09 (R-09; G-004/008): three rulings with evidence: (a) #724 §4 retro-check (starting
  state: 0 reviews + 1 bot comment → trivial); (b) termcomp#7 gates-red merge policy;
  (c) velnor#1054–58 scope (default OUT). Done when: rulings recorded + applied.
- T-10 (R-10; G-012/051-62): §8 re-sweep (7 strict checks, §H, AG-1 canonical) at post-fix
  heads + final 7-field table + change/feedback summary with links. Depends: T-01…T-05,
  T-12. Done when: table covers 34/34 with per-cell evidence; every repo dispositioned
  complete vs proven-blocker.
- T-11 (R-11): cleanup per §E.6 (gates 1–5, live re-verification, receipt). LAST. Done when:
  every inventoried goal resource has a final disposition + cleanup receipt.
- T-12 (R-12; G-046-50): mint 4 missing pin PRs (jackin, tui-snap public; cto, gtf private)
  via §H recipe; merge public two; cto/gtf per zero-capacity doctrine. java + termcomp
  excluded (covered T-04/T-03). Done when: 4 PRs §4-gated; public merged + green.
- T-13 (NEW; G-008/G-049): cache-key structural follow-up (unowned at freeze). Start:
  `/tmp/cq-velnor` on `rollout/unit-cache-keys`, zero commits; flaw analysis in §F
  (`id_segment` + prefix restore-keys + PATHS-not-in-key ⇒ poisoned exact-hit).
  Investigate generically in velnor (per-key full-key exact match? PATHS in key? key
  versioning?), implement + test + small PR + regenerate affected consumers if the fix
  changes rendered keys. Validation: poison-cache reproduction test red→green. Done when:
  merged + consumers consistent, or ruled infeasible with deferred-cause record.
- T-14 (NEW; G-020-27/G-059): merged-PR §4 spot verification (bounded). Start: "merged PRs
  gated" rests on worker reports alone. Sample ≥6 merged PRs across waves/owners (incl.
  #724, one 502-close, one agent-75 PR): re-fetch threads/reviews/checks at merge SHA,
  confirm zero-unresolved + gate-shaped replies. Record PASS/FAIL per sample; FAIL →
  file a repair task (do not rewrite history). Done when: sample recorded; no silent gaps.

FIRST ACTION after §J checkpoint retrieval + live reconciliation: T-01/tabby-#83 (§H
template). Then T-01 order → T-12 → T-02 → T-03+T-05 (parallel) → T-06 → T-04/T-07
(capacity) → T-09 as needed → T-13/T-14 (anytime parallel) → T-10 → T-11.

## O. Audit appendix 4 — audit record + outcome

- Audit scope: intent fidelity (S-00…S-22), state drift (22:46–22:55Z live re-checks),
  preservation chain, fresh-reader resumability. Agents: 99 (intent), 100 (state), 101
  (preservation), 102 (fresh-reader) — all read-only, all delivered (S-18…S-21).
- Material omissions found & repaired: (1) full verbatim objective missing → §B.5 embedded;
  (2) false "§4 quoted exactly" → corrected; (3) no G/H matrix → §M (66 G + 21 H, all
  COVERED); (4) no T-tasks → §N (T-01…T-14); (5) R-08≡R-07 duplicate → folded (traceability
  retained); (6) 4 missing pin PRs had no R-item → R-12/T-12 (pre-existing from review-98;
  verified live); (7) termcomp FF now proven; (8) RM-4 drift now confirmed;
  (9) A-18 cross-goal takeover → RETAIN; (10) ~27 missed cq clones inventoried;
  (11) cq-termpane/cq-gtf misrecords fixed; (12) #122 green everywhere; (13) squash-mandatory
  all-14; (14) E.5 close-count fix; (15) #1057 label; (16) holla SHAs; (17) source register §L.
- Unresolved gaps: NONE in handoff quality. Engineering unknowns correctly remain as future
  work (T-03 product decision, T-04/T-07 capacity, T-09 rulings, T-13 investigation).
- verify-and-stop skill: UNAVAILABLE in this environment (no skill loader; not in catalog) —
  applied its discipline manually (read-only validation, no scope expansion, every repaired
  claim tied to auditor evidence or a cited live command).
- Review mode actually performed: 4 independent audit subagents (99–102) + coordinator
  full-document re-read + live verification commands. No fabrication: every correction cites
  S-IDs, auditor findings, or shell output in this record's lineage.
- Outcome: **VERIFIED**. Source coverage complete (no UNAVAILABLE); all operative
  requirements faithfully + actionably documented (§B.5 + §M + §N); preservation/resume
  details verified live; published content verified below. Original-goal engineering
  blockers (B-1 capacity etc.) are documented, not handoff defects.

Publication (this audit): branch `goal-handoff/velnor-rollout-20260921-e3119d71`,
PR #1068 (DRAFT, auto-merge off) — head SHA updated in PR body after push; local ==
remote == PR head verified before receipt.
