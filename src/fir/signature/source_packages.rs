//! Stable source-package inventory used by qualified-name resolution in Pass 2.

use super::{ResolvedModuleIndex, SourceFileId, TypeName};

impl ResolvedModuleIndex {
    /// Whether the finalized source module contributes the direct package child `name` below
    /// `parent`.
    ///
    /// The answer comes only from stable source-package identities; retaining the legacy
    /// `SymbolTable::source_packages` prefix set would duplicate the same header fact across the
    /// pass boundary.
    pub(crate) fn source_package_child_exists(&self, parent: TypeName, name: &str) -> bool {
        let Some(candidate) = crate::types::existing_type_name_child(parent, name) else {
            return false;
        };
        self.source_package_namespaces.contains(&candidate)
    }

    pub(in crate::fir) fn publish_source_package(
        &mut self,
        source: SourceFileId,
        package: TypeName,
    ) {
        assert!(
            self.source_packages.insert(source, package).is_none(),
            "a source unit may publish one package identity"
        );
        // Once a namespace is present, all of its enclosing namespaces are present too.
        let mut current = Some(package);
        while let Some(namespace) = current {
            if namespace == TypeName::ROOT || !self.source_package_namespaces.insert(namespace) {
                break;
            }
            current = namespace.parent();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_are_published_packages_and_their_enclosing_packages() {
        let package = crate::types::type_name;
        let mut index = ResolvedModuleIndex::default();
        for (raw, name) in [
            (0, "pkgindex/app/core"),
            (1, "pkgindex/app/core"),
            (2, "pkgindex/app/model"),
            (3, "pkgindex/tools"),
        ] {
            index.publish_source_package(SourceFileId::from_raw(raw), package(name));
        }
        index.publish_source_package(SourceFileId::from_raw(4), TypeName::ROOT);

        let child = |parent: &str, name: &str| {
            let parent = if parent.is_empty() {
                TypeName::ROOT
            } else {
                package(parent)
            };
            index.source_package_child_exists(parent, name)
        };
        assert!(child("", "pkgindex"));
        assert!(child("pkgindex", "app"));
        assert!(child("pkgindex", "tools"));
        assert!(child("pkgindex/app", "core"));
        assert!(child("pkgindex/app", "model"));
        assert!(!child("pkgindex", "core"), "only a direct child");
        assert!(!child("pkgindex/tools", "core"));
        assert!(!child("", "app"));
        assert!(!child("pkgindex/app/core", "model"));
    }
}
