//! Native reimplementation of kotlinc's all-open compiler plugin (`org.jetbrains.kotlin.allopen`).
//!
//! kotlinc's plugin is a FIR status transformer: a class it matches, and every member declared in
//! such a class, takes `open` as its *default* modality. An explicit `final`, `abstract` or `sealed`
//! still wins. A class matches when it carries one of the configured annotations, when one of its
//! annotations is meta-annotated with one at any depth, or when one of its supertypes matches
//! (kotlinc's `AbstractSimpleClassPredicateMatchingService`). The frontend owns that matching and
//! the rewrite of declaration status (see `resolve::plugin_status`); this module owns only the
//! plugin's configuration: the `annotation` and `preset` options, and the preset annotation lists of
//! kotlinc's `AllOpenPluginNames.SUPPORTED_PRESETS`.

use crate::plugins::cli::PluginOption;
use crate::plugins::IrPlugin;
use crate::types::TypeName;

/// kotlinc's `AllOpenPluginNames.SUPPORTED_PRESETS`, as qualified names. The `jpa` preset is the
/// no-arg plugin's and is recognized by all-open's preset table too.
const PRESETS: &[(&str, &[&str])] = &[
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

/// The all-open plugin, configured for one compilation.
pub struct AllOpenPlugin {
    annotations: Vec<TypeName>,
}

impl AllOpenPlugin {
    /// Configure from the options the compilation gives all-open, in either syntax (see
    /// [`crate::plugins::cli::PluginConfig::options_for`]): every `annotation=<fqname>`, then the
    /// annotations of every `preset=<name>`. Like kotlinc, an unknown preset name contributes
    /// nothing.
    pub fn from_options(options: &[PluginOption]) -> AllOpenPlugin {
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
        AllOpenPlugin {
            annotations: qualified
                .into_iter()
                .flat_map(classifier_identities)
                .collect(),
        }
    }
}

impl IrPlugin for AllOpenPlugin {
    fn name(&self) -> &str {
        "allopen"
    }

    fn open_by_default_annotations(&self) -> &[TypeName] {
        &self.annotations
    }
}

/// The classifier identities a qualified name written as `a.b.C.D` can denote. The written form
/// does not say where the package ends, and kotlinc compares it with each annotation's qualified
/// name, so every package/classifier split is a candidate (`a/b/C/D`, `a/b/C$D`, …). A candidate
/// no declaration has matches nothing. kotlinc's annotation-driven plugins (no-arg too) share this
/// reading of their `annotation` option.
pub(crate) fn classifier_identities(qualified: &str) -> Vec<TypeName> {
    let segments = qualified.split('.').collect::<Vec<_>>();
    (1..=segments.len())
        .map(|package_len| {
            let (package, classifiers) = segments.split_at(package_len - 1);
            let mut path = package.join("/");
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(classifiers[0]);
            classifiers[1..]
                .iter()
                .fold(crate::types::type_name(&path), |owner, nested| {
                    owner.nested_child(nested)
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::cli::ALLOPEN_PLUGIN_ID;

    fn option(key: &str, value: &str) -> PluginOption {
        PluginOption {
            id: ALLOPEN_PLUGIN_ID.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn annotations_and_presets_configure_the_matched_annotations() {
        let plugin = AllOpenPlugin::from_options(&[
            option("annotation", "AllOpen"),
            option("preset", "spring"),
            option("preset", "no-such-preset"),
        ]);
        let annotations = plugin.open_by_default_annotations();
        assert!(annotations.contains(&crate::types::type_name("AllOpen")));
        assert!(annotations.contains(&crate::types::type_name(
            "org/springframework/stereotype/Component"
        )));
        assert!(annotations.contains(&crate::types::type_name(
            "org/springframework/validation/annotation/Validated"
        )));
    }

    #[test]
    fn a_qualified_name_denotes_every_package_and_nested_classifier_split() {
        assert_eq!(
            classifier_identities("a.B.C"),
            vec![
                crate::types::type_name("a")
                    .nested_child("B")
                    .nested_child("C"),
                crate::types::type_name("a/B").nested_child("C"),
                crate::types::type_name("a/B/C"),
            ]
        );
    }
}
