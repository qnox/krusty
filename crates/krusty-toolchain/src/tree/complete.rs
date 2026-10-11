//! Completion (`completeTree`): a refined tree whose every object has all its required properties.
//! An object missing one is reported and dropped, as is everything holding it; a list or map
//! drops its incomplete items. Values that could not be read were reported when they were read.

use std::path::Path;

use super::node::{Entry, Node, Trace, Value};
use crate::diagnostic::{Diagnostic, Diagnostics};

/// The complete form of `node`, or `None` when it is incomplete. Missing properties are reported
/// against `path` (the file the refined tree is seen from).
pub fn complete(node: &Node, path: &Path, diagnostics: &mut Diagnostics) -> Option<Node> {
    match &node.value {
        Value::Error | Value::Reference { .. } => None,
        Value::List(children) => Some(Node::new(
            Value::List(
                children
                    .iter()
                    .filter_map(|child| complete(child, path, diagnostics))
                    .collect(),
            ),
            node.trace.clone(),
            node.contexts,
        )),
        Value::Mapping {
            object: None,
            entries,
        } => {
            let entries = entries
                .iter()
                .filter_map(|entry| {
                    complete(&entry.value, path, diagnostics).map(|value| Entry {
                        value,
                        ..entry.clone()
                    })
                })
                .collect();
            Some(Node::new(
                Value::Mapping {
                    object: None,
                    entries,
                },
                node.trace.clone(),
                node.contexts,
            ))
        }
        Value::Mapping {
            object: Some(object),
            entries,
        } => {
            let mut complete_entries = Vec::new();
            let mut missing = Vec::new();
            let mut incomplete = false;
            for property in object.properties {
                let Some(entry) = entries.iter().find(|entry| entry.key == property.name) else {
                    missing.push(property.name);
                    incomplete = true;
                    continue;
                };
                match complete(&entry.value, path, diagnostics) {
                    Some(value) => complete_entries.push(Entry {
                        value,
                        ..entry.clone()
                    }),
                    None => incomplete = true,
                }
            }
            if !missing.is_empty() {
                let names: Vec<String> = missing.iter().map(|name| format!("`{name}`")).collect();
                let message = match names.as_slice() {
                    [single] => format!("No value for required property {single}."),
                    _ => format!("No value for required properties: {}.", names.join(", ")),
                };
                let span = match node.trace {
                    Trace::File { span, .. } => Some(span),
                    _ => None,
                };
                diagnostics.push(Diagnostic::error(path, span, message));
            }
            if incomplete {
                return None;
            }
            Some(Node::new(
                Value::Mapping {
                    object: Some(object),
                    entries: complete_entries,
                },
                node.trace.clone(),
                node.contexts,
            ))
        }
        _ => Some(node.clone()),
    }
}
