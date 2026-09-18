# Audit: Console Settings + Account Picker + Console Adapter

Scope: `crates/jackin-console/src/tui/screens/settings*` (all files), editor state (auth/bindings),
`tui/prompts.rs`, `components/account_picker.rs`, `crates/jackin/src/console/adapter/run.rs`,
`tui/input/list.rs` (usage open/refresh paths). No code edits.

## 1. Settings Accounts (Auth tab) — list / add / edit / enable / default

State: `SettingsAuthState` (`screens/settings/model.rs:1323`) — `pending`/`original: BTreeMap<String, AccountConfig>`,
`bindings`/`original_bindings: BTreeMap<Agent, String>`, `github`/`original_github`, `selected: usize`,
`selected_kind: Option<AuthKind>` (write-only in practice: `has_selected_kind()` hardcodes `false`,
`model/auth_impls.rs:67`), `editing_account`, `editing_text`, modal chain, `pending_op_commit`.

Row layout (`view.rs:auth_state_lines`, `auth_impls.rs:row_count`): `pending.len()` account rows +
`ACCOUNT_KINDS.len()` (8) "+ Add {kind} account" sentinels + 1 GitHub row. Rendered line per account:
`{name} [{id}] · {enabled|disabled} · {provider} · {source} [+ " · default: {slugs}"]`.

Key handling (`tui/input/global_mounts/auth.rs:handle_auth_key`):
- `d`/`D`/`Delete`: `delete_selected_account()` — removes `pending[nth(selected)]` + prunes bindings. No confirm.
- `e`/`E`: `toggle_selected_account_enabled()` — flips `enabled`; disabling prunes bindings referencing the id.
- `f`: DefaultAgent text input → `toggle_account_default(id, agent)` (toggle semantics; errors if account
  missing/incompatible). `r`/`b`/`m`: Name / BaseUrl / Model text inputs (base/model require ApiKey credential).
- `Enter` (`EnterKind`/`OpenForm` via `settings_auth_key_plan` with `has_selected_kind=false` hardcoded):
  `open_settings_auth_form()` — three branches by cursor position: GitHub row → Github form from
  `auth.github`; existing account row → form from stored credential (Profile→Sync+folder, ApiKey, OAuthToken);
  "+ Add" sentinel → blank form for that kind (Zai/Minimax default ApiKey, else Sync). New ids minted as
  `{provider-slug}-{n}` (`persist_settings_auth_form`, auth.rs:616-625); base_url/model preserved on edit.
- Form modal (`AuthForm`): Mode cycle, credential source picker (Plain→text input / Op→op picker with async
  1Password validation via `pending_op_commit`), source-folder browser (validated per-kind), Save →
  `persist_settings_auth_form` writes into `pending` (in-memory only), Reset → `clear_settings_auth_kind`
  deletes the account (or resets GitHub).
- `s`: save preview; `q`/`Esc`: discard-confirm or return.

## 2. Draft vs immediate-save behavior

All Settings edits are draft (`pending` vs `original` per panel; `is_dirty`/`change_count`/`discard_all`/
`mark_saved` in `model.rs`). Nothing persists until `s` → `MountPreviewSave` modal (`build_settings_save_lines`)
→ Commit → `SettingsModalOutcome::SaveSettings` → `ConsoleEffect::SaveSettings` → `execute_settings_save`
(`crates/jackin/src/console/effects.rs:700`) → `start_settings_save` on a worker thread →
`apply_settings_save_result`: on Ok replaces in-memory `AppConfig`, `mark_saved()`, sets
`mounts.exit_requested` (leaves Settings); on Err shows panel error. No immediate per-action writes anywhere
in Settings. Editor auth tab is likewise draft-based (`edit_account_row`, `workspace.rs:242`).

## 3. Committed-agent path default handling

`launch_with_committed_agent` (`prompts.rs:162`): resolves via `accounts_for_launch` =
enabled + workspace-authorized + agent-compatible accounts. If `<= 1`: launches immediately with
`accounts.first().id` (or `None` when zero — no error). If `> 1`: opens launch account picker; operator picks.
**Nowhere consults `account_bindings`** (global, workspace, or role). Contrast `resolve_account`
(`jackin-config/accounts.rs:382`): role → workspace → global binding chain, falling back to single-candidate
or ambiguity error. So: a configured default is ignored (extra picker step when >1; arbitrary-first pick is
moot at ≤1 but the binding is still not validated), and the zero-account case launches with `account: None`
instead of erroring. Same for the new-session picker (`list.rs:670`): filters by agent, no default preference.
Editor *displays* bindings (`auth_tab.rs:44`, "automatic" fallback) and *edits* them (`edit_account_row`
cycles candidates), but the console launch path never reads them.

