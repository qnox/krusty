//! The subset of YAML that `module.yaml` and `project.yaml` use.
//!
//! The toolchain's own files are block mappings and block sequences of plain scalars. Flow
//! collections, anchors, and tags are rejected so a construct this parser does not understand
//! cannot be read as a shorter document.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Yaml {
    Scalar(String),
    Seq(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    pub(super) fn as_map(&self) -> Option<&[(String, Yaml)]> {
        match self {
            Self::Map(entries) => Some(entries),
            _ => None,
        }
    }

    pub(super) fn as_seq(&self) -> Option<&[Yaml]> {
        match self {
            Self::Seq(items) => Some(items),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(formatter, "{}", self.message)
        } else {
            write!(formatter, "line {}: {}", self.line, self.message)
        }
    }
}

#[derive(Clone, Debug)]
struct Line {
    number: usize,
    indent: usize,
    text: String,
}

pub(super) fn parse(text: &str) -> Result<Yaml, ParseError> {
    let lines = logical_lines(text)?;
    if lines.is_empty() {
        return Err(ParseError {
            line: 0,
            message: "document is empty".to_string(),
        });
    }
    let mut parser = Parser { lines, index: 0 };
    let value = parser.parse_block()?;
    if parser.index < parser.lines.len() {
        let line = &parser.lines[parser.index];
        return Err(ParseError {
            line: line.number,
            message: "unexpected content after the document".to_string(),
        });
    }
    Ok(value)
}

struct Parser {
    lines: Vec<Line>,
    index: usize,
}

impl Parser {
    fn parse_block(&mut self) -> Result<Yaml, ParseError> {
        let line = self.current()?;
        if line.text.starts_with("- ") || line.text == "-" {
            self.parse_seq(line.indent)
        } else {
            self.parse_map(line.indent)
        }
    }

    fn parse_map(&mut self, indent: usize) -> Result<Yaml, ParseError> {
        let mut entries = Vec::new();
        while let Some(line) = self.peek() {
            if line.indent < indent {
                break;
            }
            if line.indent > indent {
                return Err(ParseError {
                    line: line.number,
                    message: "indentation does not match the enclosing block".to_string(),
                });
            }
            if line.text.starts_with("- ") || line.text == "-" {
                break;
            }
            let number = line.number;
            let text = line.text.clone();
            self.index += 1;
            let (key, inline) = split_key(&text, number)?;
            if entries.iter().any(|(existing, _)| existing == &key) {
                return Err(ParseError {
                    line: number,
                    message: format!("duplicate key '{key}'"),
                });
            }
            let value = self.value_after(indent, number, inline)?;
            entries.push((key, value));
        }
        if entries.is_empty() {
            return Err(ParseError {
                line: self.line_number(),
                message: "expected a mapping".to_string(),
            });
        }
        Ok(Yaml::Map(entries))
    }

    fn parse_seq(&mut self, indent: usize) -> Result<Yaml, ParseError> {
        let mut items = Vec::new();
        while let Some(line) = self.peek() {
            if line.indent < indent {
                break;
            }
            if line.indent > indent {
                return Err(ParseError {
                    line: line.number,
                    message: "indentation does not match the enclosing block".to_string(),
                });
            }
            if line.text != "-" && !line.text.starts_with("- ") {
                break;
            }
            let number = line.number;
            let body = line.text[1..].trim().to_string();
            self.index += 1;
            let item = if body.is_empty() {
                self.nested_block(indent, number)?
            } else if let Some((key, inline)) = split_key_optional(&body) {
                let mut entries = Vec::new();
                let value = self.value_after(indent, number, inline)?;
                entries.push((key, value));
                Yaml::Map(entries)
            } else {
                Yaml::Scalar(unquote(&body, number)?)
            };
            items.push(item);
        }
        if items.is_empty() {
            return Err(ParseError {
                line: self.line_number(),
                message: "expected a sequence".to_string(),
            });
        }
        Ok(Yaml::Seq(items))
    }

