# Session coordination registry

Binding for every agent session executing `plans/goal-execution/README.md`
against branch `velnor-estate-standard`. Operator confirmed 2026-08-24 that
three concurrent OpenCode sessions execute this goal together. These rules
prevent exclusive-scope collisions.

## Rules

1. **Claim before write.** Before any writer subagent touches leaf scope, its
   session appends a row to the Active claims table in a commit on
   `velnor-estate-standard` and pushes. A claim names exactly one leaf.
2. **One writer per leaf.** A leaf with an unexpired claim must not receive a
   second writer. Read-only investigation, verification, and review may run
   concurrently.
3. **Claim expiry.** A claim expires if no leaf-scoped commit lands within
   60 minutes of its claimed-at timestamp, or its session records RELEASED.
4. **Collision recovery.** If mixed uncommitted work from two writers exists,
   the session that arrives second must STOP writing (record evidence under
   `.velnor-compare/<date>-<leaf>-writer-conflict/`), then reconcile FORWARD
   from the coherent design already on disk once ownership resolves. Never
   silently overwrite another writer's uncommitted work.
5. **Plan wins.** Reconciliation never weakens a leaf's done criteria or STOP
   conditions; where a peer design conflicts with the leaf file, the leaf file
   governs and the divergence is recorded in the leaf's execution-evidence
   block.
6. **Shared external resources.** Fixture dispatches, live mutations, and
   status-index commits remain serialized across sessions regardless of leaf.
7. **Commit and push everything (operator directive 2026-08-24).** Every
   session commits and pushes its own outputs immediately: leaf code, plan and
   index updates, and sanitized `.velnor-compare/` evidence included. Foreign
   dirty files inside another session's active claim are the sole exception —
   never staged or committed by anyone but their owning session.

## Active claims

