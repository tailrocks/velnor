//! Reader for the runner's `trace.jsonl` span-close records.
//!
//! The runner emits one `tracing` span per checkout phase (`checkout.rs`,
//! `git_mirror.rs`) and appends span-close records with busy/idle timings to
//! `<config-base>/logs/trace.jsonl` (`telemetry.rs`). This module is the
//! benchmark side of that contract: it parses those close records back into
//! the [`CheckoutPhase`](crate::stage::CheckoutPhase) timings a `velnor-job`
//! observation carries. No new runner sink was needed — spans alone.
//!
//! The span-name table below is pinned on the runner side by
//! `checkout::tests::checkout_emits_the_four_bench_phase_spans`, which runs a
//! real checkout and asserts every name and `phase` field. This side asserts
//! the table covers [`CheckoutPhase::ALL`](crate::stage::CheckoutPhase) exactly,
//! and the round-trip test generates records with the real subscriber layer,
//! so the field paths below are proven, not guessed.

use std::collections::BTreeMap;

use crate::stage::CheckoutPhase;

/// Span names the parser accepts, each with the phase it must carry.
///
/// The name and the `phase` field must agree: a close record whose name maps
/// here but whose phase differs is ignored rather than misattributed.
const SPAN_TABLE: &[(&str, CheckoutPhase)] = &[
    ("checkout.mirror.lock_wait", CheckoutPhase::MirrorLockWait),
    ("checkout.mirror.fetch", CheckoutPhase::MirrorFetch),
    ("checkout.workspace.fetch", CheckoutPhase::WorkspaceFetch),
    (
        "checkout.workspace.checkout",
        CheckoutPhase::WorkspaceCheckout,
    ),
];

/// Look the phase up by span name.
#[must_use]
pub fn phase_for_span(name: &str) -> Option<CheckoutPhase> {
    SPAN_TABLE
        .iter()
        .find(|(span, _)| *span == name)
        .map(|(_, phase)| *phase)
}

/// Parse one `trace.jsonl` document into per-phase busy milliseconds.
///
/// Only span-close records (`fields.message == "close"`) whose span name is in
/// [`phase_for_span`] and whose `phase` field agrees are accepted. A phase
/// that closes more than once in the document (the mirror lock wait fires once
/// per lock mode) sums its busy time. Lines that are not JSON, or JSON that is
/// not a matching close record, are ignored: the reader must survive rotation
/// artifacts and foreign spans, while never misattributing a timing.
#[must_use]
pub fn checkout_phases_from_trace(text: &str) -> BTreeMap<CheckoutPhase, u64> {
    let mut phases: BTreeMap<CheckoutPhase, u64> = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let Some((phase, busy_ms)) = close_record_phase(&record) else {
            continue;
        };
        *phases.entry(phase).or_default() = phases
            .get(&phase)
            .copied()
            .unwrap_or(0)
            .saturating_add(busy_ms);
    }
    phases
}

fn close_record_phase(record: &serde_json::Value) -> Option<(CheckoutPhase, u64)> {
    if record["fields"]["message"] != "close" {
        return None;
    }
    let name = record["span"]["name"].as_str()?;
    let phase = phase_for_span(name)?;
    if record["span"]["phase"] != phase.as_str() {
        return None;
    }
    let busy = record["fields"]["time.busy"].as_str()?;
    let busy_ms = parse_busy_duration(busy)?;
    Some((phase, busy_ms))
}

