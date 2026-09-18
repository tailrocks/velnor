# F1 Tools-Availability Probe — Bastion Campaign (§9.1 historic tools)

Date (UTC): 2026-09-17. Read-only, no installs. All checks live HTTP/API unless noted.

## Verdict table

| Tool (spec §9.1) | Probed | Result | Evidence | Tested-update recommendation |
|---|---|---|---|---|
| Rust 1.97.1 | static.rust-lang.org HEAD (correct `/dist/rust-<v>-<target>.tar.gz` layout) + `rustup toolchain list` | **GO** | `rust-1.97.1-x86_64-unknown-linux-gnu.tar.gz` → 200; aarch64 → 200. Also installed locally (`1.97.1-aarch64-apple-darwin`). Stable channel today = 1.98.1 (manifest date 2026-09-03) | None required. Spec version resolvable; repo already ahead (see below). Campaign may pin 1.97.1 exactly or follow repo 1.98.1. |
| Bun 1.3.14 | github.com release asset HEAD + releases API | **GO** | `.../download/bun-v1.3.14/bun-linux-x64.zip` → 302 to live signed release asset. Latest release = `bun-v1.4.2` | None required for campaign. Note repo drift: `mise.toml` declares Bun 1.4.0, two patches behind latest 1.4.2. |
| Node 24.18.0 | nodejs.org dist HEAD + index.json | **GO** | `node-v24.18.0-linux-x64.tar.xz` → 200; `v24.18.0` present in index.json (29 v24 entries). Latest v24 = v24.21.0; latest overall = v26.9.0 | None required. Spec version resolvable. If campaign wants current v24 line, test-update to v24.21.0. |
| ubuntu-26.04 runner | actions/runner-images README + GitHub docs runner page | **GO (PREVIEW)** | Label `ubuntu-26.04` listed in both sources. README marks Ubuntu 26.04 `preview`; GA/`ubuntu-latest` = 24.04 | Campaign CAN use `ubuntu-26.04`, but flag preview status in plan. Tested fallback: `ubuntu-24.04` (GA). |
| macos-26 runner | actions/runner-images README + GitHub docs runner page | **GO (GA)** | `macos-26` listed in both; README shows `macos-latest` → macOS 26 arm64 | None. Use `macos-26` (arm64) directly. |

## Current repo declarations (jackin)

| File | Declaration | vs today |
|---|---|---|
| `rust-toolchain.toml` | `channel = "1.98.1"` | **Current** — equals stable (1.98.1, 2026-09-03). Toolchain active locally. |
| `mise.toml` | `"aqua:oven-sh/bun" = "1.4.0"` | **Minor drift** — latest is 1.4.2. Recommend tested-update 1.4.0 → 1.4.2 separately from campaign. |
| `mise.toml` | Node | **Not declared** — no node tool entry; CI presumably uses image-provided Node. No action. |

## Bottom line

**All five historic tools GO.** No version gone or superseded-off-registry. Only caveats: `ubuntu-26.04` is still preview (GA is 24.04), and repo's own Bun pin (1.4.0) trails latest (1.4.2) by two patches — campaign spec version 1.3.14 itself is unaffected.
