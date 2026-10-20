//! What a toolchain plugin's `plugin.yaml` declares, as far as krusty-toolchain implements it: the
//! Kotlin source directories and the resource directories it adds to the main fragment of a module
//! that enables it (`generated.sources`, `generated.resources`), each written as the argument of a task's action that is a directory of
//! that module (`${tasks.<task>.action.<argument>}`), or as that directory itself
//! (`${module.rootDir}/…`). The toolchain rejects a directory written out that no task's action
//! argument is, for each module that enables the plugin (`super`).
//!
//! krusty-toolchain does not run a plugin's tasks: their actions are code of the plugin's own. A
//! directory a task provides is compiled as it is when the task would run. Everything else a
//! `plugin.yaml` may declare is refused by name, where it is written.

use std::path::{Path, PathBuf};

use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::reading::{read_document, FileReader, Value};
use crate::yaml::{NodeId, Span};

/// A Kotlin source or resource directory a plugin adds to a module.
pub(super) struct GeneratedDirectory {
    /// The directory below the module's directory, as path segments.
    pub below_module: Vec<String>,
    /// The task whose action argument names the directory, when one does.
    pub task: Option<String>,
    /// Where the directory is written in `plugin.yaml`.
    pub span: Span,
}

/// A plugin's declarations.
pub(super) struct Declaration {
    pub file: PathBuf,
    /// The whole document, where the toolchain reports a problem with a generated directory.
    pub document: Option<Span>,
    pub sources: Vec<GeneratedDirectory>,
    pub resources: Vec<GeneratedDirectory>,
}

const MODULE_ROOT: &str = "${module.rootDir}";

/// The task actions' arguments, by task and argument, as written.
struct Tasks<'d> {
    arguments: Vec<(&'d str, &'d str, NodeId)>,
}

/// Read the `plugin.yaml` file `file` of plugin `id`, or why krusty-toolchain does not implement
/// what it declares.
pub(super) fn read(id: &str, file: &Path) -> Result<Declaration, Vec<Diagnostic>> {
    let mut diagnostics = Diagnostics::default();
    let Some(document) = read_document(file, &mut diagnostics) else {
        return Err(diagnostics.iter().cloned().collect());
    };
    let mut reader = FileReader::new(file, &document, &mut diagnostics);
    let mut tasks = Tasks {
        arguments: Vec::new(),
    };
    let mut generated = None;
    let document_span = document.root().map(|root| reader.span(root));
    match document.root().map(|root| (root, reader.value(root))) {
        None | Some((_, Value::Missing | Value::Null)) => {}
        Some((_, Value::Mapping(pairs))) => {
            for &(key, value) in pairs {
                match reader.key(key) {
                    "tasks" => read_tasks(id, &mut reader, value, &mut tasks),
                    "generated" => generated = Some(value),
                    other => {
                        let other = other.to_string();
                        reader.error(
                            key,
                            format!("plugin `{id}` declares `{other}`, and krusty-toolchain does not implement it"),
                        );
                    }
                }
            }
        }
        Some((root, _)) => reader.error(
            root,
            format!("krusty-toolchain reads the `plugin.yaml` of plugin `{id}` only as a mapping"),
        ),
    }
    let (sources, resources) = generated
        .map(|generated| read_generated(id, &mut reader, generated, &tasks))
        .unwrap_or_default();
    if diagnostics.has_errors() {
        return Err(diagnostics.iter().cloned().collect());
    }
    Ok(Declaration {
        file: file.to_path_buf(),
        document: document_span,
        sources,
        resources,
    })
}

