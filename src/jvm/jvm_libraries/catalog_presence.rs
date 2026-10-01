//! Promotion of exact catalog entries into semantic classifier identities.
//!
//! The classpath provider validates catalog completeness during initialization. Resolution then has
//! one presence source: the identity catalog. A miss stays a miss and never formats a JVM spelling,
//! probes bytes, or interns a speculative semantic child.

use super::JvmLibraries;
use crate::symbol_source::SymbolNamespace;
use crate::types::TypeName;

impl JvmLibraries {
    pub(super) fn proven_classifier(
        &self,
        namespace: SymbolNamespace,
        name: &str,
    ) -> Option<TypeName> {
        let tree = self.cp.package_tree();
        let declared = match namespace {
            SymbolNamespace::Package(package) => tree.contains_exact_class(package, name),
            SymbolNamespace::Classifier(owner) => tree.contains_nested_class(owner, name),
        };
        declared.then(|| match namespace {
            SymbolNamespace::Package(package) => crate::types::type_name_child(package, name),
            SymbolNamespace::Classifier(owner) => crate::types::type_name_nested_child(owner, name),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    #[test]
    fn nested_classifier_probe_reads_the_catalog_without_interning_a_miss() {
        let directory = std::env::temp_dir().join(format!(
            "krusty-nested-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(directory.join("probe/nest6044")).expect("create package");
        let write = |internal: &str| {
            let bytes =
                crate::jvm::classfile::ClassWriter::new(internal, "java/lang/Object").finish();
            std::fs::write(directory.join(format!("{internal}.class")), bytes)
                .expect("write class");
        };
        write("probe/nest6044/Outer");
        write("probe/nest6044/Outer$Inner");
        let libraries = JvmLibraries::new(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            vec![directory.clone()],
        )))
        .expect("JVM provider initialization");
        let owner = type_name("probe/nest6044/Outer");
        let found = libraries.symbols(SymbolNamespace::Classifier(owner), "Inner");
        assert_eq!(
            found.classifier_name,
            Some(type_name("probe/nest6044/Outer$Inner"))
        );
        assert!(libraries
            .symbols(SymbolNamespace::Classifier(owner), "Missing")
            .is_empty());
        assert!(crate::types::existing_type_name("probe/nest6044/Outer$Missing").is_none());

        drop(libraries);
        std::fs::remove_dir_all(directory).expect("remove nested probe directory");
    }
}
