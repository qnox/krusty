//! Native reimplementation of kotlinc's all-open compiler plugin (`org.jetbrains.kotlin.allopen`).
//!
//! kotlinc's plugin is a FIR status transformer: a class it matches, and every member declared in
//! such a class, takes `open` as its *default* modality. An explicit `final`, `abstract` or `sealed`
//! still wins. A class matches when it carries one of the configured annotations, when one of its
//! annotations is meta-annotated with one at any depth, or when one of its supertypes matches
//! (kotlinc's `AbstractSimpleClassPredicateMatchingService`). The frontend owns that matching and
//! the rewrite of declaration status (see `resolve::plugin_status`); this crate owns only the
//! plugin's configuration: the `annotation` and `preset` options, and the preset annotation lists of
//! kotlinc's `AllOpenPluginNames.SUPPORTED_PRESETS`.

use krusty_plugin_api::{
    annotations_from_options, Convention, ConventionPlugin, PluginOption, Presets, TypeName,
};

/// kotlinc's plugin id for all-open.
pub const PLUGIN_ID: &str = "org.jetbrains.kotlin.allopen";

/// The all-open plugin.
pub static PLUGIN: ConventionPlugin = ConventionPlugin {
    plugin_id: PLUGIN_ID,
    name: "allopen",
    convention: Convention::OpenByDefault,
    annotations,
};

/// kotlinc's `AllOpenPluginNames.SUPPORTED_PRESETS`, as qualified names. The `jpa` preset is the
/// no-arg plugin's and is recognized by all-open's preset table too.
const PRESETS: Presets = &[
    (
        "spring",
        &[
            "org.springframework.stereotype.Component",
            "org.springframework.transaction.annotation.Transactional",
            "org.springframework.scheduling.annotation.Async",
            "org.springframework.cache.annotation.Cacheable",
            "org.springframework.boot.test.context.SpringBootTest",
            "org.springframework.validation.annotation.Validated",
        ],
    ),
    (
        "quarkus",
        &[
            "javax.enterprise.context.ApplicationScoped",
            "javax.enterprise.context.RequestScoped",
        ],
    ),
    (
        "micronaut",
        &[
            "io.micronaut.aop.Around",
            "io.micronaut.aop.Introduction",
            "io.micronaut.aop.InterceptorBinding",
            "io.micronaut.aop.InterceptorBindingDefinitions",
        ],
    ),
    (
        "jpa",
        &[
            "javax.persistence.Entity",
            "javax.persistence.Embeddable",
            "javax.persistence.MappedSuperclass",
            "jakarta.persistence.Entity",
            "jakarta.persistence.Embeddable",
            "jakarta.persistence.MappedSuperclass",
        ],
    ),
];

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
    fn annotations_and_presets_configure_the_matched_annotations() {
        let annotations = (PLUGIN.annotations)(&[
            option("annotation", "AllOpen"),
            option("preset", "spring"),
            option("preset", "no-such-preset"),
        ]);
        assert!(annotations.contains(&name("AllOpen")));
        assert!(annotations.contains(&name("org/springframework/stereotype/Component")));
        assert!(annotations.contains(&name("org/springframework/validation/annotation/Validated")));
    }
}