| Leaf | Session | Claimed at (UTC) | Status |
|---|---|---|---|
| 039 | ox-alpha session C (takeover) | 2026-08-24 ~12:00Z | ACTIVE — prior `parallel opencode actor` claim EXPIRED per rule 3 (last leaf-scoped commit eea87eb 10:39Z, >60 min idle; no uncommitted fleet files, no unpushed commits at takeover) |
| 065 | Session B | 2026-08-24 ~10:40Z | DONE @8734b4b — reconciling-executor convergence complete (gates 943/943 at landing); verification chain closed 2026-08-24 |
| 066 | orchestrating ox-alpha (validator session) | 2026-08-24 ~13:30Z | CLAIMED — Track B next-ready leaf; implementation via fresh writer subagent on this branch; other sessions read-only on 066 scope |
| 065-defect-fix | Session C validator (ox-alpha) | 2026-08-24 ~13:45Z | CLOSED @f06d439 — fail-closed opaque rejection landed; verifier PASS 973/973; reviewer APPROVE-CLOSE; evidence addendum in 065 file |
| C005-man | Session C validator (ox-alpha) | 2026-08-24 ~14:05Z | IMPLEMENTED+REVIEWED, pushed @b98801e+0630a98 (18/18 command_c005, full check 997/997, verifier PASS, reviewer APPROVE-FLIP findings closed). FIXTURE DISPATCH WINDOW OPENED ~15:20Z: pin `bd4be09375154e891052b5159801f613fa0b4f09`, cancel-clean-dispatch-monitor hygiene, single dispatch, sanitized evidence only. Other sessions: do NOT dispatch fixture runs until this row closes. DONE flip after fixture proof lands |
| clap-cli-migration | third-peer session (074-078 pool owner) | 2026-08-24 ~15:45Z | CLAIMED — operator directive: rewrite `crates/velnorctl` CLI on idiomatic stable `clap` derive APIs (PR #286 surface); scope = crates/velnorctl/* + workspace Cargo.toml/Cargo.lock clap deps + its tests; deletes handwritten parser/metadata/man generation in favor of clap/clap_complete/clap_mangen; supersedes temporary 065 parser implementation per operator authority; divergence from landed 065/C005 evidence recorded in this row. Other sessions read-only on velnorctl until CLOSED | |

## Decisions

- **2026-08-24 OPERATOR RULING — leaf 039 removals + probes** (verbatim
  intent): "Never remove any repositories I listed. We must keep all of them
  to run on Velnor by default, same as any other repositories we have. Those
  repositories aren't unique and must follow the same principles."
  Consequences recorded by ox-alpha session C:
  1. NO removals: cloudflare-tofu (tailrocks, ChainArgos),
     ChainArgos/github-terraform, jackin-project/jackin-github-terraform stay
     selected in `velnor-trusted` groups. The leaf's removal diff is dropped;
     its STOP was honored (nothing was ever applied).
  2. The four repos must become FIRST-CLASS estate members ("same
     principles"): each needs canonical estate-standard workflows so real
     release-ref closure entries can replace operator mandate. Follow-up
     program work: extend class map + generator coverage + repo workflows;
     until then generated policy cannot yet include them via closure alone.
  3. Ref-shape ruling DEFERRED by operator: no probe dispatches now; live
     restriction flip stays gated on it.
  4. Plan 039 therefore remains IN PROGRESS: code/docs/evidence surface DONE
     (see leaf Evidence block @75a60d0); steps 3–5 (live) await ref-shape
     ruling + closure path for the four kept repos. Required doc
     reconciliation (VELNOR_PROJECTS_SETUP class map, roadmap) flagged for
     the owning sessions before any further live step.
  5. Foreign `git stash@{0}` ("s-e-verify", holds a velnorctl src snapshot)
     belongs to another session — nobody drops/pops it blindly.

- **2026-08-24 leaf 039 slice wave 2** (ox-alpha session C): lanes-checker
  alignment `8b8b4b6` (generator-owned plural `lanes` is canonical; audit-ci
  exits 0 on repo), removal-reason labeling + dual-input guard `357410c`,
  docs truth `4d24356`+`faff2a2` (all active-doc 13-repo claims marked
  superseded), leaf Evidence + execution-reconciliation block `75a60d0`.
  Reviewer APPROVE on 8b45fe0+8b8b4b6. Ref-shape STOP + per-org digest
  approvals now the blocking operator decisions for live steps 3–5;
  repo-side systemd audit-timer unit slice queued next.

- **2026-08-24 leaf 065 defect-reopen** (Session C validator): DONE flip
  `c27a83d` predates round-2 review findings; reviewer verdict was BLOCK on a
  MAJOR (opaque/cannot-be-a-base URLs bypass redaction via silent
  `set_username` failure) and deferral item 4 was unrecorded. Per "no DONE
  item with unresolved finding", the 065 surface is reopened as a narrow
  defect-fix slice by this session's subagents; status stays DONE only with
  the fix landed, gates rerun, and an evidence addendum in the 065 file.

- **2026-08-24 leaf 065 DONE**: verification chain complete (convergence
  ce98a27 → FIX-FIRST repairs 8734b4b → seam-review DBG fix 04321e9); minors
  deferred to C002 metadata task.

- **2026-08-24 leaf 039 progress** (ox-alpha session C): step-1/6 code surface
  landed and independently verified — `041079c` deterministic offline
  `fleet-policy generate` (byte-identical to reviewed snapshots, audit-ci rules
  `fleet-policy-ledger/-current/-extra/-generate`, `fleet-generate` in check
  chain) and `cf3ba76` idempotency + concrete TOML-parse coverage from review
  findings. Verifier C1–C9 VERIFIED; reviewer APPROVE, actionable findings
  fixed in `cf3ba76`. Remaining before DONE: ref-shape regeneration ruling,
  terraform-repo selection contradiction resolution, scheduled read-only audit
  wiring, stale-docs sweep, operator digest approval, then live steps 4–5.
  Pre-existing finding recorded for owners: this repo's `.github/workflows/ci.yml`
  fails four `generated-caller` lane checks (audit-ci exit 1 independent of
  fleet scope).

- **2026-08-24 leaf 065**: two writers interleaved inside `crates/velnor-model`
  (evidence: `.velnor-compare/2026-08-24-065-writer-conflict/`). Per operator
  direction, sessions cooperate: the coherent on-disk design is the canonical
  base; this session's conflicting files yield to it except where the leaf file
  requires otherwise (fail-closed serde, schema-versioned envelope, Source
  LOCAL\|GITHUB\|MERGED semantics). One reconciling executor finishes 065.
  OUTCOME 19:20Z+07: reconciliation commit ce98a27 adopted the canonical
  surface wholesale, repaired non-compiling defects (snapshotted under
  /tmp/p065-snap/), full gates 943/943 exit 0, pushed.
- **2026-08-24 ownership map** (Plan 039 prerequisites-first session):
  - **Session A** (039 claim row): Plan 039 Track A, fleet surface
    (`fleet/release-refs.toml`, org policy drafts, `restricted_to_workflows`
    prerequisites), fixture control-plane validation follow-ups.
  - **Session B**: Track B sequence Plans 064–073 including 065 in flight;
    owns current `crates/velnor-model/*` + `crates/velnorctl/*` WIP and
    `Cargo.lock`.
  - **Session C**: unassigned / C-command pool once Session B dependencies
    close.
  - Standing constraints restated: foreign dirty files are never touched or
    staged by another session; leaf status flips are atomic commits by the
    owning session only; fixture `main` changes go through PRs only.
  - Evidence for this map: `.velnor-compare/2026-08-24-039-snapshots/`.
- **2026-08-24 ~12:05Z third-peer registration** (ox-alpha, fresh OpenCode
  session): takes the unassigned Plans 074–078 pool that the ownership map
  leaves outside Session A (039) and Session B (064–073). Claim order follows
  the execution graph: 076 first after 068 DONE; 074 after 065+066+068; 075
  after 066+067; 077 after 069; 078 after its full range. Each claim lands as
  an Active-claims row before any writer subagent starts. C-command pool claims
  follow later per graph priority. No leaf is claimed by this session yet;
  nothing dependency-ready and unclaimed exists at registration time.
- **2026-08-24 ~19:05Z+07 registry restore** (orchestrating ox-alpha session,
  Track-B validator role): the previous registry content at this path was
  unintentionally replaced by a shorter protocol draft in commit 24d09d6; this
  commit restores the 34a6dad registry verbatim and appends only outcome
  records. Lesson recorded: READ this file at HEAD before every write to it;
  treat an existing registry as authoritative over any local draft. Role
  restated for disambiguation: this session performs orchestration,
  independent verification/review of landings (including Session B's), atomic
  status flips for leaves it verified, and fixture-repo PR work; it does not
  hold standing leaf-implementation claims while another session is active in
  a lane.