    fn value_after(
        &mut self,
        indent: usize,
        number: usize,
        inline: Option<String>,
    ) -> Result<Yaml, ParseError> {
        match inline {
            Some(value) if is_block_scalar(&value) => self.block_scalar(indent),
            Some(value) => {
                if self.peek().is_some_and(|next| next.indent > indent) {
                    return Err(ParseError {
                        line: number,
                        message: format!("'{value}' cannot be followed by a nested block"),
                    });
                }
                Ok(Yaml::Scalar(unquote(&value, number)?))
            }
            None => {
                if self.peek().is_some_and(|next| next.indent > indent) {
                    self.parse_block()
                } else {
                    Ok(Yaml::Scalar(String::new()))
                }
            }
        }
    }

    fn nested_block(&mut self, indent: usize, number: usize) -> Result<Yaml, ParseError> {
        if self.peek().is_some_and(|next| next.indent > indent) {
            self.parse_block()
        } else {
            Err(ParseError {
                line: number,
                message: "expected a nested block".to_string(),
            })
        }
    }

    fn block_scalar(&mut self, key_indent: usize) -> Result<Yaml, ParseError> {
        let mut rows = Vec::new();
        let mut content_indent = None;
        while let Some(line) = self.peek() {
            if line.indent <= key_indent {
                break;
            }
            let indent = content_indent.get_or_insert(line.indent);
            if line.indent < *indent {
                return Err(ParseError {
                    line: line.number,
                    message: "block scalar indentation decreased".to_string(),
                });
            }
            let padding = line.indent - *indent;
            rows.push(format!("{}{}", " ".repeat(padding), line.text));
            self.index += 1;
        }
        Ok(Yaml::Scalar(rows.join("\n")))
    }

    fn peek(&self) -> Option<&Line> {
        self.lines.get(self.index)
    }

    fn current(&self) -> Result<&Line, ParseError> {
        self.peek().ok_or_else(|| ParseError {
            line: self.line_number(),
            message: "unexpected end of document".to_string(),
        })
    }

    fn line_number(&self) -> usize {
        self.lines
            .get(self.index)
            .or_else(|| self.lines.last())
            .map(|line| line.number)
            .unwrap_or(0)
    }
}

