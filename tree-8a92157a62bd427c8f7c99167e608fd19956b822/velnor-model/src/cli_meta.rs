//! CLI metadata types consumed by the schema, help, completion, and man
//! leaf commands (C002-C005) without domain duplication.
//!
//! The data lives in `velnor-model` so every consumer serializes the exact
//! same shape; `velnorctl` owns building and rendering it.

use serde::{Deserialize, Serialize};

/// One command-line flag as exposed to metadata consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlagMetadata {
    /// Long spelling without dashes, for example `field-selector`.
    pub long: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<char>,
    /// Placeholder shown for the value (`<FORMAT>`), absent for switches.
    pub value_name: Option<String>,
    pub help: String,
    /// True when the flag may appear before or after the subcommand.
    pub global: bool,
}

impl FlagMetadata {
    /// `-o/--output`-style rendering used by help text.
    #[must_use]
    pub fn invocation(&self) -> String {
        let value = self
            .value_name
            .as_ref()
            .map_or_else(String::new, |name| format!(" {name}"));
        match self.short {
            Some(short) => format!("-{short}, --{}{value}", self.long),
            None => format!("--{}{value}", self.long),
        }
    }
}

/// Metadata describing one leaf command's CLI surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandMetadata {
    /// Exact subcommand spelling.
    pub name: String,
    pub about: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<FlagMetadata>,
}

/// Full CLI surface document served to the schema leaf command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaDocument {
    pub binary: String,
    pub version: String,
    /// Global flags accepted before or after any subcommand.
    pub global_flags: Vec<FlagMetadata>,
    pub commands: Vec<CommandMetadata>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_invocation_renders_short_and_long() {
        let flag = FlagMetadata {
            long: "output".to_owned(),
            short: Some('o'),
            value_name: Some("<FORMAT>".to_owned()),
            help: "output format".to_owned(),
            global: true,
        };
        assert_eq!(flag.invocation(), "-o, --output <FORMAT>");
        let long_only = FlagMetadata {
            long: "no-color".to_owned(),
            short: None,
            value_name: None,
            help: "disable color".to_owned(),
            global: true,
        };
        assert_eq!(long_only.invocation(), "--no-color");
    }

    #[test]
    fn schema_document_round_trips() {
        let doc = SchemaDocument {
            binary: "velnorctl".to_owned(),
            version: "0.1.0".to_owned(),
            global_flags: vec![FlagMetadata {
                long: "context".to_owned(),
                short: None,
                value_name: Some("<NAME>".to_owned()),
                help: "named context".to_owned(),
                global: true,
            }],
            commands: Vec::new(),
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"globalFlags\""), "{json}");
        assert_eq!(serde_json::from_str::<SchemaDocument>(&json).unwrap(), doc);
    }
}
