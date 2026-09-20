# Telemetry schema4 follow-up review

Historical review HOLD remains unchanged. Follow-up of marker-bounds patch `/tmp/velnor-telemetry-marker-bounds.patch` (SHA256 `5aa4cc8f9b9054ddb75a676dc76ccfca823b8b5105061c15bb0141154565a7bc`) is PASS.

Independent probes show future, stale, and post-job candidate markers produce null candidate durations and `candidate_phase_order_invalid`; malformed/reversed/incomplete graphs censor invalid fields while retaining valid preparation intervals; malformed cache-save phases stay unknown; valid seed+bundle phases aggregate correctly. Focused Rust test `report_action_prefers_fractional_queue_time_and_bounds_markers` passed 2/2. This follow-up does not overwrite the earlier HOLD report and does not claim generated parity until parent regeneration completes.