/// `tasks:`: each task's `action`, a tagged mapping of its arguments.
fn read_tasks<'d>(id: &str, reader: &mut FileReader<'d>, node: NodeId, tasks: &mut Tasks<'d>) {
    let Value::Mapping(entries) = reader.value(node) else {
        reader.error(
            node,
            format!("krusty-toolchain reads `tasks` of plugin `{id}` only as a mapping of tasks"),
        );
        return;
    };
    for &(task_key, task) in entries {
        let name = reader.key(task_key);
        let Value::Mapping(properties) = reader.value(task) else {
            reader.error(
                task,
                format!("krusty-toolchain reads task `{name}` of plugin `{id}` only as a mapping"),
            );
            continue;
        };
        for &(property, value) in properties {
            match reader.key(property) {
                // The action's tag names the plugin's function, which krusty-toolchain does not
                // run; only its arguments are read.
                "action" => match reader.shape(value) {
                    Value::Mapping(arguments) => {
                        for &(argument, given) in arguments {
                            tasks
                                .arguments
                                .push((name, reader.key(argument), given));
                        }
                    }
                    Value::Missing | Value::Absent => {}
                    _ => reader.error(
                        value,
                        format!("krusty-toolchain reads the action of task `{name}` of plugin `{id}` only as a mapping of arguments"),
                    ),
                },
                other => {
                    let other = other.to_string();
                    reader.error(
                        property,
                        format!("task `{name}` of plugin `{id}` sets `{other}`, and krusty-toolchain does not implement it"),
                    );
                }
            }
        }
    }
}

/// `generated:`: its Kotlin `sources` and its `resources`.
fn read_generated(
    id: &str,
    reader: &mut FileReader<'_>,
    node: NodeId,
    tasks: &Tasks<'_>,
) -> (Vec<GeneratedDirectory>, Vec<GeneratedDirectory>) {
    let Value::Mapping(entries) = reader.value(node) else {
        reader.error(
            node,
            format!("krusty-toolchain reads `generated` of plugin `{id}` only as a mapping"),
        );
        return (Vec::new(), Vec::new());
    };
    let mut sources = Vec::new();
    let mut resources = Vec::new();
    for &(key, value) in entries {
        let kind = reader.key(key).to_string();
        let (read, directories): (Reader, _) = match kind.as_str() {
            "sources" => (read_source, &mut sources),
            "resources" => (read_resources, &mut resources),
            other => {
                let other = other.to_string();
                reader.error(
                    key,
                    format!("plugin `{id}` declares `generated.{other}`, and krusty-toolchain does not implement it"),
                );
                continue;
            }
        };
        let Value::Sequence(items) = reader.value(value) else {
            reader.error(
                value,
                format!(
                    "krusty-toolchain reads `generated.{kind}` of plugin `{id}` only as a list"
                ),
            );
            continue;
        };
        directories.extend(
            items
                .iter()
                .filter_map(|&item| read(id, reader, item, tasks)),
        );
    }
    (sources, resources)
}

/// How one item of a `generated` list is read.
type Reader = fn(&str, &mut FileReader<'_>, NodeId, &Tasks<'_>) -> Option<GeneratedDirectory>;

/// One `generated.resources` item: its `directory`.
fn read_resources(
    id: &str,
    reader: &mut FileReader<'_>,
    item: NodeId,
    tasks: &Tasks<'_>,
) -> Option<GeneratedDirectory> {
    let Value::Mapping(properties) = reader.value(item) else {
        reader.error(
            item,
            format!("krusty-toolchain reads a `generated.resources` item of plugin `{id}` only as a mapping"),
        );
        return None;
    };
    let mut directory = None;
    let mut fine = true;
    for &(key, value) in properties {
        match reader.key(key) {
            "directory" => directory = Some(value),
            other => {
                fine = false;
                let other = other.to_string();
                reader.error(
                    key,
                    format!("plugin `{id}` sets `{other}` on generated resources, and krusty-toolchain does not implement it"),
                );
            }
        }
    }
    let Some(directory) = directory else {
        reader.error(
            item,
            format!("generated resources of plugin `{id}` name no `directory`"),
        );
        return None;
    };
    let span = reader.span(directory);
    let (below_module, task) = directory_of(id, reader, directory, tasks)?;
    fine.then_some(GeneratedDirectory {
        below_module,
        task,
        span,
    })
}

