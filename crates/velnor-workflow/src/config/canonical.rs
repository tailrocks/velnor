//! Canonical serialization of the repository generation config.
//!
//! Digesting needs one byte sequence per configuration value: object keys are
//! sorted, array and declaration order is preserved, and every number is an
//! integer (floats and datetimes have no stable canonical form). The output is
//! valid JSON so it stays inspectable in a failing `--check` report.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use crate::GeneratorError;

/// Canonical serialization of the empty generation input: a repository with no
/// `.github-gen/velnor-workflow.toml` digests this form, so introducing a
/// config file later is a detected input change instead of a silent one.
pub(crate) const EMPTY_CANONICAL_FORM: &str = "{}";

/// Render `value` in canonical form.
///
/// # Errors
/// Returns an error for values without a stable canonical form.
pub(crate) fn canonical_value(value: &Value) -> Result<String, GeneratorError> {
    let mut output = String::new();
    write_value(value, &mut output)?;
    Ok(output)
}

fn write_value(value: &Value, output: &mut String) -> Result<(), GeneratorError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(inner) => output.push_str(if *inner { "true" } else { "false" }),
        Value::Number(number) => {
            if number.is_f64() {
                return Err(GeneratorError::usage(
                    "generation config contains a float; floats have no canonical form, use an integer or string",
                ));
            }
            let _ = write!(output, "{number}");
        }
        Value::String(inner) => {
            let encoded = serde_json::to_string(inner)
                .map_err(|error| GeneratorError::usage(format!("encode config string: {error}")))?;
            output.push_str(&encoded);
        }
        Value::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_value(item, output)?;
            }
            output.push(']');
        }
        Value::Object(entries) => {
            // Sort keys at the last moment: declaration order inside the TOML
            // table is meaningless, array order is not.
            let sorted = entries.iter().collect::<BTreeMap<_, _>>();
            output.push('{');
            for (index, (key, item)) in sorted.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                let encoded = serde_json::to_string(key).map_err(|error| {
                    GeneratorError::usage(format!("encode config key: {error}"))
                })?;
                output.push_str(&encoded);
                output.push(':');
                write_value(item, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}
