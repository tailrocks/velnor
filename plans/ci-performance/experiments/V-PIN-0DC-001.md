# V-PIN-0DC-001: restore the source-owned base runtime pin

Status: accepted as an isolated propagation proof; integration and hosted CI review remain parent-owned.

The committed candidate at `217996550d737fc2d26b99f08be7e938c7b4af43` still declared the published base runtime pin `325719f1e05d3d46322c9fd3eeb9ad545e175638`. The base pull-request policy workflow executes the source-owned pin from the target branch, so the source declaration and every generated runtime reference must move together. This experiment changes only `[generator].revision` to the published base runtime `0dc79895ff1c5e88be7c3822c437e1c5b5282e12` in a clean checkout of the committed candidate. It does not add a candidate-slot shim or regenerate with the old runtime source.

## Controlled procedure

- Checkout: `/tmp/velnor-pin0dc-21799655` at `217996550d737fc2d26b99f08be7e938c7b4af43`; no dirty source.
- Candidate binary: `/tmp/velnor-pin0dc-21799655-target/debug/velnor-workflow`.
- Candidate identity: revision `217996550d737fc2d26b99f08be7e938c7b4af43`; closure `a67d55386f0034c7ac4c86ac12db2aaba47f8d670704700c1898a831f8fc0aa0`.
- Source change: `.github-gen/velnor-workflow.toml` pin only.
- Render: `velnor-workflow . --force --plain`.
- Check: `VELNOR_WORKFLOW_PINNED_BINARY=/tmp/velnor-runtime-0dc-macOS-ARM64 velnor-workflow . --check --plain`; exit `0`.
- Repeated render: a second `--force` render produced byte-identical `git diff --binary` output.

The generated patch is retained at `/tmp/velnor-pin0dc-21799655-generated.patch`; SHA-256 is `a59ccad09705e17ccd971af9518be8f39e1963bf6d0c7056352c495ac9685146`. It changes 13 generated files, 94 insertions and 94 deletions. Every generated textual change is the old 325 pin replaced by 0dc; the ownership state updates its derived input/output digests. No generated file was modified in the shared checkout.

## Correctness observations

- Generated workflows contain the 0dc pin and contain no old 325 pin.
- Current source-owned MBX remains `1.12.0`: four generated files contain `1.12.0`, and none contain `1.11.1`.
- The generated policy workflow retains the candidate product path and current candidate execution contract. The base pin is only the trusted base runtime identity.
- The old 0dc runtime accepted the current candidate-rendered tree in the controlled local check. Any live GitHub ruleset lookup remains an external policy input and was not substituted with local success.

This proves source-owned pin propagation and local deterministic rendering. It does not prove that the old base runtime implements every current policy feature or that hosted artifact publication succeeds; those require the parent’s independent review and real CI run.
