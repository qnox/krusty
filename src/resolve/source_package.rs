//! Dotted source-package spellings as interned package identities.

use std::sync::OnceLock;

use crate::types::{type_name_child, TypeName};

/// The package of a source file, interned one segment at a time.
/// An absent or empty package is the root. The conversion does not build a joined spelling.
pub(super) fn identity(package: Option<&str>) -> TypeName {
    let Some(package) = package.filter(|spelling| !spelling.is_empty()) else {
        return TypeName::ROOT;
    };
    package.split('.').fold(TypeName::ROOT, type_name_child)
}

/// Kotlin's common default imports, interned once for the process.
pub(super) fn kotlin_default_packages() -> &'static [TypeName] {
    static PACKAGES: OnceLock<Vec<TypeName>> = OnceLock::new();
    PACKAGES.get_or_init(|| {
        super::KOTLIN_DEFAULT_IMPORT_PACKAGES
            .iter()
            .copied()
            .map(|package| identity(Some(package)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    #[test]
    fn dotted_package_matches_the_slashed_identity() {
        assert_eq!(identity(Some("com.example")), type_name("com/example"));
        assert_eq!(identity(Some("kotlin")), type_name("kotlin"));
        assert_eq!(
            identity(Some("kotlin.collections")),
            type_name("kotlin/collections")
        );
        assert_eq!(identity(Some("a.b.c.d")), type_name("a/b/c/d"));
        assert_eq!(identity(None), TypeName::ROOT);
        assert_eq!(identity(Some("")), TypeName::ROOT);
        assert_eq!(identity(Some("com.example")), identity(Some("com.example")));
    }

    #[test]
    fn kotlin_default_packages_match_their_slashed_identities() {
        let defaults = kotlin_default_packages();
        assert_eq!(
            defaults.len(),
            super::super::KOTLIN_DEFAULT_IMPORT_PACKAGES.len()
        );
        for (package, dotted) in defaults
            .iter()
            .zip(super::super::KOTLIN_DEFAULT_IMPORT_PACKAGES)
        {
            assert_eq!(*package, type_name(&dotted.replace('.', "/")));
        }
        assert!(std::ptr::eq(kotlin_default_packages(), defaults));
    }
}