## 4. Console adapter broker-read flow (`console/adapter/run.rs`)

`load_console_usage_state(paths, force_refresh)` (run.rs:54): builds `HostUsageRuntime`, `open_with_discovery`
(Live policy, HostDesktop scope), `validated_discovery`, `ensure_usage_broker_process`, then per
`usage_broker_capabilities(discovery)`: `current()` (+ `refresh(gen, true)` and blocking `join(10s)` when
force), `apply_broker_generation`, finally `canonical_projection("und")` → `UsageScreenState::from_projection`.
Called at startup with `force_refresh=false` (run.rs:1142, result cached into `manager.usage_accounts`/
`usage_notice`) and on `r` with `true` (see §5). Doc comment claims "Refresh remains broker-owned and is
requested by the route later" — but the route never requests it; refresh is driven solely by the `r` key hook.

## 5. Usage open/refresh flow

- Open: `handle_list_key` (`list.rs:56`) on `u` snapshots cached `usage_accounts`/`usage_notice` into a fresh
  `UsageScreenState` (selection reset) and sets `usage_screen`. No broker call on open — shows startup snapshot.
- Keys (`usage.rs:handle_key`): `Esc`/`q` close, arrows/`j`/`k` move, `Enter` toggles detail, `r` is a **no-op
  inside the route** (`KeyCode::Char('r') => {}`).
- Refresh: adapter-level `refresh_console_usage_on_key` (run.rs:121) intercepts **every** `r` keypress
  *after* normal dispatch, when `usage_screen.is_some()`: calls `load_console_usage_state(paths, true)`
  **synchronously on the UI thread** (broker `refresh` + up-to-10s blocking `join` per capability), then
  overwrites both the cached and visible accounts/notice; on error sets "Usage unavailable: …" notice.
  Also fires for `r` pressed in unrelated contexts while the usage overlay is open.

## 6. Gaps vs spec

1. **No scan action.** CLI has `account scan` (`app/account_cmd.rs:77`, `discover_default_accounts` + env import);
   Console Settings Auth tab has no equivalent key, button, or row — zero references to scan/discover in
   `tui/`. Operators can only add accounts one-by-one via sentinels.
2. **Sync joins.** Refresh path runs broker `refresh` + blocking `join(Duration 10s)` per capability on the
   event-loop thread during key handling (`run.rs:92-114` via `refresh_console_usage_on_key`). Contrasts with
   the codebase's `spawn_blocking_subscription` pattern used for saves/refreshes elsewhere; a slow broker
   freezes the TUI up to 10s per capability with no progress UI.
3. **Positional selection.** Auth list cursor is a raw index into `BTreeMap` iteration order
   (`pending.iter().nth(selected)` in `toggle/enable/open/delete`, `auth.rs:48,134,195`, `auth_impls.rs:175,195,224`):
   insert/delete shifts every row's identity; `selected` is clamped by position (`settings_auth_selected_index`),
   not re-anchored to an id (only form-save re-anchors, `auth.rs:640`). Same positional pattern in
   `UsageScreenState::{set_accounts, move_selection}` (index 0 = Overview sentinel, accounts at n+1).
   `has_selected_kind()` hardcoded `false` also deadens the `ClearKind`/`EnterKind` distinction in the key plan
   (Enter always opens the form; Esc never clears kind).
4. **Dropped IDs.** `UsageScreenState::from_projection` retains only display strings
   (`UsageAccount { provider, account, status, windows }` — no `provider_id`/`capability_id`/account key;
   unresolved rows stringify the capability id into `"Unresolved ({id})"`). Downstream code therefore cannot
   target refresh/status per capability — refresh is all-or-nothing, and rows are matched by position only.
   (Account pickers do preserve `AccountChoice.id` end-to-end — the drop is specific to the usage route.)
5. **Secondary (in scope of "default flows"):** `toggle_account_default` rejects *disabled* accounts only via
   `supports_agent` (which folds enabled in — actually blocks disabled correctly), but `validate_accounts`
   permits bindings to disabled accounts (no enabled check, `accounts.rs:459-485`); editor binding cycling
   filters by `supports_agent` over *assigned* accounts only, so a default can point at an unassigned id only
   via Settings-global bindings + workspace without that id — `resolve_account` then hard-errors at launch.
   Also `delete_selected_account` silently no-ops on sentinel/GitHub rows (cursor can rest on them with `d`).
