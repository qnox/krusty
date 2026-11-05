//! Native reimplementation of kotlinc's assignment compiler plugin
//! (`org.jetbrains.kotlin.assignment`).
//!
//! kotlinc's plugin rewrites an assignment `target.p = v` to the call `target.p.assign(v)` when
//! `p` is a `val` property (or a final Java field) and the class of its type, or one of that
//! class's supertypes, carries one of the configured annotations. Gradle's `Property<T>`
//! (`@SupportsKotlinAssignmentOverloading`) is the best-known user. The frontend applies the
//! rewrite where it checks the assignment (see `resolve::assignment_targets`); this module owns
//! only the plugin's configuration: the `annotation` option, the only one its command-line
//! processor registers.

use crate::plugins::cli::PluginOption;
use crate::plugins::IrPlugin;
use crate::types::TypeName;

/// The assignment plugin, configured for one compilation.
pub struct AssignmentPlugin {
    annotations: Vec<TypeName>,
}

impl AssignmentPlugin {
    /// Configure from the options the compilation gives the assignment plugin, in either syntax
    /// (see [`crate::plugins::cli::PluginConfig::options_for`]): every `annotation=<fqname>`.
    pub fn from_options(options: &[PluginOption]) -> AssignmentPlugin {
        AssignmentPlugin {
            annotations: options
                .iter()
                .filter(|option| option.key == "annotation")
                .flat_map(|option| crate::plugins::allopen::classifier_identities(&option.value))
                .collect(),
        }
    }
}

impl IrPlugin for AssignmentPlugin {
    fn name(&self) -> &str {
        "assignment"
    }

    fn assignment_annotations(&self) -> &[TypeName] {
        &self.annotations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::cli::ASSIGNMENT_PLUGIN_ID;

    #[test]
    fn annotations_configure_the_rewrite() {
        let plugin = AssignmentPlugin::from_options(&[PluginOption {
            id: ASSIGNMENT_PLUGIN_ID.to_string(),
            key: "annotation".to_string(),
            value: "test.Assignable".to_string(),
        }]);
        assert_eq!(
            plugin.assignment_annotations(),
            [
                crate::types::type_name("test").nested_child("Assignable"),
                crate::types::type_name("test/Assignable"),
            ]
        );
    }
}
