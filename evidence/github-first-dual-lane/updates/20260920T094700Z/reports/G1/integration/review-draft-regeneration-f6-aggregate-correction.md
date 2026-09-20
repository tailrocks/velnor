# Aggregate metadata correction review — f6 disposable render

**Review type:** bounded, read-only metadata review. No rerender, source mutation, generated adoption, publication, or G1 approval.

**Correction artifact:** `G1/integration/draft-regeneration-f6-aggregate-correction.json`  
**SHA-256:** `05d50ce9ffa162a47a15516bbf5025813f569dac9380a291b59a53932866b5ef`

**Verdict:** **PASS for the narrow aggregate correction.** The corrected digest reproduces from the now-explicit algorithm; per-file bytes remain unchanged; the superseded evidence remains preserved by hash.

## Exact checks

- Superseded `draft-regeneration-f6.json` remains present and hashes to its recorded `dc8f01c18a8ab793bd32fe03cbcb935f2c0c9f607fe46d3d5be3ba62851b57c3`.
- Correction records the same source commit `f6cb27c4606103d0c879bbc60a6a060911ce7b91`, source closure, generator pin, and contract `54`.
- Correction changes metadata only; it records no rerender.
- Stored old aggregate: `4151e4af7308a7d4788afa26155afea03868cad0374ad955e31c6d7fb4f94b4d`.
- Corrected aggregate: `2e164a3d07f64bc608c82e0c96ff4648c88034f0387ca2a8629da00474cfddbd`.

## Algorithm reproduction

The correction now specifies: lowercase file SHA-256, two ASCII spaces, relative UTF-8 path without `./`, one LF; C-locale bytewise sort of complete records; SHA-256 over their exact concatenation.

I ran the recorded reproduction command against draft A. It returned exactly:

```text
2e164a3d07f64bc608c82e0c96ff4648c88034f0387ca2a8629da00474cfddbd  -
```

The same command against draft B returned the same digest. Draft A/B recursive comparison was empty; each contains 21 files.

## Preservation and boundary

- Original per-file records remain unchanged; all 21 superseded draft-A stored file hashes independently match their output files.
- Four canonical drift paths remain unchanged and are not adopted.
- Correction records canonical worktree clean, generated files untouched, and pin untouched; exact f6 detached tree independently remained clean.
- No source, generated-file, authority, publication, or G1 claim is created by this metadata correction.

No further correction is required for this narrow aggregate artifact. This does not approve generated-output adoption or any G1 gate.
