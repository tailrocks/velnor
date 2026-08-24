//! Output renderers for Velnor operator surfaces.
//!
//! Plan 065 owns the table/wide/json/yaml/jsonl/name renderer matrix; this
//! crate depends only on the shared model types and never on Clap or Axum.

/// Marker renderer seam consumed by `velnorctl` output formats.
pub const RENDER_FORMATS: [&str; 6] = ["table", "wide", "json", "yaml", "jsonl", "name"];

#[cfg(test)]
mod tests {
    #[test]
    fn planned_format_matrix_is_declared() {
        assert_eq!(super::RENDER_FORMATS.len(), 6);
    }
}
