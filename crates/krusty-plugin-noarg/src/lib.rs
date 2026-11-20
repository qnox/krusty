//! Native reimplementation of kotlinc's no-arg compiler plugin (`org.jetbrains.kotlin.noarg`).
//!
//! kotlinc's plugin generates a zero-argument constructor for every class it matches, so a
//! framework (JPA above all) can instantiate the class reflectively. The constructor delegates to
//! the superclass's zero-argument constructor and is hidden from Kotlin callers by
//! `@Deprecated(level = HIDDEN)`; `@java.lang.Deprecated` keeps it callable from Java. A class
//! matches as it does for all-open: through one of the configured annotations, a meta-annotation
//! at any depth, or a supertype. The frontend owns that matching, the plugin's diagnostics and the
//! decision to generate (see `resolve::plugin_noarg`); this crate owns only the plugin's
//! configuration: the `annotation`, `preset` and `invokeInitializers` options, and the `jpa`
//! preset of kotlinc's `NoArgPluginNames.SUPPORTED_PRESETS`. `invokeInitializers=true` is refused
//! by the registry before a plugin is built, so the constructor never runs initializers.

use krusty_plugin_api::{
    annotations_from_options, Convention, ConventionPlugin, PluginOption, Presets, TypeName,
};

/// kotlinc's plugin id for no-arg.
pub const PLUGIN_ID: &str = "org.jetbrains.kotlin.noarg";

/// The no-arg plugin.
pub static PLUGIN: ConventionPlugin = ConventionPlugin {
    plugin_id: PLUGIN_ID,
    name: "noarg",
    convention: Convention::NoArgConstructor,
    annotations,
};

/// kotlinc's `NoArgPluginNames.SUPPORTED_PRESETS`, as qualified names.
const PRESETS: Presets = &[(
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

fn annotations(options: &[PluginOption]) -> Vec<TypeName> {
    annotations_from_options(options, PRESETS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(key: &str, value: &str) -> PluginOption {
        PluginOption {
            id: PLUGIN_ID.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    fn name(internal: &str) -> TypeName {
        TypeName::from(internal)
    }

    #[test]
    fn annotations_and_the_jpa_preset_configure_the_matched_annotations() {
        let annotations = (PLUGIN.annotations)(&[
            option("annotation", "NoArg"),
            option("preset", "jpa"),
            option("preset", "spring"),
        ]);
        assert!(annotations.contains(&name("NoArg")));
        assert!(annotations.contains(&name("jakarta/persistence/Entity")));
        assert!(annotations.contains(&name("javax/persistence/MappedSuperclass")));
        assert!(!annotations.contains(&name("org/springframework/stereotype/Component")));
    }
}
