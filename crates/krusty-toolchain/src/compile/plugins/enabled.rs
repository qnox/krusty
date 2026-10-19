//! The toolchain plugins a module enables (`plugins:`), read from the one file of the module that
//! writes them.

use std::path::{Path, PathBuf};

use crate::configuration::Configuration;
use crate::diagnostic::{Diagnostic, Diagnostics};
use crate::reading::{read_document, FileReader, Value};
use crate::yaml::Span;

/// A plugin a module enables, and where.
pub(super) struct Enabled {
    pub id: String,
    pub file: PathBuf,
    pub span: Span,
}

impl Enabled {
    /// A refusal placed where the plugin is enabled.
    pub fn refusal(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::error(&self.file, Some(self.span), message)
    }
}

/// The plugins `module` enables, in the order they are written, or why krusty-toolchain does not
/// read them: an entry other than `<id>: enabled` or `<id>: {enabled: <boolean>}`, or `plugins`
/// written in more than one of the module's files (the module's and its templates', which the
/// toolchain merges). A plugin written `enabled: false` is not enabled.
pub(super) fn enabled(
    module: &str,
    configuration: &Configuration,
) -> Result<Vec<Enabled>, Vec<Diagnostic>> {
    let files = &configuration.files.files;
    let mut written: Vec<(PathBuf, Vec<Enabled>)> = Vec::new();
    let mut refused = Vec::new();
    for file in files.ids() {
        let path = files.path(file);
        let mut diagnostics = Diagnostics::default();
        let Some(document) = read_document(path, &mut diagnostics) else {
            refused.extend(diagnostics.iter().cloned());
            continue;
        };
        let mut reader = FileReader::new(path, &document, &mut diagnostics);
        let Some(Value::Mapping(pairs)) = document.root().map(|root| reader.shape(root)) else {
            continue;
        };
        let Some(&(_, plugins)) = pairs.iter().find(|&&(key, _)| reader.key(key) == "plugins")
        else {
            continue;
        };
        let read = entries(module, path, &mut reader, plugins);
        refused.extend(diagnostics.iter().cloned());
        written.push((path.to_path_buf(), read));
    }
    if written.len() > 1 {
        let shown: Vec<String> = written
            .iter()
            .map(|(path, _)| format!("`{}`", path.display()))
            .collect();
        refused.push(Diagnostic::error(
            &written[0].0,
            None,
            format!(
                "module `{module}` writes `plugins` in more than one file ({}), and krusty-toolchain reads them from one file only",
                shown.join(", ")
            ),
        ));
    }
    if !refused.is_empty() {
        return Err(refused);
    }
    Ok(written
        .into_iter()
        .next()
        .map(|(_, enabled)| enabled)
        .unwrap_or_default())
}

/// The enabled entries of a `plugins:` mapping; what is not read is reported to `reader`.
fn entries(
    module: &str,
    path: &Path,
    reader: &mut FileReader<'_>,
    plugins: crate::yaml::NodeId,
) -> Vec<Enabled> {
    let pairs = match reader.value(plugins) {
        Value::Missing | Value::Null => return Vec::new(),
        Value::Mapping(pairs) => pairs,
        _ => {
            reader.error(
                plugins,
                format!("krusty-toolchain reads `plugins` of module `{module}` only as a mapping of plugin names"),
            );
            return Vec::new();
        }
    };
    let mut enabled = Vec::new();
    for &(key, value) in pairs {
        let id = reader.key(key).to_string();
        let on = match reader.value(value) {
            Value::Scalar("enabled") => Some(true),
            Value::Mapping(settings) => match settings {
                [(setting, flag)] if reader.key(*setting) == "enabled" => {
                    match reader.value(*flag) {
                        Value::Scalar("true") => Some(true),
                        Value::Scalar("false") => Some(false),
                        _ => None,
                    }
                }
                _ => None,
            },
            _ => None,
        };
        match on {
            Some(true) => enabled.push(Enabled {
                id,
                file: path.to_path_buf(),
                span: reader.span(key),
            }),
            Some(false) => {}
            None => reader.error(
                value,
                format!(
                    "krusty-toolchain reads plugin `{id}` of module `{module}` only as `enabled` or `enabled: <boolean>`, without plugin settings"
                ),
            ),
        }
    }
    enabled
}