/// Parse the busy-time rendering `tracing-subscriber` writes on span close
/// (`"1.23ms"`, `"456µs"`, `"789ns"`, `"2s"`), truncating to milliseconds.
fn parse_busy_duration(rendered: &str) -> Option<u64> {
    let text = rendered.trim();
    let (number, unit) = text
        .char_indices()
        .find(|(_, character)| character.is_alphabetic() || *character == 'µ')
        .map(|(index, _)| text.split_at(index))?;
    let value: f64 = number.trim().parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let millis = match unit.trim() {
        "ns" => value / 1_000_000.0,
        "us" | "µs" => value / 1_000.0,
        "ms" => value,
        "s" => value * 1_000.0,
        "m" => value * 60_000.0,
        _ => return None,
    };
    if !millis.is_finite() || millis < 0.0 {
        return None;
    }
    Some(millis.trunc().clamp(0.0, u64::MAX as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_span_table_covers_every_checkout_phase_exactly_once() {
        let mut covered: Vec<CheckoutPhase> = SPAN_TABLE.iter().map(|(_, phase)| *phase).collect();
        covered.sort_by_key(|phase| phase.as_str());
        let mut expected = CheckoutPhase::ALL.to_vec();
        expected.sort_by_key(|phase| phase.as_str());
        assert_eq!(covered, expected);
        let mut names: Vec<&str> = SPAN_TABLE.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SPAN_TABLE.len());
    }

    #[test]
    fn busy_durations_parse_in_every_rendered_unit() {
        assert_eq!(parse_busy_duration("1.9ms"), Some(1));
        assert_eq!(parse_busy_duration("456µs"), Some(0));
        assert_eq!(parse_busy_duration("1500us"), Some(1));
        assert_eq!(parse_busy_duration("789ns"), Some(0));
        assert_eq!(parse_busy_duration("2s"), Some(2000));
        assert_eq!(parse_busy_duration("1.5m"), Some(90_000));
        assert_eq!(parse_busy_duration("0ms"), Some(0));
        assert_eq!(parse_busy_duration(""), None);
        assert_eq!(parse_busy_duration("12"), None);
        assert_eq!(parse_busy_duration("3fortnights"), None);
        assert_eq!(parse_busy_duration("-1ms"), None);
        assert_eq!(parse_busy_duration("NaNms"), None);
    }

    fn close_line(name: &str, phase: &str, busy: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-09-13T00:00:00.000000Z",
            "level": "INFO",
            "fields": {
                "message": "close",
                "time.busy": busy,
                "time.idle": "1µs",
            },
            "target": "velnor_runner::checkout",
            "span": {"name": name, "phase": phase},
            "spans": [],
        })
        .to_string()
    }

    #[test]
    fn matching_close_records_sum_per_phase() {
        let text = [
            close_line("checkout.mirror.lock_wait", "mirror-lock-wait", "2ms"),
            close_line("checkout.mirror.lock_wait", "mirror-lock-wait", "3.7ms"),
            close_line("checkout.workspace.fetch", "workspace-fetch", "1500us"),
            String::new(),
            "not json at all".to_owned(),
        ]
        .join("\n");
        let phases = checkout_phases_from_trace(&text);
        assert_eq!(phases.get(&CheckoutPhase::MirrorLockWait), Some(&5));
        assert_eq!(phases.get(&CheckoutPhase::WorkspaceFetch), Some(&1));
        assert_eq!(phases.len(), 2);
    }

    #[test]
    fn foreign_and_mismatched_records_are_ignored_not_misattributed() {
        let text = [
            // Not a close record.
            serde_json::json!({
                "fields": {"message": "new"},
                "span": {"name": "checkout.mirror.fetch", "phase": "mirror-fetch"},
            })
            .to_string(),
            // Unknown span.
            close_line("checkout.something_else", "mirror-fetch", "9ms"),
            // Name/phase disagreement.
            close_line("checkout.mirror.fetch", "workspace-fetch", "9ms"),
            // Missing phase field.
            serde_json::json!({
                "fields": {"message": "close", "time.busy": "9ms"},
                "span": {"name": "checkout.mirror.fetch"},
            })
            .to_string(),
            // Unparseable busy time.
            close_line("checkout.mirror.fetch", "mirror-fetch", "soon"),
        ]
        .join("\n");
        assert!(checkout_phases_from_trace(&text).is_empty());
    }

    /// In-memory JSONL sink shaped exactly like the production file layer.
    #[derive(Clone)]
    struct BufferWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for BufferWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_parser_reads_records_the_real_subscriber_writes() {
        // The field paths above are proven here: these records are generated by
        // the real `fmt::json` close-event layer, not hand-written.
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .with_current_span(true)
            .with_writer({
                let buffer = buffer.clone();
                move || BufferWriter(buffer.clone())
            })
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("checkout.workspace.fetch", phase = "workspace-fetch",);
            let _guard = span.enter();
            std::thread::sleep(std::time::Duration::from_millis(12));
        });
        let bytes = buffer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let text = String::from_utf8(bytes).expect("trace output is UTF-8");
        let phases = checkout_phases_from_trace(&text);
        let busy = phases
            .get(&CheckoutPhase::WorkspaceFetch)
            .unwrap_or_else(|| panic!("parser missed the real close record; raw text:\n{text}"));
        assert!(
            *busy >= 12,
            "busy time {busy}ms is below the 12ms sleep; raw text:\n{text}"
        );
        assert!(text.contains("\"message\":\"close\""), "raw text:\n{text}");
    }
}
