//! Choosing the variants of a Gradle module that serve a JVM resolution scope (`resolveVariants`,
//! `resolveBomVariants`): its capability, category, platform, environment and usage, then the
//! tie-breakers the toolchain applies when several remain.

use super::artifact::Scope;
use super::gradle_module::{
    Capability, Variant, BUNDLING, CATEGORY, JVM_ENVIRONMENT, NATIVE_TARGET, PLATFORM_TYPE,
    PLUGIN_API_VERSION, USAGE, WASM_TARGET,
};

/// The artifact whose variants are chosen.
pub struct Requested<'a> {
    pub group: &'a str,
    pub module: &'a str,
    pub version: &'a str,
    pub is_bom: bool,
}

/// The variants that are not documentation or Kotlin metadata.
pub fn without_documentation_and_metadata(variants: &[&Variant]) -> usize {
    variants
        .iter()
        .filter(|variant| !variant.is_documentation_or_metadata())
        .count()
}

impl Requested<'_> {
    fn capability(&self) -> Capability {
        Capability {
            group: self.group.to_string(),
            name: self.module.to_string(),
            version: self.version.to_string(),
        }
    }

    /// No capability, or exactly the requested artifact's, or one of the known libraries that
    /// declare a second capability (`capabilityMatches`).
    fn capability_matches(&self, variant: &Variant) -> bool {
        let own = self.capability();
        variant.capabilities.is_empty()
            || variant.capabilities == [own.clone()]
            || self.kotlin_test_exception(variant, &own)
            || self.group == "com.google.guava"
                && self.module == "guava"
                && variant.capabilities.contains(&own)
                && variant.capabilities.contains(&Capability {
                    group: "com.google.collections".to_string(),
                    name: "google-collections".to_string(),
                    version: self.version.to_string(),
                })
            || self.group == "org.hibernate.orm"
                && variant.capabilities.contains(&own)
                && variant.capabilities.contains(&Capability {
                    group: "org.hibernate".to_string(),
                    name: self.module.to_string(),
                    version: self.version.to_string(),
                })
    }

    fn kotlin_test_exception(&self, variant: &Variant, own: &Capability) -> bool {
        if self.group != "org.jetbrains.kotlin"
            || !matches!(self.module, "kotlin-test-junit" | "kotlin-test-junit5")
        {
            return false;
        }
        let mut capabilities = variant.capabilities.clone();
        capabilities.sort_by(|a, b| a.name.cmp(&b.name));
        capabilities
            == [
                Capability {
                    group: self.group.to_string(),
                    name: "kotlin-test-framework-impl".to_string(),
                    version: self.version.to_string(),
                },
                own.clone(),
            ]
    }

    /// The variants that serve the JVM in `scope` (`resolveVariants`).
    pub fn variants<'v>(&self, variants: &'v [Variant], scope: Scope) -> Vec<&'v Variant> {
        let candidates: Vec<&Variant> = variants
            .iter()
            .filter(|variant| self.capability_matches(variant))
            // A native or Wasm variant for a specific target never serves the JVM.
            .filter(|variant| {
                variant.attribute(PLATFORM_TYPE) != Some("native")
                    || variant.attribute(NATIVE_TARGET).is_none()
            })
            .filter(|variant| {
                variant.attribute(PLATFORM_TYPE) != Some("wasm")
                    || variant.attribute(WASM_TARGET).is_none()
            })
            .filter(|variant| variant.is_bom() == self.is_bom)
            .filter(|variant| matches!(variant.attribute(PLATFORM_TYPE), None | Some("jvm")))
            .filter(|variant| {
                matches!(
                    variant.attribute(JVM_ENVIRONMENT),
                    None | Some("standard-jvm")
                )
            })
            .collect();
        let scoped = with_fallback_scope(&candidates, scope);
        let fewest = by_unused_attributes(scoped);
        let special = self.well_known_special_libraries(fewest);
        by_bundling(special)
    }

    /// A BOM's variants for `scope` (`resolveBomVariants`).
    pub fn bom_variants<'v>(&self, variants: &'v [Variant], scope: Scope) -> Vec<&'v Variant> {
        let candidates: Vec<&Variant> = variants
            .iter()
            .filter(|variant| variant.is_bom() == self.is_bom)
            .collect();
        with_fallback_scope(&candidates, scope)
    }

    /// The Kotlin Gradle plugins publish a variant per Gradle version; the one without a Gradle
    /// version is chosen (`filterWellKnowSpecialLibraries`).
    fn well_known_special_libraries<'v>(&self, variants: Vec<&'v Variant>) -> Vec<&'v Variant> {
        if without_documentation_and_metadata(&variants) <= 1 {
            return variants;
        }
        if self.group == "org.jetbrains.kotlin"
            && matches!(
                self.module,
                "kotlin-gradle-plugin" | "fus-statistics-gradle-plugin"
            )
        {
            let unversioned: Vec<&Variant> = variants
                .iter()
                .copied()
                .filter(|variant| variant.attribute(PLUGIN_API_VERSION).is_none())
                .collect();
            if without_documentation_and_metadata(&unversioned) == 1 {
                return unversioned;
            }
        }
        variants
    }
}

