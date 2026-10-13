# Velnor e135 validation

Source: `e1357dd620fe4bc18c9fa6e0d7b0c636a0f9175d`.
Observed 2026-09-20. Raw run/job API responses and collector outputs share the
run-ID filename prefix in this directory. Each jobs response contains all 68
jobs for the PR runs; the policy response contains its single job.

- [PR run 35487077663](https://github.com/tailrocks/velnor/actions/runs/35487077663)
  succeeded. Trigger to both required gates: 644 seconds; aggregate executed
  job time: 2,967 seconds. These are different measures. The generator job
  remains largest at 569 seconds, followed by runner at 520 seconds.
- Generator checks occupied 530 seconds: initial dev build 59.98 seconds,
  nextest compilation 253 seconds, then Clippy 185 seconds. Test execution and
  command overhead remain included in the check step. Candidate preparation
  and publication added eight seconds; they are product work, despite the
  current telemetry classifying them as cleanup.
- Compiler evidence reports three hits, zero misses, 1,813 operations not
  looked up and 132 bypasses after nextest; Clippy reports three hits, three
  misses, 1,836 not looked up and 132 bypasses. Both include the generator's
  `build-script-always-rerun` bypass. Cargo fallback restoration and zero
  reported crate downloads do not prove warm compiler reuse.
- [Policy run 35487076653](https://github.com/tailrocks/velnor/actions/runs/35487076653)
  failed after acquiring the candidate: enforcement treated the candidate
  binary as the declared pin. The binary reported closure `70fe7766...`,
  which differs from that pin. Preserve this separately from earlier candidate
  timeouts. The proposed distinct pin/candidate slots address this mechanism,
  but remain held on the independently reproduced dirty-source identity defect.
- Preceding [run 35486689573](https://github.com/tailrocks/velnor/actions/runs/35486689573)
  concluded cancelled although its executed jobs and final gates succeeded.
  Retain it as a cancelled attempt, not a successful performance sample.

No matched repeated comparison or accepted speedup follows from these runs.
The pushed revision has successful PR coverage and failed separate policy;
it is not all-green campaign completion.
