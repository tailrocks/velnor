# Refactoring completion execution plan

**Status: completion reopened for a source-first, line-by-line re-audit.** The previous 73-task catalog passed its recorded checks, but the renewed independent audit has identified scope and contract gaps. Execution readiness is not currently accepted. Findings, repairs and independent rechecks are tracked in [the re-audit register](../refactoring-plan/reaudit-plan.md). The production refactor and production verification harness have not been implemented or executed. The [previous completion audit](../refactoring-plan/planning-acceptance.md) and [previous verification record](../refactoring-plan/planning-verification.md) remain historical evidence, not acceptance of the revised plan.

The additional whole-branch review is summarized in [the main continuation proposal](../refactoring-plan/branch-continuation-proposal.md). Its exact inventory covers 7,885 changed paths. Preserve completed main architecture, continue unfinished owners, restore source-qualified behavior, and explicitly disposition superseded tests and historical captures. Full source reading, compatibility decisions and independent repair review remain in progress; partition reports are not a final execution certificate.

## 1. Executive goal

Complete main's accepted reusable component architecture and APIs while reproducing the exact user-visible experience of `visual-baseline` in Showcase, Holla, Jackin and TablePro. Architecture changes; product experience does not. This planning deliverable must remove the need for the next executor to reconstruct architectural intent or guess the correct UI.

The dominant regression mechanism is independent ownership of visible content and interactive state. Main sometimes calls a reusable component, then paints another model over its output. Other screens display controls without live state or handlers. The execution graph must eliminate that condition through real reusable component composition, rather than preserve it with more application-local painting.

## 2. Source-of-truth hierarchy

User-visible behavior comes from the immutable tag, then deterministic evidence captured from that tag. Moving `visual-baseline` is supporting context only. Accepted, non-superseded architectural decisions reconstructed from history govern APIs and ownership, followed by current authoritative checkpoints and their commits. Actual main code is evidence of implementation, not proof that an architectural exception was accepted.

The current planning goal supersedes older visual oracle pins, discretionary visual-polish permissions, baseline blessing permissions, implementation mandates, stale stop/restart orders and historical model-routing instructions. Rejected patches must not be confused with rejected requirements. Keep historical evidence and its provenance; never relabel an older capture as a September 10 oracle capture.

Task contracts use current canonical task-format. Verification uses the qualified tui-snap revision, ordinary deterministic Rust tests and accepted architecture/API/performance gates. No candidate output can redefine expected output.

## 3. Resolved source identities

Measured locally and against remote references on 2026-09-11:

| Authority | Exact identity |
| --- | --- |
| Immutable UI oracle | `02f5294bfdbf38004cc49130d0aff1d01f31434c` |
| Annotated tag object | `a643909d9a782adaf0aa1e3357710a5ed3f24443` |
| Architectural main | `7b27732a8c3c131760ec3438f641cb3c11343a42` |
| Planning checkout `visual-baseline` | `2e2401393c47360741ebd321679de08982dca50a` |
| Main/`visual-baseline` merge base | `cc14dd6beae526884aabdf897e309be837b4f504` |
| Current task-format main and installed CLI | `52d9f1eb7721f409bc47beb9fced7997b5c13ede` |
| Tui-snap upstream main before required repairs | `5036cf87e621e6beb66deffe3224abdbefc955cb` |
| Reviewed tui-snap PR #1 head | `883d03f19d890bbbf27468798db78b04e85297ac` |
| Reviewed tui-snap tree | `dadbaa70facc317cfabb52f0374c1f3cdceb46a1` |

