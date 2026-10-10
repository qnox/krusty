//! Refinement (`TreeRefiner`): the trees of a module and its templates merged into the one value
//! seen from a set of contexts, the most specific value of each property winning, lists and
//! objects merged, and schema defaults filled in where nothing (valid) was written.

use std::cmp::Ordering;

use super::contexts::{Contexts, FileOrder, Specificity};
use super::node::{Entry, Node, Trace, Value};
use crate::schema::{Default, ObjectType, Property, Type};

/// The values seen from `selected`, merged across `trees` (the module's, then its templates').
pub struct Refiner<'a> {
    pub order: &'a FileOrder,
    pub selected: Contexts,
}

/// A conflict between equally specific values of one property: each value, rendered as the
/// toolchain renders it in the message, and where it is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub key: String,
    pub values: Vec<(String, Trace)>,
}

/// A value as a conflict names it (`renderValue`).
fn conflict_rendering(value: &Value) -> String {
    match value {
        Value::Error | Value::Opaque => "error".to_string(),
        Value::Null => "null".to_string(),
        Value::Boolean(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Enum(_, value) => value.to_string(),
        Value::Path(path) => path.display().to_string(),
        Value::Reference { path, .. } => format!("${{{}}}", path.join(".")),
        Value::List(_) => "list […]".to_string(),
        Value::Mapping { .. } => "object {…}".to_string(),
    }
}

impl Refiner<'_> {
    fn visible(&self, contexts: &Contexts) -> bool {
        self.order
            .compare_contexts(&self.selected, contexts)
            .same_or_more()
    }

    /// Refine the root objects `trees`, which all have type `object`.
    pub fn refine_roots(
        &self,
        trees: &[&Node],
        object: &'static ObjectType,
        conflicts: &mut Vec<Conflict>,
    ) -> Node {
        let entries: Vec<&Entry> = trees
            .iter()
            .flat_map(|tree| match &tree.value {
                Value::Mapping { entries, .. } => entries.iter().collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        let trace = trees
            .first()
            .map(|tree| tree.trace.clone())
            .unwrap_or(Trace::Default);
        let entries = self.entries(&entries, conflicts);
        with_defaults(
            entries,
            Some(object),
            trace,
            Contexts::default(),
            self,
            conflicts,
        )
    }

    fn node(&self, node: &Node, conflicts: &mut Vec<Conflict>) -> Node {
        match &node.value {
            Value::List(children) => Node::new(
                Value::List(
                    children
                        .iter()
                        .filter(|child| self.visible(&child.contexts))
                        .map(|child| self.node(child, conflicts))
                        .collect(),
                ),
                node.trace.clone(),
                node.contexts,
            ),
            Value::Mapping { object, entries } => {
                let entries: Vec<&Entry> = entries.iter().collect();
                let entries = self.entries(&entries, conflicts);
                with_defaults(
                    entries,
                    *object,
                    node.trace.clone(),
                    node.contexts,
                    self,
                    conflicts,
                )
            }
            _ => node.clone(),
        }
    }

    /// Merge the entries of one mapping, by key, in the order keys first appear.
    fn entries(&self, entries: &[&Entry], conflicts: &mut Vec<Conflict>) -> Vec<Entry> {
        let visible: Vec<&Entry> = entries
            .iter()
            .copied()
            .filter(|entry| self.visible(&entry.value.contexts))
            .collect();
        let mut keys: Vec<&str> = Vec::new();
        for entry in &visible {
            if !keys.contains(&entry.key.as_str()) {
                keys.push(&entry.key);
            }
        }
        keys.into_iter()
            .map(|key| {
                let group: Vec<&Entry> = visible
                    .iter()
                    .copied()
                    .filter(|entry| entry.key == key)
                    .collect();
                match group.as_slice() {
                    [single] => Entry {
                        value: self.node(&single.value, conflicts),
                        ..(*single).clone()
                    },
                    _ => self.merge(&group, conflicts),
                }
            })
            .collect()
    }

    fn compare(&self, this: &Contexts, other: &Contexts) -> Specificity {
        self.order.compare_contexts(this, other)
    }

    /// Several values of one key: the most specific wins; lists and objects merge.
    fn merge(&self, group: &[&Entry], conflicts: &mut Vec<Conflict>) -> Entry {
        let mut most: Vec<&Entry> = Vec::new();
        for &entry in group {
            most.retain(|kept| {
                self.compare(&kept.value.contexts, &entry.value.contexts) != Specificity::Less
            });
            if !most.iter().any(|kept| {
                self.compare(&kept.value.contexts, &entry.value.contexts) == Specificity::More
            }) {
                most.push(entry);
            }
        }
        let sorted = sort_by_specificity(group, |a, b| {
            match self.compare(&a.value.contexts, &b.value.contexts) {
                Specificity::More => Ordering::Greater,
                Specificity::Less => Ordering::Less,
                _ => Ordering::Equal,
            }
        });
        let trace = sorted
            .last()
            .map(|entry| entry.value.trace.clone())
            .unwrap_or(Trace::Default);
        let first = most[0];
        if most
            .iter()
            .any(|entry| !same_value(&entry.value, &first.value))
        {
            conflicts.push(Conflict {
                key: first.key.clone(),
                values: most
                    .iter()
                    .map(|entry| {
                        (
                            conflict_rendering(&entry.value.value),
                            entry.value.trace.clone(),
                        )
                    })
                    .collect(),
            });
            return Entry {
                value: Node::new(Value::Error, trace, first.value.contexts),
                ..first.clone()
            };
        }
        let value = match &first.value.value {
            Value::List(_) => {
                let children: Vec<Node> = sorted
                    .iter()
                    .flat_map(|entry| match &entry.value.value {
                        Value::List(children) => children.clone(),
                        _ => Vec::new(),
                    })
                    .filter(|child| self.visible(&child.contexts))
                    .map(|child| self.node(&child, conflicts))
                    .collect();
                Node::new(Value::List(children), trace, first.value.contexts)
            }
            Value::Mapping { object, .. } => {
                let entries: Vec<&Entry> = sorted
                    .iter()
                    .flat_map(|entry| match &entry.value.value {
                        Value::Mapping { entries, .. } => entries.iter().collect::<Vec<_>>(),
                        _ => Vec::new(),
                    })
                    .collect();
                let entries = self.entries(&entries, conflicts);
                with_defaults(
                    entries,
                    *object,
                    trace,
                    first.value.contexts,
                    self,
                    conflicts,
                )
            }
            leaf => Node::new(leaf.clone(), trace, first.value.contexts),
        };
        Entry {
            value,
            ..first.clone()
        }
    }
}

/// Whether two most-specific values agree; lists, objects and errors always merge.
fn same_value(a: &Node, b: &Node) -> bool {
    match (&a.value, &b.value) {
        (
            Value::List(_) | Value::Mapping { .. } | Value::Error | Value::Opaque,
            Value::List(_) | Value::Mapping { .. } | Value::Error | Value::Opaque,
        ) => true,
        (Value::Null, Value::Null) => true,
        (Value::Boolean(a), Value::Boolean(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Enum(_, a), Value::Enum(_, b)) => a == b,
        (Value::Path(a), Value::Path(b)) => a == b,
        _ => false,
    }
}

/// A stable sort as the JVM sorts a short list (`TimSort`'s run detection, then binary
/// insertion), so that values whose contexts do not compare keep the toolchain's order.
fn sort_by_specificity<'e>(
    items: &[&'e Entry],
    compare: impl Fn(&Entry, &Entry) -> Ordering,
) -> Vec<&'e Entry> {
    let mut items = items.to_vec();
    let length = items.len();
    if length < 2 {
        return items;
    }
    // The initial run: ascending, or strictly descending (then reversed).
    let mut run = 2;
    if compare(items[1], items[0]) == Ordering::Less {
        while run < length && compare(items[run], items[run - 1]) == Ordering::Less {
            run += 1;
        }
        items[..run].reverse();
    } else {
        while run < length && compare(items[run], items[run - 1]) != Ordering::Less {
            run += 1;
        }
    }
    for start in run..length {
        let pivot = items[start];
        let (mut left, mut right) = (0, start);
        while left < right {
            let middle = (left + right) / 2;
            if compare(pivot, items[middle]) == Ordering::Less {
                right = middle;
            } else {
                left = middle + 1;
            }
        }
        items.copy_within(left..start, left + 1);
        items[left] = pivot;
    }
    items
}

/// An object's or map's refined entries, with the defaults of the properties nothing (valid) set.
fn with_defaults(
    mut entries: Vec<Entry>,
    object: Option<&'static ObjectType>,
    trace: Trace,
    contexts: Contexts,
    refiner: &Refiner<'_>,
    conflicts: &mut Vec<Conflict>,
) -> Node {
    if let Some(object) = object {
        for property in object.properties {
            let existing = entries.iter().position(|entry| entry.key == property.name);
            if let Some(index) = existing {
                if !matches!(entries[index].value.value, Value::Error) {
                    continue;
                }
            }
            let Some(default) = default_value(property, refiner, conflicts) else {
                continue;
            };
            let entry = Entry {
                key: property.name.to_string(),
                property: Some(property),
                key_trace: Trace::Default,
                value: default,
            };
            match existing {
                Some(index) => {
                    entries.remove(index);
                    entries.push(entry);
                }
                None => entries.push(entry),
            }
        }
    }
    Node::new(Value::Mapping { object, entries }, trace, contexts)
}

/// The schema's default for `property`, or `None` for a required property.
fn default_value(
    property: &'static Property,
    refiner: &Refiner<'_>,
    conflicts: &mut Vec<Conflict>,
) -> Option<Node> {
    let node = |value| Node::new(value, Trace::Default, Contexts::default());
    let value = match property.default {
        Default::Required => return None,
        Default::Null => Value::Null,
        Default::Boolean(value) => Value::Boolean(value),
        Default::Int(value) => Value::Int(value as i32),
        Default::String(value) => Value::String(value.to_string()),
        Default::Enum(value) => match property.ty {
            Type::Enum(enumeration) => Value::Enum(enumeration, value),
            _ => unreachable!("an enum default belongs to an enum property"),
        },
        Default::List(values) => Value::List(
            values
                .iter()
                .map(|value| {
                    node(match property.ty {
                        Type::List(Type::Enum(enumeration)) => Value::Enum(enumeration, value),
                        _ => Value::String(value.to_string()),
                    })
                })
                .collect(),
        ),
        Default::EmptyMap => Value::Mapping {
            object: None,
            entries: Vec::new(),
        },
        Default::Nested => match property.ty {
            Type::Object(object) => {
                return Some(with_defaults(
                    Vec::new(),
                    Some(object),
                    Trace::Default,
                    Contexts::default(),
                    refiner,
                    conflicts,
                ))
            }
            _ => unreachable!("a nested default belongs to an object property"),
        },
        Default::Reference { path, derivation } => Value::Reference { path, derivation },
    };
    Some(node(value))
}
