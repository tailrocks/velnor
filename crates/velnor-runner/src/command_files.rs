#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCommand {
    pub name: String,
    pub value: String,
}

pub fn parse_command_file(path: &Path) -> Result<Vec<FileCommand>> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let contents = decode_file_contents(&bytes);
    parse_command_file_contents(&contents)
}

/// Match `File.ReadAllText`'s BOM detection for UTF-8, UTF-16 and UTF-32
/// command files. Replacement characters mirror .NET's default decoder
/// fallback for malformed sequences.
pub(crate) fn decode_file_contents(bytes: &[u8]) -> String {
    const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];
    const UTF32_BE_BOM: &[u8] = &[0x00, 0x00, 0xfe, 0xff];
    const UTF32_LE_BOM: &[u8] = &[0xff, 0xfe, 0x00, 0x00];
    const UTF16_BE_BOM: &[u8] = &[0xfe, 0xff];
    const UTF16_LE_BOM: &[u8] = &[0xff, 0xfe];

    if let Some(contents) = bytes.strip_prefix(UTF32_BE_BOM) {
        return decode_utf32(contents, false);
    }
    if let Some(contents) = bytes.strip_prefix(UTF32_LE_BOM) {
        return decode_utf32(contents, true);
    }
    if let Some(contents) = bytes.strip_prefix(UTF16_BE_BOM) {
        return decode_utf16(contents, false);
    }
    if let Some(contents) = bytes.strip_prefix(UTF16_LE_BOM) {
        return decode_utf16(contents, true);
    }
    String::from_utf8_lossy(bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes)).into_owned()
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let (chunks, remainder) = bytes.as_chunks::<2>();
    let mut units = chunks
        .iter()
        .map(|pair| {
            let pair = [pair[0], pair[1]];
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        })
        .collect::<Vec<_>>();
    if !remainder.is_empty() {
        units.push(0xfffd);
    }
    String::from_utf16_lossy(&units)
}

fn decode_utf32(bytes: &[u8], little_endian: bool) -> String {
    let mut decoded = String::new();
    let (chunks, remainder) = bytes.as_chunks::<4>();
    for chunk in chunks {
        let chunk = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let scalar = if little_endian {
            u32::from_le_bytes(chunk)
        } else {
            u32::from_be_bytes(chunk)
        };
        decoded.push(char::from_u32(scalar).unwrap_or('\u{fffd}'));
    }
    if !remainder.is_empty() {
        decoded.push('\u{fffd}');
    }
    decoded
}

pub fn parse_command_file_contents(contents: &str) -> Result<Vec<FileCommand>> {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    parse_command_file_contents_for_os::<{ cfg!(windows) }>(contents)
}

fn parse_command_file_contents_for_os<const WINDOWS: bool>(
    contents: &str,
) -> Result<Vec<FileCommand>> {
    let mut commands = Vec::new();
    let mut index = 0;

    while let Some(line) = read_line::<WINDOWS>(contents, &mut index) {
        let line = line.contents;
        if line.is_empty() {
            continue;
        }

        let equals_index = line.find('=');
        let heredoc_index = line.find("<<");

        if let (Some(equals_index), Some(heredoc_index)) = (equals_index, heredoc_index)
            && equals_index < heredoc_index
        {
            let name = &line[..equals_index];
            let value = &line[equals_index + 1..];
            commands.push(FileCommand {
                name: name.to_string(),
                value: value.to_string(),
            });
            continue;
        }

        if let Some((name, delimiter)) = line.split_once("<<") {
            if name.is_empty() || delimiter.is_empty() {
                bail!(
                    "Invalid format '{line}'. Name must not be empty and delimiter must not be empty"
                );
            }

            let value_start = index;
            let mut value_end = index;
            loop {
                let Some(value_line) = read_line::<WINDOWS>(contents, &mut index) else {
                    bail!("Invalid value. Matching delimiter not found '{delimiter}'");
                };
                if value_line.contents == delimiter {
                    break;
                }
                if value_line.newline.is_empty() {
                    bail!("Invalid value. EOF marker missing new line.");
                }
                value_end = index - value_line.newline.len();
            }

            commands.push(FileCommand {
                name: name.to_string(),
                value: contents[value_start..value_end].to_string(),
            });
            continue;
        }

        if let Some((name, value)) = line.split_once('=') {
            commands.push(FileCommand {
                name: name.to_string(),
                value: value.to_string(),
            });
            continue;
        }

        bail!("Invalid format '{line}'");
    }

    Ok(commands)
}

struct CommandFileLine<'a> {
    contents: &'a str,
    newline: &'a str,
}

