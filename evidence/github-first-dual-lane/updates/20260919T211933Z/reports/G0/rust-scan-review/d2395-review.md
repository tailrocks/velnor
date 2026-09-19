# G3 Rust scanner review — exact `d2395fc70a32864796a6cec271966d6cb002bcb5`

Status: **REJECT** exact candidate `d2395fc70a32864796a6cec271966d6cb002bcb5` (base `abe9ad82`). This is an independent scanner review only; it is not a G3 fleet claim.

## Exact-tree and verification evidence

- Detached tree: `/private/tmp/g3-rust-review-d2395`; `HEAD` was exact `d2395fc7`; `git status --short --branch` was clean.
- `rtk cargo test -p velnor-workflow --lib`: **1750 passed**, exit 0 (25.21s).
- `rtk cargo clippy -p velnor-workflow --lib --tests -- -D warnings`: **no issues**.
- Focused exact-tree runs: `include_str` **20 passed**; `include_str_through` **6 passed**; `manifest_dir` **8 passed**.
- The first package-wide `rtk cargo test -p velnor-workflow` executed all **1750** tests but returned exit 101 because the shared `mbx` cache did not materialize the bin test executable (`No such file or directory`, never executed). It is not counted as a clean package pass; the lib suite above is the clean result.

## Blocking parser false negative

`crates/velnor-workflow/src/rust_include.rs:154` rejects every identifier immediately preceded by one colon:

```rust
if index > 0 && is_colon(&tokens[index - 1]) {
    return Ok(None);
}
```

That is intended to avoid a qualified user macro (`foo::include_str!`) but also suppresses a valid Rust macro in a struct/enum field value. A valid compiler comparator (compiled with `rustc 1.98.1`) was:

```rust
const _: &str = include_str /* c */ ! /* d */ { "data.txt" };
const _: &[u8] = include_bytes /* c */ ! /* d */ [ "bytes.bin" ];

struct Holder {
    value: &'static str,
}

const _: Holder = Holder {
    value: include_str!("data.txt"),
};
```

The same source passed through the exact shared parser returned only `data.txt` and `bytes.bin`; it omitted the field include. A CLI fixture put the field target outside the generic `src/**` watch. The generated watch contained `assets/bytes.bin`, `assets/direct.txt`, and `assets/nested.txt`, but **not** `assets/field.txt`.

This also suppresses real configuration errors. In that same field position, `value: include_str!(PATH)` (a dynamic argument) made the scanner succeed and omit the include; the equivalent top-level `include_str!(PATH)` correctly returned `include_str! must use a static string expression`. Thus a valid include is hidden and an invalid/dynamic include can be hidden solely by syntactic context. The fix must distinguish `::` qualification from a single field colon (or use equivalent Rust AST context), and must never silently skip a recognized include invocation.

## Adversarial/security checks

- Rust comments/trivia, all delimiters, raw/cooked literals, `include_bytes!`, nested `concat!`, and manifest-dir concatenation tests pass. The corrected comparator includes the required `include_bytes!` bang.
- Existing exact tests cover manifest-dir exact-boundary joining, traversal rejection, dynamic-expression rejection, tracked symlink directories, external final symlinks, and external `.github` parent symlinks; the focused runs above pass for both schema scanner copies through the shared parser/resolver.
- `resolve_include_path` canonicalizes the repository root, package root, complete target, and existing parent chain before accepting a path; no new canonical-root escape was found.
- Scanner code in `rust_include.rs` and both schema scanner modules only token-parses source and reads metadata/files. No project `Command`, Cargo, rustc, or build-script execution occurs in this path. A scratch fixture with a `build.rs` sentinel was scanned with `--dry-run`; the sentinel file was not created. The CLI reported the static-analysis boundary.

## Termrock consumer proof

The exact built binary dry-run against the external Termrock research clone completed successfully with **9 units: 8 Rust + 1 Bun**. Generated external output contained exactly these six `rust-termrock-raster` font watches:

```text
crates/termrock-raster/assets/fonts/JetBrainsMono-Bold.ttf
crates/termrock-raster/assets/fonts/JetBrainsMono-Italic.ttf
crates/termrock-raster/assets/fonts/JetBrainsMono-Regular.ttf
crates/termrock-raster/assets/fonts/NotoEmoji-Regular.ttf
crates/termrock-raster/assets/fonts/NotoSansMath-Regular.ttf
crates/termrock-raster/assets/fonts/NotoSansSymbols2-Regular.ttf
```

Therefore the original Termrock hidden-unit/font-watch regression is fixed, but the exact candidate remains rejected for the independent valid-field include false negative and silent dynamic-error suppression.

No source tree or remote ref was modified. Historical `final.md` for the rejected `4320e628` review was preserved; this exact-candidate report is the adjacent `d2395-review.md`.
