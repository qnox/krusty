//! Native reimplementation of kotlinc's assignment compiler plugin
//! (`org.jetbrains.kotlin.assignment`).
//!
//! kotlinc's plugin rewrites an assignment `target.p = v` to the call `target.p.assign(v)` when
//! `p` is a `val` property (or a final Java field) and the class of its type, or one of that
//! class's supertypes, carries one of the configured annotations. Gradle's `Property<T>`
//! (`@SupportsKotlinAssignmentOverloading`) is the best-known user. The frontend applies the
//! rewrite where it checks the assignment (see `resolve::assignment_targets`); this crate owns
//! only the plugin's configuration: the `annotation` option, the only one its command-line
//! processor registers.

use krusty_plugin_api::{
    annotations_from_options, Convention, ConventionPlugin, PluginOption, TypeName,
};

/// kotlinc's plugin id for assignment.
pub const PLUGIN_ID: &str = "org.jetbrains.kotlin.assignment";

/// The assignment plugin.
pub static PLUGIN: ConventionPlugin = ConventionPlugin {
    plugin_id: PLUGIN_ID,
    name: "assignment",
    convention: Convention::Assignment,
    annotations,
};

/// Every `annotation=<fqname>` the compilation gives the assignment plugin, in either option
/// syntax.
fn annotations(options: &[PluginOption]) -> Vec<TypeName> {
    annotations_from_options(options, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_configure_the_rewrite() {
        let annotations = (PLUGIN.annotations)(&[PluginOption {
            id: PLUGIN_ID.to_string(),
            key: "annotation".to_string(),
            value: "test.Assignable".to_string(),
        }]);
        assert_eq!(
            annotations,
            [
                TypeName::from("test").nested_child("Assignable"),
                TypeName::from("test/Assignable"),
            ]
        );
    }
}
