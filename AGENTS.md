# Rules

- `actions/runner` (https://github.com/actions/runner) is the protocol source of truth: before writing runner protocol code (job messages, broker, expressions, credentials, run-service, or timeline), find and match its equivalent logic exactly. Never guess.
- GitHub-hosted macOS jobs must use the highest macOS major currently offered by GitHub, including a public preview when that is the newest offered image, with the exact official label for the required architecture. As of 2026-09-20, macOS 27 arm64 is offered as `xcode-27` (public preview), while macOS 27 Intel is not offered; use `xcode-27` for arm64 and treat an Intel requirement as an explicit unsupported/blocker until a macOS 27 Intel label exists—never invent `macos-27`, use `macos-latest`, macOS 15, or silently fall back to macOS 26/another architecture. Recheck the official [runner image matrix](https://github.com/actions/runner-images#available-images) when changing workflows.
- Prefer the latest stable versions when selecting or updating tools and dependencies. Keep action and image references immutable; advance SHA/digest pins only to verified newer revisions, never replace them with floating references.
- No legacy code. Finish every migration: remove old paths completely—no compatibility shims, aliases, or deprecation periods. Breaking changes are preferred.
- This is a research project. It is unsafe and expected to contain breaking changes; never treat it as production-ready. Break things when needed and deliver new implementations fast.
- Always apply these principles:
  - Judge work by correctness, consistency, and project fit. Never defer a known-wrong state because of ROI, cost, effort, or claims that it is low-value, marginal, or an edge case.
  - Stop only when the required change is proven impossible with the available tools or model. When uncertain, inspect, test, and measure first.
  - Before fixing a bug, identify why the architecture allowed the bug class and whether the same structure permits related bugs.
  - Prefer fixes that remove the enabling condition. Use a symptom-layer patch only when the root fix is infeasible or belongs in a separate change, and name the deferred root cause.
