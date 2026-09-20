# Native-policy exact review — `598bf6d9d408c43da092e7ed84694cf1a267945a`

Review date: 2026-09-20 (Asia/Ho_Chi_Minh)

Disposition: **NATIVE POLICY DELTA PASS; FULL ROLLOUT CHANGES REQUIRED.**
The exact candidate fixes the prior native-policy findings. Runtime-product
integration was intentionally not part of this candidate and remains a P1
required follow-up; this report does not waive it. No source, generated tree,
host, Docker runtime, macOS build, publication, merge, or permission action was
performed.

## Exact scope and evidence boundary

- Review tree: `dual-lane-native-policy`
- Branch: `codex/github-first-native-policy`
- Reviewed revision: `598bf6d9d408c43da092e7ed84694cf1a267945a`
  (`fix(native): enforce latest Apple host contract`)
- Exact parent: `18d6a3cf3148b47204b216bca94e5a971e475d29`
- Exact delta: `crates/velnor-workflow/src/lib.rs`,
  `crates/velnor-workflow/src/native_contract.rs`, and
  `crates/velnor-workflow/src/platform.rs` only.
- The exact worktree was clean. `git diff --check` passed.
- Prior findings compared: `G0/native-review/review-native-policy-18d6a3cf.md`.

Official capability evidence retained from the prior pinned review:

- [GitHub runner image matrix](https://github.com/actions/runner-images#available-images)
  lists Xcode 27 arm64 labels `xcode-27` and `xcode-27-xlarge`, macOS 26
  Intel/arm64 labels, and no macOS 27 Intel label.
- [The Xcode 27 arm64 image README](https://raw.githubusercontent.com/actions/runner-images/main/images/macos/xcode-27-arm64-Readme.md)
  identifies macOS 27.0, Xcode 27, and macOS/iOS device/simulator 27 SDKs.

Independent read-only recheck of those primary sources at review time found
the matrix entries `xcode-27`/`xcode-27-xlarge` as arm64 and the README's
installed SDK table entries `macosx27.0`, `iphoneos27.0`, and
`iphonesimulator27.0`. The README also identifies the image as macOS 27.0
and Xcode 27.0. This is capability evidence only; no hosted job was run.

## Capability matrix

| Requirement | Exact source proof | Result |
|---|---|---|
| Exact newest Apple label | `native_contract.rs:486-489` defines `LATEST_HOSTED_APPLE_RUNNER = "xcode-27"`; `:521-531` accepts only equality with that value; legacy validation calls it at `platform.rs:283-328`; scanner defaults it at `scan/mod.rs:220-223`. | **PASS** |
| No alias, Intel fallback, or invented label | `native_contract.rs:815-831` rejects `macos-15`, `macos-26`, `macos-26-intel`, `macos-latest`, and `macos-27` before rendering. The production selector has no prefix/wildcard path. | **PASS** |
| macOS/iOS-device/iOS-simulator SDK 27 evidence | `native_contract.rs:480-484` adds all three `(family, 27.0.0)` rows; `:534-543` attaches them to the production `xcode-27` offer; `:797-812` independently asserts all three rows. | **PASS** |
| Intel negative versus arm execution | `native_contract.rs:141` documents that host execution and compiler output differ; `:541-542` records arm64 execution for `xcode-27`; `offer_mismatches` independently checks execution and build architectures at `:573-588`; hostile Intel behavior is exercised at `:834-862`. | **PASS** |
| x86 artifact is not Intel execution proof | The latest offer advertises only its evidenced arm64 build set (`:473`, `:534-543`). The preflight checks `uname -m` against `APPLE_EXECUTION_ARCH` and separately probes each requested compiler `-arch` at `:729-731` and `:772-776`; its comment explicitly says cross-build does not grant execution. An x86_64 artifact requirement therefore fails closed without claiming an Intel host. | **PASS / fail-closed** |
| No generic target identity in this delta | Changed production selection is exact-label equality. Generic aliases occur only in hostile rejection fixtures. No broad `macos-*`, `xcode-*`, `macos-latest`, or target-identity admission was added. Normal generation validates before rendering at `lib.rs:1389-1394`. | **PASS** |
| Runtime-product producer integration | This candidate does not touch either producer mapping or the generated publisher matrix. Legacy `primitives/runtime_products.rs:77-79`, S2 `s2/primitives/runtime_products.rs:71-73`, and committed `.github/workflows/ci-runtime-products.yml:104-107` still use `macos-15`. | **OPEN P1** |

## Prior finding resolution

1. The prior SDK-table omission is fixed: device and simulator 27 rows now
   live in the production offer and have an independent exact test.
2. The prior self-referential positive test is fixed: the new test asserts
   literal `xcode-27`, macOS/Xcode 27, arm64 execution, arm64 build output,
   and all three SDK families (`native_contract.rs:797-812`).
3. The prior direct renderer acceptance of `macos-26-intel` is fixed:
   `lib.rs:10227-10238` validates it and requires the exact latest-label
   error before a renderer output can be treated as valid.

## Verification performed

All commands ran in the clean exact candidate tree and were source-only:

```text
rtk cargo test -p velnor-workflow native_contract::tests --lib
  cargo test: 5 passed, 1744 filtered out

rtk cargo test -p velnor-workflow macos_runner_label_overrides_the_apple_lane --lib
  cargo test: 1 passed, 1748 filtered out

rtk cargo test -p velnor-workflow --test platform_prerequisites
  cargo test: 9 passed

rtk git diff --check 18d6a3cf 598bf6d9
  pass
```

No macOS build, Docker command, workflow dispatch, host mutation, or runtime
operation was attempted.

## Required bounded follow-up

Use the already reviewed PR957 source seam
`92387e88c32f933a9061b819256e535662655cb2` to migrate both legacy and S2
runtime-product producer mappings to the same central exact selector, then
regenerate `.github/workflows/ci-runtime-products.yml` and its ownership/
provenance state. Keep the xcode-27 arm64 admission, explicit Intel-27
unsupported result, and the execution/build-architecture split. Do not add a
second selector or use `macos-latest`, `macos-27`, old-major fallback, or
architecture substitution.

For completeness, add near-miss `xcode-*` labels to the hostile selector
fixture if the exact-label test is intended to document exhaustive rejection;
the current production equality check already rejects every non-`xcode-27`
value, so this is test coverage strengthening, not an observed admission
bug.

No full rollout or merge approval is implied by this native-policy pass.
