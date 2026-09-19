# G3 Rust scanner review

Status: **REJECT** exact candidate `4320e628518a62b61b4c97a2e0557b513d6dbe59` (base `abe9ad82`).

## What passed

- Exact tree was clean at the reviewed commit.
- `rtk cargo test -p velnor-workflow rust_include -- --nocapture`: 3 passed.
- `rtk cargo test -p velnor-workflow include_str -- --nocapture --test-threads=1`: 16 passed in both schema scanner copies and compatibility tests.
- `rtk cargo clippy -p velnor-workflow --lib --tests -- -D warnings`: no issues.
- Live `target/debug/velnor-workflow .../dual-lane-evidence/G0/rust-consumers/research/termrock --plain --dry-run` produced 9 units: 8 Rust + 1 Bun. It removed the old no-workflows marker failure. Generated temporary output showed all six `termrock-raster` font files in `rust-termrock-raster.watch`.
- The parser is read-only/static: no `Command`, build-script, Cargo, rustc, environment lookup, or code execution exists in `rust_include.rs`. Existing traversal and external-final-symlink tests pass.

## Blocking finding

`parse_include_paths` only recognizes the contiguous token `include_str!`/`include_bytes!` and then silently skips anything whose next delimiter is not `(` (`crates/velnor-workflow/src/rust_include.rs:60-71`). Rust accepts trivia between the macro name and `!`, and accepts all three macro delimiters.

Independent compiler probe (plain `rustc`, no project/build script execution) compiled both forms successfully:

```rust
const _: &str = include_str /* c */ ! /* d */ { "/etc/hosts" };
const _: &[u8] = include_bytes /* c */ [ "/etc/hosts" ];
```

The candidate parser returns `Ok([])` for those same forms. Therefore a valid static include is silently omitted from watch inputs; a missing target in that syntax is also silently accepted instead of surfacing the Rust compile/config error. A dynamic expression using that syntax is not rejected. This violates the fail-closed/no-hidden-input requirement even though termrock’s current source uses contiguous parentheses.

The scanner must either parse Rust macro trivia/delimiters and resolve them, or detect the macro token and return an explicit unsupported/malformed error. It must never silently advance past a recognized include macro.

## Additional semantic gap to fix or explicitly reject

For `concat!(env!("CARGO_MANIFEST_DIR"), "src/lib.rs")`, Rust concatenates the literal directly to the manifest directory (`.../appsrc/lib.rs`). The scanner instead strips leading slashes and resolves `src/lib.rs` relative to `package_root` (`crates/velnor-workflow/src/scan/rust.rs:756-760`), effectively treating it as `.../app/src/lib.rs`. That can fabricate a successful target/watch edge for a source that Rust cannot compile. Require the suffix/path joining semantics to match the actual environment value, or reject manifest-dir concatenations without an explicit separator.

The `.github` static helper also uses `symlink_metadata(root.join(target))` without canonicalizing parent components (`scan/rust.rs:822-841`); a `.github/fixtures` symlink directory can therefore make an external regular file look like an accepted static helper input. Add an external-parent-symlink fixture or route helper inputs through the same canonical-root check.

## Required follow-up evidence

- Add parser fixtures for trivia between macro name/`!`, `()`, `[]`, `{}` delimiters, and malformed/dynamic versions; assert rejection or correct watch paths.
- Add a manifest-dir no-separator fixture and explicit external-parent-symlink fixture.
- Re-run exact schema1/schema2 tests and the termrock 8-Rust + 1-Bun scan after the fix. Preserve the existing generated-workflow/template boundary and no-build-execution checks.
