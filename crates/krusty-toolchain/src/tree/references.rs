//! Resolving the defaults that name another property (`resolveReferences`): each takes a copy of
//! that property's refined value, or a value derived from it, traced to where that value is
//! written.

use super::node::{Node, Trace, Value};
use crate::maven::ComparableVersion;
use crate::schema::Derivation;

/// Resolve every reference in `node`, each against the object that holds it.
pub fn resolve(node: &mut Node) {
    let Value::Mapping { entries, .. } = &mut node.value else {
        if let Value::List(children) = &mut node.value {
            children.iter_mut().for_each(resolve);
        }
        return;
    };
    for index in 0..entries.len() {
        if let Value::Reference { path, derivation } = entries[index].value.value {
            let snapshot = Node::new(
                Value::Mapping {
                    object: None,
                    entries: entries.clone(),
                },
                Trace::Default,
                Default::default(),
            );
            if let Some(resolved) = resolved(&snapshot, path, derivation, 0) {
                entries[index].value = resolved;
            }
        }
    }
    for entry in entries.iter_mut() {
        resolve(&mut entry.value);
    }
}

/// The value a reference from `object` resolves to; `depth` bounds chains of references.
fn resolved(
    object: &Node,
    path: &'static [&'static str],
    derivation: Option<Derivation>,
    depth: usize,
) -> Option<Node> {
    let mut target = object;
    for segment in path {
        target = target.get(segment)?;
    }
    let target = match target.value {
        // Within one object; the schema's chains are short and acyclic.
        Value::Reference {
            path: next,
            derivation: next_derivation,
        } if depth < 8 => resolved(object, next, next_derivation, depth + 1)?,
        _ => target.clone(),
    };
    let source = target.trace.file();
    let path = path.join(".");
    Some(match derivation {
        None => Node::new(
            target.value,
            Trace::Derived {
                description: format!("default, from ${{{path}}}"),
                source,
            },
            target.contexts,
        ),
        Some(derivation) => Node::new(
            derive(derivation, &target.value),
            Trace::Derived {
                description: format!(
                    "default, based on ${{{path}}}: {}",
                    derivation.description()
                ),
                source,
            },
            target.contexts,
        ),
    })
}

fn derive(derivation: Derivation, source: &Value) -> Value {
    match derivation {
        Derivation::KotlinIncrementalCompilation => Value::Boolean(matches!(
            source,
            Value::String(version)
                if ComparableVersion::new(version) >= ComparableVersion::new("2.4.0")
        )),
        Derivation::EnabledWhenSpecified => Value::Boolean(!matches!(source, Value::Null)),
        Derivation::ScmConnection | Derivation::ScmDeveloperConnection => match source {
            Value::String(url) => Value::String(format!("scm:git:{url}")),
            _ => Value::Null,
        },
    }
}
