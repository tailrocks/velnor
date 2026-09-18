# Lane C04/C05: committed-agent default fix — report

Branch: `feat/multi-account-support`. Own files only:
`crates/jackin-console/src/tui/prompts.rs`,
`crates/jackin-console/src/tui/input/list.rs` (new-session picker blocks),
`crates/jackin-console/src/tui/input/list/tests.rs`. No commit, per instructions.

## What changed

**`tui/prompts.rs`** — new shared resolver + rewritten `launch_with_committed_agent`:
- `resolve_agent_default(config, workspace, role, agent) -> AgentDefaultResolution`
  (`Launch(id)` / `NoDefault` / `Invalid(msg)`). Line-for-line mirror of
  `jackin_config::resolve_account` binding lookup: role → workspace → global,
  global filtered by workspace authorization (never widens access),
  role/workspace unauthorized + unknown-id + incompatible/disabled-account
  bindings are hard `Invalid`, unknown workspace is `Invalid`.
- `select_launch_account(config, workspace, role, agent, eligible)`:
  valid default → `Launch` (no picker, even with many candidates);
  no default → sole eligible launches, several become `Pick` in
  id-ascending order, zero is an actionable error (names agent + scope,
  points at add-account / set-binding remedies); dangling allowlist ids
  error like `resolve_account` instead of being skipped.
- `sort_account_choices_by_id` — the single documented stable order
  (**ascending account id**) used by both pickers.
- `no_eligible_account_message(agent, scope)` — shared actionable text.
- `launch_with_committed_agent` now parses the workspace name early and
  dispatches on `select_launch_account`. `Launch` always carries
  `account: Some(id)` (the old `None` launch is gone); `Pick` opens the
  picker; `Err` restores pending launch + role plans under the existing
  restore contract (adapter shows "Launch failed" popup, console stays alive).
- 21 inline precedence-table tests (see below).

**`tui/input/list.rs`** — new-session picker, open + commit (usage `u` block untouched):
- Open path factored into `open_new_session_picker` (also fixes a
  clippy `too_many_lines` my lines introduced). It calls
  `prepare_new_session_accounts`, which sorts by id and resolves
  `account_bindings` for **every** agent up front (commit runs without
  config access — `ManagerState` holds none and dispatch threading is
  out of scope): valid default → other accounts stop offering that agent
  (commit dispatches the default, no picker); explicitly invalid binding →
  agent hidden everywhere (commit fails atomically, no silent fallback);
  no default → candidates intact (commit opens the picker when several).
- `handle_new_session_picker` commit: re-sorts filtered candidates
  (deterministic for any producer), empty → dismisses picker + opens
  `ErrorPopup("No eligible account", …)` naming agent + workspace/container
  instead of dispatching `NewSessionWithAccount { account: None }`.
  Doc comment updated (old daemon-owns-env note was stale).
- NOTE for orchestrator: precise invalid-binding reasons (unknown id vs
  unauthorized vs incompatible) cannot reach the commit-time popup without
  config; threading `config` into `handle_new_session_picker` (1-line
  `dispatch.rs` change + signature) would let commit resolve directly and
  retire the prune encoding. Current popup is actionable but generic.

**`tui/input/list/tests.rs`**:
- 8 new tests (empty/other-agent commit → popup, id-order render,
  open-honors-default → direct `Some("z-claude")` dispatch, no-default →
  sorted picker, invalid role binding → atomic error, global-unauthorized
  filtered → picker, workspace-unauthorized → hard error).
- 1 existing assertion updated: `new_session_…_codex_with_multiple_providers`
  now expects `[minimax, openai]` — the old `[openai, minimax]` insertion
  order contradicts the newly required documented id-ascending order.

## Behavior deltas (operator-visible)
- Configured default + several accounts → launches default, no picker (was: picker).
- Zero eligible → error popup (was: launch/session with `account: None`).
  New sessions on running containers with no host-config account for the
  agent now error instead of daemon-default dispatch — intended per
  no-ambient-fallback (T02 §3); flag if C11/C13 wants manifest-scoped softer handling.
- Invalid explicit binding → atomic error, never fallback to other candidates.
- Picker order is now contractually ascending account id.

## Verification
- `cargo check -p jackin-console`: clean. `cargo clippy -p jackin-console`:
  only pre-existing sibling warning (`single_match_else` in
  `input/global_mounts/auth.rs`, untouched). `rustfmt --check` on owned files: clean.
- `cargo test -p jackin-console` (incl. prompts/picker filters) **cannot link**:
  the lib test target fails with 5× E0063 in `tui/screens/usage/tests.rs`
  (sibling lane: initializers missing `metric_groups`,
  `credential_expires_at_epoch`, `remaining/used_raw_percent`), a file this
  lane must not touch. `cargo check --tests` proves all 5 errors are in that
  file only — prompts.rs/list.rs/list/tests.rs (incl. all new tests) compile.
- Stand-in proof: 29 temporary integration probes mirroring the committed
  tests 1:1 through public API — **21/21 prompts + 8/8 list green** — then
  deleted (`crates/jackin-console/tests/` left empty, no repo footprint).
- Existing `launch_with_committed_agent` restore test path unaffected
  (errors before selection, same restore); `update/tests.rs` followup-plan
  tests unaffected (that fn untouched).

## Test inventory (committed)
prompts.rs (21): role>workspace>global precedence ×2, other-role ignored,
global-honored-with-several, sole-eligible fast start, picker id-order,
order stability, zero-eligible actionable (saved + ad-hoc), empty-allowlist
vs missing-workspace, unknown workspace, role/workspace unauthorized hard
errors, global unauthorized filtered, global unknown-id filtered,
unknown-id at role/workspace scope + dangling allowlist id, incompatible,
disabled, empty-string binding atomic failure, cross-agent isolation.
list/tests.rs (8, listed above).
