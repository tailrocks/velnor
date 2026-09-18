# Lane: zshrc shell-env importer — completion report

## Status: DONE

New file only: `crates/jackin-config/src/accounts/zshrc.rs` (~1000 lines incl. tests).
No existing file edited. No `mod` declaration added (orchestrator wires it, e.g.
`pub(crate) mod zshrc;` in `crates/jackin-config/src/accounts.rs` plus re-exports).

## API (std-only, zero deps → standalone-compilable)

- `pub fn is_account_relevant(name: &str) -> bool` — exact: `CLAUDE_CONFIG_DIR`,
  `CODEX_HOME`, 5 XDG roots, `CLAUDE_CODE_OAUTH_TOKEN`; contains `MODEL`; suffixes
  `_API_KEY`, `_API_TOKEN`, `_AUTH_TOKEN`, `_OAUTH_TOKEN`, `_BASE_URL`, `_API_BASE`,
  `_API_URL`, `_PROFILE`, `_PROFILE_NAME`.
- `pub fn parse_zshrc_source(source: &str) -> ZshrcImport` — total parser.
- `pub struct ZshrcImport { values: BTreeMap<String, String>, unresolved: Vec<UnresolvedEntry> }`
- `pub struct UnresolvedEntry { line: usize, name: String, kind: UnresolvedKind, detail: String }`
  (`detail` truncated to 64 chars + ellipsis)
- `pub enum UnresolvedKind { CommandSubstitution, FunctionCall, OpRead, UnresolvableExpansion }`

## Behavior

- Handles `export`/`typeset`/`declare`/`local`/`readonly` prefixes + `-x`-style flags,
  multi-assign lines, bare `export FOO` (skipped), `;` separators, `\`-newline
  continuations, `#` comments (quote- and word-start-aware), single/double/unquoted
  values with shell escape rules (`$` stays literal in single quotes).
- Any dynamic construct voids that assignment's literal and emits one typed entry per
  construct occurrence: `$(...)`, backticks, `<(...)`/`>(...)`/`=(...)`,
  `$VAR`/`${VAR}`/`$@`-style, `$((...))`, leading `~`, unquoted globs, `+=`, arrays.
- `FunctionCall` vs `CommandSubstitution`: a pre-pass collects `name() …` /
  `function name …` definitions; `$(name …)` is `FunctionCall` iff `name` is defined
  in-source, else `CommandSubstitution`. `op` (incl. `sudo`/`env`/`command` prefix)
  → `OpRead`.
- Only account-relevant names are reported; later literals overwrite earlier ones;
  unresolved entries accumulate in source order. Never spawns shells/helpers/reads env.

## Error style

Parser is total by design, so no `ConfigResult` surface was needed; file loading stays
with the caller (`ConfigError::Io` on failure). All public items documented
(`missing_docs`-clean); no slicing/indexing/`unwrap` in non-test code
(crate `clippy::indexing_slicing/string_slice/get_unwrap`-clean by construction).

## Verification (standalone; unwired module invisible to cargo test)

- `rustfmt --edition 2021 --check` → clean
- `rustc --edition 2021 --test …/zshrc.rs && /tmp/zshrc_test` → **17 passed, 0 failed**
- `rustc --edition 2021 --crate-type lib …` → clean, zero warnings
- `git status` confirms the only new file of mine is `zshrc.rs`; all other
  modifications are other lanes' in-flight work, untouched.

## Wiring suggestion for orchestrator

```rust
// accounts.rs
pub(crate) mod zshrc;
// lib.rs (optional)
pub use accounts::zshrc::{UnresolvedEntry, UnresolvedKind, ZshrcImport, is_account_relevant, parse_zshrc_source};
```

## Known limitations

- `#` inside `$(…)` is treated as a comment start (matches shell for word-start `#`,
  may cut exotic one-liners).
- `eval`/`source` lines are not followed (no var attribution possible statically).
