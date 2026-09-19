# G3 Rust scanner review — exact `eca2460e9b8b3eddfc5883f0c1f0c8850cc36484`

Status: **APPROVE exact scanner candidate for this bounded G3 scanner scope** (base `d2395fc70a32864796a6cec271966d6cb002bcb5`). This is source approval only, not consumer rollout, hosted proof, or a G3 fleet claim.

## Exact-tree gates

- Fresh detached tree: `/private/tmp/g3-rust-review-eca`; `HEAD` was exact `eca2460e`; status and `git diff --check` were clean.
- `rtk cargo test -p velnor-workflow --lib`: **1752 passed**.
- `rtk cargo test -p velnor-workflow`: **1875 passed, 20 suites** (after an explicit `--no-run` materialization gate).
- `rtk cargo clippy -p velnor-workflow --all-targets --all-features --locked -- -D warnings`: **no issues**.
- Focused exact tests: `rust_include` **7**, `include_str` **20**, `include_str_through` **6**, and `manifest_dir` **8** passed.

## Corrected hostile cases

The correction replaces the single-colon suppression with `is_double_colon_before`, requiring proc-macro2 `Spacing::Joint` followed by `Spacing::Alone`. This distinguishes a qualified user macro from a struct/enum field value.

Independent `rustc 1.98.1` compiled this valid comparator:

```rust
struct Holder {
    text: &'static str,
    raw: &'static str,
    bytes: &'static [u8],
}

const _: Holder = Holder {
    text: include_str!("data.txt"),
    raw: include_str!(r#"data.txt"#),
    bytes: include_bytes!(concat!("assets/", r#"bytes.bin"#)),
};
```

The exact shared parser returned all three paths, including the field values. A field dynamic case, `text: include_str!(PATH)`, now returns the explicit `include_str! must use a static string expression` error; exact CLI scanning also exits nonzero with that diagnostic. Missing delimiters and malformed include arguments remain explicit errors.

The qualified-path comparator was an actual compiling user macro:

```rust
mod foo {
    macro_rules! include_str { ($path:literal) => { "user macro" }; }
    pub(crate) use include_str;
}
const _: &str = foo /* trivia */ :: /* more */ include_str!("user-macro.txt");
```

The exact parser returned `Ok([])`, so it does not fabricate a built-in include for `foo::include_str!`. Tight field syntax (`text:include_str!`) and whitespace around `::` were also exercised.

The corrected compiler comparator used the required bang for `include_bytes!`; all three delimiters, comments/trivia, raw/cooked escapes, nested `concat!`, and manifest-dir forms were compiled or covered by exact tests.

## Boundary and scan safety

- Exact resolver tests passed for manifest-dir concatenation boundaries, traversal, tracked symlink directories, external final symlinks, and external `.github` parent symlinks in both schema scanner copies through the shared resolver.
- A valid scratch `build.rs` wrote a sentinel if executed. Exact eca CLI `--dry-run` detected/watched the build script but did not create the sentinel. Scanner source uses token parsing, source reads, metadata, and canonicalization; no project Cargo/rustc/build-script/task execution occurs during analysis.
- Exact eca CLI scan of the Termrock research clone produced **9 units: 8 Rust + 1 Bun**. External generated output contained all six raster font watches: JetBrainsMono Bold/Italic/Regular and NotoEmoji/NotoSansMath/NotoSansSymbols2.

## Bounded conservative behavior

The parser intentionally supports the declared static subset. Valid Rust `env!("CARGO_MANIFEST_DIR", "diagnostic message")` is explicitly rejected because the evaluator requires exactly one argument; this is fail-closed (scan error), not a hidden include or fabricated compile result. A bare user macro that shadows `include_str!` is likewise conservatively interpreted as the built-in and will fail path resolution when its literal is not a file; qualified user macros are excluded. Neither pattern occurs in the Termrock consumer evidence or the requested acceptance surface. Supporting full macro name resolution would be a separate scope expansion.

No source tree, consumer checkout, or remote ref was modified. Historical `final.md` and the prior `d2395-review.md` rejection remain preserved.