fn read_line<'a, const WINDOWS: bool>(
    contents: &'a str,
    index: &mut usize,
) -> Option<CommandFileLine<'a>> {
    if *index >= contents.len() {
        return None;
    }

    let start = *index;
    let lf_index = contents[start..].find('\n').map(|offset| start + offset);
    let Some(lf_index) = lf_index else {
        *index = contents.len();
        return Some(CommandFileLine {
            contents: &contents[start..],
            newline: "",
        });
    };

    // Runner splits at LF everywhere, but recognizes CRLF only in Windows builds.
    let crlf = WINDOWS && lf_index > start && contents.as_bytes()[lf_index - 1] == b'\r';
    let line_end = if crlf { lf_index - 1 } else { lf_index };
    let line_end_with_newline = lf_index + 1;
    *index = line_end_with_newline;

    Some(CommandFileLine {
        contents: &contents[start..line_end],
        newline: &contents[line_end..line_end_with_newline],
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
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
    fn parses_legacy_simple_record_names_and_first_equals() {
        let commands =
            parse_command_file_contents("OUT KEY=left=right<<literal\n=value\n").unwrap();

        assert_eq!(
            commands,
            vec![
                FileCommand {
                    name: "OUT KEY".into(),
                    value: "left=right<<literal".into(),
                },
                FileCommand {
                    name: "".into(),
                    value: "value".into(),
                }
            ]
        );
    }

    #[test]
    fn preserves_duplicate_records_and_skips_empty_lines() {
        let commands = parse_command_file_contents("A=first\n\nA=second\n\n").unwrap();

        assert_eq!(
            commands,
            vec![
                FileCommand {
                    name: "A".into(),
                    value: "first".into(),
                },
                FileCommand {
                    name: "A".into(),
                    value: "second".into(),
                }
            ]
        );
    }

    #[test]
    fn reads_command_files_with_runner_text_boms() {
        let text = "OUTPUT=value\n";
        let expected = vec![FileCommand {
            name: "OUTPUT".into(),
            value: "value".into(),
        }];
        let encodings = [
            {
                let mut bytes = vec![0xef, 0xbb, 0xbf];
                bytes.extend_from_slice(text.as_bytes());
                bytes
            },
            {
                let mut bytes = vec![0xff, 0xfe];
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                }
                bytes
            },
            {
                let mut bytes = vec![0xfe, 0xff];
                for unit in text.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_be_bytes());
                }
                bytes
            },
            {
                let mut bytes = vec![0xff, 0xfe, 0x00, 0x00];
                for character in text.chars() {
                    bytes.extend_from_slice(&(character as u32).to_le_bytes());
                }
                bytes
            },
            {
                let mut bytes = vec![0x00, 0x00, 0xfe, 0xff];
                for character in text.chars() {
                    bytes.extend_from_slice(&(character as u32).to_be_bytes());
                }
                bytes
            },
        ];

        for bytes in encodings {
            let decoded = decode_file_contents(&bytes);
            assert_eq!(decoded, text);
            assert_eq!(parse_command_file_contents(&decoded).unwrap(), expected);
        }

        assert_eq!(
            parse_command_file_contents("\u{feff}OUTPUT=value\n").unwrap(),
            expected
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
    fn heredoc_allows_space_in_name_and_delimiter() {
        let commands = parse_command_file_contents("OUT KEY<<END MARK\nbody\nEND MARK\n").unwrap();

        assert_eq!(
            commands,
            vec![FileCommand {
                name: "OUT KEY".into(),
                value: "body".into(),
            }]
        );
    }

    #[test]
    fn heredoc_preserves_blank_body_lines_and_accepts_unterminated_delimiter_line() {
        let commands = parse_command_file_contents("MULTI<<END\n\nmiddle\n\nEND").unwrap();

        assert_eq!(
            commands,
            vec![FileCommand {
                name: "MULTI".into(),
                value: "\nmiddle\n".into(),
            }]
        );
    }

    #[test]
    fn rejects_empty_heredoc_name_or_delimiter() {
        for contents in ["<<END\nbody\nEND\n", "OUT<<\nEND\n"] {
            assert!(
                parse_command_file_contents(contents).is_err(),
                "{contents:?}"
            );
        }
    }

    #[test]
    fn matches_runner_line_endings_for_both_platforms() {
        let contents = "OUT=value\r\nMULTI<<END\r\none\r\ntwo\r\nEND\r\n";

        assert_eq!(
            parse_command_file_contents_for_os::<false>(contents).unwrap(),
            vec![
                FileCommand {
                    name: "OUT".into(),
                    value: "value\r".into(),
                },
                FileCommand {
                    name: "MULTI".into(),
                    value: "one\r\ntwo\r".into(),
                }
            ]
        );

        let blank_crlf = "A=first\r\n\r\nA=second\r\n";
        assert!(parse_command_file_contents_for_os::<false>(blank_crlf).is_err());
        assert_eq!(
            parse_command_file_contents_for_os::<true>(blank_crlf).unwrap(),
            vec![
                FileCommand {
                    name: "A".into(),
                    value: "first".into(),
                },
                FileCommand {
                    name: "A".into(),
                    value: "second".into(),
                }
            ]
        );
        assert_eq!(
            parse_command_file_contents_for_os::<true>(contents).unwrap(),
            vec![
                FileCommand {
                    name: "OUT".into(),
                    value: "value".into(),
                },
                FileCommand {
                    name: "MULTI".into(),
                    value: "one\r\ntwo".into(),
                }
            ]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_command_files_preserve_cr_from_crlf() {
        let commands =
            parse_command_file_contents("OUT=value\r\nMULTI<<END\nbody\r\nEND\n").unwrap();

        assert_eq!(
            commands,
            vec![
                FileCommand {
                    name: "OUT".into(),
                    value: "value\r".into(),
                },
                FileCommand {
                    name: "MULTI".into(),
                    value: "body\r".into(),
                }
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_files_strip_cr_from_crlf() {
        let commands =
            parse_command_file_contents("OUT=value\r\nMULTI<<END\r\nbody\r\nEND\r\n").unwrap();

        assert_eq!(
            commands,
            vec![
                FileCommand {
                    name: "OUT".into(),
                    value: "value".into(),
                },
                FileCommand {
                    name: "MULTI".into(),
                    value: "body".into(),
                }
            ]
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

        assert!(err.to_string().contains("Matching delimiter not found"));
    }

    #[test]
    fn rejects_heredoc_body_line_without_newline() {
        let err = parse_command_file_contents("payload<<EOF\none").unwrap_err();

        assert!(err.to_string().contains("EOF marker missing new line"));
    }
}