/// Whether a variant serves `scope`: its usage, or a usage no scope owns (Kotlin metadata,
/// documentation).
fn scope_matches(variant: &Variant, scope: Scope) -> bool {
    let usage = variant.attribute(USAGE);
    let scope_agnostic = variant.is_documentation()
        || variant.attribute(PLATFORM_TYPE) == Some("common") && usage == Some("kotlin-metadata");
    let suffix = match scope {
        Scope::Compile => "-api",
        Scope::Runtime => "-runtime",
    };
    usage.is_some_and(|usage| usage.ends_with(suffix)) || scope_agnostic
}

/// The variants for `scope`, else for the other scope when `scope` leaves none on the classpath
/// (`filterWithFallbackScope`).
fn with_fallback_scope<'v>(variants: &[&'v Variant], scope: Scope) -> Vec<&'v Variant> {
    let scoped: Vec<&Variant> = variants
        .iter()
        .copied()
        .filter(|variant| scope_matches(variant, scope))
        .collect();
    if without_documentation_and_metadata(&scoped) > 0 {
        return scoped;
    }
    let fallback: Vec<&Variant> = variants
        .iter()
        .copied()
        .filter(|variant| scope_matches(variant, scope.other()))
        .collect();
    if without_documentation_and_metadata(&fallback) > 0 {
        return fallback;
    }
    scoped
}

/// Among several variants, the one with the fewest attributes resolution does not look at, if that
/// leaves one (`filterMultipleVariantsByUnusedAttributes`).
fn by_unused_attributes(variants: Vec<&Variant>) -> Vec<&Variant> {
    if without_documentation_and_metadata(&variants) == 1 {
        return variants;
    }
    const USED: [&str; 5] = [
        CATEGORY,
        USAGE,
        NATIVE_TARGET,
        PLATFORM_TYPE,
        JVM_ENVIRONMENT,
    ];
    let unused = |variant: &Variant| {
        variant
            .attributes
            .iter()
            .filter(|(name, _)| !USED.contains(&name.as_str()))
            .count()
    };
    let Some(fewest) = variants.iter().map(|variant| unused(variant)).min() else {
        return variants;
    };
    let kept: Vec<&Variant> = variants
        .iter()
        .copied()
        .filter(|variant| unused(variant) == fewest)
        .collect();
    if without_documentation_and_metadata(&kept) == 1 {
        kept
    } else {
        variants
    }
}

/// When every candidate declares its bundling, the one with external dependencies, if that leaves
/// one (`filterMultipleVariantsByAttributePreferredValue`).
fn by_bundling(variants: Vec<&Variant>) -> Vec<&Variant> {
    if without_documentation_and_metadata(&variants) <= 1 {
        return variants;
    }
    let all_declare = variants
        .iter()
        .filter(|variant| !variant.is_documentation_or_metadata())
        .all(|variant| variant.attribute(BUNDLING).is_some());
    if all_declare {
        let external: Vec<&Variant> = variants
            .iter()
            .copied()
            .filter(|variant| matches!(variant.attribute(BUNDLING), None | Some("external")))
            .collect();
        if without_documentation_and_metadata(&external) == 1 {
            return external;
        }
    }
    variants
}

#[cfg(test)]
mod tests {
    use super::Requested;
    use crate::maven::artifact::Scope;
    use crate::maven::gradle_module::parse;

    fn names(variants: &[&crate::maven::gradle_module::Variant]) -> Vec<String> {
        variants
            .iter()
            .map(|variant| variant.name.clone())
            .collect()
    }

    #[test]
    fn the_jvm_variant_for_each_scope_is_chosen() {
        let variants = parse(
            r#"{"variants":[
              {"name":"metadataApiElements","attributes":{"org.gradle.category":"library","org.gradle.usage":"kotlin-metadata","org.jetbrains.kotlin.platform.type":"common"}},
              {"name":"jvmApiElements","attributes":{"org.gradle.category":"library","org.gradle.usage":"java-api","org.jetbrains.kotlin.platform.type":"jvm","org.gradle.jvm.environment":"standard-jvm"}},
              {"name":"jvmRuntimeElements","attributes":{"org.gradle.category":"library","org.gradle.usage":"java-runtime","org.jetbrains.kotlin.platform.type":"jvm","org.gradle.jvm.environment":"standard-jvm"}},
              {"name":"jsApiElements","attributes":{"org.gradle.category":"library","org.gradle.usage":"kotlin-api","org.jetbrains.kotlin.platform.type":"js"}},
              {"name":"sourcesElements","attributes":{"org.gradle.category":"documentation","org.gradle.docstype":"sources"}}
            ]}"#,
        )
        .expect("valid metadata");
        let requested = Requested {
            group: "g",
            module: "m",
            version: "1",
            is_bom: false,
        };
        assert_eq!(
            names(&requested.variants(&variants, Scope::Compile)),
            ["jvmApiElements", "sourcesElements"]
        );
        assert_eq!(
            names(&requested.variants(&variants, Scope::Runtime)),
            ["jvmRuntimeElements", "sourcesElements"]
        );
    }
}
