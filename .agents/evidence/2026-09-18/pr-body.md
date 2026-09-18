## Summary

This pull request ships complete multi-account support for jackin❯: Console Settings manages every AI account including several accounts per provider, first-run discovery registers evidenced accounts from real default paths, workspaces authorize and admit account sets into shared containers, and the Usage route reports per-account quota from a broker-fed canonical model. It also retires the one-account-per-agent assumptions across config, launch, session identity, and usage projection.

## What ships

- Settings → Accounts lists, adds, edits, and scans all accounts: subscriptions, API keys, custom folders/profiles, environment references, and 1Password references, with masked secrets throughout logs, docs, and diagnostics.
- First-run bootstrap distinguishes fresh install, installer-seeded config, and upgrades (never resurrecting removed accounts); Settings scan joins a pending draft with atomic apply/cancel under lock.
- Workspace authorization vs container admission vs session bindings are three separate decisions, with ordered default resolution (launch → workspace-role → workspace → global → sole-eligible) and atomic rejection of invalid selections.
- One container hosts repeated agent instances (e.g. two Claude accounts plus Codex) with per-instance config/HOME/XDG/keyring contexts, ambient-credential scrubbing, and guaranteed exclusion of unselected accounts from staged stores, Docker metadata, and relay capabilities.
- Session/tab/pane metadata carries real account identity end to end; new tabs validate against the container manifest and cross-account changes require explicit manifest revision.
- Usage overview shows cached per-account values with per-source ages, percentage bars with reset countdowns, and honest alternative text for balance-only or unpublished quotas; detail preserves raw over-100% values and scope-correct balances, credits, and caps.
- Provider coverage for Claude, Codex, Amp, Antigravity, Kimi (both auth families plus Codex-routed configs), Z.AI, Muse, Cursor, Grok, OpenRouter (exact model IDs), omp, Hermes, OpenCode, MiniMax, and Gemini CLI as a separate Google client, with a catalog-to-support ledger and exact reasons for unsupported metrics.
- Live three-account tracer proof: one container running Claude · Work, Claude · Personal, and Codex · Work concurrently with labels, plus new tab, split, resize, exit, reattach, and restore.

## What this addresses

- Operators with several subscriptions for the same provider can finally register and launch all of them instead of fighting one-slot-per-agent config.
- Upgrades no longer lose deliberately removed accounts, and concurrent scans can no longer silently overwrite edits.
- Usage numbers stop conflating dollars, credits, requests, and tokens, and stop inventing refills or merging unrelated organization caps.
- Container launches stop leaking ambient logins or unselected credentials into shared trust boundaries.

## Not included

- Antigravity deep usage beyond the official read-only commands, and providers with no local credentials on the verification Mac (live-unverified lanes stay recorded in the ledger).
- Automated jackin-dev release publishing (removed upstream by the velnor-workflow regeneration; validation remains in the generated PR workflow).

## Verify locally

### Checkout

```sh
export TIRITH=0
```

```sh
jackin-dev pr sync 1002
cd "$(jackin-dev pr path 1002)/jackin"
source "$(jackin-dev pr path 1002)/env.sh"
which jackin
```

### Static checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

### Rust tests

```sh
cargo nextest run -p jackin-core -p jackin-instance -p jackin-config -p jackin-env -p jackin-protocol
cargo nextest run -p jackin-usage -p jackin-usage-ffi -p jackin-runtime -p jackin-console -p jackin-capsule -p jackin
cargo xtask ci
```

Covers account/discovery/launch/session/usage units, feature-powerset, doctests, audits, and the docs/roadmap/research partitions.

### Schema migration smoke

```sh
if [ -d "$HOME/.config/jackin" ]; then
  cp -a "$HOME/.config/jackin" "$JACKIN_CONFIG_DIR"
else
  mkdir -p "$JACKIN_CONFIG_DIR"
fi

mkdir -p "$JACKIN_HOME_DIR"
```

Expected: config migrates to v1alpha11 and workspace defaults to v1alpha10 without resurrecting removed accounts. Live `~/.jackin` is never read or mutated.

### Docs checks

```sh
(
  cd docs
  bun install --frozen-lockfile
  bun run build
  cargo xtask docs repo-links
  cargo xtask roadmap audit
  bunx tsc --noEmit
  bun test
)
```

### User smoke

```sh
jackin console --debug
```

Press `S` for Settings, open the Accounts tab, and confirm every registered account renders with masked secrets plus `+ Add` rows. Open Usage and confirm per-account rows with ages, then a detail view with quota bars and reset countdowns. Faster repeat checks: `jackin account scan --debug` and `jackin load the-architect . --dry-run --debug` (dry run lists resolved instances).

### jackin-capsule smoke

```sh
jackin load the-architect . --debug
```

Inside the container, verify:

- Row 0 status bar is visible: `jackin❯  [<agent-name>]`
- Agent TUI starts and renders correctly below the status bar
- `Ctrl+\` opens the command palette (override with `JACKIN_PALETTE_KEY`)
- Mouse clicks, arrow keys, and paste reach the agent unmodified
- `Ctrl+\ → Split pane` offers every admitted instance and stamps each pane with its account identity
- Tabs keep working after split, resize, exit, reattach, and restore

```sh
export JACKIN_PREFIX=C-b
```

Set before launching to opt into the tmux-style prefix surface (`Ctrl+B "` / `Ctrl+B %` splits, `Ctrl+B d` detach).

## Migration notes

Config migrates to v1alpha11 and workspace defaults to v1alpha10; run the schema migration smoke above against a copy of the real config.
