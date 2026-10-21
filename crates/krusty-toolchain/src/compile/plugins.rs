//! The toolchain plugins a module enables, and the source and resource directories they add to its
//! main fragment.
//!
//! A plugin is a local plugin module the project lists (`plugins:` of `project.yaml`), known by
//! its module's name. krusty-toolchain implements the plugins whose `plugin.yaml` only adds Kotlin
//! source directories and resource directories of the module (`declaration`); it runs none of their
//! tasks, so a directory a task provides must already be there.

mod declaration;
mod enabled;

use std::path::{Path, PathBuf};

use crate::configuration::Configuration;
use crate::diagnostic::Diagnostic;
use crate::reading::{read_document, FileReader, Value};

/// The file a plugin module declares what it adds to a module in.
const PLUGIN_FILE: &str = "plugin.yaml";

/// The directories the plugins a module enables add to its main fragment, each in the order the
/// plugins are enabled and declare them.
#[derive(Default)]
pub(super) struct Generated {
    pub sources: Vec<PathBuf>,
    pub resources: Vec<PathBuf>,
}

/// The directories the plugins `module` enables add to its main fragment, or why krusty-toolchain
/// does not implement them. `module_directory` is the module's directory, `plugins` the module
/// files of the project's plugins.
pub(super) fn generated(
    module: &str,
    module_directory: &Path,
    configuration: &Configuration,
    plugins: &[PathBuf],
) -> Result<Generated, Vec<Diagnostic>> {
    let enabled = enabled::enabled(module, configuration)?;
    let mut generated = Generated::default();
    let mut refused = Vec::new();
    for plugin in &enabled {
        let Some(plugin_module) = plugins.iter().find(|file| {
            file.parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == plugin.id.as_str())
        }) else {
            refused.push(plugin.refusal(format!(
                "module `{module}` enables toolchain plugin `{}`, which the project does not list in its `plugins`",
                plugin.id
            )));
            continue;
        };
        if let Some(diagnostic) = plugin_info(&plugin.id, plugin_module) {
            refused.push(diagnostic);
            continue;
        }
        let plugin_file = plugin_module.with_file_name(PLUGIN_FILE);
        if !plugin_file.is_file() {
            // The toolchain warns where the plugin module is read that it has no effect.
            continue;
        }
        let declaration = match declaration::read(&plugin.id, &plugin_file) {
            Ok(declaration) => declaration,
            Err(diagnostics) => {
                refused.extend(diagnostics);
                continue;
            }
        };
        let sources = declaration.sources.iter().map(|source| (source, false));
        let resources = declaration.resources.iter().map(|source| (source, true));
        for (source, is_resources) in sources.chain(resources) {
            let directory = source
                .below_module
                .iter()
                .fold(module_directory.to_path_buf(), |path, segment| {
                    path.join(segment)
                });
            let Some(task) = &source.task else {
                // The toolchain's own problem, word for word.
                refused.push(Diagnostic::error(
                    &declaration.file,
                    declaration.document,
                    format!(
                        "The `directory` path `{}` in must be an `@Output` of some task registered in this plugin",
                        directory.display()
                    ),
                ));
                continue;
            };
            if !directory.is_dir() {
                refused.push(Diagnostic::error(
                    &declaration.file,
                    Some(source.span),
                    format!(
                        "module `{module}` needs the directory `{}` that task `{task}` of toolchain plugin `{}` provides, and krusty-toolchain does not run plugin tasks",
                        directory.display(),
                        plugin.id
                    ),
                ));
                continue;
            }
            if is_resources {
                generated.resources.push(directory);
            } else {
                generated.sources.push(directory);
            }
        }
    }
    if refused.is_empty() {
        Ok(generated)
    } else {
        Err(refused)
    }
}

/// A refusal of `pluginInfo` in the plugin's module file, which krusty-toolchain does not read:
/// it can give the plugin another name, settings and a description.
fn plugin_info(id: &str, plugin_module: &Path) -> Option<Diagnostic> {
    let mut diagnostics = crate::diagnostic::Diagnostics::default();
    let document = read_document(plugin_module, &mut diagnostics)?;
    let reader = FileReader::new(plugin_module, &document, &mut diagnostics);
    let Some(Value::Mapping(pairs)) = document.root().map(|root| reader.shape(root)) else {
        return None;
    };
    let &(key, _) = pairs
        .iter()
        .find(|&&(key, _)| reader.key(key) == "pluginInfo")?;
    Some(Diagnostic::error(
        plugin_module,
        Some(reader.span(key)),
        format!("toolchain plugin `{id}` sets `pluginInfo`, and krusty-toolchain does not read it"),
    ))
}
