//! Native reimplementation of kotlinc's no-arg compiler plugin (`org.jetbrains.kotlin.noarg`).
//!
//! kotlinc's plugin generates a zero-argument constructor for every class it matches, so a
//! framework (JPA above all) can instantiate the class reflectively. The constructor delegates to
//! the superclass's zero-argument constructor and is hidden from Kotlin callers by
//! `@Deprecated(level = HIDDEN)`; `@java.lang.Deprecated` keeps it callable from Java. A class
//! matches as it does for all-open: through one of the configured annotations, a meta-annotation
//! at any depth, or a supertype. The frontend owns that matching, the plugin's diagnostics and the
//! decision to generate (see `resolve::plugin_noarg`); this module owns only the plugin's
//! configuration: the `annotation`, `preset` and `invokeInitializers` options, and the `jpa`
//! preset of kotlinc's `NoArgPluginNames.SUPPORTED_PRESETS`.

use crate::plugins::cli::PluginOption;
use crate::plugins::IrPlugin;
use crate::types::TypeName;

/// kotlinc's `NoArgPluginNames.SUPPORTED_PRESETS`, as qualified names.
const PRESETS: &[(&str, &[&str])] = &[(
    "jpa",
    &[
        "javax.persistence.Entity",
        "javax.persistence.Embeddable",
        "javax.persistence.MappedSuperclass",
        "jakarta.persistence.Entity",
        "jakarta.persistence.Embeddable",
        "jakarta.persistence.MappedSuperclass",
    ],
)];

/// The no-arg plugin, configured for one compilation.
pub struct NoArgPlugin {
    annotations: Vec<TypeName>,
}

impl NoArgPlugin {
    /// Configure from the options the compilation gives no-arg, in either syntax (see
    /// [`crate::plugins::cli::PluginConfig::options_for`]): every `annotation=<fqname>`, then the annotations of every `preset=<name>`. Like
    /// kotlinc, an unknown preset name contributes nothing. `invokeInitializers=true` is refused
    /// by the registry before a plugin is built, so the constructor never runs initializers.
    pub fn from_options(options: &[PluginOption]) -> NoArgPlugin {
        let mut qualified = Vec::new();
        let mut presets = Vec::new();
        for option in options {
            match option.key.as_str() {
                "annotation" => qualified.push(option.value.as_str()),
                "preset" => presets.push(option.value.as_str()),
                _ => {}
            }
        }
        for preset in presets {
            if let Some((_, names)) = PRESETS.iter().find(|(name, _)| *name == preset) {
                qualified.extend(names.iter().copied());
            }
        }
        NoArgPlugin {
            annotations: qualified
                .into_iter()
                .flat_map(crate::plugins::allopen::classifier_identities)
                .collect(),
        }
    }
}

impl IrPlugin for NoArgPlugin {
    fn name(&self) -> &str {
        "noarg"
    }

    fn no_arg_constructor_annotations(&self) -> &[TypeName] {
        &self.annotations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::cli::NOARG_PLUGIN_ID;

    fn option(key: &str, value: &str) -> PluginOption {
        PluginOption {
            id: NOARG_PLUGIN_ID.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn annotations_and_the_jpa_preset_configure_the_matched_annotations() {
        let plugin = NoArgPlugin::from_options(&[
            option("annotation", "NoArg"),
            option("preset", "jpa"),
            option("preset", "spring"),
        ]);
        let annotations = plugin.no_arg_constructor_annotations();
        assert!(annotations.contains(&crate::types::type_name("NoArg")));
        assert!(annotations.contains(&crate::types::type_name("jakarta/persistence/Entity")));
        assert!(annotations.contains(&crate::types::type_name(
            "javax/persistence/MappedSuperclass"
        )));
        assert!(!annotations.contains(&crate::types::type_name(
            "org/springframework/stereotype/Component"
        )));
    }
}
