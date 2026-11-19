//! Native reimplementation of kotlinc's sam-with-receiver compiler plugin
//! (`org.jetbrains.kotlin.samWithReceiver`).
//!
//! kotlinc's plugin changes how a lambda converts to a functional interface: when the interface
//! that declares the abstract method carries one of the configured annotations, the method's first
//! value parameter becomes the converted function type's receiver, so the lambda reads it as
//! `this` rather than `it`. Gradle's `Action<T>` (`@HasImplicitReceiver`) is the best-known user.
//! The frontend applies the convention where it derives a SAM conversion's function type (see
//! `symbol_resolver::semantic_sam_signature`); this crate owns only the plugin's configuration:
//! the `annotation` option. Although kotlinc's implementation contains an unused preset option,
//! its command-line processor does not register that option, so requesting it is an error.

use krusty_plugin_api::{
    annotations_from_options, Convention, ConventionPlugin, PluginOption, TypeName,
};

/// kotlinc's plugin id for sam-with-receiver.
pub const PLUGIN_ID: &str = "org.jetbrains.kotlin.samWithReceiver";

/// The sam-with-receiver plugin.
pub static PLUGIN: ConventionPlugin = ConventionPlugin {
    plugin_id: PLUGIN_ID,
    name: "sam-with-receiver",
    convention: Convention::SamWithReceiver,
    annotations,
};

/// Every `annotation=<fqname>` the compilation gives sam-with-receiver, in either option syntax.
fn annotations(options: &[PluginOption]) -> Vec<TypeName> {
    annotations_from_options(options, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_configure_the_convention() {
        let annotations = (PLUGIN.annotations)(&[PluginOption {
            id: PLUGIN_ID.to_string(),
            key: "annotation".to_string(),
            value: "test.WithReceiver".to_string(),
        }]);
        assert_eq!(
            annotations,
            [
                TypeName::from("test").nested_child("WithReceiver"),
                TypeName::from("test/WithReceiver"),
            ]
        );
    }
}
