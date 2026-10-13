use anyhow::{bail, Context, Result};
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCommand {
    pub name: String,
    pub value: String,
}

pub fn parse_command_file(path: &Path) -> Result<Vec<FileCommand>> {
    let contents = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_command_file_contents(&contents)
}

pub fn parse_command_file_contents(contents: &str) -> Result<Vec<FileCommand>> {
    let lines = contents.lines().collect::<Vec<_>>();
    let mut commands = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        index += 1;

        if line.is_empty() {
            continue;
        }

        let equals_index = line.find('=');
        let heredoc_index = line.find("<<");

        if let (Some(equals_index), Some(heredoc_index)) = (equals_index, heredoc_index) {
            if equals_index < heredoc_index {
                let (name, value) = line.split_once('=').expect("line contains equals");
                validate_name(name)?;
                commands.push(FileCommand {
                    name: name.to_string(),
                    value: value.to_string(),
                });
                continue;
            }
        }

        if let Some((name, delimiter)) = line.split_once("<<") {
            validate_name(name)?;
            let mut value_lines = Vec::new();
            let mut found_end = false;

            while index < lines.len() {
                let value_line = lines[index];
                index += 1;
                if value_line == delimiter {
                    found_end = true;
                    break;
                }
                value_lines.push(value_line);
            }

            if !found_end {
                bail!("missing heredoc delimiter '{delimiter}' for command '{name}'");
            }

            commands.push(FileCommand {
                name: name.to_string(),
                value: value_lines.join("\n"),
            });
            continue;
        }

        if let Some((name, value)) = line.split_once('=') {
            validate_name(name)?;
            commands.push(FileCommand {
                name: name.to_string(),
                value: value.to_string(),
            });
            continue;
        }

        bail!("invalid command-file line: {line}");
    }

    Ok(commands)
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("command-file entry has empty name");
    }

    if name.contains(char::is_whitespace) {
        bail!("command-file entry name contains whitespace: {name}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_key_values() {
        let commands = parse_command_file_contents("one=two\nEMPTY=\n").unwrap();

        assert_eq!(
            commands,
            vec![
                FileCommand {
                    name: "one".into(),
                    value: "two".into(),
                },
                FileCommand {
                    name: "EMPTY".into(),
                    value: "".into(),
                }
            ]
        );
    }

    #[test]
    fn parses_multiline_heredoc() {
        let commands = parse_command_file_contents("payload<<EOF\none\ntwo\nEOF\n").unwrap();

        assert_eq!(
            commands,
            vec![FileCommand {
                name: "payload".into(),
                value: "one\ntwo".into(),
            }]
        );
    }

    #[test]
    fn treats_equals_before_heredoc_marker_as_key_value() {
        let commands = parse_command_file_contents("payload=value<<not-heredoc\n").unwrap();

        assert_eq!(
            commands,
            vec![FileCommand {
                name: "payload".into(),
                value: "value<<not-heredoc".into(),
            }]
        );
    }

    #[test]
    fn rejects_missing_heredoc_delimiter() {
        let err = parse_command_file_contents("payload<<EOF\none\n").unwrap_err();

        assert!(err.to_string().contains("missing heredoc delimiter"));
    }
}
