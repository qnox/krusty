//! Parsed import syntax. Semantic binding belongs to the resolver's import inventory.

use crate::diag::Span;

/// One parsed import directive with the source span of each path segment.
#[derive(Clone, Debug)]
pub struct ImportPath {
    pub segments: Vec<(String, Span)>,
    pub wildcard: bool,
    pub alias: Option<String>,
}

impl ImportPath {
    pub fn path(&self) -> String {
        self.segments
            .iter()
            .map(|(segment, _)| segment.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }

    pub fn imported_name(&self) -> Option<&str> {
        if self.wildcard {
            return None;
        }
        self.alias
            .as_deref()
            .or_else(|| self.segments.last().map(|(segment, _)| segment.as_str()))
    }
}
