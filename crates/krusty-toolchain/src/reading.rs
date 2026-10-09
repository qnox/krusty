//! Reading typed values out of a [`Document`] the way the toolchain's schema reader does, with its
//! type-mismatch messages: `Expected `<type>`, but got `<value kind>``, `Expected a value: `<type>``
//! for an empty value, and `Unknown property `<name>`` for a key the schema does not have.
//!
//! The order is the toolchain's too: a value's custom `!tag` is refused where the value is met, and
//! unknown properties are reported after everything else the file's mapping holds.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::inventory;
use crate::yaml::{Document, Limits, NodeId, NodeKind, Position, Style};

/// Read a file of at most `limit` bytes as UTF-8, never through a symbolic link, or report why
/// not.
pub fn read_bounded(path: &Path, limit: usize) -> Result<String, String> {
    let bytes = inventory::read_file(path, limit).map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|_| format!("{}: the file is not UTF-8", path.display()))
}

/// Parse a YAML build file, reporting its problems. `None` when it could not be read or parsed.
pub fn read_document(path: &Path, diagnostics: &mut Diagnostics) -> Option<Document> {
    let text = match read_bounded(path, Limits::DEFAULT.bytes) {
        Ok(text) => text,
        Err(message) => {
            diagnostics.push(Diagnostic::error(path, None, message));
            return None;
        }
    };
    match Document::parse(&text, Limits::DEFAULT) {
        Ok(parsed) => {
            for problem in parsed.problems {
                diagnostics.push(Diagnostic::error(
                    path,
                    Some(problem.position),
                    problem.message,
                ));
            }
            Some(parsed.document)
        }
        Err(error) => {
            diagnostics.push(Diagnostic::error(path, Some(error.position), error.message));
            None
        }
    }
}

/// One file being read, and where its problems go.
pub struct FileReader<'a> {
    pub file: PathBuf,
    pub document: &'a Document,
    pub diagnostics: &'a mut Diagnostics,
    /// Nodes whose tag has been refused, so a value looked at twice is reported once.
    tags_reported: HashSet<NodeId>,
    /// Keys the schema does not have, reported by [`FileReader::finish`].
    unknown: Vec<NodeId>,
}

/// What a node holds, for type checks.
pub enum Value<'d> {
    /// A plain empty value (`key:` or `-` with nothing after it).
    Missing,
    /// An empty value with a tag: not even `null`.
    Absent,
    /// A plain `null`.
    Null,
    Scalar(&'d str),
    Sequence(&'d [NodeId]),
    Mapping(&'d [(NodeId, NodeId)]),
}

impl<'a> FileReader<'a> {
    pub fn new(file: &Path, document: &'a Document, diagnostics: &'a mut Diagnostics) -> Self {
        Self {
            file: file.to_path_buf(),
            document,
            diagnostics,
            tags_reported: HashSet::new(),
            unknown: Vec::new(),
        }
    }

    /// Refuse a custom tag on `node`. The top node's tag is accepted, as the toolchain accepts it.
    fn meet(&mut self, node: NodeId) {
        let Some(position) = self.document.tag(node) else {
            return;
        };
        if self.document.root() == Some(node) || !self.tags_reported.insert(node) {
            return;
        }
        self.diagnostics.push(Diagnostic::error(
            &self.file,
            Some(position),
            "Unexpected custom YAML type tag",
        ));
    }

    /// Meet every node of a value that is not read further, in document order.
    pub fn skip(&mut self, node: NodeId) {
        self.meet(node);
        let document: &'a Document = self.document;
        match &document.node(node).kind {
            NodeKind::Scalar { .. } => {}
            NodeKind::Sequence(items) => {
                for &item in items {
                    self.skip(item);
                }
            }
            NodeKind::Mapping(entries) => {
                for &(key, value) in entries {
                    self.skip(key);
                    self.skip(value);
                }
            }
        }
    }

    pub fn position(&self, node: NodeId) -> Position {
        self.document.node(node).span.start
    }

    /// What `node` holds, met by the reader: a custom tag on it is refused.
    pub fn value(&mut self, node: NodeId) -> Value<'a> {
        self.meet(node);
        let document: &'a Document = self.document;
        match &document.node(node).kind {
            NodeKind::Scalar { value, style } if *style == Style::Plain && value.is_empty() => {
                if document.tag(node).is_some() {
                    Value::Absent
                } else {
                    Value::Missing
                }
            }
            NodeKind::Scalar { value, style } if *style == Style::Plain && value == "null" => {
                Value::Null
            }
            NodeKind::Scalar { value, .. } => Value::Scalar(value),
            NodeKind::Sequence(items) => Value::Sequence(items),
            NodeKind::Mapping(entries) => Value::Mapping(entries),
        }
    }

    /// The text of a scalar key.
    pub fn key(&self, key: NodeId) -> &'a str {
        let document: &'a Document = self.document;
        match &document.node(key).kind {
            NodeKind::Scalar { value, .. } => value,
            _ => unreachable!("yaml.rs refuses non-scalar keys"),
        }
    }

    pub fn error(&mut self, node: NodeId, message: impl Into<String>) {
        let position = self.position(node);
        self.diagnostics
            .push(Diagnostic::error(&self.file, Some(position), message));
    }

    /// Report that `node` does not hold a value of `expected` (rendered as the toolchain renders the
    /// schema type).
    pub fn mismatch(&mut self, node: NodeId, expected: &str) {
        let message = match self.value(node) {
            Value::Missing | Value::Absent => format!("Expected a value: `{expected}`"),
            Value::Null => "`null` value is unexpected here. Use quotes to treat it as a regular string literal".to_string(),
            Value::Scalar(_) => format!("Expected `{expected}`, but got `scalar`"),
            Value::Sequence(_) => format!("Expected `{expected}`, but got `sequence []`"),
            Value::Mapping(_) => format!("Expected `{expected}`, but got `mapping {{}}`"),
        };
        self.error(node, message);
    }

    /// A key the schema does not have: its value's tags are refused now, the key itself by
    /// [`FileReader::finish`].
    pub fn unknown_property(&mut self, key: NodeId, value: NodeId) {
        self.skip(key);
        self.skip(value);
        self.unknown.push(key);
    }

    /// Report the unknown properties, after the rest of the file's mapping.
    pub fn finish(&mut self) {
        for key in std::mem::take(&mut self.unknown) {
            let name = self.key(key).to_string();
            self.error(key, format!("Unknown property `{name}`"));
        }
    }

    /// A string, or a reported mismatch.
    pub fn string(&mut self, node: NodeId) -> Option<&'a str> {
        match self.value(node) {
            Value::Scalar(text) => Some(text),
            _ => {
                self.mismatch(node, "string");
                None
            }
        }
    }

    /// A sequence of strings: each element that is not one is reported and skipped.
    pub fn strings(&mut self, node: NodeId) -> Vec<(NodeId, &'a str)> {
        let Value::Sequence(items) = self.value(node) else {
            self.mismatch(node, "sequence [string]");
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|&item| self.string(item).map(|text| (item, text)))
            .collect()
    }
}
