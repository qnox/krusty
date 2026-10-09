//! A conflict between equally specific values, as the toolchain prints it
//! (`renderConflictingProperties`).

use crate::tree::{Conflict, Contexts, Files, Trace};

/// The contexts the values were seen from, then each value with every place it is written.
pub(super) fn message(conflict: &Conflict, selected: Contexts, files: &Files) -> String {
    let mut contexts = Vec::new();
    if selected.test {
        contexts.push("test".to_string());
    }
    if selected.jvm {
        contexts.push("jvm".to_string());
    }
    if let Some(file) = selected.file {
        contexts.push(files.path(file).display().to_string());
    }
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for (value, trace) in &conflict.values {
        let Trace::File { file, position } = trace else {
            continue;
        };
        let place = format!(
            "{}:{}:{}",
            files.path(*file).display(),
            position.line,
            position.column
        );
        match groups.iter_mut().find(|(known, _)| known == value) {
            Some((_, places)) => places.push(place),
            None => groups.push((value, vec![place])),
        }
    }
    let groups: Vec<String> = groups
        .into_iter()
        .map(|(value, places)| {
            let last = places.len().saturating_sub(1);
            let places: Vec<String> = places
                .into_iter()
                .enumerate()
                .map(|(index, place)| {
                    let branch = if index == last { "╰─" } else { "├─" };
                    format!("    {branch} {place}")
                })
                .collect();
            format!("  - `{value}`, defined at:\n{}", places.join("\n"))
        })
        .collect();
    format!(
        "Conflicting values for property `{}` in context [{}]:\n{}",
        conflict.key,
        contexts.join(", "),
        groups.join("\n")
    )
}