fn logical_lines(text: &str) -> Result<Vec<Line>, ParseError> {
    let mut lines = Vec::new();
    for (offset, raw) in text.lines().enumerate() {
        let number = offset + 1;
        if raw.chars().any(|character| character == '\t') {
            return Err(ParseError {
                line: number,
                message: "tabs are not allowed; indent with spaces".to_string(),
            });
        }
        let trimmed = strip_comment(raw, number)?;
        let trimmed = trimmed.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim() == "---" {
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        let text = trimmed.trim_start().to_string();
        if unsupported_syntax(&text) {
            return Err(ParseError {
                line: number,
                message: "flow collections, anchors, and aliases are not supported".to_string(),
            });
        }
        lines.push(Line {
            number,
            indent,
            text,
        });
    }
    Ok(lines)
}

fn strip_comment(raw: &str, number: usize) -> Result<String, ParseError> {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut chars = raw.chars().peekable();
    while let Some(character) = chars.next() {
        match quote {
            Some(mark) => {
                out.push(character);
                if character == '\\' && mark == '"' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if character == mark {
                    quote = None;
                }
            }
            None if character == '"' || character == '\'' => {
                quote = Some(character);
                out.push(character);
            }
            None if character == '#' => break,
            None => out.push(character),
        }
    }
    if quote.is_some() {
        return Err(ParseError {
            line: number,
            message: "unterminated quote".to_string(),
        });
    }
    Ok(out)
}

fn split_key(text: &str, number: usize) -> Result<(String, Option<String>), ParseError> {
    split_key_optional(text).ok_or_else(|| ParseError {
        line: number,
        message: format!("expected 'key: value', found '{text}'"),
    })
}

fn split_key_optional(text: &str) -> Option<(String, Option<String>)> {
    if let Some(key) = text.strip_suffix(':') {
        let key = unquote_key(key.trim())?;
        return Some((key, None));
    }
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(mark) = quote {
            if byte == b'\\' && mark == b'"' {
                index += 2;
                continue;
            }
            if byte == mark {
                quote = None;
            }
            index += 1;
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if byte == b':' && bytes.get(index + 1) == Some(&b' ') {
            let key = unquote_key(text[..index].trim())?;
            let value = text[index + 2..].trim().to_string();
            return Some((key, Some(value)));
        }
        index += 1;
    }
    None
}

fn unquote_key(key: &str) -> Option<String> {
    unquote(key, 0).ok().filter(|unquoted| !unquoted.is_empty())
}

fn unquote(text: &str, number: usize) -> Result<String, ParseError> {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return Ok(String::new());
    };
    if first != '"' && first != '\'' {
        return Ok(text.to_string());
    }
    let Some(last) = text.chars().next_back() else {
        return Ok(text.to_string());
    };
    if last != first || text.chars().count() < 2 {
        return Err(ParseError {
            line: number,
            message: "unterminated quote".to_string(),
        });
    }
    let inner: String = text
        .chars()
        .skip(1)
        .take(text.chars().count() - 2)
        .collect();
    if first == '\'' {
        return Ok(inner.replace("''", "'"));
    }
    let mut out = String::new();
    let mut escaped = false;
    for character in inner.chars() {
        if escaped {
            out.push(match character {
                'n' => '\n',
                't' => '\t',
                '\\' => '\\',
                '"' => '"',
                other => other,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            out.push(character);
        }
    }
    if escaped {
        return Err(ParseError {
            line: number,
            message: "unterminated escape".to_string(),
        });
    }
    Ok(out)
}

fn unsupported_syntax(text: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    let mut token_start = true;
    while let Some(character) = chars.next() {
        if let Some(mark) = quote {
            if character == '\\' && mark == '"' {
                chars.next();
            } else if character == mark {
                quote = None;
            }
            token_start = false;
            continue;
        }
        match character {
            '"' | '\'' => {
                quote = Some(character);
                token_start = false;
            }
            '[' | '{' => return true,
            '&' if token_start => return true,
            '*' if token_start => {
                let alias = chars
                    .clone()
                    .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                    .count();
                if alias > 0 {
                    return true;
                }
                token_start = false;
            }
            other => token_start = other.is_whitespace(),
        }
    }
    false
}

fn is_block_scalar(value: &str) -> bool {
    matches!(value, "|" | "|-" | "|+" | ">" | ">-" | ">+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_file_parses_as_a_block_mapping() {
        let yaml = parse(
            "\
# a console application
product: jvm/app
layout: \"maven-like\"
dependencies:
  - //libs/lib1
  - //libs/lib2: exported
description: |
  first line
  second line
",
        )
        .expect("parse");
        let map = yaml.as_map().expect("mapping");
        assert_eq!(map[0].0, "product");
        assert_eq!(map[0].1, Yaml::Scalar("jvm/app".to_string()));
        assert_eq!(map[1].1, Yaml::Scalar("maven-like".to_string()));
        let dependencies = map[2].1.as_seq().expect("dependencies");
        assert_eq!(dependencies[0], Yaml::Scalar("//libs/lib1".to_string()));
        assert_eq!(
            dependencies[1],
            Yaml::Map(vec![(
                "//libs/lib2".to_string(),
                Yaml::Scalar("exported".to_string())
            )])
        );
        assert_eq!(
            map[3].1,
            Yaml::Scalar("first line\nsecond line".to_string())
        );
    }

    #[test]
    fn a_nested_dependency_attribute_map_keeps_its_keys() {
        let yaml = parse(
            "\
dependencies:
  - //core:
      exported: true
      scope: compile-only
",
        )
        .expect("parse");
        let item = &yaml.as_map().unwrap()[0].1.as_seq().unwrap()[0];
        let entry = item.as_map().unwrap();
        assert_eq!(entry[0].0, "//core");
        let attributes = entry[0].1.as_map().unwrap();
        assert_eq!(
            attributes[0],
            ("exported".to_string(), Yaml::Scalar("true".to_string()))
        );
        assert_eq!(
            attributes[1],
            (
                "scope".to_string(),
                Yaml::Scalar("compile-only".to_string())
            )
        );
    }

    #[test]
    fn tabs_duplicate_keys_and_flow_syntax_are_rejected() {
        assert_eq!(
            parse("product:\tjvm/app").unwrap_err().to_string(),
            "line 1: tabs are not allowed; indent with spaces"
        );
        assert_eq!(
            parse("product: jvm/app\nproduct: jvm/lib")
                .unwrap_err()
                .to_string(),
            "line 2: duplicate key 'product'"
        );
        assert_eq!(
            parse("modules: [app, lib]").unwrap_err().to_string(),
            "line 1: flow collections, anchors, and aliases are not supported"
        );
    }

    #[test]
    fn a_module_glob_is_a_plain_scalar() {
        let yaml = parse("modules:\n  - libs/*\n").expect("parse");
        let modules = yaml.as_map().unwrap()[0].1.as_seq().unwrap();
        assert_eq!(modules[0], Yaml::Scalar("libs/*".to_string()));
    }

    #[test]
    fn comments_quotes_and_block_scalars_keep_their_text() {
        let yaml = parse(
            "\
---
# header
product: \"jvm/app\" # trailing
description: >
  folded
  lines
note: 'it''s # not a comment'
",
        )
        .expect("parse");
        let map = yaml.as_map().expect("mapping");
        assert_eq!(
            map[0],
            ("product".to_string(), Yaml::Scalar("jvm/app".to_string()))
        );
        assert_eq!(map[1].1, Yaml::Scalar("folded\nlines".to_string()));
        assert_eq!(map[2].1, Yaml::Scalar("it's # not a comment".to_string()));
    }

    #[test]
    fn escapes_and_structural_errors_are_reported_in_full() {
        assert_eq!(
            parse("description: \"a\\nb\\t\\\"c\\\\\"")
                .unwrap()
                .as_map()
                .unwrap()[0]
                .1,
            Yaml::Scalar("a\nb\t\"c\\".to_string())
        );
        assert_eq!(parse("").unwrap_err().to_string(), "document is empty");
        assert_eq!(
            parse("# only\n").unwrap_err().to_string(),
            "document is empty"
        );
        assert_eq!(
            parse("product: \"jvm/app").unwrap_err().to_string(),
            "line 1: unterminated quote"
        );
        assert_eq!(
            parse("product: \"jvm\\\\\"").unwrap().as_map().unwrap()[0].1,
            Yaml::Scalar("jvm\\".to_string())
        );
        assert_eq!(
            parse("product: &app jvm/app").unwrap_err().to_string(),
            "line 1: flow collections, anchors, and aliases are not supported"
        );
        assert_eq!(
            parse("product: *app").unwrap_err().to_string(),
            "line 1: flow collections, anchors, and aliases are not supported"
        );
        assert_eq!(
            parse("product:\n    type: jvm/app\n  layout: amper")
                .unwrap_err()
                .to_string(),
            "line 3: indentation does not match the enclosing block"
        );
        assert_eq!(
            parse("product: jvm/app\n  layout: amper")
                .unwrap_err()
                .to_string(),
            "line 1: 'jvm/app' cannot be followed by a nested block"
        );
        assert_eq!(
            parse("dependencies:\n  -\n").unwrap_err().to_string(),
            "line 2: expected a nested block"
        );
        assert_eq!(
            parse("description: |\n  one\n two")
                .unwrap_err()
                .to_string(),
            "line 3: block scalar indentation decreased"
        );
        assert_eq!(
            parse("- item\nproduct: jvm/app").unwrap_err().to_string(),
            "line 2: unexpected content after the document"
        );
        assert_eq!(
            parse("jvm/app").unwrap_err().to_string(),
            "line 1: expected 'key: value', found 'jvm/app'"
        );
        assert_eq!(
            parse("- one\n  - two").unwrap_err().to_string(),
            "line 2: indentation does not match the enclosing block"
        );
        assert_eq!(
            parse("description: |+\n  kept\n")
                .unwrap()
                .as_map()
                .unwrap()[0]
                .1,
            Yaml::Scalar("kept".to_string())
        );
    }
}