[Tui-snap PR #1](https://github.com/donbeave/tui-snap/pull/1) is open and unmerged; its remote head matches the reviewed commit above. Reference acquisition must pin that head, not a moving PR branch. The final verification contract must also bind tool source, lockfiles, binary, terminal engine, profile, font, adapters, scenario manifest and reference source.

## 4. Repository and branch topology

Main and `visual-baseline` have 774 and 38 commits respectively outside their merge base, including merges. Main already contains a substantial prior integration, merged as PR #1 at `7b27732a`. Repeating those ports would duplicate or overwrite later work.

Main is now a virtual workspace with `junie-tui`, `junie-tui-testing`, `xtask` and four application packages. Oracle retains the old root library and `src/bin` applications. Deleted old paths often indicate relocation, not feature deletion. Current `visual-baseline` differs from the immutable oracle only in `docs/sources/PLANNING_GOAL.md`; that measured equality does not grant permission to follow future `visual-baseline` changes.

See [history and topology](../refactoring-plan/history.md) for parent identities, changed-path counts, chronological commits and PR evidence. Source moves make exact-path overlap an inadequate estimate of integration risk.

## 5. Refactoring timeline

The [history timeline](../refactoring-plan/history.md#timeline) reconstructs the initial app goals, architecture A–Q and later adjudications, interrupted Slice 4, component/application migrations, workspace cut, restoration attempts, prior integration, and subsequent Holla-Fable work. [The revision index](../refactoring-plan/history-revision-index.tsv) identifies parent-relative document edges and exact blobs.

The later oracle adds substantive behavior beyond the previous integration reference: expanded Holla worlds and journeys, source-preserving text and retention work, one-click editing, scroll-edge fades and truthful scroll boundaries. These are acceptance obligations even when an older main integration was called complete.

The current finite revision index contains 281 parent-relative edges: 96 architecture, 104 state and 81 other/renamed/linked-document edges, with independent Git enumeration showing no missing, extra or duplicate keys in its thirteen-path universe. [Source continuation](../refactoring-plan/history-source-continuation.md) adds previously omitted JACKIN_GOAL, prompt/reference and DESIGN history, plus explicitly recorded linked-source rereads. Earlier reports cover fifteen linked inputs; counting those reports is not a fresh semantic reread. The current 620-clause authority/task-proof re-audit is still in progress. [Ledger reconciliation](../refactoring-plan/history-ledger-reconciliation.md) preserves independently enforceable supplemental clauses and explicit supersessions. Historical pass claims remain evidence of claims, not present completion proof.

## 6. Historical decision ledger

[The canonical historical ledger](../refactoring-plan/historical-obligations-canonical.tsv) contains 620 independently retained source clauses across nine ledgers. [Architecture assessment](../refactoring-plan/architecture.md), [the current explicit adjudications](../refactoring-plan/architecture-adjudication.md) and direct-history reports distinguish current authority from proposed, rejected, amended, superseded and deferred work. [The decision index](../refactoring-plan/decision-ledger.tsv) binds each new adjudication to its implementation owner. ADJ-09–14 cover source-specific routing, controlled choice navigation, reusable ChipBar, total Form validation, the narrow backend signal broker and streaming grapheme allocation domains. Their fresh acceptance follows the current independent re-audit, not the earlier eight-decision review.

Fixed architectural direction includes caller-owned durable state, short-lived borrowed props, separate mutable update and shared-reference draw, typed responses/actions, stable identity, runtime-owned input/focus/layers/time, semantic paint provenance and one reusable implementation per family. Keep backend-free core consumers and curated application/author facades.

Do not resurrect the old widget facade, owned runtime component tree, untyped action bus, universal Widget/Theme abstraction, application-owned generic overlay stack, Grid SQL semantics or generic RGB role inference. Old illustrative names and dependency descriptions must yield to their precise accepted amendments. Product-specific domain models and legitimate custom art remain application-owned.

Deferred operational Holla providers and HP16 headless/list/run/doctor expansion remain outside scope. Preserve the oracle's deterministic simulated product experience and existing preview CLI behavior.

## 7. Current-state assessment

Current main passes the measured Rust 1.88.0 locked workspace all-target/all-feature check with warnings denied. Its document resolver passes with 880 resolved and 35 allowlisted references. The architecture boundary gate fails on capture evidence portability/completeness and missing parity evidence. Its anti-copy check passes despite demonstrated paint-over, proving that check is too weak to certify ownership.

| Application | Source-proven state |
| --- | --- |
| Showcase | 22 main pages versus 23 oracle pages; Diff missing. Enabled inputs/settings and customer Grid are partly inert or use different visible and interactive models. |
| Holla | Migrated package exists, but 11 main worlds versus 34 oracle worlds; narrower routes, different keys, preview composition, frame semantics and simulated outcomes. |
| Jackin | Migrated routes and tests exist; Settings/Usage are simplified; selected 120×40 states use historical painters alongside different live paths. |
| TablePro | Migrated models exist; query editor replaced by a small field, plan/error/history simplified, declared overlays and commands not all mounted or dispatched. |

Existing oracle-equivalent binary suites passed: Showcase 41, Holla 143, Jackin 63, TablePro 35. Main Holla passed 173 tests across five suites. These results prove those assertions execute; they do not prove cross-revision parity.

## 8. Refactoring obligation matrix

The [canonical historical obligation matrix](../refactoring-plan/historical-obligations-canonical.tsv) records 620 requirements, authority, source/implementation evidence, current assessment, remaining work, gates and assigned tasks. The [architecture matrix](../refactoring-plan/architecture-matrix.tsv) records 32 cross-cutting contracts. [Bidirectional traceability](../refactoring-plan/traceability.tsv) joins historical clauses, architecture contracts, 54 component families, 361 app scenarios, the current adjudications and separately identified source-derived contributions. New branch findings are being incorporated; final synchronized totals remain pending. Derived contributions never replace intact parent scenarios. Each task's protected source-obligations.tsv preserves exact source clauses and requirement/acceptance/check mapping.

Use namespaced references: `HIST:A01` and `ARCH:A01` are different rows. Rejected and deferred decisions remain traceable to a forbidden-path or scope disposition. A token's presence, an API export or a historical test name alone does not discharge its semantic obligation.

## 9. Component parity matrix

The [component matrix](../refactoring-plan/component-parity.tsv) covers 54 families, all 31 oracle widget modules, all 37 main component modules and the public component type inventory. [Detailed component contracts](../refactoring-plan/components.md) describe exact gaps and architecture destinations.

Shared corrections include scroll/fade ownership and arithmetic, keyboard focus versus completed-click editing, current-cell Grid activation, cross-span graphemes, retained output identity and selection, adaptive Diff projection and borrowed row/part override propagation. Runtime capture/disabled behavior, exact query attribution, registry completeness and public Props enforcement have additional historical obligations. Every family also needs preserved styling, clipping, identity, performance and production-path proof.

## 10. Application parity matrix

[Application parity](../refactoring-plan/application-parity.tsv) joins all 361 current scenario contracts: 76 Showcase, 135 Holla, 70 Jackin and 80 TablePro. Individual inventories define fixtures, actions, checkpoints, component dependencies and exact source references:

- [Showcase](../refactoring-plan/showcase.md) and [scenarios](../refactoring-plan/showcase-scenarios.tsv).
- [Holla](../refactoring-plan/holla.md) and [scenarios](../refactoring-plan/holla-scenarios.tsv).
- [Jackin](../refactoring-plan/jackin.md) and [scenarios](../refactoring-plan/jackin-scenarios.tsv).
- [TablePro](../refactoring-plan/tablepro.md) and [scenarios](../refactoring-plan/tablepro-scenarios.tsv).

These are scenario specifications, explicitly not newly captured full baselines. Required parameter expansions must be frozen from the oracle before candidate replay. Candidate selectors cannot search changed geometry to retarget an incorrect layout. [Parity synthesis](../refactoring-plan/parity-synthesis.md) records applicability and separates direct production state proof from actual PTY reachability.

## 11. Verification architecture

[Verification contract](../refactoring-plan/verification.md) specifies direct production-view frames plus real PTY executable interactions. Compare complete canonical cells and cursor, then semantic observations that distinguish identical-looking states: focus, target identity, editing, selection, scroll position, overlays and effects. Capture every specified checkpoint, including required no-op transitions. PNG/HTML diffs aid review; they do not replace exact machine equality.

Reference adapters may translate observation and controlled time only. A concrete isolated oracle clock adapter has passed all 76 existing Showcase/TablePro binary tests plus four deadline probes. Original duration literals, comparison operators and handlers remain unchanged. Unmodified CLI executables remain a separate PTY lane; elapsed wall time does not establish exact deadline boundaries.

Protect the whole proof chain: oracle SHA, adapters, required scenario membership, numeric actions, expected frames/state, comparator, hashes, tool/profile/font and environment. Missing, corrupt, skipped, duplicated, mismatched or candidate-generated expected evidence fails. No `accept`, blessing environment, weakening comparison or deleting fixtures can satisfy a task.

The [proof contract](../refactoring-plan/proof-contract.md) fixes the future command chain and host trust boundary. The previous independently reviewed comparator preparation supplies 72 cases and requires 141 invocations including recovery after every negative case. Host freeze/isolation/ref-update and project-runner qualification are separately reviewed preparation suites, with an independently controlled Darwin observer that actually launches workers. The previously qualified Rust corpus contains 41 standalone and 25 actual-production cases, with 55 required recovery invocations. The current re-audit is extending actual source-policy and style-time qualification; the [151-row previous frozen asset manifest](../refactoring-plan/bootstrap-assets.tsv) is stale while those repairs proceed and must be resealed only after independent review. No preparation result means the production harness exists. Full expected bundle hashes cannot be fabricated before capture. Their creation remains oracle-only, independently qualified and sealed outside implementation writable scope.

Early shell work cannot claim a complete cross-route scenario while later pages remain unfinished. [Shell contribution contracts](../refactoring-plan/shell-contribution-contract.md) bind specific state/routing invariants to architecture checks and separately identify nonempty complete-frame checkpoint subsets for exact comparison. Whole parent scenarios remain required, unmasked and assigned to their full closure owners. A partial contribution never marks the parent scenario closed.

## 12. Tui-snap assessment

Existing tui-snap supports production Ratatui capture, canonical frame comparison, PTY keyboard/mouse/resize, literal protocol input, cursor, style-aware settling and fail-closed stores. New tooling is not justified merely by an absent high-level convenience method.

Measured upstream defects include DIM arithmetic and wide continuation style loss. Existing bundled repairs also retain hidden/blink/strike state, autowrap/physical cursor and literal LF paste. A fresh review found four additional defects: normal downstream compilation, wrap-mode serialization, hidden SVG spacing and non-reproducible migration. All four were corrected and independently re-reviewed at `883d03f19d890bbbf27468798db78b04e85297ac` before publishing the dedicated PR.

Final measured gates passed: 51 all-target/all-feature tests; 41 no-default-feature tests including doctest; an explicit doctest; formatting; Clippy with warnings denied; an ordinary downstream consumer without a root patch; reproducible migration and negative guards; and an independent 384-cell PTY fixture. Fresh review also checked all 256 modifier combinations across full/differential serialization and both wrap modes. See [tool evidence](../refactoring-plan/evidence/tuisnap-assessment.md) and [independent review](../refactoring-plan/tuisnap-review.md). No tool PR will be merged automatically. The exact reviewed PR head is a hard prerequisite of reference qualification.

## 13. Chosen integration strategy

At the start of future execution, create the isolated integration branch from pinned main before proof and baseline preparation. Those accepted preparation commits form the same auditable ancestry chain as later work. Production refactoring remains prohibited until complete oracle and test-disposition receipts are sealed. Preserve the workspace, reusable API, runtime and accepted safety/identity protections. Adapt oracle app composition, deterministic domain fixtures and behavior through those APIs. Do not merge the two full branches, recreate the legacy API, or cherry-pick stale ports wholesale. No branch is created during this planning goal.

Use canonical standalone `taskfmt verify` on isolated worktrees. The pinned automated dispatcher and promotion workflow hardcode main; they must not target the original repository for this integration. Standalone verification supports explicit root, task directory and immutable base. The host must independently freeze and verify each candidate, enforce predecessor ancestry and trusted inputs, and integrate the exact tested tree with an expected-parent check. Monitoring readiness alone is not code ancestry or a host verdict.

The [proof contract](../refactoring-plan/proof-contract.md) defines host preparation, isolation, frozen-tree verification, sealing and expected-parent integration. No execution branch, staging remote, production port or merge is created by this planning goal.

## 14. Dependency DAG

The [canonical catalog](../../refactoring-tasks/README.md) contains 73 tasks. [The derived graph](../refactoring-plan/task-graph.md) is computed from actual task.toml dependencies and verify.toml scopes; [its complete machine representation](../refactoring-plan/task-graph.json) retains every edge and longest-path predecessor. The initial 69-outcome proposal is historical decomposition evidence, not dependency authority.

TASK-001 qualifies comparator/host core. TASK-070, TASK-071 and TASK-072 separately qualify runner, accounting and architecture operations; their numbers do not place them after implementation. TASK-073 creates truthful attribution/registry infrastructure before reusable repairs. TASK-031 closes complete conformance after those repairs. This removes circular acceptance and prevents one narrow bootstrap suite from certifying unrelated commands.

## 15. Execution waves

The graph uses these dependency-defined waves; exact earliest layers appear in the derived graph.

| Wave | Tasks | Exit condition |
| --- | --- | --- |
| Qualified proof | 001, 070–072 | Independently accepted comparator, host, runner, accounting and architecture products |
| Immutable preparation | 002–008 | Four oracle namespaces, complete component baseline, exact inventory and approved test dispositions sealed |
| Attribution foundation | 073 | Truthful resolution/registry observations; no premature whole-family closure |
| Reusable system | 009–031 | Shared runtime/theme/layout/text/components restored and generated conformance complete |
| Applications | 032–064 | Four independently staged application chains, each ending in complete oracle closure |
| System closure | 065–069 | Ownership, exact test relocation, real performance, public API/docs and merge readiness |

## 16. Task index

All packages live under [refactoring-tasks/terminal-components/completion](../../refactoring-tasks/terminal-components/completion/README.md). [The task index](../refactoring-plan/task-index.tsv) lists all 73 IDs, keys, titles, hard dependencies, scopes and purposes. Packages contain task/v5 README, canonical execution protocol, verify/v2, task-meta/v1 and protected detailed obligations. [Task-format assessment](../refactoring-plan/task-format.md) records the exact current source and parser behavior. Final whole-catalog validation is recorded in the acceptance audit, not inferred from package existence.

## 17. Per-task purposes and dependencies

The task index and individual packages distinguish live behavior restoration, already-implemented preservation proof, architecture gate strengthening, fixture capture and final validation. Every package names exact paths, fixed decisions, non-goals, requirement/acceptance/check mappings and source/scenario ownership. The traceability join permits both requirement-to-task and task-to-requirement queries; rejection and deferral rows remain explicit scope/disposition contracts. A future command has a named prerequisite producer and independently authored qualification inputs; its mere name never establishes proof.

## 18. Parallelization opportunities

Independent oracle app capture and inventory preparation may proceed in parallel after shared contracts exist. Ready disjoint reusable modules may run concurrently; four application directories allow independent restoration after their shared prerequisites. Hard dependencies serialize every currently measured source overlap, including TASK-065 before TASK-066 for xtask/src/main.rs. The graph derivation fails on unsupported wildcard scope and records any new unordered overlap for explicit resolution. Separate worktrees and target/artifact directories prevent worker interference. Joins require integrated-tree verification, not only individually green siblings.

## 19. Critical path

The current canonical DAG has maximum dependency depth 35 and 24 equally deepest paths. Its complete longest-path predecessor representation and one exact 35-task path are in the derived graph. Proof qualification, complete oracle preparation, attribution, reusable runtime/scroll/field/layer/editor work, the Holla chain and final closure drive these paths. The TASK-065/TASK-066 serialization is a hard edge and therefore included. This is an unweighted structural result, not a fabricated duration estimate.

## 20. Merge-readiness gate

One exact final integration head must pass all authoritative stable/MSRV/build/test/lint/doc/backend-free/API/architecture/performance gates, every required direct and PTY scenario for all four apps, immutable expected-evidence checks, exact historical-test disposition and final independent reviews. No obsolete duplicate architecture or app-local component replacement may remain.

The gate must compare final candidate output with the immutable oracle, prove expected artifacts were not regenerated from candidate code, and establish the planned branch ancestry. A source change after verification invalidates its verdict. Planning does not authorize the merge; later execution must verify the resulting merge tree as well.

## 21. Known risks and required resolutions

Historical authority is layered and sometimes contradicts later commits. Main test names and green source scanners can overstate their actual proof. Old self-baseline expectations can conflict with the new immutable product oracle. These conflicts require an explicit oracle-derived test disposition that preserves historical artifacts and useful structural assertions; repair tasks cannot silently change expected output or suppress tests.

Parameterized scenarios need complete, deterministic oracle-only expansion; broad prose such as “all states” is insufficient. Exact clock proof and unmodified PTY proof have distinct boundaries. Tool schema migration and vendored terminal behavior require fresh downstream tests. Dynamic identity, capture, query attribution, callbacks and alternative paint paths need nonvacuous adversarial tests.

## 22. Explicit non-goals

No UI redesign, UX polish, new product features, approximate parity, operational provider wiring, deferred Holla CLI expansion, main merge, automatic tool PR merge, legacy API revival or opportunistic terminal-components bugfix is part of this planning run. Legitimate product art and app domain decisions remain where they belong; shared component behavior must not be copied into apps.

## 23. Evidence and commit references

All evidence is linked through [planning progress](../refactoring-plan/PROGRESS.md), the historical revision index, matrices, app inventories and tool evidence. Read commit-qualified sources rather than assuming a same-named working-tree path is authoritative. Current measurements record their environment and proof limits. [Planning acceptance audit](../refactoring-plan/planning-acceptance.md) tracks every success requirement.

## 24. Independent review and corrections

Independent app/component/API/history investigations precede decomposition. Cross-application synthesis corrected a Jackin fixture world and rejected treating a moved Holla path as app absence. Fresh tui-snap review rejected four concrete defects, then approved their independently verified corrections at the exact PR head recorded above.

Bootstrap review reproduced concrete false-pass executables rather than accepting self-test counts: case-label lookup bypassed comparison, and fabricated worker logs bypassed execution/isolation proof. Repairs add opaque challenges, strict complete result schemas, independent worker execution and protected observation. [Bootstrap rereview](../refactoring-plan/bootstrap-review.md) independently accepts the repaired comparator and bounded Darwin host preparation. Later production implementations still have to pass the frozen suites.

The earlier five-reviewer pass covered architecture/API, UI/interaction, decomposition/dependencies, verification and integration safety. Its 18 findings in [the previous final-review register](../refactoring-plan/review-findings.tsv) have recorded independent repair-review closure. That is historical evidence, not acceptance of today's revisions. The reopened source-first audit found further scope omissions, source-inconsistent transcripts, missing shared APIs and qualification gaps. Current findings and independent dispositions are in [the re-audit finding register](../refactoring-plan/reaudit-findings.tsv); material work remains open until repaired bytes, all source/task joins, executable qualifiers and the final manifest are independently checked.