/// One `generated.sources` item: `language: kotlin` and its `directory`.
fn read_source(
    id: &str,
    reader: &mut FileReader<'_>,
    item: NodeId,
    tasks: &Tasks<'_>,
) -> Option<GeneratedDirectory> {
    let Value::Mapping(properties) = reader.value(item) else {
        reader.error(
            item,
            format!("krusty-toolchain reads a `generated.sources` item of plugin `{id}` only as a mapping"),
        );
        return None;
    };
    let mut kotlin = false;
    let mut directory = None;
    let mut fine = true;
    for &(key, value) in properties {
        match reader.key(key) {
            "language" => match reader.value(value) {
                Value::Scalar("kotlin") => kotlin = true,
                _ => {
                    fine = false;
                    reader.error(
                        value,
                        format!("plugin `{id}` generates sources other than Kotlin sources, and krusty-toolchain compiles Kotlin sources only"),
                    );
                }
            },
            "directory" => directory = Some(value),
            other => {
                fine = false;
                let other = other.to_string();
                reader.error(
                    key,
                    format!("plugin `{id}` sets `{other}` on generated sources, and krusty-toolchain does not implement it"),
                );
            }
        }
    }
    if !kotlin && fine {
        reader.error(
            item,
            format!("krusty-toolchain reads generated sources of plugin `{id}` only with `language: kotlin`"),
        );
        return None;
    }
    let Some(directory) = directory else {
        reader.error(
            item,
            format!("generated sources of plugin `{id}` name no `directory`"),
        );
        return None;
    };
    let span = reader.span(directory);
    let (below_module, task) = directory_of(id, reader, directory, tasks)?;
    fine.then_some(GeneratedDirectory {
        below_module,
        task,
        span,
    })
}

/// The directory `node` names, below the module's directory, and the task whose argument it is,
/// when it is one.
fn directory_of(
    id: &str,
    reader: &mut FileReader<'_>,
    node: NodeId,
    tasks: &Tasks<'_>,
) -> Option<(Vec<String>, Option<String>)> {
    let Value::Scalar(text) = reader.value(node) else {
        reader.error(
            node,
            format!("krusty-toolchain reads a directory of plugin `{id}` only as a string"),
        );
        return None;
    };
    let Some(reference) = text
        .strip_prefix("${tasks.")
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return match below_module(text) {
            // The toolchain takes a directory written out as the output of the task whose action
            // argument is the same directory, and rejects it when no task's is (`super`).
            Some(below) => {
                let task = tasks.arguments.iter().find_map(|&(task, _, argument)| {
                    match reader.value(argument) {
                        Value::Scalar(given) if below_module(given).as_ref() == Some(&below) => {
                            Some(task.to_string())
                        }
                        _ => None,
                    }
                });
                Some((below, task))
            }
            None => {
                reader.error(
                    node,
                    format!("krusty-toolchain reads a directory of plugin `{id}` only as a task action's argument that is `{MODULE_ROOT}` or a path below it"),
                );
                None
            }
        };
    };
    let found = reference
        .split_once(".action.")
        .and_then(|(task, argument)| {
            tasks
                .arguments
                .iter()
                .find(|(name, given, _)| *name == task && *given == argument)
        });
    let Some(&(task, _, argument)) = found else {
        reader.error(
            node,
            format!("plugin `{id}` refers to `{text}`, which is not an argument of one of its task actions"),
        );
        return None;
    };
    let argument_text = match reader.value(argument) {
        Value::Scalar(text) => text,
        _ => "",
    };
    let Some(below) = below_module(argument_text) else {
        reader.error(
            argument,
            format!("krusty-toolchain reads a directory of plugin `{id}` only as `{MODULE_ROOT}` or a path below it"),
        );
        return None;
    };
    Some((below, Some(task.to_string())))
}

/// The segments of `text` below `${module.rootDir}`, when it is that directory or a plain path
/// below it.
fn below_module(text: &str) -> Option<Vec<String>> {
    let rest = text.strip_prefix(MODULE_ROOT)?;
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let segments: Vec<String> = rest
        .strip_prefix('/')?
        .split('/')
        .map(str::to_string)
        .collect();
    segments
        .iter()
        .all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && !segment.contains(['$', '\\'])
        })
        .then_some(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_is_the_module_root_or_a_plain_path_below_it() {
        assert_eq!(below_module("${module.rootDir}"), Some(Vec::new()));
        assert_eq!(
            below_module("${module.rootDir}/gen/kotlin"),
            Some(vec!["gen".to_string(), "kotlin".to_string()])
        );
        for refused in [
            "gen/kotlin",
            "${module.rootDir}gen",
            "${module.rootDir}/",
            "${module.rootDir}/a//b",
            "${module.rootDir}/../b",
            "${module.rootDir}/./b",
            "${module.rootDir}/${pluginSettings.dir}",
            "/abs/gen",
        ] {
            assert_eq!(below_module(refused), None, "{refused}");
        }
    }
}
