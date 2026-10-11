//! Native reimplementation of kotlinc's sam-with-receiver compiler plugin
//! (`org.jetbrains.kotlin.samWithReceiver`).
//!
//! kotlinc's plugin changes how a lambda converts to a functional interface: when the interface
//! that declares the abstract method carries one of the configured annotations, the method's first
//! value parameter becomes the converted function type's receiver, so the lambda reads it as
//! `this` rather than `it`. Gradle's `Action<T>` (`@HasImplicitReceiver`) is the best-known user.
//! The frontend applies the convention where it derives a SAM conversion's function type (see
//! `symbol_resolver::semantic_sam_signature`); this module owns only the plugin's configuration:
//! the `annotation` option. Although kotlinc's implementation contains an unused preset option,
//! its command-line processor does not register that option, so requesting it is an error.

use crate::plugins::cli::PluginOption;
use crate::plugins::IrPlugin;
use crate::types::TypeName;

/// The sam-with-receiver plugin, configured for one compilation.
pub struct SamWithReceiverPlugin {
    annotations: Vec<TypeName>,
}

impl SamWithReceiverPlugin {
    /// Configure from the options the compilation gives sam-with-receiver, in either syntax (see
    /// [`crate::plugins::cli::PluginConfig::options_for`]): every `annotation=<fqname>`.
    pub fn from_options(options: &[PluginOption]) -> SamWithReceiverPlugin {
        SamWithReceiverPlugin {
            annotations: options
                .iter()
                .filter(|option| option.key == "annotation")
                .flat_map(|option| crate::plugins::allopen::classifier_identities(&option.value))
                .collect(),
        }
    }
}

impl IrPlugin for SamWithReceiverPlugin {
    fn name(&self) -> &str {
        "sam-with-receiver"
    }

    fn sam_with_receiver_annotations(&self) -> &[TypeName] {
        &self.annotations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::cli::SAM_WITH_RECEIVER_PLUGIN_ID;

    fn option(key: &str, value: &str) -> PluginOption {
        PluginOption {
            id: SAM_WITH_RECEIVER_PLUGIN_ID.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn annotations_configure_the_convention() {
        let plugin =
            SamWithReceiverPlugin::from_options(&[option("annotation", "test.WithReceiver")]);
        let annotations = plugin.sam_with_receiver_annotations();
        assert_eq!(annotations, [crate::types::type_name("test/WithReceiver")]);
    }
}
