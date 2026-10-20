//! What a module's configuration asks of its compilation that krusty-toolchain does not implement:
//! every compiler setting the toolchain turns into compiler arguments or compiler plugins other
//! than the ones krusty-toolchain passes. Each is refused where it is
//! set, naming the module, rather than compiled without it.

use crate::configuration::Configuration;
use crate::diagnostic::Diagnostic;
use crate::tree::{Node, Trace, Value};

/// A diagnostic at the place `node` is set: its file and position, or its file alone for a value
/// derived from one written there, or the project for a schema default.
pub(super) fn at_setting(
    configuration: &Configuration,
    node: &Node,
    message: impl Into<String>,
) -> Diagnostic {
    let files = &configuration.files.files;
    match &node.trace {
        Trace::File { file, span } => Diagnostic::error(files.path(*file), Some(*span), message),
        trace => match trace.file() {
            Some(file) => Diagnostic::error(files.path(file), None, message),
            None => Diagnostic::project_error(message),
        },
    }
}

/// When a setting changes the compilation.
#[derive(Clone, Copy)]
enum Effect {
    /// Any value.
    Set,
    /// A list with items.
    Items,
    /// `true`.
    Enabled,
    /// A value other than this one.
    Other(&'static str),
}

impl Effect {
    fn applies(self, value: &Value) -> bool {
        match (self, value) {
            (_, Value::Error) => false,
            (Effect::Set, value) => !matches!(value, Value::Null),
            (Effect::Items, Value::List(items)) => !items.is_empty(),
            (Effect::Enabled, Value::Boolean(enabled)) => *enabled,
            (Effect::Other(default), Value::Enum(_, value)) => *value != default,
            _ => false,
        }
    }
}

/// The settings below `settings:` that change the compiler's arguments or plugins, which
/// krusty-toolchain does not pass yet.
const COMPILER_SETTINGS: [(&[&str], Effect); 19] = [
    (&["kotlin", "languageVersion"], Effect::Set),
    (&["kotlin", "apiVersion"], Effect::Set),
    (&["kotlin", "freeCompilerArgs"], Effect::Items),
    (&["kotlin", "optIns"], Effect::Items),
    (&["kotlin", "languageFeatures"], Effect::Items),
    (&["kotlin", "allWarningsAsErrors"], Effect::Enabled),
    (&["kotlin", "suppressWarnings"], Effect::Enabled),
    (&["kotlin", "progressiveMode"], Effect::Enabled),
    (&["kotlin", "verbose"], Effect::Enabled),
    (&["kotlin", "explicitApi"], Effect::Other("disable")),
    (&["kotlin", "debug"], Effect::Set),
    (&["kotlin", "compilerPlugins"], Effect::Items),
    (&["kotlin", "noArg", "enabled"], Effect::Enabled),
    (&["kotlin", "allOpen", "enabled"], Effect::Enabled),
    (&["kotlin", "powerAssert", "enabled"], Effect::Enabled),
    (&["kotlin", "dataframe", "enabled"], Effect::Enabled),
    (&["kotlin", "rpc", "enabled"], Effect::Enabled),
    (&["compose", "enabled"], Effect::Enabled),
    (&["lombok", "enabled"], Effect::Enabled),
];

/// Settings the toolchain implements with compiler plugins of its own.
const PLUGIN_SETTINGS: [&[&str]; 1] = [&["springBoot", "enabled"]];

/// The refusals of `module`'s configuration, in the order its settings are listed above. A value
/// from the schema's defaults is never refused.
pub(super) fn refusals(
    module: &str,
    configuration: &Configuration,
    main: &Node,
) -> Vec<Diagnostic> {
    let mut refused = Vec::new();
    let written = |node: &&Node| !node.trace.is_default();
    let Some(settings) = main.get("settings") else {
        return refused;
    };
    for (path, effect) in COMPILER_SETTINGS {
        if let Some(node) = settings
            .at(path)
            .filter(written)
            .filter(|node| effect.applies(&node.value))
        {
            refused.push(at_setting(
                configuration,
                node,
                format!(
                    "module `{module}` sets `settings.{}`, and krusty-toolchain does not implement compiling with it",
                    path.join(".")
                ),
            ));
        }
    }
    for path in PLUGIN_SETTINGS {
        if let Some(node) = settings
            .at(path)
            .filter(written)
            .filter(|node| Effect::Enabled.applies(&node.value))
        {
            refused.push(at_setting(
                configuration,
                node,
                format!(
                    "module `{module}` sets `settings.{}`, which the toolchain compiles with compiler plugins krusty-toolchain does not implement",
                    path.join(".")
                ),
            ));
        }
    }
    refused
}
