# Ownership inventory / relevance diagnostic

Source: Velnor current source at 07bc3e23 (same generator code at738feae5).
Probe: EVENT_NAME=pull_request BASE_SHA=07bc3e231b8955880974bb8142f3425e3011f616 HEAD_SHA=738feae5 VELNOR_PROVIDERS=github-hosted VELNOR_EVENT_TRUSTED=true velnor-workflow plan.
Result: all17 units selected; raw output /tmp/velnor-evidence-only-selection.log.
Exactchangedfilelist /tmp/velnor-evidence-only-files.txt: evidence files and ownershipstate only.
Only generatedstate delta is inputs.scan. Runtime s2/runtime.rs any .github path invokes full_selection. Existing state test check_fails_when_scan_inputs_change_but_output_does_not requires inventory changes even when generated outputs unchanged.

Architectural hypothesis: inventory identity and execution-graph identity share one invalidation path. A policy bookkeeping update can therefore select every product although no product input changed.

Alternatives requiring independent challenge:
1. Fingerprint semantic discovery/configuration facts used by rendering instead of arbitrary inventory membership; retain safety checks for new/deleted/renamed manifests and source families.
2. Keep inventory proof but distinguish control-only ownership changes from executable workflow/graph changes in runtime relevance. Validate actual state shape/content rather than blind path exclusion.
3. Derive and compare typed per-product graph/input identities between revisions; retain full conservative fallback when comparison is incomplete. This may subsume the broader artifact/relevance contract but must not become an opaque special case.

Next diagnostic: paired same-source scratch fixture with evidence changes and with/without ownershipstate, followed by independently reviewed correctness scenarios including newmanifest, newcrate, deletion/rename, generatedworkflow edits, changedtoolchain and unknown/incomplete diff. No implementation yet. No completed iteration credit yet.

Current PR cumulative diff includes real generator source changes: do not label those actual PR runs false positives from this local bounded-range probe.
