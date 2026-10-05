//! A classpath entry's `.kotlin_module` files: the package-part catalog and the optional annotation
//! classes.
//!
//! kotlinc's `JvmPackagePartProvider.addRoots` loads every `META-INF/<name>.kotlin_module` directly
//! inside a binary root, and `OptionalAnnotationClassesProvider` publishes each loaded module's
//! optional annotation classes. Those are read here with the entry's catalog, so the entry snapshot
//! that keys the catalog also keys them.

use super::{Classpath, JarPackages};
use crate::jvm::optional_annotations::{module_optional_annotations, OptionalAnnotationClass};

/// The optional annotation classes one classpath entry publishes, or why its module file could not
/// be read.
#[derive(Default)]
pub(super) struct EntryOptionalAnnotations {
    classes: Vec<OptionalAnnotationClass>,
    errors: Vec<String>,
}

/// kotlinc reads `META-INF`'s direct children whose name ends in `.kotlin_module`.
fn is_module_mapping(name: &str) -> bool {
    name.strip_prefix("META-INF/")
        .is_some_and(|file| !file.contains('/') && file.ends_with(".kotlin_module"))
}

/// Record one `.kotlin_module` file found at `name` (relative to the entry root).
pub(super) fn record_kotlin_module(name: &str, bytes: &[u8], jp: &mut JarPackages) {
    for (pkg, facades) in crate::jvm::metadata::read_kotlin_module(bytes) {
        let pkg_id = jp.names.insert(&pkg);
        let facades = facades
            .iter()
            .map(|facade| jp.names.insert(facade))
            .collect::<Vec<_>>();
        jp.facades.extend(facades.iter().copied());
        jp.packages
            .entry(pkg_id)
            .or_default()
            .facades
            .extend(facades);
    }
    if !is_module_mapping(name) {
        return;
    }
    match module_optional_annotations(bytes) {
        Ok(classes) => jp.optional_annotations.classes.extend(classes),
        Err(error) => jp
            .optional_annotations
            .errors
            .push(format!("{name}: {error}")),
    }
}

impl Classpath {
    /// Every entry's optional annotation classes in classpath order. A module file whose optional
    /// annotations cannot be decoded is an error, never an empty contribution.
    pub(in crate::jvm) fn optional_annotation_classes(
        &self,
    ) -> Result<Vec<OptionalAnnotationClass>, String> {
        let mut classes = Vec::new();
        for (entry_id, entry) in self.entries.iter().enumerate() {
            let packages = self.entry_packages(entry_id);
            if let Some(error) = packages.optional_annotations.errors.first() {
                return Err(format!(
                    "cannot read the Kotlin module of {}: {error}",
                    entry.path().display()
                ));
            }
            classes.extend(packages.optional_annotations.classes.iter().cloned());
        }
        Ok(classes)
    }
}

#[cfg(test)]
mod tests {
    use super::is_module_mapping;

    #[test]
    fn only_direct_meta_inf_module_files_carry_optional_annotations() {
        assert!(is_module_mapping("META-INF/kotlin-stdlib.kotlin_module"));
        assert!(!is_module_mapping("META-INF/versions/9/lib.kotlin_module"));
        assert!(!is_module_mapping("lib.kotlin_module"));
        assert!(!is_module_mapping("META-INF/MANIFEST.MF"));
    }
}
